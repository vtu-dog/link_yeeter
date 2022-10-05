use async_process::Command;
use linkify::{LinkFinder, LinkKind};
use once_cell::sync::Lazy;
use rand::{distributions::Alphanumeric, Rng};
use url::Url;

// Initalise a whitelist of websites to download from.
// Format: `site1.com,site2.net,site3.edu`.
static WHITELIST: Lazy<Vec<String>> = Lazy::new(|| {
    std::env::var("WHITELIST")
        .expect("WHITELIST environment variable not set")
        .split(',')
        .map(|s| s.trim().to_string())
        .collect()
});

/// Obtain a random string of the specified length.
pub fn random_string(size: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(size)
        .map(char::from)
        .collect()
}

/// Finds a single URL in a given message.
pub fn find_url(msg: &str) -> Option<&str> {
    // create LinkFinder and initialise it with a proper config
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);

    // find all the links in msg
    let links: Vec<_> = finder.links(msg).collect();

    // proceed if there's just one link
    if links.len() == 1 {
        let url = links.first().unwrap().as_str();
        let parsed_url = Url::parse(url).ok()?;
        let netloc = parsed_url.host_str()?;

        // www.youtube.com -> youtube.com; vm.tiktok.com -> tiktok.com etc.
        let netloc_parts = &netloc.split('.').collect::<Vec<_>>();
        let whitelist_item = netloc_parts[netloc_parts.len() - 2..].join(".");

        // make sure that the whitelist contains our netloc
        if WHITELIST.contains(&whitelist_item) {
            return Some(url);
        }
    }

    None
}

/// Downloads a video from an URL in .mp4 format, converting if needed.
pub async fn download_and_convert(url: &str, dirname: &str, filename: &str) {
    let mut command = Command::new("yt-dlp");
    let yt_dlp = command.args(&[
        "--max-filesize",
        "50M", // Telegram API limit
        "--output",
        &format!("{}/%(title)s.%(ext)s", dirname),
        url,
    ]);

    // run the command and wait for it to finish
    yt_dlp.status().await.ok();

    let mut files = std::fs::read_dir(dirname)
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();

    // check if yt-dlp downloaded the video by checking if dir contains a file
    if files.len() == 1 {
        let entry = files.pop().unwrap().unwrap();
        let path = entry.path().to_str().unwrap().to_string();

        let mut command = Command::new("ffmpeg");
        let ffmpeg = command.args(&[
            "-i",
            &path,
            "-c:v",
            "libx265",
            "-pix_fmt",
            "yuv420p",
            &format!("{}/{}", dirname, filename),
        ]);

        // run the command and wait for it to finish
        ffmpeg.status().await.ok();
    }
}
