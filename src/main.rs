mod utils;
mod ytdl;

use futures::future::select;
use tbot::types::chat::member::Status::{Administrator, Member};
use tbot::types::input_file::Video;
use tokio::signal::unix::{signal, SignalKind};

#[tokio::main]
async fn main() {
    // register a SIGTERM handler
    let mut sigstream =
        signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");
    let sig = sigstream.recv();

    // create the bot and obtain its ID
    let bot = tbot::Bot::from_env("TOKEN");
    let bot_id = match bot.get_me().call().await {
        Ok(me) => me.user.id,
        _ => panic!("Cannot obtain bot's user ID"),
    };

    // create an event loop
    let mut event_loop = bot.event_loop();

    // add a callback for non-command messages
    event_loop.text(move |context| async move {
        // check permissions
        // we need can_delete_messages and can_post_messages
        let chat_id = context.chat.id;
        let get_member = &context.bot.get_chat_member(chat_id, bot_id).call().await;

        match get_member {
            Ok(member) => match member.status {
                Administrator {
                    can_delete_messages: true,
                    can_post_messages: None,
                    ..
                } => { /* pass */ }
                Administrator {
                    can_delete_messages: true,
                    can_post_messages: Some(true),
                    ..
                } => { /* pass */ }
                Member => { /* pass */ }
                _ => return,
            },
            _ => return,
        }

        // prepare the video's caption
        let mut caption = String::new();
        let mut signature: Option<String> = None;
        let msg = &context.text.value;

        // get original poster's nickname when not in a channel
        if let Some(from) = &context.from {
            if &from.first_name == "Telegram" {
                return;
            }
            if let Some(nick) = &from.username {
                signature = Some(nick.to_string())
            }
        }

        // get original poster's nickname when in a channel
        if let Some(nick) = &context.author_signature {
            signature = Some(nick.to_string())
        }

        // copy the original message and add a signature (if applicable)
        if let Some(s) = signature {
            caption.push_str(&format!("[original poster: {}]\n", s));
        }
        caption.push_str(msg);

        // pick a random filename for the .mp4 file
        let filename = format!("{}.mp4", utils::random_string(10));

        // find links in a message
        match ytdl::find_link(msg) {
            None => return, // no link (or too many links) found
            Some(url) => {
                // found a link
                // download the video and log errors
                if let Err(output) = ytdl::download(url, &filename) {
                    eprintln!("youtube-dl failed to download {}", url);
                    eprintln!("{}", output);
                    return;
                }

                // check if youtube-dl downloaded a file
                if !utils::file_exists(&filename) {
                    // the file was over 50MB
                    eprintln!("Cannot download {}, the file is too large", url);
                    return;
                }
            }
        }

        // prepare the video
        let video_bytes = utils::read_file(&filename);
        let video = Video::with_bytes(&video_bytes).caption(&caption);

        // send the video message
        let call_result = utils::exponential_retry_async(|| async {
            let call_base = context.bot.send_video(chat_id, video);

            if let Some(message) = &context.reply_to {
                Ok(call_base.in_reply_to(message.id).call().await?)
            } else {
                Ok(call_base.call().await?)
            }
        })
        .await;

        // make sure everything went smoothly
        if let Err(err) = call_result {
            eprintln!("An error occurred during send_video: {}", err);
        }

        // delete the original message
        let call_result = utils::exponential_retry_async(|| async {
            Ok(context
                .bot
                .delete_message(chat_id, context.message_id)
                .call()
                .await?)
        })
        .await;

        // make sure everything went smoothly
        if let Err(err) = call_result {
            eprintln!("An error occurred during send_video: {}", err);
        }

        // delete the leftover file
        utils::delete_file(&filename);
    });

    // start the bot, ignore the pesky timeout messages
    let polling = event_loop.polling().error_handler(|_| async {}).start();

    // await SIGTERM and ensure that polling is stopped
    select(Box::pin(polling), Box::pin(sig)).await;
}
