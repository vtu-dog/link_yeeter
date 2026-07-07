//! Media processing utilities: downloading, conversion, probing.

use crate::env;

use std::ops::Div;
use std::time::Duration;

use async_process::{Command, Stdio};
use teloxide::types::InputFile;
use tracing::{debug, debug_span, warn};

/// Timeout for external commands (yt-dlp, ffmpeg).
static COMMAND_TIMEOUT: Duration = Duration::from_mins(5);

/// Telegram's upload limit for bots, in MB (2000 when self-hosting the Bot API server).
pub const TELEGRAM_FILESIZE_LIMIT_MB: u32 = 50;

/// Error produced by [`download`].
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// Failed to spawn the yt-dlp process.
    #[error("failed to spawn yt-dlp: {0}")]
    Spawn(std::io::Error),
    /// The yt-dlp process exceeded the command timeout.
    #[error("yt-dlp timed out")]
    Timeout,
    /// The yt-dlp process could not be awaited.
    #[error("yt-dlp execution failed: {0}")]
    Execution(std::io::Error),
    /// The file was rejected by yt-dlp for exceeding the configured size limit.
    #[error("file size exceeded {limit} MB")]
    FileTooLarge {
        /// The size limit that was exceeded, in MB.
        limit: u64,
    },
    /// yt-dlp exited with a non-zero status code.
    #[error("yt-dlp failed with exit code {0}")]
    ExitFailure(i32),
}

/// Error produced by [`convert`].
#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    /// Failed to spawn the ffmpeg process.
    #[error("failed to spawn ffmpeg: {0}")]
    Spawn(std::io::Error),
    /// The ffmpeg process exceeded the command timeout.
    #[error("ffmpeg timed out")]
    Timeout,
    /// The ffmpeg process could not be awaited.
    #[error("ffmpeg execution failed: {0}")]
    Execution(std::io::Error),
    /// ffmpeg exited with a non-zero status code.
    #[error("ffmpeg failed with exit code {0}")]
    ExitFailure(i32),
}

/// Output of [`ffprobe`].
#[derive(Default, Debug)]
pub struct Probe {
    /// Duration of the video in seconds.
    pub duration: u32,
    /// Bitrate of the video in kbps.
    pub bitrate: u32,
    /// Width of the video in pixels.
    pub width: u32,
    /// Height of the video in pixels.
    pub height: u32,
}

/// Probes a video file for its duration, bitrate, width, and height.
pub fn ffprobe(path: &str) -> Option<Probe> {
    let probe = match ffprobe::ffprobe(path) {
        Ok(p) => p,
        Err(e) => {
            warn!(path = %path, error = %e, "ffprobe failed");
            return None;
        }
    };

    let streams = probe.streams;
    let video_stream = streams
        .iter()
        .find(|&s| s.codec_type == Some("video".to_string()))?;

    let width = video_stream.width.unwrap_or(0);
    let height = video_stream.height.unwrap_or(0);

    let bitrate = u32::try_from(
        probe
            .format
            .bit_rate
            .clone()
            .unwrap_or_else(|| "0".to_string())
            .parse()
            .unwrap_or(0),
    )
    .unwrap_or(0)
    .div(1000);

    let duration = probe
        .format
        .try_get_duration()
        .and_then(Result::ok)
        .map_or(0, |d| u32::try_from(d.as_secs()).unwrap_or(0));

    Some(Probe {
        duration,
        bitrate,
        width: u32::try_from(width).unwrap_or(0),
        height: u32::try_from(height).unwrap_or(0),
    })
}

/// Downloads a video from a URL using yt-dlp.
pub async fn download(
    url: &str,
    dirname: &str,
    enable_fallback: bool,
) -> Result<(), DownloadError> {
    let max_filesize = if enable_fallback {
        *env::FALLBACK_FILESIZE
    } else {
        *env::MAX_FILESIZE
    };

    debug!(
        url = %url,
        max_filesize_mb = max_filesize,
        enable_fallback,
        "invoking yt-dlp"
    );

    // yt-dlp uses mebibytes (M suffix), convert from megabytes
    // (truncation keeps the limit conservative)
    let max_filesize_mib = max_filesize * 1_000_000 / (1024 * 1024);
    let max_filesize_str = format!("{max_filesize_mib}M");
    let output_template = format!("{dirname}/%(id)s.%(ext)s");

    let mut args = vec![
        "--ignore-config", // ignore local setup
        "--no-playlist",
        "--max-filesize",
        &max_filesize_str,
    ];

    // reddit workaround, still needed as of v2026.03.13
    // TODO: add generic handling for other sites?
    if url.starts_with("https://www.reddit.com") {
        debug!("applying reddit workaround headers");
        args.push("--add-header");
        args.push("accept:*/*");
    }

    args.push("--output");
    args.push(&output_template);
    args.push(url);

    // run yt-dlp and wait for it to finish (with timeout)
    let child = Command::new("yt-dlp")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(DownloadError::Spawn)?;

    let output = tokio::time::timeout(COMMAND_TIMEOUT, child.output())
        .await
        .map_err(|_| DownloadError::Timeout)?
        .map_err(DownloadError::Execution)?;

    let stdout_str = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr_str = String::from_utf8_lossy(&output.stderr).trim().to_owned();

    if !stdout_str.is_empty() {
        debug_span!("yt-dlp", "stdout").in_scope(|| debug!("{}", stdout_str));
    }

    if !stderr_str.is_empty() {
        debug_span!("yt-dlp", "stderr").in_scope(|| debug!("{}", stderr_str));
    }

    let file_too_big_msg = "File is larger than max-filesize";
    if stderr_str.contains(file_too_big_msg) || stdout_str.contains(file_too_big_msg) {
        warn!(url = %url, max_filesize_mb = max_filesize, "yt-dlp reported file too large");
        return Err(DownloadError::FileTooLarge {
            limit: max_filesize,
        });
    }

    if !output.status.success() {
        let exit_code = output.status.code().unwrap_or(-1);
        warn!(url = %url, exit_code, "yt-dlp exited with error");
        return Err(DownloadError::ExitFailure(exit_code));
    }

    debug!(url = %url, "yt-dlp completed successfully");
    Ok(())
}

/// Converts a video to .mp4 format using ffmpeg.
pub async fn convert(
    input: &str,
    output: &str,
    maybe_bitrate: Option<u32>,
) -> Result<(), ConvertError> {
    debug!(
        input = %input,
        output = %output,
        bitrate_kbps = ?maybe_bitrate,
        "invoking ffmpeg for conversion"
    );

    // ffmpeg's 'M' suffix is 10^6 bytes; 'MB' would multiply by 8
    let fs_limit = format!("{TELEGRAM_FILESIZE_LIMIT_MB}M");

    // compose the ffmpeg command arguments
    let mut args = vec![
        "-y", // overwrite output files if they already exist
        "-i", // input file
        input,
        "-c:v", // video codec
        "libx264",
        "-movflags", // faststart
        "+faststart",
        "-pix_fmt", // pixel format
        "yuv420p",
        "-b:a", // audio bitrate
        "128k",
        "-fs", // stop writing at Telegram's upload limit
        &fs_limit,
        "-vf", // make sure the video dimensions are even
        "crop=trunc(iw/2)*2:trunc(ih/2)*2",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect::<Vec<_>>();

    // add bitrate if specified
    if let Some(bitrate) = maybe_bitrate {
        args.push("-b:v".to_string()); // video bitrate
        args.push(format!("{bitrate}k"));
    }

    args.push(output.to_string());

    // run ffmpeg and wait for it to finish (with timeout)
    let child = Command::new("ffmpeg")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ConvertError::Spawn)?;

    let cmd_output = tokio::time::timeout(COMMAND_TIMEOUT, child.output())
        .await
        .map_err(|_| ConvertError::Timeout)?
        .map_err(ConvertError::Execution)?;

    let stdout_str = String::from_utf8_lossy(&cmd_output.stdout)
        .trim()
        .to_owned();
    let stderr_str = String::from_utf8_lossy(&cmd_output.stderr)
        .trim()
        .to_owned();

    if !stdout_str.is_empty() {
        debug_span!("ffmpeg", "stdout").in_scope(|| debug!("{}", stdout_str));
    }

    if !stderr_str.is_empty() {
        debug_span!("ffmpeg", "stderr").in_scope(|| debug!("{}", stderr_str));
    }

    let status = cmd_output.status;
    if !status.success() {
        let exit_code = status.code().unwrap_or(-1);
        warn!(exit_code, "ffmpeg conversion failed");
        return Err(ConvertError::ExitFailure(exit_code));
    }

    debug!(output = %output, "ffmpeg conversion completed successfully");
    Ok(())
}

/// Extracts a thumbnail from a video, saves it as a .jpg file, and returns it.
pub async fn get_thumbnail(video_path: &str) -> Option<InputFile> {
    debug!(video_path = %video_path, "extracting thumbnail");

    // get the parent folder of the video and construct the thumbnail path
    let thumbnail_path = std::path::Path::new(video_path)
        .parent()
        .map(|p| p.join("thumbnail.jpg").to_string_lossy().to_string())?;

    // create a new ffmpeg command
    let exit_code = Command::new("ffmpeg")
        .args([
            "-y", // overwrite output files if they already exist
            "-i", // input file
            video_path,
            "-vframes", // number of frames to output
            "1",
            "-q:v", // quality of the thumbnail (1-31)
            "3",
            "-update", // suppress warning
            "true",
            &thumbnail_path,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .status()
        .await
        .map(|s| s.success());

    if matches!(exit_code, Ok(true)) {
        debug!(thumbnail_path = %thumbnail_path, "thumbnail extracted successfully");
        Some(InputFile::file(thumbnail_path))
    } else {
        debug!("thumbnail extraction failed or skipped");
        None
    }
}
