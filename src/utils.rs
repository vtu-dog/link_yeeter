//! Utility functions used throughout the project.

use crate::env;

use std::ops::Div;
use std::time::Duration;

/// Timeout for external commands (yt-dlp, ffmpeg).
static COMMAND_TIMEOUT: Duration = Duration::from_secs(300);

use anyhow::{Context, bail};
use async_process::{Command, Stdio};
use linkify::{LinkFinder, LinkKind};
use rand::distr::{Alphanumeric, SampleString};
use teloxide::types::InputFile;
use tracing::{debug, debug_span, warn};
use url::Url;

/// Obtain a random string of specified length.
pub fn random_string(size: usize) -> String {
    Alphanumeric.sample_string(&mut rand::rng(), size)
}

/// Information about URLs found in a message.
#[derive(Debug, PartialEq, Eq)]
pub enum URLsFound {
    /// No URLs found.
    None,
    /// One URL found.
    One {
        /// The URL found.
        url: String,
        /// Whether the URL is in the allowlist.
        supported: bool,
    },
    /// Multiple URLs found.
    Multiple,
}

/// Parse a message and return information about URLs found inside it.
pub fn get_url_info(msg: &str) -> URLsFound {
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);

    let urls: Vec<Url> = finder
        .links(msg)
        .filter_map(|link| Url::parse(link.as_str()).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .collect();

    match urls.len() {
        0 => URLsFound::None,
        1 => {
            let url = &urls[0];
            let Some(host) = url.host_str() else {
                return URLsFound::None;
            };

            // extract base domain (last two parts): www.youtube.com -> youtube.com
            let parts: Vec<_> = host.split('.').collect();
            let netloc = if parts.len() >= 2 {
                format!("{}.{}", parts[parts.len() - 2], parts[parts.len() - 1])
            } else {
                host.to_string()
            };

            URLsFound::One {
                url: url.to_string(),
                supported: env::ALLOWLIST.contains(&netloc),
            }
        }
        _ => URLsFound::Multiple,
    }
}

/// `FFprobe` result.
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

/// Probe a video file for its duration, bitrate, width and height.
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
        .and_then(std::result::Result::ok)
        .map_or(0, |d| u32::try_from(d.as_secs()).unwrap_or(0));

    Some(Probe {
        duration,
        bitrate,
        width: u32::try_from(width).unwrap_or(0),
        height: u32::try_from(height).unwrap_or(0),
    })
}

/// Download a video from an URL.
pub async fn download(url: &str, dirname: &str, enable_fallback: bool) -> anyhow::Result<()> {
    let max_filesize = {
        if enable_fallback {
            *env::FALLBACK_FILESIZE
        } else {
            *env::MAX_FILESIZE
        }
    };

    debug!(
        url = %url,
        max_filesize_mb = max_filesize,
        enable_fallback,
        "invoking yt-dlp"
    );

    // yt-dlp uses mebibytes (M suffix), convert from megabytes
    let max_filesize_mib = max_filesize * 1000 / 1024;
    let max_filesize_str = format!("{max_filesize_mib}M");
    let output_str = format!("{dirname}/%(id)s.%(ext)s");

    let mut args = vec![
        "--ignore-config", // ignore local setup
        "--no-playlist",
        "--max-filesize",
        &max_filesize_str,
    ];

    // reddit workaround, still needed as of v2026.02.04
    // TODO: add generic handling for other sites?
    if url.starts_with("https://www.reddit.com") {
        debug!("applying reddit workaround headers");
        args.push("--add-header");
        args.push("accept:*/*");
    }

    args.push("--output");
    args.push(&output_str);
    args.push(url);

    // run yt-dlp and wait for it to finish (with timeout)
    let child = Command::new("yt-dlp")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn yt-dlp")?;

    let output = tokio::time::timeout(COMMAND_TIMEOUT, child.output())
        .await
        .context("yt-dlp timed out")?
        .context("yt-dlp execution failed")?;

    let stdout_str = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr_str = String::from_utf8_lossy(&output.stderr).trim().to_owned();

    if !stdout_str.is_empty() {
        debug_span!("yt-dlp", "stdout").in_scope(|| debug!("{}", stdout_str));
    }

    if !stderr_str.is_empty() {
        debug_span!("yt-dlp", "stderr").in_scope(|| debug!("{}", stderr_str));
    }

    let file_too_big_msg = "File is larger than max-filesize";
    let output_str =
        String::from_utf8_lossy(&output.stderr) + String::from_utf8_lossy(&output.stdout);

    if output_str.contains(file_too_big_msg) {
        warn!(url = %url, max_filesize_mb = max_filesize, "yt-dlp reported file too large");
        bail!("file size exceeded {max_filesize} MB");
    }

    if !output.status.success() {
        let exit_code = output.status.code().unwrap_or(-1);
        warn!(url = %url, exit_code, "yt-dlp exited with error");
        bail!("yt-dlp failed with status code {exit_code}")
    }

    debug!(url = %url, "yt-dlp completed successfully");
    Ok(())
}

/// Convert a video to .mp4 format.
pub async fn convert(input: &str, output: &str, maybe_bitrate: Option<u32>) -> anyhow::Result<()> {
    debug!(
        input = %input,
        output = %output,
        bitrate_kbps = ?maybe_bitrate,
        "invoking ffmpeg for conversion"
    );

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
        "-fs", // max filesize
        "50MB",
        "-vf", // make sure the video dimensions are even
        "crop=trunc(iw/2)*2:trunc(ih/2)*2",
    ]
    .into_iter()
    .map(std::string::ToString::to_string)
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
        .context("failed to spawn ffmpeg")?;

    let cmd_output = tokio::time::timeout(COMMAND_TIMEOUT, child.output())
        .await
        .context("ffmpeg timed out")?
        .context("ffmpeg execution failed")?;

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
        bail!("ffmpeg failed with status code {exit_code}");
    }

    debug!(output = %output, "ffmpeg conversion completed successfully");
    Ok(())
}

/// Extract a thumbnail from a video, saving it as a .jpg file and returning its path.
pub async fn get_thumbnail(video_path: &str) -> Option<InputFile> {
    debug!(video_path = %video_path, "extracting thumbnail");

    // get the parent folder of the video and construct the thumbnail path
    let parent_folder = std::path::Path::new(video_path).parent();
    let thumbnail_path = parent_folder
        .map(|p| p.join("thumbnail.jpg"))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap();

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

#[cfg(test)]
mod tests {
    use super::*;

    mod random_string {
        use super::*;

        #[test]
        fn returns_empty_string_for_zero_size() {
            assert_eq!(random_string(0), "");
        }

        #[test]
        fn returns_correct_length() {
            assert_eq!(random_string(10).len(), 10);
            assert_eq!(random_string(100).len(), 100);
        }

        #[test]
        fn contains_only_alphanumeric_chars() {
            let s = random_string(1000);
            assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
        }

        #[test]
        fn generates_different_strings() {
            let s1 = random_string(20);
            let s2 = random_string(20);
            // Extremely unlikely to be equal by chance
            assert_ne!(s1, s2);
        }
    }

    mod get_url_info {
        use super::*;

        #[test]
        fn returns_none_for_empty_string() {
            assert_eq!(get_url_info(""), URLsFound::None);
        }

        #[test]
        fn returns_none_for_plain_text() {
            assert_eq!(get_url_info("just some plain text"), URLsFound::None);
        }

        #[test]
        fn rejects_non_http_schemes() {
            assert_eq!(get_url_info("ftp://files.example.com"), URLsFound::None);
            assert_eq!(get_url_info("mailto:test@example.com"), URLsFound::None);
            assert_eq!(get_url_info("not://a-valid-url"), URLsFound::None);
        }

        #[test]
        fn extracts_single_url() {
            let result = get_url_info("check out https://example.com/video");
            match result {
                URLsFound::One { url, .. } => {
                    assert_eq!(url, "https://example.com/video");
                }
                _ => panic!("expected URLsFound::One, got {result:?}"),
            }
        }

        #[test]
        fn extracts_url_with_query_params() {
            let result = get_url_info("https://example.com/watch?v=abc123&t=10");
            match result {
                URLsFound::One { url, .. } => {
                    assert!(url.contains("v=abc123"));
                    assert!(url.contains("t=10"));
                }
                _ => panic!("expected URLsFound::One, got {result:?}"),
            }
        }

        #[test]
        fn returns_multiple_for_two_urls() {
            assert_eq!(
                get_url_info("https://example.com and https://other.com"),
                URLsFound::Multiple
            );
        }

        #[test]
        fn extracts_netloc_without_subdomain() {
            // The function extracts the last two parts of the domain
            // So www.youtube.com -> youtube.com
            let result = get_url_info("https://www.example.com/path");
            match result {
                URLsFound::One { url, .. } => {
                    assert!(url.contains("www.example.com"));
                }
                _ => panic!("expected URLsFound::One"),
            }
        }

        #[test]
        fn handles_url_in_middle_of_text() {
            let result = get_url_info("Please download https://example.com/file.mp4 thanks!");
            match result {
                URLsFound::One { url, .. } => {
                    assert_eq!(url, "https://example.com/file.mp4");
                }
                _ => panic!("expected URLsFound::One"),
            }
        }

        #[test]
        fn handles_url_with_port() {
            let result = get_url_info("https://example.com:8080/path");
            match result {
                URLsFound::One { url, .. } => {
                    assert!(url.contains(":8080"));
                }
                _ => panic!("expected URLsFound::One"),
            }
        }

        #[test]
        fn handles_url_with_fragment() {
            let result = get_url_info("https://example.com/page#section");
            match result {
                URLsFound::One { url, .. } => {
                    assert!(url.contains("#section"));
                }
                _ => panic!("expected URLsFound::One"),
            }
        }

        #[test]
        fn handles_http_url() {
            let result = get_url_info("http://example.com/video");
            match result {
                URLsFound::One { url, .. } => {
                    assert!(url.starts_with("http://"));
                }
                _ => panic!("expected URLsFound::One"),
            }
        }

        #[test]
        fn handles_url_with_encoded_chars() {
            let result = get_url_info("https://example.com/path%20with%20spaces");
            match result {
                URLsFound::One { url, .. } => {
                    assert!(url.contains("%20"));
                }
                _ => panic!("expected URLsFound::One"),
            }
        }

        #[test]
        fn returns_multiple_for_three_urls() {
            assert_eq!(
                get_url_info("https://a.com https://b.com https://c.com"),
                URLsFound::Multiple
            );
        }
    }
}
