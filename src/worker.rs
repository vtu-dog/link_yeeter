//! Worker module, responsible for processing download tasks.

use crate::{
    media,
    queue::{AcceptPermit, QueuePosition, TaskQueue, TentativeToken},
    task::{ProcessingError, Task, TaskOutput},
    utils,
};
use futures::StreamExt;
use teloxide::types::InputFile;
use tempfile::TempDir;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn};

/// A worker that processes download tasks.
pub struct Worker {
    /// Queue state: task queue, tentative counter, busy flag, accept ordering.
    queue: TaskQueue,
}

impl Worker {
    /// Creates a new [`Worker`].
    pub fn new() -> Self {
        Self {
            queue: TaskQueue::new(),
        }
    }

    /// Returns the current size of the queue.
    pub fn queue_size(&self) -> usize {
        self.queue.queue_size()
    }

    /// Tentatively accepts a task, returning a token, its queue position, and
    /// an accept permit.
    pub fn tentative_enqueue(&self) -> (TentativeToken, QueuePosition, AcceptPermit) {
        self.queue.tentative_enqueue()
    }

    /// Pushes a task onto the queue, consuming the tentative token.
    pub fn push(&self, item: Task, token: TentativeToken) {
        self.queue.push(item, token);
    }

    /// Starts the worker, spawning a background task that runs until cancelled.
    pub fn start(&self, cancellation_token: CancellationToken) -> tokio::task::JoinHandle<()> {
        let queue = self.queue.clone();

        tokio::spawn(async move {
            info!("worker started");
            loop {
                select! {
                    biased; // always check for cancellation first
                    () = cancellation_token.cancelled() => {
                        info!("worker cancelled via token");
                        break;
                    }
                    task = queue.pop() => {
                        let url = task.url.clone();
                        let remaining = queue.len();
                        info!(url = %url, remaining_in_queue = remaining, "worker picked up task");

                        queue.acquire_busy_guard()
                            .then(|| handle_task(task))
                            .await;

                        debug!(url = %url, "worker finished processing task");
                    }
                }
            }
            info!("worker stopped");
        })
    }

    /// Calculates the maximum bitrate that fits within [`media::TELEGRAM_FILESIZE_LIMIT_MB`].
    ///
    /// Returns [`None`] if `duration` is zero, as bitrate cannot be computed.
    fn calculate_max_api_bitrate(duration: u32) -> Option<u32> {
        if duration == 0 {
            return None;
        }

        // 1 MB = 8000 kbit
        let br_no_audio = f64::from(media::TELEGRAM_FILESIZE_LIMIT_MB * 8000) / f64::from(duration);
        // reserve 128 kbps for audio
        let br_with_audio = br_no_audio - 128.0;
        // reduce by 3% to account for container overhead
        let br_container = br_with_audio * 0.97;

        // `.max(0.0)` ensures the value is non-negative before the cast
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "value is clamped non-negative by .max(0.0); truncation of a kbps float to u32 is intentional"
        )]
        Some(br_container.max(0.0) as u32)
    }

    /// Validates bitrate against API limits.
    ///
    /// Returns an error if the bitrate would need to be reduced too much
    /// (more than 15% reduction) and fallback mode is not enabled.
    fn validate_bitrate(
        original_bitrate: u32,
        max_api_bitrate: Option<u32>,
        enable_fallback: bool,
    ) -> Result<(), ProcessingError> {
        let Some(max_api_br) = max_api_bitrate else {
            return Ok(());
        };

        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "cutoff is 85% of a non-negative bitrate value, always fits in u32"
        )]
        let cutoff = (f64::from(original_bitrate) * 0.85) as u32;

        debug!(
            original_bitrate,
            max_api_bitrate = max_api_br,
            cutoff,
            "bitrate calculation"
        );

        if max_api_br < cutoff && !enable_fallback {
            warn!(
                original_bitrate,
                max_api_bitrate = max_api_br,
                "API-adjusted bitrate too low, rejecting"
            );
            return Err(ProcessingError::BitrateTooHigh);
        }

        Ok(())
    }
}

/// Handles a download task and sends the result back via the task's return channel.
async fn handle_task(task: Task) {
    let url = &task.url;
    let res = handle_task_internal(&task).await;

    let is_success = res.is_ok();
    match task.return_channel.send(res.map(Box::new)) {
        Ok(()) => {
            if is_success {
                debug!(url = %url, "task result sent to caller");
            } else {
                debug!(url = %url, "task error sent to caller");
            }
        }
        Err(_e) => {
            error!(url = %url, "failed to send task result: channel closed (caller may have timed out)");
        }
    }
}

/// Handles a download task end-to-end: download, probe, convert, thumbnail.
#[instrument(level = "debug", skip(task), fields(url = %task.url, fallback = task.enable_fallback))]
async fn handle_task_internal(task: &Task) -> Result<TaskOutput, ProcessingError> {
    use crate::env;

    // prepare a temp arena for files
    debug!("creating temp directory");
    let temp_dir = TempDir::new().map_err(ProcessingError::TempDir)?;
    let output_dir_path = TempDir::path(&temp_dir);

    // download the video
    debug!("starting yt-dlp download");
    media::download(
        &task.url,
        &output_dir_path.to_string_lossy(),
        task.enable_fallback,
    )
    .await?;
    debug!("yt-dlp download completed");

    // find all files in the directory
    let files = async_fs::read_dir(temp_dir.path())
        .await
        .map_err(ProcessingError::ReadDir)?
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    let num_files = files.len();
    debug!(num_files, "found files in temp directory");
    if num_files != 1 {
        warn!(num_files, "unexpected number of files");
        return Err(ProcessingError::UnexpectedFileCount(num_files));
    }

    let entry = files.first().unwrap().path();
    let bytes = entry
        .metadata()
        .map_err(ProcessingError::FileMetadata)?
        .len();
    let megabytes = bytes / 1_000_000;
    debug!(bytes, megabytes, "downloaded file size");

    // compare in bytes
    // dividing into megabytes first would admit files up to 1 MB over the limit
    let limit = if task.enable_fallback {
        *env::FALLBACK_FILESIZE
    } else {
        *env::MAX_FILESIZE
    };

    if bytes > limit * 1_000_000 {
        warn!(
            megabytes,
            limit,
            fallback = task.enable_fallback,
            "file exceeds size limit"
        );
        return Err(ProcessingError::FileTooLarge { limit });
    }

    // extract video metadata
    let entry_path = entry.to_string_lossy();
    debug!("extracting video metadata via ffprobe");
    let metadata = media::ffprobe(&entry_path).unwrap_or_default();
    debug!(
        duration_secs = metadata.duration,
        bitrate_kbps = metadata.bitrate,
        width = metadata.width,
        height = metadata.height,
        "video metadata extracted"
    );

    // calculate and validate bitrate
    let max_bitrate = Worker::calculate_max_api_bitrate(metadata.duration);
    Worker::validate_bitrate(metadata.bitrate, max_bitrate, task.enable_fallback)?;

    // convert video to mp4
    let output_filename = format!("{}.mp4", utils::random_string(10));
    let output_pathbuf = output_dir_path.join(output_filename);
    let output_path = output_pathbuf.to_string_lossy();

    let original_bitrate = metadata.bitrate;
    let (target_bitrate, is_bitrate_reduced) = match max_bitrate {
        Some(max) if original_bitrate < max => (Some(original_bitrate), false),
        Some(max) => (Some(max), true),
        None => (None, false),
    };

    debug!(
        target_bitrate,
        is_bitrate_reduced, "starting video conversion"
    );
    media::convert(&entry_path, &output_path, target_bitrate).await?;
    debug!("video conversion completed");

    debug!("extracting thumbnail");
    let maybe_thumbnail = media::get_thumbnail(&output_path).await;
    debug!(
        has_thumbnail = maybe_thumbnail.is_some(),
        "thumbnail extraction finished"
    );

    Ok(TaskOutput {
        _dir: temp_dir,
        video_file: InputFile::file(output_pathbuf),
        maybe_thumbnail,
        metadata,
        reduced_bitrate: if is_bitrate_reduced {
            target_bitrate
        } else {
            None
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    mod calculate_max_api_bitrate {
        use super::*;

        #[test]
        fn returns_none_for_zero_duration() {
            assert_eq!(Worker::calculate_max_api_bitrate(0), None);
        }

        #[test]
        fn returns_some_for_nonzero_duration() {
            assert!(Worker::calculate_max_api_bitrate(60).is_some());
        }

        #[test]
        fn shorter_video_allows_higher_bitrate() {
            let short = Worker::calculate_max_api_bitrate(10).unwrap();
            let long = Worker::calculate_max_api_bitrate(100).unwrap();
            assert!(short > long);
        }

        #[test]
        fn calculates_expected_bitrate_for_60s_video() {
            // formula: ((50 * 8000) / 60 - 128) * 0.97
            // = (400000 / 60 - 128) * 0.97
            // = (6666.67 - 128) * 0.97
            // = 6538.67 * 0.97
            // ~ 6342
            let result = Worker::calculate_max_api_bitrate(60).unwrap();
            assert!(result > 6000 && result < 6500, "got {result}");
        }

        #[test]
        fn calculates_expected_bitrate_for_10min_video() {
            // 10 minutes = 600 seconds
            // ((50 * 8000) / 600 - 128) * 0.97
            // = (666.67 - 128) * 0.97
            // ~ 522
            let result = Worker::calculate_max_api_bitrate(600).unwrap();
            assert!(result > 400 && result < 600, "got {result}");
        }
    }

    mod validate_bitrate {
        use super::*;

        #[test]
        fn allows_any_bitrate_when_no_api_limit() {
            assert!(Worker::validate_bitrate(10000, None, false).is_ok());
        }

        #[test]
        fn allows_bitrate_within_threshold() {
            // 15% reduction: 10000 -> 8500 is at the threshold
            assert!(Worker::validate_bitrate(10000, Some(8500), false).is_ok());
        }

        #[test]
        fn allows_bitrate_above_threshold() {
            // less than 15% reduction
            assert!(Worker::validate_bitrate(10000, Some(9000), false).is_ok());
        }

        #[test]
        fn rejects_bitrate_below_threshold_without_fallback() {
            // more than 15% reduction: 10000 -> 8400 (16% reduction)
            assert!(Worker::validate_bitrate(10000, Some(8400), false).is_err());
        }

        #[test]
        fn allows_bitrate_below_threshold_with_fallback() {
            // with fallback enabled, any reduction is allowed
            assert!(Worker::validate_bitrate(10000, Some(1000), true).is_ok());
        }

        #[test]
        fn handles_zero_original_bitrate() {
            // edge case: original bitrate is 0
            assert!(Worker::validate_bitrate(0, Some(0), false).is_ok());
        }

        #[test]
        fn rejects_zero_api_limit_without_fallback() {
            // if max is 0 and original > 0, reduction exceeds threshold
            assert!(Worker::validate_bitrate(1000, Some(0), false).is_err());
        }
    }
}
