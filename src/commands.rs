//! Bot logic.

use crate::{
    env,
    task::{Task, TaskOutput, TaskResult},
    task_manager::TaskManager,
    utils::{self, URLsFound},
};

use color_eyre::eyre::{Context, bail};
use teloxide::{
    prelude::*,
    sugar::request::RequestReplyExt,
    types::{MessageId, MessageKind, ParseMode},
    utils::command::BotCommands,
};
use tokio::sync::oneshot;

#[derive(BotCommands, Clone, Debug)]
#[command(
    rename_rule = "snake_case",
    description = "The following commands are supported:"
)]
pub enum Command {
    #[command(description = "display this text.", aliases = ["start"])]
    Help,
    #[command(description = "display the current status of the bot.")]
    Status,
    #[command(description = "download a video from a supported website.")]
    Yeet(String),
    #[command(description = "try to download a video using fallback methods (bypasses allowlist).", aliases = ["yeet_pls", "plz", "pls"], hide_aliases)]
    YeetPlz(String),
    #[command(description = "list all supported websites.")]
    Allowlist,
}

/// Answer a plaintext message (by wrapping it in `Command::Yeet`).
pub async fn answer_plaintext(
    bot: Bot,
    msg: Message,
    task_manager: TaskManager,
) -> color_eyre::Result<()> {
    let maybe_msg_text = msg.text();
    let msg_text = maybe_msg_text.unwrap_or_default().to_owned();
    answer_command(bot, msg, Command::Yeet(msg_text), task_manager).await
}

/// Possible answers to a command.
enum Answer {
    Nothing,
    SendMessage {
        text: String,
    },
    StartDownload {
        accept_message: String,
        url: String,
        enable_fallback: bool,
    },
    SendVideo {
        contents: Box<TaskOutput>,
        maybe_caption: Option<String>,
    },
}

/// Answer a `Command`, starting with the entrypoint.
pub async fn answer_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    task_manager: TaskManager,
) -> color_eyre::Result<()> {
    Box::pin(handle_answer(
        &bot,
        &msg,
        &task_manager,
        answer_entrypoint(&msg, &cmd, &task_manager),
    ))
    .await
}

/// Internal implementation of answering a `Command`.
async fn handle_answer(
    bot: &Bot,
    msg: &Message,
    task_manager: &TaskManager,
    answer: Answer,
) -> color_eyre::Result<()> {
    let sanitise = |text: &str| {
        text.replace('.', r"\.")
            .replace('(', r"\(")
            .replace(')', r"\)")
            .replace('-', r"\-")
            .replace('_', r"\_")
    };

    let send_msg_with_reply =
        async |text: String, reply_to_id: MessageId| -> color_eyre::Result<()> {
            bot.send_message(msg.chat.id, sanitise(&text))
                .reply_to(reply_to_id)
                .parse_mode(ParseMode::MarkdownV2)
                .await
                .wrap_err("failed to send message")?;

            Ok(())
        };

    let send_msg =
        async |text: String| -> color_eyre::Result<()> { send_msg_with_reply(text, msg.id).await };

    match answer {
        Answer::Nothing => Ok(()),
        Answer::SendMessage { text } => send_msg(text).await,
        Answer::StartDownload {
            accept_message,
            url,
            enable_fallback,
        } => {
            send_msg(accept_message).await?;
            match download(task_manager, &url, enable_fallback).await {
                Ok(dl_ok) => {
                    Box::pin(
                        // recursive call, pinned to avoid infinite future size
                        handle_answer(bot, msg, task_manager, dl_ok),
                    )
                    .await
                }
                Err(e) => send_msg(format!("Failed to download video ({e}).")).await,
            }
        }
        Answer::SendVideo {
            contents,
            maybe_caption,
        } => {
            let mut request = bot
                .send_video(msg.chat.id, contents.video_file)
                .reply_to(msg.id)
                .width(contents.metadata.width)
                .height(contents.metadata.height)
                .duration(contents.metadata.duration)
                .supports_streaming(true)
                .reply_to(msg.id);

            if let Some(thumb) = contents.maybe_thumbnail {
                request = request.thumbnail(thumb);
            }

            let video_msg_id = request.await.wrap_err("failed to send video")?.id;

            if let Some(caption) = maybe_caption {
                send_msg_with_reply(caption, video_msg_id).await?;
            }

            Ok(())
        }
    }
}

/// Starting point for answering a `Command`.
fn answer_entrypoint(msg: &Message, cmd: &Command, task_manager: &TaskManager) -> Answer {
    // 1. do not react to pins, polls, etc.
    // 2. bail if forwarded to a non-private chat
    if !matches!(msg.kind, MessageKind::Common(_))
        || (!msg.chat.is_private() && msg.forward_date().is_some())
    {
        return Answer::Nothing;
    }

    let allowlist_str = env::ALLOWLIST
        .iter()
        .map(|x| format!("`{x}`"))
        .collect::<Vec<_>>()
        .join(", ");

    // basic commands
    let maybe_response = match cmd {
        Command::Help => Some(Command::descriptions().to_string()),
        Command::Status => Some(format!(
            "Number of active tasks: {}.",
            task_manager.get_queue_size()
        )),
        Command::Allowlist => {
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

    let urls_found = extract_urls(msg, msg_text);

    let maybe_error_msg = match &urls_found {
        URLsFound::None => Some("No URLs found.".to_string()),
        URLsFound::Multiple => {
            Some("Downloading more than one video at a time is unsupported.".to_string())
        }
        URLsFound::One { supported, .. } => {
            if *supported || fallback_enabled {
                None
            } else {
                let allowlist_str = if env::ALLOWLIST.is_empty() {
                    "none".to_string()
                } else {
                    allowlist_str
                };

                Some(format!(
                    "URL is unsupported.\n\nSupported websites: {allowlist_str}."
                ))
            }
        }
    };

    if let Some(error_msg) = maybe_error_msg {
        let final_msg = if env::MAINTAINER.is_none() {
            error_msg
        } else {
            format!(
                "{error_msg}\n\nFor more information, please contact {}.",
                env::MAINTAINER.as_ref().unwrap()
            )
        };

        return Answer::SendMessage { text: final_msg };
    }

    // proceed to download routine
    let queue_position = task_manager.get_queue_size();

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
) -> color_eyre::Result<Answer> {
    let (tx, rx) = oneshot::channel::<TaskResult>();

    task_manager.enqueue_task(Task {
        url: url.to_string(),
        enable_fallback,
        return_channel: tx,
    });

    let recv_ok = rx.await.wrap_err("internal error: channel closed")?;

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
