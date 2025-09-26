//! Types related to processing download tasks.

use teloxide::types::InputFile;
use tempfile::TempDir;
use tokio::sync::oneshot;

/// Represents the output of a processed `Task`.
pub struct TaskOutput {
    /// Handle to a directory containing video files, passed around to defer `Drop`.
    pub _dir: TempDir,
    /// Contents of a file to be uploaded.
    pub video_file: InputFile,
    /// Thumbnail, if able to be extracted.
    pub maybe_thumbnail: Option<InputFile>,
    /// Metadata of the video file.
    pub metadata: crate::utils::Probe,
    /// Either `None`, or reduced bitrate value.
    pub reduced_bitrate: Option<u32>,
}

impl std::fmt::Debug for TaskOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskOutput")
            .field(
                "maybe_thumbnail",
                if self.maybe_thumbnail.is_some() {
                    &"Some(_)"
                } else {
                    &"None"
                },
            )
            .field("metadata", &self.metadata)
            .field("reduced_bitrate", &self.reduced_bitrate)
            .finish_non_exhaustive()
    }
}

/// Possible result of processing a `Task`.
///
/// `Ok` variant boxed to prevent stack blowup - might not be necessary though.
pub type TaskResult = Result<Box<TaskOutput>, String>;

/// A task created by a user, to be processed by a `Worker` and sent back.
#[derive(Debug)]
pub struct Task {
    /// URL of the video to be processed.
    pub url: String,
    /// Whether to enable fallback processing.
    pub enable_fallback: bool,
    /// Channel to send the result back to the sender.
    pub return_channel: oneshot::Sender<TaskResult>,
}
