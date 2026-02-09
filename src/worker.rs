//! Worker module, responsible for processing download tasks.

use crate::{
    env,
    task::{Task, TaskOutput},
    utils,
};

use std::sync::Arc;

use anyhow::{Context, bail};
use deadqueue::unlimited::Queue;
use futures::StreamExt;
use teloxide::types::InputFile;
use tempfile::TempDir;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn};

/// A worker that processes download tasks.
pub struct Worker {
    /// Queue of tasks to be processed.
    queue: Arc<Queue<Task>>,
    /// Internal state of the worker.
    state: Arc<std::sync::Mutex<InternalState>>,
}

/// Internal `Worker` state.
struct InternalState {
    /// Counter for tentatively accepted tasks not yet in the queue.
    tentative: usize,
    /// Flag indicating whether the worker is currently processing a task.
    is_busy: bool,
}

/// RAII guard that sets `is_busy` to `true` on creation and `false` on drop.
struct BusyGuard<'a>(&'a std::sync::Mutex<InternalState>);

impl<'a> BusyGuard<'a> {
    /// Acquire the busy guard, setting `is_busy` to `true`.
    fn acquire(state: &'a std::sync::Mutex<InternalState>) -> Self {
        state.lock().unwrap().is_busy = true;
        Self(state)
    }

    /// Execute an async closure while holding the guard, then drop it.
    async fn then<F, Fut, R>(self, f: F) -> R
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = R>,
    {
        let result = f().await;
        drop(self);
        result
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap().is_busy = false;
    }
}

/// Position in the task queue at the time of enqueueing.
pub type QueuePosition = usize;

/// Token representing a tentatively accepted task.
///
/// Automatically decrements the tentative counter on drop (cancel path).
/// Call [`TentativeToken::disarm`] to consume the token without cancelling.
pub struct TentativeToken {
    /// Shared worker state; `Some` while active, `None` after disarming.
    state: Option<Arc<std::sync::Mutex<InternalState>>>,
}

impl TentativeToken {
    /// Disarm the token, preventing the automatic cancel on drop.
    fn disarm(mut self) {
        self.state.take();
    }
}

impl Drop for TentativeToken {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            let mut st = state.lock().unwrap();
            let old_tentative = st.tentative;
            st.tentative = st.tentative.saturating_sub(1);
            debug!(
                old_tentative,
                new_tentative = st.tentative,
                "cancelled tentative task"
            );
        }
    }
}

impl Worker {
    /// Create a new `Worker` instance.
    pub fn new() -> Self {
        Self {
            queue: Arc::new(Queue::new()),
            state: Arc::new(std::sync::Mutex::new(InternalState {
                tentative: 0,
                is_busy: false,
            })),
        }
    }

    /// Get the current size of the queue.
    pub fn queue_size(&self) -> usize {
        let st = self.state.lock().unwrap();
        self.queue.len() + st.tentative + usize::from(st.is_busy)
    }

    /// Tentatively accept a task, returning a token and the current queue position.
    ///
    /// The token increments the tentative counter immediately. If dropped
    /// without being committed via [`Self::push`], it decrements the counter
    /// automatically.
    pub fn tentative_enqueue(&self) -> (TentativeToken, QueuePosition) {
        let mut st = self.state.lock().unwrap();

        let qsize = self.queue.len() + st.tentative + usize::from(st.is_busy);

        st.tentative += 1;
        drop(st);

        let token = TentativeToken {
            state: Some(Arc::clone(&self.state)),
        };

        (token, qsize)
    }

    /// Push a task onto the queue, consuming the tentative token.
    #[allow(clippy::significant_drop_tightening)] // late drop ensures consistency
    pub fn push(&self, item: Task, token: TentativeToken) {
        token.disarm();

        let mut st = self.state.lock().unwrap();
        st.tentative -= 1;
        let queue_len = self.queue.len() + 1; // +1 for the item we're about to push
        debug!(
            url = %item.url,
            queue_len,
            tentative = st.tentative,
            "task pushed to queue"
        );
        self.queue.push(item);
    }

    /// Start the worker.
    pub fn start(&self, cancellation_token: CancellationToken) -> tokio::task::JoinHandle<()> {
        let queue_inner = self.queue.clone();
        let state_inner = Arc::clone(&self.state);

        tokio::spawn(async move {
            info!("worker started");
            loop {
                select! {
                    biased; // always go for token first
                    () = cancellation_token.cancelled() => {
                        info!("worker cancelled via token");
                        break;
                    }
                    task = queue_inner.pop() => {
                        let url = task.url.clone();
                        let remaining = queue_inner.len();
                        info!(url = %url, remaining_in_queue = remaining, "worker picked up task");

                        BusyGuard::acquire(&state_inner)
                            .then(|| Self::handle_task(task))
                            .await;

                        debug!(url = %url, "worker finished processing task");
                    }
                }
            }
            info!("worker stopped");
        })
    }

    /// Handle a download task and send the result back to the caller.
    async fn handle_task(task: Task) {
        let url = &task.url;
        let res = Self::handle_task_internal(&task).await;

        let is_success = res.is_ok();
        match task
            .return_channel
            .send(res.map(std::boxed::Box::new).map_err(|x| x.to_string()))
        {
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

    /// Calculate the maximum bitrate allowed for Telegram's API limits.
    ///
    /// Returns `None` if duration is zero (can't calculate bitrate).
    fn calculate_max_api_bitrate(duration: u32) -> Option<u32> {
        if duration == 0 {
            return None;
        }

        let api_limit = 50; // MB
        let br_no_audio = f64::from(api_limit * 8000) / f64::from(duration);
        // reserve 128 kbps for audio
        let br_with_audio = br_no_audio - 128.0;
        // reduce by 3% to account for container overhead
        let br_container = br_with_audio * 0.97;

        // SAFETY: `.max(0.0)` ensures non-negative value
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(br_container.max(0.0) as u32)
    }

    /// Validate bitrate against API limits.
    ///
    /// Returns an error if the bitrate would need to be reduced too much
    /// (more than 15% reduction) and fallback mode is not enabled.
    fn validate_bitrate(
        original_bitrate: u32,
        max_api_bitrate: Option<u32>,
        enable_fallback: bool,
    ) -> anyhow::Result<()> {
        let Some(max_api_br) = max_api_bitrate else {
            return Ok(());
        };

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
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
            bail!("API-adjusted bitrate is too low");
        }

        Ok(())
    }

    /// Handle a download task.
    #[instrument(level = "debug", skip(task), fields(url = %task.url, fallback = task.enable_fallback))]
    async fn handle_task_internal(task: &Task) -> anyhow::Result<TaskOutput> {
        // prepare a temp arena for files
        debug!("creating temp directory");
        let temp_dir = TempDir::new().context("could not create temp dir")?;
        let output_dir_path = TempDir::path(&temp_dir);

        // download the video
        debug!("starting yt-dlp download");
        utils::download(
            &task.url,
            &output_dir_path.to_string_lossy(),
            task.enable_fallback,
        )
        .await?;
        debug!("yt-dlp download completed");

        // find all files in the directory
        let files = async_fs::read_dir(temp_dir.path())
            .await
            .context("could not read tempdir contents")?
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();

        let num_files = files.len();
        debug!(num_files, "found files in temp directory");
        if num_files != 1 {
            warn!(num_files, "unexpected number of files");
            bail!("{num_files} files found, expected 1")
        }

        let entry = files.first().unwrap().path();
        let bytes = entry
            .metadata()
            .context("could not read file metadata")?
            .len();
        let megabytes = bytes / 1000 / 1000;
        debug!(bytes, megabytes, "downloaded file size");

        let size_err = |mbs: u64| format!("base file size exceeded {mbs} MB");

        if task.enable_fallback && (megabytes > *env::FALLBACK_FILESIZE) {
            warn!(
                megabytes,
                limit = *env::FALLBACK_FILESIZE,
                "file exceeds fallback size limit"
            );
            bail!(size_err(*env::FALLBACK_FILESIZE));
        } else if megabytes > *env::MAX_FILESIZE {
            warn!(
                megabytes,
                limit = *env::MAX_FILESIZE,
                "file exceeds max size limit"
            );
            bail!(size_err(*env::MAX_FILESIZE));
        }

        // extract video metadata
        let entry_path = entry.to_string_lossy();
        debug!("extracting video metadata via ffprobe");
        let metadata = utils::ffprobe(&entry_path).unwrap_or_default();
        debug!(
            duration_secs = metadata.duration,
            bitrate_kbps = metadata.bitrate,
            width = metadata.width,
            height = metadata.height,
            "video metadata extracted"
        );

        // calculate and validate bitrate
        let max_bitrate = Self::calculate_max_api_bitrate(metadata.duration);
        Self::validate_bitrate(metadata.bitrate, max_bitrate, task.enable_fallback)?;

        // convert video to mp4
        let output_filename = format!("{}.mp4", utils::random_string(10));
        let output_pathbuf = output_dir_path.join(output_filename);
        let output_path = output_pathbuf.to_string_lossy();

        let original_bitrate = metadata.bitrate;
        let (target_bitrate, is_bitrate_reduced) = if original_bitrate < max_bitrate.unwrap_or(0) {
            (Some(original_bitrate), false)
        } else {
            (max_bitrate, true)
        };

        debug!(
            target_bitrate,
            is_bitrate_reduced, "starting video conversion"
        );
        utils::convert(&entry_path, &output_path, target_bitrate).await?;
        debug!("video conversion completed");

        debug!("extracting thumbnail");
        let maybe_thumbnail = utils::get_thumbnail(&output_path).await;
        debug!(
            has_thumbnail = maybe_thumbnail.is_some(),
            "thumbnail extraction finished"
        );

        Ok(TaskOutput {
            _dir: temp_dir,
            video_file: InputFile::file(output_pathbuf.clone()),
            maybe_thumbnail,
            metadata,
            reduced_bitrate: if is_bitrate_reduced {
                target_bitrate
            } else {
                None
            },
        })
    }
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
            // Formula: ((50 * 8000) / 60 - 128) * 0.97
            // = (400000 / 60 - 128) * 0.97
            // = (6666.67 - 128) * 0.97
            // = 6538.67 * 0.97
            // ≈ 6342
            let result = Worker::calculate_max_api_bitrate(60).unwrap();
            assert!(result > 6000 && result < 6500, "got {result}");
        }

        #[test]
        fn calculates_expected_bitrate_for_10min_video() {
            // 10 minutes = 600 seconds
            // ((50 * 8000) / 600 - 128) * 0.97
            // = (666.67 - 128) * 0.97
            // ≈ 522
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
            // Less than 15% reduction
            assert!(Worker::validate_bitrate(10000, Some(9000), false).is_ok());
        }

        #[test]
        fn rejects_bitrate_below_threshold_without_fallback() {
            // More than 15% reduction: 10000 -> 8400 (16% reduction)
            assert!(Worker::validate_bitrate(10000, Some(8400), false).is_err());
        }

        #[test]
        fn allows_bitrate_below_threshold_with_fallback() {
            // With fallback enabled, any reduction is allowed
            assert!(Worker::validate_bitrate(10000, Some(1000), true).is_ok());
        }

        #[test]
        fn handles_zero_original_bitrate() {
            // Edge case: original bitrate is 0
            assert!(Worker::validate_bitrate(0, Some(0), false).is_ok());
        }

        #[test]
        fn rejects_zero_api_limit_without_fallback() {
            // If max is 0 and original > 0, reduction exceeds threshold
            assert!(Worker::validate_bitrate(1000, Some(0), false).is_err());
        }
    }

    mod queue_operations {
        use super::*;
        use tokio::sync::oneshot;

        fn make_test_task(url: &str) -> Task {
            let (tx, _rx) = oneshot::channel();
            Task {
                url: url.to_string(),
                enable_fallback: false,
                return_channel: tx,
            }
        }

        #[test]
        fn new_worker_has_empty_queue() {
            let worker = Worker::new();
            assert_eq!(worker.queue_size(), 0);
        }

        #[test]
        fn tentative_enqueue_increments_size() {
            let worker = Worker::new();
            let (token, pos) = worker.tentative_enqueue();
            assert_eq!(pos, 0);
            assert_eq!(worker.queue_size(), 1);
            drop(token);
        }

        #[test]
        fn multiple_tentative_enqueues_stack() {
            let worker = Worker::new();
            let (t1, p1) = worker.tentative_enqueue();
            assert_eq!(p1, 0);
            let (t2, p2) = worker.tentative_enqueue();
            assert_eq!(p2, 1);
            let (t3, p3) = worker.tentative_enqueue();
            assert_eq!(p3, 2);
            assert_eq!(worker.queue_size(), 3);
            drop((t1, t2, t3));
        }

        #[test]
        fn drop_token_decrements_size() {
            let worker = Worker::new();
            let (t1, _) = worker.tentative_enqueue();
            let (t2, _) = worker.tentative_enqueue();
            assert_eq!(worker.queue_size(), 2);

            drop(t1);
            assert_eq!(worker.queue_size(), 1);

            drop(t2);
            assert_eq!(worker.queue_size(), 0);
        }

        #[test]
        fn push_moves_from_tentative_to_queue() {
            let worker = Worker::new();
            let (token, _) = worker.tentative_enqueue();
            assert_eq!(worker.queue_size(), 1);

            worker.push(make_test_task("https://example.com"), token);
            // Size should still be 1: tentative decremented, queue incremented
            assert_eq!(worker.queue_size(), 1);
        }

        #[test]
        fn push_multiple_tasks() {
            let worker = Worker::new();
            let (t1, _) = worker.tentative_enqueue();
            let (t2, _) = worker.tentative_enqueue();
            let (t3, _) = worker.tentative_enqueue();

            worker.push(make_test_task("https://a.com"), t1);
            worker.push(make_test_task("https://b.com"), t2);
            worker.push(make_test_task("https://c.com"), t3);

            assert_eq!(worker.queue_size(), 3);
        }
    }
}
