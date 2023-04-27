//! Utility functions used throughout the project.

use async_process::Command;
use linkify::{LinkFinder, LinkKind};
use once_cell::sync::Lazy;
use rand::{distributions::Alphanumeric, Rng};
use teloxide::types::InputFile;
use url::Url;

// Initalise a whitelist of websites to download from.
// Format: `site1.com,site2.net,site3.edu`.
pub static WHITELIST: Lazy<Vec<String>> = Lazy::new(|| {
    std::env::var("WHITELIST")
        .expect("WHITELIST environment variable not set")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
});

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

    let links_enumerated = links.into_iter().enumerate().collect::<Vec<_>>();

    // parse the links and extract the netlocs
    let urls = links_enumerated
        .into_iter()
        .map(|(i, l)| (i, l.as_str()))
        .collect::<Vec<_>>();

    let parsed_urls = urls
        .iter()
        .map(|(i, u)| (i, Url::parse(u)))
        .filter_map(|(i, u)| match u {
            Ok(u) => Some((i, u)),
            Err(_) => None,
        })
        .collect::<Vec<_>>();

    let netlocs = parsed_urls
        .into_iter()
        .map(|(i, u)| (i, u.host_str().map(|s| s.to_string())))
        .filter_map(|(i, u)| u.map(|u| (i, u)))
        .collect::<Vec<_>>();

    // split the netlocs into parts and extract the last two parts
    // for example, vm.tiktok.com -> tiktok.com
    let netloc_parts = netlocs
        .into_iter()
        .map(|(&i, n)| {
            let parts = n
                .split('.')
                .rev()
                .take(2)
                .collect::<Vec<_>>()
                .iter()
                .rev()
                .map(|s| s.to_string())
                .collect::<Vec<_>>();
            (i, parts)
        })
        .collect::<Vec<_>>();

    // check the netlocs against the whitelist
    let whitelist_items = netloc_parts
        .into_iter()
        .map(|(i, n)| (i, n.join(".")))
        .collect::<Vec<_>>();

    let whitelisted_urls = whitelist_items
        .into_iter()
        .filter(|(_, w)| WHITELIST.contains(w))
        .collect::<Vec<_>>();

    let whitelisted_urls_len = whitelisted_urls.len();

    URLInfo {
        maybe_url: if whitelisted_urls_len == 1 {
            let index = whitelisted_urls.get(0).unwrap().0;
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
        &format!("{}/%(id)s.%(ext)s", dirname),
        url,
    ]);

    // run the command and wait for it to finish
    match yt_dlp.status().await {
        Ok(status) => status.success(),
        Err(_) => false,
    }
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

                let bitrate = probe
                    .format
                    .bit_rate
                    .to_owned()
                    .unwrap_or("0".to_string())
                    .parse()
                    .unwrap_or(0) as u32
                    / 1000;

                let duration = probe
                    .format
                    .try_get_duration()
                    .and_then(|d| d.ok())
                    .map(|d| d.as_secs() as u32)
                    .unwrap_or(0);

                Some(Probe {
                    duration,
                    bitrate,
                    width: width as u32,
                    height: height as u32,
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
    .map(|s| s.to_string())
    .collect::<Vec<_>>();

    // add bitrate if specified
    if let Some(bitrate) = bitrate {
        args.push("-b:v".to_string()); // video bitrate
        args.push(format!("{}k", bitrate));
    }

    args.push(output.to_string());

    // create a new ffmpeg command
    let mut command = Command::new("ffmpeg");

    // run the command and wait for it to finish
    match command.args(&args).status().await {
        Ok(status) => status.success(),
        Err(_) => false,
    }
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

    if let Ok(true) = exit_code {
        Some(InputFile::file(thumbnail_path))
    } else {
        None
    }
}
