//! Bot logic.

use crate::{
    env,
    task::{Task, TaskOutput, TaskResult},
    task_manager::TaskManager,
    utils::{self, URLsFound},
    worker::TentativeToken,
};

use anyhow::{Context, bail};
use teloxide::{
    prelude::*,
    sugar::request::RequestReplyExt,
    types::{MessageKind, ParseMode},
    utils::command::BotCommands,
};
use tokio::sync::oneshot;
use tracing::{Instrument, debug, error, info, info_span, warn};

#[derive(BotCommands, Clone, Debug)]
#[command(
    rename_rule = "snake_case",
    description = "The following commands are supported:"
)]
/// Commands accepted by the bot.
pub enum Command {
    /// Display a help message.
    #[command(description = "display this text.", aliases = ["start"])]
    Help,
    /// Display the current status of the bot.
    #[command(description = "display the current status of the bot.")]
    Status,
    /// Download a video (no fallback).
    #[command(description = "download a video from a supported website.")]
    Yeet(String),
    /// Download a video (with fallback).
    #[command(description = "try to download a video using fallback methods (bypasses allowlist).", aliases = ["yeet_pls", "plz", "pls"], hide_aliases)]
    YeetPlz(String),
    /// Display a list of all supported websites.
    #[command(description = "list all supported websites.")]
    Allowlist,
}

/// Answer a plaintext message (by wrapping it in `Command::Yeet`).
pub async fn answer_plaintext(
    bot: Bot,
    msg: Message,
    task_manager: TaskManager,
) -> anyhow::Result<()> {
    let maybe_msg_text = msg.text();
    let msg_text = maybe_msg_text.unwrap_or_default().to_owned();
    answer_command(bot, msg, Command::Yeet(msg_text), task_manager).await
}

/// Generate a short request ID for tracing.
fn generate_request_id() -> String {
    petname::petname(2, "-").unwrap_or_else(|| utils::random_string(8))
}

/// Answer a `Command`, starting with the entrypoint.
pub async fn answer_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    task_manager: TaskManager,
) -> anyhow::Result<()> {
    let request_id = generate_request_id();
    let chat_id = msg.chat.id.0;
    let msg_id = msg.id.0;

    let span = info_span!(
        "request",
        id = %request_id,
        chat_id,
        msg_id,
    );

    async {
        info!(cmd = ?cmd, "received command");
        let result = Box::pin(handle_answer(
            &bot,
            &msg,
            &task_manager,
            answer_entrypoint(&msg, &cmd, &task_manager),
        ))
        .await;

        match &result {
            Ok(()) => info!("request completed successfully"),
            Err(e) => warn!(error = %e, "request completed with error"),
        }

        result
    }
    .instrument(span)
    .await
}

/// Possible answers to a command.
enum Answer {
    /// Terminate communication.
    Nothing,
    /// Send a text message.
    SendMessage {
        /// Text to send.
        text: String,
    },
    /// Initiate the download process.
    StartDownload {
        /// Message to send when download starts.
        accept_message: String,
        /// URL to download.
        url: String,
        /// Whether to enable fallback mode.
        enable_fallback: bool,
        /// Token for the tentatively accepted task.
        token: TentativeToken,
    },
    /// Initiate the video upload process.
    SendVideo {
        /// Contents of the video message.
        contents: Box<TaskOutput>,
        /// Optional caption for the video.
        maybe_caption: Option<String>,
    },
}

/// Sanitise text for `MarkdownV2` formatting.
fn sanitise_markdown(text: &str) -> String {
    text.replace('.', r"\.")
        .replace('(', r"\(")
        .replace(')', r"\)")
        .replace('-', r"\-")
        .replace('_', r"\_")
}

/// Send a video to the user.
async fn send_video(
    bot: &Bot,
    msg: &Message,
    contents: Box<TaskOutput>,
    maybe_caption: Option<String>,
) -> anyhow::Result<()> {
    let chat_id = msg.chat.id.0;

    debug!(
        chat_id,
        width = contents.metadata.width,
        height = contents.metadata.height,
        duration_secs = contents.metadata.duration,
        bitrate_kbps = contents.metadata.bitrate,
        "sending video"
    );

    let mut request = bot
        .send_video(msg.chat.id, contents.video_file)
        .reply_to(msg.id)
        .width(contents.metadata.width)
        .height(contents.metadata.height)
        .duration(contents.metadata.duration)
        .supports_streaming(true);

    if let Some(thumb) = contents.maybe_thumbnail {
        request = request.thumbnail(thumb);
    }

    let video_msg_id = match request.await {
        Ok(sent_video) => {
            info!(chat_id, "video sent successfully");
            sent_video.id
        }
        Err(e) => {
            error!(chat_id, error = %e, "failed to send video");
            return Err(e).context("failed to send video");
        }
    };

    if let Some(caption) = maybe_caption {
        bot.send_message(msg.chat.id, sanitise_markdown(&caption))
            .reply_to(video_msg_id)
            .parse_mode(ParseMode::MarkdownV2)
            .await
            .context("failed to send caption")?;
    }

    Ok(())
}

/// Internal implementation of answering a `Command`.
async fn handle_answer(
    bot: &Bot,
    msg: &Message,
    task_manager: &TaskManager,
    answer: Answer,
) -> anyhow::Result<()> {
    let chat_id = msg.chat.id.0;

    let send_msg = async |text: String| -> anyhow::Result<()> {
        bot.send_message(msg.chat.id, sanitise_markdown(&text))
            .reply_to(msg.id)
            .parse_mode(ParseMode::MarkdownV2)
            .await
            .context("failed to send message")?;
        Ok(())
    };

    match answer {
        Answer::Nothing => {
            debug!(chat_id, "ignoring message (nothing to do)");
            Ok(())
        }
        Answer::SendMessage { text } => {
            debug!(chat_id, "sending text response");
            send_msg(text).await
        }
        Answer::StartDownload {
            accept_message,
            url,
            enable_fallback,
            token,
        } => {
            info!(chat_id, url = %url, fallback = enable_fallback, "starting download");
            // If sending the acceptance message fails, the token is dropped
            // and the tentative counter is decremented automatically.
            send_msg(accept_message).await?;
            match download(task_manager, &url, enable_fallback, token).await {
                Ok(dl_ok) => {
                    debug!(chat_id, url = %url, "download completed, sending video");
                    Box::pin(handle_answer(bot, msg, task_manager, dl_ok)).await
                }
                Err(e) => {
                    error!(chat_id, url = %url, error = %e, "download failed");
                    send_msg(format!("Failed to download video ({e}).")).await
                }
            }
        }
        Answer::SendVideo {
            contents,
            maybe_caption,
        } => send_video(bot, msg, contents, maybe_caption).await,
    }
}

/// Starting point for answering a `Command`.
fn answer_entrypoint(msg: &Message, cmd: &Command, task_manager: &TaskManager) -> Answer {
    let chat_id = msg.chat.id.0;

    // 1. do not react to pins, polls, etc.
    // 2. bail if forwarded to a non-private chat
    if !matches!(msg.kind, MessageKind::Common(_))
        || (!msg.chat.is_private() && msg.forward_date().is_some())
    {
        debug!(chat_id, "ignoring non-common or forwarded message");
        return Answer::Nothing;
    }

    let allowlist_str = env::ALLOWLIST
        .iter()
        .map(|x| format!("`{x}`"))
        .collect::<Vec<_>>()
        .join(", ");

    // basic commands
    let maybe_response = match cmd {
        Command::Help => {
            debug!(chat_id, "received /help command");
            Some(Command::descriptions().to_string())
        }
        Command::Status => {
            let queue_size = task_manager.get_queue_size();
            debug!(chat_id, queue_size, "received /status command");
            Some(format!("Number of active tasks: {queue_size}."))
        }
        Command::Allowlist => {
            debug!(chat_id, "received /allowlist command");
            let allowlist_str = if env::ALLOWLIST.is_empty() {
                " none".to_string()
            } else {
                format!("\n{allowlist_str}")
            };

            Some(format!("Supported websites:{allowlist_str}."))
        }
        _ => None,
    };

    if let Some(response) = maybe_response {
        return Answer::SendMessage { text: response };
    }

    // download commands
    let fallback_enabled = matches!(cmd, Command::YeetPlz(_));
    let (Command::Yeet(msg_text) | Command::YeetPlz(msg_text)) = cmd else {
        unreachable!()
    };

    debug!(
        chat_id,
        fallback = fallback_enabled,
        "received download command"
    );

    let urls_found = extract_urls(msg, msg_text);

    let has_reply = matches!(&msg.kind, MessageKind::Common(mc) if mc.reply_to_message.is_some());

    if let Some(error_msg) = url_error(
        &urls_found,
        msg_text,
        has_reply,
        fallback_enabled,
        &allowlist_str,
        chat_id,
    ) {
        let final_msg = match env::MAINTAINER.as_ref() {
            Some(maintainer) => {
                format!("{error_msg}\n\nFor more information, please contact {maintainer}.")
            }
            None => error_msg,
        };

        return Answer::SendMessage { text: final_msg };
    }

    // Proceed to download routine by registering a tentative task.
    // The returned token keeps the tentative counter incremented.
    // It is either committed via enqueue_task() or auto-cancelled on drop.
    let (token, queue_position) = task_manager.tentative_enqueue();
    debug!(chat_id, queue_position, "task tentatively enqueued");

    let accept_message = if queue_position == 0 {
        "Request accepted.\nThe queue is empty, downloading now.".to_string()
    } else {
        format!("Request accepted.\nYour position in the queue: {queue_position}.")
    };

    let URLsFound::One { url, .. } = urls_found else {
        unreachable!()
    };

    Answer::StartDownload {
        accept_message,
        url,
        enable_fallback: fallback_enabled,
        token,
    }
}

/// Check whether the URL extraction result is an error condition.
///
/// Returns `Some(error_message)` if the download should not proceed.
fn url_error(
    urls_found: &URLsFound,
    msg_text: &str,
    has_reply: bool,
    fallback_enabled: bool,
    allowlist_str: &str,
    chat_id: i64,
) -> Option<String> {
    match urls_found {
        URLsFound::None => {
            debug!(chat_id, "no URLs found in message");
            let text = if msg_text.is_empty() && !has_reply {
                "Provide a URL after the command, or reply to a message containing one.".to_string()
            } else {
                "No URLs found.".to_string()
            };
            Some(text)
        }
        URLsFound::Multiple => {
            debug!(chat_id, "multiple URLs found, rejecting");
            Some("Downloading more than one video at a time is unsupported.".to_string())
        }
        URLsFound::One { url, supported } => {
            debug!(chat_id, url = %url, supported = *supported, "URL extracted");
            if *supported || fallback_enabled {
                None
            } else {
                let allowlist_str = if env::ALLOWLIST.is_empty() {
                    "none".to_string()
                } else {
                    allowlist_str.to_string()
                };
                Some(format!(
                    "URL is unsupported.\n\nSupported websites: {allowlist_str}."
                ))
            }
        }
    }
}

/// Search for URLs in a message, optionally falling back to one it's replying to.
fn extract_urls(msg: &Message, msg_text: &str) -> URLsFound {
    let maybe_original_msg_url = utils::get_url_info(msg_text);

    // case 1: original message contains a single URL
    if matches!(maybe_original_msg_url, URLsFound::One { .. }) {
        return maybe_original_msg_url;
    }

    // case 2: let's look at the message that the original is replying to
    // will not work if the bot is in privacy mode: https://core.telegram.org/bots/features#privacy-mode
    let in_reply_to = if let MessageKind::Common(mc) = &msg.kind {
        mc.reply_to_message
            .as_ref()
            .and_then(|reply| reply.text())
            .map(std::string::ToString::to_string)
    } else {
        // don't unwrap other message kinds
        return maybe_original_msg_url;
    };

    // no reply, bail
    if in_reply_to.is_none() {
        return maybe_original_msg_url;
    }

    let maybe_reply_url = utils::get_url_info(&in_reply_to.unwrap());

    if matches!(maybe_reply_url, URLsFound::One { .. }) {
        maybe_reply_url
    } else {
        maybe_original_msg_url
    }
}

/// Video download routine.
async fn download(
    task_manager: &TaskManager,
    url: &str,
    enable_fallback: bool,
    token: TentativeToken,
) -> anyhow::Result<Answer> {
    let (tx, rx) = oneshot::channel::<TaskResult>();

    debug!(url = %url, "enqueueing task to worker");
    task_manager.enqueue_task(
        Task {
            url: url.to_string(),
            enable_fallback,
            return_channel: tx,
        },
        token,
    );

    debug!(url = %url, "waiting for worker result");
    let recv_ok = rx.await.context("internal error: channel closed")?;

    match recv_ok {
        Err(e) => {
            bail!("processing error: {e}");
        }
        Ok(contents) => {
            let maybe_caption = contents.reduced_bitrate.map(|fallback_bitrate| {
                let ratio = f64::from(fallback_bitrate) / f64::from(contents.metadata.bitrate);
                let reduction_percentage = (1.0 - ratio) * 100.0;

                format!(
                    "Warning: the bitrate of the video has been reduced \
                    from {} kbps to {} kbps ({:.1}% reduction) to meet \
                    Telegram's file size limit.",
                    contents.metadata.bitrate, fallback_bitrate, reduction_percentage,
                )
            });

            Ok(Answer::SendVideo {
                contents,
                maybe_caption,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod sanitise_markdown {
        use super::*;

        #[test]
        fn leaves_plain_text_unchanged() {
            assert_eq!(sanitise_markdown("hello world"), "hello world");
        }

        #[test]
        fn escapes_periods() {
            assert_eq!(sanitise_markdown("end."), r"end\.");
            assert_eq!(sanitise_markdown("a.b.c"), r"a\.b\.c");
        }

        #[test]
        fn escapes_parentheses() {
            assert_eq!(sanitise_markdown("(test)"), r"\(test\)");
        }

        #[test]
        fn escapes_hyphens() {
            assert_eq!(sanitise_markdown("a-b"), r"a\-b");
        }

        #[test]
        fn escapes_underscores() {
            assert_eq!(sanitise_markdown("hello_world"), r"hello\_world");
        }

        #[test]
        fn escapes_multiple_special_chars() {
            assert_eq!(
                sanitise_markdown("Error: (unavailable - retry)."),
                r"Error: \(unavailable \- retry\)\."
            );
        }

        #[test]
        fn handles_empty_string() {
            assert_eq!(sanitise_markdown(""), "");
        }

        #[test]
        fn preserves_unicode() {
            assert_eq!(sanitise_markdown("สวัสดีโลก"), "สวัสดีโลก");
            assert_eq!(sanitise_markdown("日本語"), "日本語");
        }

        #[test]
        fn escapes_consecutive_special_chars() {
            assert_eq!(sanitise_markdown("..."), r"\.\.\.");
            assert_eq!(sanitise_markdown("---"), r"\-\-\-");
        }
    }
}
