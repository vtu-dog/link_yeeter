//! Types related to processing download tasks.

use teloxide::types::InputFile;
use tempfile::TempDir;
use tokio::sync::oneshot;

/// Error produced by the video processing pipeline.
#[derive(Debug, thiserror::Error)]
pub enum ProcessingError {
    /// Failed to create the temporary working directory.
    #[error("could not create temp directory: {0}")]
    TempDir(std::io::Error),
    /// Failed to read the temporary directory contents.
    #[error("could not read temp directory contents: {0}")]
    ReadDir(std::io::Error),
    /// Failed to read the metadata of the downloaded file.
    #[error("could not read file metadata: {0}")]
    FileMetadata(std::io::Error),
    /// An unexpected number of output files was found.
    #[error("{0} files found in output, expected 1")]
    UnexpectedFileCount(usize),
    /// The downloaded file exceeds the configured size limit.
    #[error("file size exceeded {limit} MB")]
    FileTooLarge {
        /// The size limit that was exceeded, in MB.
        limit: u64,
    },
    /// The download step failed.
    #[error(transparent)]
    Download(#[from] crate::media::DownloadError),
    /// The video conversion step failed.
    #[error(transparent)]
    Convert(#[from] crate::media::ConvertError),
    /// The video bitrate exceeds what can fit within Telegram's file size limit.
    #[error("video bitrate is too high to fit within Telegram's file size limit")]
    BitrateTooHigh,
}

/// Represents the output of a processed [`Task`].
pub struct TaskOutput {
    /// Handle to the directory containing video files, kept alive to defer [`Drop`].
    pub _dir: TempDir,
    /// Contents of a file to be uploaded.
    pub video_file: InputFile,
    /// Thumbnail, if able to be extracted.
    pub maybe_thumbnail: Option<InputFile>,
    /// Metadata of the video file.
    pub metadata: crate::media::Probe,
    /// The reduced bitrate if it was lowered to meet API limits, or [`None`] if unchanged.
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

/// Possible result of processing a [`Task`].
///
/// The [`Ok`] variant is boxed to avoid large futures; may not be strictly necessary.
pub type TaskResult = Result<Box<TaskOutput>, ProcessingError>;

/// A download task created by a user, processed by the [`Worker`][crate::worker::Worker] and returned via channel.
#[derive(Debug)]
pub struct Task {
    /// URL of the video to be processed.
    pub url: String,
    /// Whether to enable fallback processing.
    pub enable_fallback: bool,
    /// Channel to send the result back to the sender.
    pub return_channel: oneshot::Sender<TaskResult>,
}
