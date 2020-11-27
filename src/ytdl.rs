use lazy_static::lazy_static;
use linkify::{LinkFinder, LinkKind};
use url::Url;

use std::io::Read;
use std::process::{Command, Stdio};

// initalize a whitelist of websites to download from
// format: site1.com,site2.net,site3.edu
lazy_static! {
    static ref WHITELIST: Vec<String> = std::env::var("WHITELIST")
        .unwrap()
        .split(",")
        .map(|s| s.to_owned())
        .collect();
}

// finds a single link in a given message
pub fn find_link(msg: &str) -> Option<&str> {
    // create LinkFinder and initialize it with proper config
    let mut finder: LinkFinder = LinkFinder::new();
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

// downloads video from an url in .mp4 format, converting if needed
pub fn download(url: &str, filename: &str) -> Result<(), String> {
    let mut ytdl = Command::new("youtube-dl")
        .args(&[
            "--max-filesize",
            "50M", // Telegram API limit
            "--output",
            filename,
            "--format",
            "bestvideo+bestaudio[ext=m4a]/bestvideo+bestaudio/best",
            "--postprocessor-args",
            "-c:v libx264",
            "--merge-output-format",
            "mp4",
            url,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("youtube-dl failed to start");

    // obtain the exit status
    let exit_status = ytdl.wait().expect("Command wasn't running");

    // in case of an error, return stdout as a string
    if !exit_status.success() {
        let mut buf = String::new();
        if let Ok(_) = ytdl.stderr.take().unwrap().read_to_string(&mut buf) {
            return Err(buf);
        }
    }

    Ok(())
}
