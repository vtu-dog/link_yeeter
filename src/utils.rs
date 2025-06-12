//! Utility functions used throughout the project.

use crate::env;

use std::ops::Div;

use async_process::{Command, Stdio};
use color_eyre::eyre::{Context, bail};
use linkify::{LinkFinder, LinkKind};
use rand::{Rng, distr::Alphanumeric};
use teloxide::types::InputFile;
use url::Url;

/// Obtain a random string of specified length.
pub fn random_string(size: usize) -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(size)
        .map(char::from)
        .collect()
}

/// Information about URLs found in a message.
pub enum URLsFound {
    None,
    One { url: String, supported: bool },
    Multiple,
}

/// Parse a message and returns information about URLs found inside it.
pub fn get_url_info(msg: &str) -> URLsFound {
    // create LinkFinder and initialise it with a proper config
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);

    // find all the links in msg
    let links: Vec<_> = finder.links(msg).collect();

    // parse the links and extract their netlocs
    let all_urls = links
        .iter()
        .flat_map(|u| Url::parse(u.as_str()))
        .collect::<Vec<_>>();

    if all_urls.is_empty() {
        return URLsFound::None;
    } else if all_urls.len() > 1 {
        return URLsFound::Multiple;
    }

    // compare the netloc of a single URL against the allowlist
    let single_url = all_urls.first().unwrap().to_owned();

    // bail if host_str not found (for example, in mailto:_)
    // otherwise, extract netloc and check if it's supported
    single_url.host_str().map_or(URLsFound::None, |hs| {
        let netloc = hs
            .split('.')
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(".");

        URLsFound::One {
            url: single_url.to_string(),
            supported: env::ALLOWLIST.contains(&netloc),
        }
    })
}

/// `FFprobe` result.
#[derive(Default)]
pub struct Probe {
    pub duration: u32,
    pub bitrate: u32,
    pub width: u32,
    pub height: u32,
}

/// Probe a video file for its duration, bitrate, width and height.
pub fn ffprobe(path: &str) -> Option<Probe> {
    let maybe_probe = ffprobe::ffprobe(path);

    if maybe_probe.is_err() {
        return None;
    }

    let probe = maybe_probe.unwrap();

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
pub async fn download(url: &str, dirname: &str, enable_fallback: bool) -> color_eyre::Result<()> {
    let max_filesize = {
        if enable_fallback {
            *env::FALLBACK_FILESIZE
        } else {
            *env::MAX_FILESIZE
        }
    };

    let args = [
        "--ignore-config", // ignore local setup
        "--no-playlist",
        "--max-filesize",
        &format!("{max_filesize}M"),
        "--add-header", // reddit workaround, hopefully doesn't break other sites
        "accept:*/*",
        "--output",
        &format!("{dirname}/%(id)s.%(ext)s"),
        url,
    ];

    // run yt-dlp and wait for it to finish
    let child = Command::new("yt-dlp")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err("failed to spawn yt-dlp")?;

    let output = child.output().await.wrap_err("yt-dlp execution failed")?;

    let file_too_big_msg = "File is larger than max-filesize";
    let output_str =
        String::from_utf8_lossy(&output.stderr) + String::from_utf8_lossy(&output.stdout);

    if output_str.contains(file_too_big_msg) {
        bail!("file size exceeded {} megabytes", max_filesize);
    }

    if !output.status.success() {
        bail!(
            "yt-dlp failed with status code {}",
            output.status.code().unwrap_or(-1),
        )
    }

    Ok(())
}

/// Convert a video to .mp4 format.
pub async fn convert(
    input: &str,
    output: &str,
    maybe_bitrate: Option<u32>,
) -> color_eyre::Result<()> {
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
        "50M",
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

    // run ffmpeg and wait for it to finish
    let child = Command::new("ffmpeg")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err("failed to spawn ffmpeg")?;

    let output = child.output().await.wrap_err("ffmpeg execution failed")?;

    let status = output.status;
    if !status.success() {
        bail!(
            "ffmpeg failed with status code {}",
            status.code().unwrap_or(-1)
        );
    }

    Ok(())
}

/// Extract a thumbnail from a video, saving it as a .jpg file and returning its path.
pub async fn get_thumbnail(video_path: &str) -> Option<InputFile> {
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
        Some(InputFile::file(thumbnail_path))
    } else {
        None
    }
}
