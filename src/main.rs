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

    // if the message is forwarded, ignore it
    if message.forward_date().is_some() {
        debug!("message is forwarded");
        return Ok(());
    }

    // download one video at a time
    MUTEX.lock().await;

    let url = maybe_url.unwrap();

    info!("downloading video from {}", url);

    let filename = format!("{}.mp4", utils::random_string(10));
    let temp_dir = tempdir().unwrap();
    let dir_path = temp_dir.path().to_str().unwrap();
    let full_path = temp_dir.path().join(&filename);
    let full_path_str = full_path.to_str().unwrap();

    utils::download_and_convert(url, &dir_path, &filename).await;

    if !full_path.exists() {
        error!(
            "failed to download the video: path {} does not exist",
            full_path_str
        );

        // if in a private chat, send a message
        if let ChatKind::Private(_) = message.chat.kind {
            bot.send_message(message.chat.id, "Failed to download the video.")
                .reply_to_message_id(message.id)
                .await
                .log_on_error()
                .await;
        }
        return Ok(());
    } else {
        info!("downloaded the video: {}", full_path_str);
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

    // if in a private chat, send the video directly
    if let ChatKind::Private(_) = message.chat.kind {
        let result = bot.send_video(chat_id, file).await;

        if let Err(e) = result {
            error!("failed to send the video: {}", e);
        } else {
            info!("the video has been sent");
        }
    }
    // if in a group, send the video with the original message
    else {
        // if the message was a reply, send the video as a reply
        let result = if let Some(reply_to_message) = message.reply_to_message() {
            bot.send_video(chat_id, file)
                .reply_to_message_id(reply_to_message.id)
                .caption(message_with_prefix)
                .await
        } else {
            bot.send_video(chat_id, file)
                .caption(message_with_prefix)
                .await
        };

        if let Err(e) = result {
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
