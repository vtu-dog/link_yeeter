use std::sync::Arc;

use dotenvy::dotenv;
use futures::lock::Mutex;
use once_cell::sync::Lazy;
use teloxide::{
    dispatching::UpdateHandler,
    prelude::*,
    types::{ChatKind, InputFile, MessageCommon},
};
use tempfile::tempdir;

#[macro_use]
extern crate simple_log;

mod utils;

/// Starts the application.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    dotenv().expect("failed to load .env file");
    simple_log::quick!("info");
    info!("application started");

    let bot = Bot::from_env().auto_send();
    Dispatcher::builder(bot, schema())
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}

type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Defines routes for the bot.
fn schema() -> UpdateHandler<Box<dyn std::error::Error + Send + Sync + 'static>> {
    let message_handler = Update::filter_message().endpoint(handler);
    let channel_handler = Update::filter_channel_post().endpoint(handler);

    dptree::entry()
        .branch(message_handler)
        .branch(channel_handler)
}

static MUTEX: Lazy<Arc<Mutex<()>>> = Lazy::new(|| Arc::new(Mutex::new(())));

/// Handles incoming messages.
async fn handler(message: Message, bot: AutoSend<Bot>) -> HandlerResult {
    let text = message.text().unwrap_or_default();
    let maybe_url = utils::find_url(&text);
    if maybe_url.is_none() {
        debug!("no URLs found");
        return Ok(());
    }

    let url = maybe_url.unwrap();

    // if the message is forwarded, ignore it
    if message.forward_date().is_some() {
        debug!("message is forwarded");
        return Ok(());
    }

    // download one video at a time
    MUTEX.lock().await;

    let in_private_chat = if let ChatKind::Private(_) = message.chat.kind {
        true
    } else {
        false
    };

    info!("downloading video from {}", url);

    let filename = format!("{}.mp4", utils::random_string(10));
    let temp_dir = tempdir().unwrap();
    let dir_path = temp_dir.path().to_str().unwrap();
    let full_path = temp_dir.path().join(&filename);
    let full_path_str = full_path.to_str().unwrap();

    // download the video
    utils::download(url, &dir_path).await;

    let mut files = std::fs::read_dir(dir_path)
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();

    // check if yt-dlp downloaded the video by checking if dir contains a file
    if files.len() != 1 {
        if in_private_chat {
            bot.send_message(message.chat.id, "Failed to download video.")
                .reply_to_message_id(message.id)
                .await
                .log_on_error()
                .await;
        }
        return Ok(());
    }

    // get the path...
    let entry = files.pop().unwrap().unwrap();
    let file_path = entry.path().to_str().unwrap().to_string();

    info!("video downloaded to {}", file_path);

    // ..and convert the video to mp4
    utils::convert(&file_path, full_path_str).await;

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
        info!("converted the video: {}", full_path_str);
    }

    let file = InputFile::file(&full_path);
    let metadata = utils::probe(full_path_str).unwrap_or_default();
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

    let mut request = bot
        .send_video(chat_id, file)
        .width(metadata.width)
        .height(metadata.height)
        .duration(metadata.duration)
        .supports_streaming(true);

    // if in a private chat, send the video directly
    if in_private_chat {
        if let Err(e) = request.await {
            error!("failed to send the video: {}", e);
        } else {
            info!("the video has been sent");
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
            bot.delete_message(chat_id, message.id).await.ok();
        }
    }

    info!("finished processing");

    Ok(())
}
