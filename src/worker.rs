//! Worker module, responsible for processing download tasks.

use crate::{
    env,
    task::{Task, TaskOutput},
    utils,
};

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use color_eyre::eyre::{WrapErr, bail};
use deadqueue::unlimited::Queue;
use futures::StreamExt;
use teloxide::types::InputFile;
use tempfile::TempDir;
use tokio::select;
use tokio_util::sync::CancellationToken;

/// A worker that processes download tasks.
pub struct Worker {
    queue: Arc<Queue<Task>>,
    pub is_busy: Arc<AtomicBool>,
}

impl Worker {
    /// Create a new `Worker` instance.
    pub fn new() -> Self {
        Self {
            queue: Arc::new(Queue::new()),
            is_busy: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Get the size of the queue.
    pub fn queue_size(&self) -> usize {
        self.queue.len()
    }

    /// Push a task onto the queue.
    pub fn push(&self, item: Task) {
        self.queue.push(item);
    }

    /// Start the worker.
    pub fn start(&self, cancellation_token: CancellationToken) -> tokio::task::JoinHandle<()> {
        let queue_inner = self.queue.clone();
        let busy_inner = Arc::clone(&self.is_busy);

        tokio::spawn(async move {
            tracing::debug!("worker started");
            loop {
                select! {
                    biased; // always go for token first
                    () = cancellation_token.cancelled() => {
                        tracing::debug!("worker cancelled");
                        break;
                    }
                    task = queue_inner.pop() => {
                        let ord = Ordering::Release;
                        busy_inner.store(true, ord);
                        Self::handle_task(task).await;
                        busy_inner.store(false, ord);
                    }
                }
            }
            tracing::debug!("worker stopped");
        })
    }

    /// Handle a download task and send the result back to the caller.
    async fn handle_task(task: Task) {
        let res = Self::handle_task_internal(&task).await;
        match task
            .return_channel
            .send(res.map(std::boxed::Box::new).map_err(|x| x.to_string()))
        {
            Ok(()) => (),
            Err(_e) => tracing::error!("failed to send task result: channel closed"),
        }
    }

    /// Handle a download task.
    async fn handle_task_internal(task: &Task) -> color_eyre::Result<TaskOutput> {
        // prepare a temp arena for files
        let temp_dir = TempDir::new().wrap_err("could not create temp dir")?;
        let output_dir_path = TempDir::path(&temp_dir);

        // download the video
        utils::download(
            &task.url,
            &output_dir_path.to_string_lossy(),
            task.enable_fallback,
        )
        .await?;

        // find all files in the directory
        let files = async_fs::read_dir(temp_dir.path())
            .await
            .wrap_err("could not read tempdir contents")?
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();

        let num_files = files.len();
        if num_files != 1 {
            bail!("{num_files} files found, expected 1")
        }

        let entry = files.first().unwrap().path();
        let bytes = entry.metadata().unwrap().len();
        let megabytes = bytes / 1000 / 1000;

        let size_err = |mbs: u64| format!("base file size exceeded {mbs} MB");

        if task.enable_fallback && (megabytes > *env::FALLBACK_FILESIZE) {
            bail!(size_err(*env::FALLBACK_FILESIZE));
        } else if megabytes > *env::MAX_FILESIZE {
            bail!(size_err(*env::MAX_FILESIZE));
        }

        // extract video metadata
        let entry_path = entry.to_string_lossy();
        let metadata = utils::ffprobe(&entry_path).unwrap_or_default();

        let max_bitrate: Option<u32> = if metadata.duration != 0 {
            let api_limit = 50; // megabytes

            let br_no_audio = f64::from(api_limit * 8000) / f64::from(metadata.duration);
            // notice that we reserved 128 kbps for the audio
            let br_with_audio = br_no_audio - 128.0;
            // reduce by 3% to account for container overhead
            let br_container = br_with_audio * 0.97;

            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Some(br_container as u32)
        } else {
            None
        };

        // bail if the new bitrate is less than 85% of original...
        // ...unless fallback is enabled
        let original_bitrate = metadata.bitrate;

        if let Some(max_api_br) = max_bitrate {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let cutoff = (f64::from(original_bitrate) * 0.85) as u32;
            if max_api_br < cutoff {
                bail!("API-adjusted bitrate is too low");
            }
        }

        // convert video to mp4
        let output_filename = format!("{}.mp4", utils::random_string(10));
        let output_pathbuf = output_dir_path.join(output_filename);
        let output_path = output_pathbuf.to_string_lossy();

        let target_bitrate;
        let is_bitrate_reduced = if original_bitrate < max_bitrate.unwrap_or(0) {
            target_bitrate = Some(original_bitrate);
            false
        } else {
            target_bitrate = max_bitrate;
            true
        };

        utils::convert(&entry_path, &output_path, target_bitrate).await?;

        Ok(TaskOutput {
            _dir: temp_dir,
            video_file: InputFile::file(output_pathbuf.clone()),
            maybe_thumbnail: utils::get_thumbnail(&output_path).await,
            metadata,
            reduced_bitrate: if is_bitrate_reduced {
                target_bitrate
            } else {
                None
            },
        })
    }
}
