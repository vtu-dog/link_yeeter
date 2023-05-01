//! This is the main file of the application.

use async_lock::Mutex;
use dotenvy::dotenv;
use once_cell::sync::Lazy;
use teloxide::{
    dispatching::UpdateHandler,
    prelude::*,
    types::{ChatKind, InputFile, MessageCommon, ParseMode},
};
use tempfile::tempdir;

#[macro_use]
extern crate simple_log;

mod utils;

static MAX_FILESIZE: Lazy<u64> = Lazy::new(|| {
    std::env::var("MAX_FILESIZE")
        .unwrap_or("250".to_string())
        .parse()
        .unwrap_or(250)
});

static MAINTAINER: Lazy<String> = Lazy::new(|| {
    let temp = std::env::var("MAINTAINER")
        .map(|x| x.trim().to_string())
        .unwrap_or_default();

    if temp.is_empty() {
        "the maintainer".to_string()
    } else {
        format!("@{}", temp)
    }
});

static NETLOCS: Lazy<String> = Lazy::new(|| {
    utils::WHITELIST
        .iter()
        .map(|x| format!("`{}`", x))
        .collect::<Vec<_>>()
        .join(", ")
});

/// Starts the application.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    // initialise the application, logger included
    dotenv().expect("failed to load .env file");
    simple_log::quick!("info");

    // make sure that the process can access essential binaries
    ["ffmpeg", "ffprobe", "yt-dlp"].into_iter().for_each(|x| {
        if which::which(x).is_err() {
            panic!("failed to find {} in PATH", x);
        }
    });

    info!("application started");

    let bot = Bot::from_env();
    Dispatcher::builder(bot, schema())
        .enable_ctrlc_handler()
        .distribution_function(|_| None::<std::convert::Infallible>)
        .build()
        .dispatch()
        .await;
}

type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

static MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static COUNT: Lazy<Mutex<u32>> = Lazy::new(|| Mutex::new(0));

/// Defines routes for the bot.
fn schema() -> UpdateHandler<Box<dyn std::error::Error + Send + Sync + 'static>> {
    let call = dptree::entry()
        // first, we increment the counter
        .map_async(|| change_count_by(1))
        // then, we call the handler
        .map_async(handler)
        // finally, we decrement the counter
        .endpoint(|| change_count_by(-1));

    // we want to handle both messages and channel posts
    dptree::entry()
        .branch(Update::filter_message().chain(call.clone()))
        .branch(Update::filter_channel_post().chain(call))
}

/// Changes COUNT by the specified delta.
async fn change_count_by(delta: i32) -> HandlerResult {
    let mut count = COUNT.lock().await;
    *count = (*count as i64 + delta as i64).max(0) as u32;
    Ok(())
}

/// Handles incoming messages.
async fn handler(message: Message, bot: Bot) -> HandlerResult {
    let in_private_chat = matches!(message.chat.kind, ChatKind::Private(_));
    let text = message.text().unwrap_or_default();
    let url_info = utils::get_url_info(text);

    if url_info.maybe_url.is_none() {
        let msg = if url_info.total_urls == 0 {
            debug!("no URLs found");
            "No URLs found.".to_string()
        } else if url_info.whitelisted_urls == 0 {
            debug!("no whitelisted URLs found");
            format!(
                "No whitelisted URLs found.\n\nSupported netlocs: {}.",
                *NETLOCS
            )
        } else {
            debug!("more than one URL found");
            "Downloading more than one video at a time is unsupported.".to_string()
        };

        if in_private_chat {
            bot.send_message(
                message.chat.id,
                format!(
                    "{}\n\nFor more information, please contact {}.",
                    msg, *MAINTAINER
                )
                .replace('.', r"\."),
            )
            .reply_to_message_id(message.id)
            .parse_mode(ParseMode::MarkdownV2)
            .await
            .log_on_error()
            .await;
        }

        return Ok(());
    }

    let url = url_info.maybe_url.unwrap();

    // if the message is forwarded, ignore it
    if message.forward_date().is_some() && !in_private_chat {
        debug!("message is forwarded and not in private chat");
        return Ok(());
    }

    // we want to download one video at a time
    // first, acquire the counter mutex and get the current count
    let _count = COUNT.lock().await;
    let count = *_count;
    drop(_count);

    // send a message if the bot is busy
    let mut queue_msg_id = None;

    // we also don't want to clutter non-private chats
    if in_private_chat {
        let msg = if count >= 2 {
            format!(
                "Request accepted.\nYour position in the queue: {}.",
                count - 1
            )
        } else {
            "Request accepted.\nThe queue is empty, downloading now.".to_string()
        };

        let queue_msg_result = bot
            .send_message(message.chat.id, msg)
            .reply_to_message_id(message.id)
            .await;

        // we'd like to delete the queue message later
        queue_msg_id = match queue_msg_result {
            Ok(x) => Some(x.id),
            Err(e) => {
                error!("failed to send queue message: {}", e);
                None
            }
        };
    };

    // wait for the mutex to be unlocked
    let _guard = MUTEX.lock().await;
    info!("downloading video from {}", url);

    let filename = format!("{}.mp4", utils::random_string(10));
    let temp_dir = tempdir().unwrap();
    let dir_path = temp_dir.path().to_str().unwrap();
    let full_path = temp_dir.path().join(&filename);
    let full_path_str = full_path.to_str().unwrap();

    // download the video
    let exit_success = utils::download(&url, dir_path).await;

    // find all files in the directory
    let mut files = std::fs::read_dir(dir_path)
        .unwrap()
        .filter_map(|x| x.ok())
        .collect::<Vec<_>>();

    // check if yt-dlp downloaded the video by checking if dir contains a file
    if files.len() != 1 {
        if in_private_chat {
            let flen = files.len();
            let msg = if flen == 0 {
                "no".to_string()
            } else {
                flen.to_string()
            };

            bot.send_message(
                message.chat.id,
                format!("Failed to download video ({} files found).", msg),
            )
            .reply_to_message_id(message.id)
            .await
            .log_on_error()
            .await;
        }
        return Ok(());
    }

    if !exit_success {
        if in_private_chat {
            bot.send_message(
                message.chat.id,
                "Failed to download video (extractor exited with non-zero code).",
            )
            .reply_to_message_id(message.id)
            .await
            .log_on_error()
            .await;
        }
        return Ok(());
    }

    // get the path...
    let entry = files.pop().unwrap();
    let file_path = entry.path().to_str().unwrap().to_string();

    // if file exceeds MAX_FILESIZE megabytes, bail
    let bytes = entry.metadata().unwrap().len();
    let megabytes = bytes / 1000 / 1000;

    if megabytes > *MAX_FILESIZE {
        if in_private_chat {
            bot.send_message(
                message.chat.id,
                format!(
                    "Failed to convert video (base file size exceeds {} MB).",
                    *MAX_FILESIZE
                ),
            )
            .reply_to_message_id(message.id)
            .await
            .log_on_error()
            .await;
        }
        return Ok(());
    }

    info!("video downloaded to {}", file_path);

    // ...and probe the video for metadata
    let metadata = utils::probe(&file_path).unwrap_or_default();
    let original_bitrate = metadata.bitrate;

    // calculate the fallback bitrate
    let fallback_bitrate = if metadata.duration != 0 {
        // notice that we reserved 128 kbps for the audio
        // the total bitrate has been reduced by 3% to account for container overhead
        Some(
            ((((50 * 8000) as f64 / metadata.duration as f64) - 128.0 - 5.0) * 0.97).floor() as u32,
        )
    } else {
        None
    };

    // if the fallback bitrate is less than 85% of the original bitrate, skip to fallback
    let mut skip_to_fallback = false;

    let mut reduction_percentage = None;
    if fallback_bitrate.is_some() {
        let ratio = fallback_bitrate.unwrap() as f64 / original_bitrate as f64;
        reduction_percentage = Some((1.0 - ratio) * 100.0);

        if ratio < 0.85 {
            warn!(
                "fallback bitrate ({} kbps) is {} lower than the original bitrate ({} kbps)",
                fallback_bitrate.unwrap(),
                format!("{:.1}%", reduction_percentage.unwrap()),
                original_bitrate
            );

            skip_to_fallback = true;
        }
    }

    let mut bitrate_reduced = false;

    // first, try to convert the video without adjusting the bitrate
    // (if it seems unlikely that the conversion will fail)
    let exit_success = if !skip_to_fallback {
        utils::convert(&file_path, full_path_str, None).await
    } else {
        false
    };

    // if the conversion failed, try to adjust the bitrate
    // this cannot be done if metadata is not available
    if exit_success {
        info!("converted the video (no bitrate adjustment)");
    } else if !exit_success && fallback_bitrate.is_some() {
        let exit_success = utils::convert(&file_path, full_path_str, fallback_bitrate).await;

        if exit_success {
            info!(
                "converted the video (bitrate adjusted to {} kbps)",
                fallback_bitrate.unwrap(),
            );
            bitrate_reduced = true;
        } else {
            // remove leftover files
            tokio::fs::remove_file(&full_path).await.unwrap();
            error!(
                "failed to convert the video (bitrate adjusted to {} kbps): {}",
                fallback_bitrate.unwrap(),
                url
            );
        }
    } else {
        // remove leftover files
        tokio::fs::remove_file(&full_path).await.unwrap();
        error!(
            "failed to convert the video (no bitrate adjustment): {}",
            url
        );
    }

    if !full_path.exists() {
        error!(
            "failed to download video: path {} does not exist",
            full_path_str
        );

        if in_private_chat {
            bot.send_message(message.chat.id, "Failed to convert the video.")
                .reply_to_message_id(message.id)
                .await
                .log_on_error()
                .await;
        }
        return Ok(());
    } else {
        info!("video converted successfully");
    }

    let file = InputFile::file(&full_path);
    let chat_id = message.chat.id;
    let mut username = None;

    if let Some(user) = message.from() {
        username = user.username.clone();
    } else if let teloxide::types::MessageKind::Common(MessageCommon {
        ref author_signature,
        ..
    }) = message.kind
    {
        username = author_signature.clone(); // channel post
    }

    let mut prefix = String::new();
    if let Some(username) = username {
        prefix = format!("[original poster: {}]", username)
    };

    let message_with_prefix = format!("{}\n{}", prefix, text);
    let thumbnail = utils::get_thumbnail(full_path_str).await;

    let mut request = bot
        .send_video(chat_id, file)
        .width(metadata.width)
        .height(metadata.height)
        .duration(metadata.duration)
        .supports_streaming(true)
        .reply_to_message_id(message.id);

    if let Some(thumbnail) = thumbnail {
        request = request.thumb(thumbnail);
    }

    // if in a private chat, send the video directly
    if in_private_chat {
        if let Err(e) = request.await {
            error!("failed to send the video: {}", e);
        } else {
            info!("the video has been sent");
        }

        // if the bitrate was reduced, send a warning
        if bitrate_reduced {
            bot.send_message(
                chat_id,
                format!(
                    "Warning: the bitrate of the video has been reduced \
                    from {} kbps to {} kbps ({:.1}% reduction) to meet \
                    Telegram's file size limit.",
                    original_bitrate,
                    fallback_bitrate.unwrap(),
                    reduction_percentage.unwrap_or_default(),
                ),
            )
            .reply_to_message_id(message.id)
            .await
            .log_on_error()
            .await;
        }
    } else {
        // if in a group, send the video with the original message
        request = request.caption(message_with_prefix);

        // if the message was a reply, send the video as a reply
        if let Some(reply_to_message) = message.reply_to_message() {
            request = request.reply_to_message_id(reply_to_message.id);
        }

        if let Err(e) = request.await {
            error!("failed to send the video: {}", e);
        } else {
            // delete the original message
            info!("the video has been sent");
            bot.delete_message(chat_id, message.id)
                .await
                .log_on_error()
                .await;
        }
    }

    // remove leftover message
    if let Some(id) = queue_msg_id {
        bot.delete_message(chat_id, id).await.log_on_error().await;
    }

    info!("finished processing");

    Ok(())
}
