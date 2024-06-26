//! Utility functions used throughout the project.

use std::{ops::Div, sync::OnceLock};

use async_process::Command;
use linkify::{LinkFinder, LinkKind};
use rand::{distributions::Alphanumeric, Rng};
use teloxide::types::InputFile;
use url::Url;

pub static WHITELIST: OnceLock<Vec<String>> = OnceLock::new();

/// Initialise the whitelist of websites to allow downloads from.
/// Format: `site1.com,site2.net,site3.edu`.
pub fn init_statics() {
    WHITELIST
        .set(
            std::env::var("WHITELIST")
                .expect("WHITELIST environment variable not set")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        )
        .expect("WHITELIST was already initialised");
}

/// Obtain a random string of specified length.
pub fn random_string(size: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(size)
        .map(char::from)
        .collect()
}

/// Information about URLs found in a message.
pub struct URLInfo {
    pub maybe_url: Option<String>,
    pub total_urls: usize,
    pub whitelisted_urls: usize,
}

/// Parses a message and returns information about URLs found in it.
pub fn get_url_info(msg: &str) -> URLInfo {
    // create LinkFinder and initialise it with a proper config
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);

    // find all the links in msg
    let links: Vec<_> = finder.links(msg).collect();
    let links_len = links.len();

    let links_enumerated = links.into_iter().enumerate();

    // parse the links and extract the netlocs
    let urls = links_enumerated
        .into_iter()
        .map(|(i, l)| (i, l.as_str()))
        .collect::<Vec<_>>();

    let parsed_urls = urls
        .iter()
        .map(|(i, u)| (i, Url::parse(u)))
        .filter_map(|(i, u)| u.map_or(None, |u| Some((i, u))));

    let netlocs = parsed_urls
        .into_iter()
        .map(|(i, u)| (i, u.host_str().map(std::string::ToString::to_string)))
        .filter_map(|(i, u)| u.map(|u| (i, u)));

    // split the netlocs into parts and extract the last two parts
    // for example, vm.tiktok.com -> tiktok.com
    let netloc_parts = netlocs.into_iter().map(|(&i, n)| {
        let parts = n
            .split('.')
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .iter()
            .rev()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();

        (i, parts)
    });

    // check the netlocs against the whitelist
    let whitelist_items = netloc_parts.into_iter().map(|(i, n)| (i, n.join(".")));

    let whitelisted_urls = whitelist_items
        .into_iter()
        .filter(|(_, w)| {
            WHITELIST
                .get()
                .expect("WHITELIST not initialised")
                .contains(w)
        })
        .collect::<Vec<_>>();

    let whitelisted_urls_len = whitelisted_urls.len();

    URLInfo {
        maybe_url: if whitelisted_urls_len == 1 {
            let index = whitelisted_urls.first().unwrap().0;
            let url = urls.get(index).unwrap().1;
            Some(url.to_string())
        } else {
            None
        },
        total_urls: links_len,
        whitelisted_urls: whitelisted_urls_len,
    }
}

/// Downloads a video from an URL in .mp4 format.
pub async fn download(url: &str, dirname: &str) -> bool {
    let mut command = Command::new("yt-dlp");
    let yt_dlp = command.args([
        "--no-playlist",
        "--output",
        &format!("{dirname}/%(id)s.%(ext)s"),
        url,
    ]);

    // run the command and wait for it to finish
    yt_dlp
        .status()
        .await
        .map_or(false, |status| status.success())
}

/// Probe result.
pub struct Probe {
    pub duration: u32,
    pub bitrate: u32,
    pub width: u32,
    pub height: u32,
}

/// Implements a `Default` trait for `Probe`.
impl Default for Probe {
    fn default() -> Self {
        Self {
            duration: 0,
            bitrate: 0,
            width: 0,
            height: 0,
        }
    }
}

/// Probes a video file for its duration, width and height.
pub fn probe(path: &str) -> Option<Probe> {
    match ffprobe::ffprobe(path) {
        Ok(probe) => {
            let streams = probe.streams;
            let video_stream = streams
                .iter()
                .find(|&s| s.codec_type == Some("video".to_string()));

            if let Some(video_stream) = video_stream {
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
                    //.map_or(0, |d| d.as_secs() as u32);
                    .map_or(0, |d| u32::try_from(d.as_secs()).unwrap_or(0));

                Some(Probe {
                    duration,
                    bitrate,
                    width: u32::try_from(width).unwrap_or(0),
                    height: u32::try_from(height).unwrap_or(0),
                })
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

/// Converts a video to .mp4.
pub async fn convert(input: &str, output: &str, bitrate: Option<u32>) -> bool {
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
        "-vf", // making sure the video dimensions are even
        "crop=trunc(iw/2)*2:trunc(ih/2)*2",
    ]
    .into_iter()
    .map(std::string::ToString::to_string)
    .collect::<Vec<_>>();

    // add bitrate if specified
    if let Some(bitrate) = bitrate {
        args.push("-b:v".to_string()); // video bitrate
        args.push(format!("{bitrate}k"));
    }

    args.push(output.to_string());

    // create a new ffmpeg command
    let mut command = Command::new("ffmpeg");

    // run the command and wait for it to finish
    command
        .args(&args)
        .status()
        .await
        .map_or(false, |status| status.success())
}

/// Extracts a thumbnail from a video, saving it as a .jpg file and returning its path.
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
            &thumbnail_path,
        ])
        .status()
        .await
        .map(|s| s.success());

    if matches!(exit_code, Ok(true)) {
        Some(InputFile::file(thumbnail_path))
    } else {
        None
    }
}
