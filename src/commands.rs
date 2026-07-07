//! Bot command routing: parse incoming messages and decide what to do.

use crate::{
    env,
    queue::{AcceptPermit, TentativeToken},
    task::TaskOutput,
    task_manager::TaskManager,
    utils::{self, URLsFound},
};

use teloxide::{
    prelude::*,
    types::MessageKind,
    utils::{command::BotCommands, markdown},
};
use tracing::{Instrument, debug, info, info_span, warn};

/// Commands accepted by the bot.
#[derive(BotCommands, Clone, Debug)]
#[command(
    rename_rule = "snake_case",
    description = "The following commands are supported:"
)]
pub enum Command {
    /// Displays a help message.
    #[command(description = "display this text.", aliases = ["start"])]
    Help,
    /// Displays the current status of the bot.
    #[command(description = "display the current status of the bot.")]
    Status,
    /// Downloads a video (no fallback).
    #[command(description = "download a video from a supported website.")]
    Yeet(String),
    /// Downloads a video (with fallback).
    #[command(description = "try to download a video using fallback methods (bypasses allowlist).", aliases = ["yeet_pls", "plz", "pls"], hide_aliases)]
    YeetPlz(String),
    /// Displays a list of all supported websites.
    #[command(description = "list all supported websites.")]
    Allowlist,
}

/// Possible outcomes of routing a command.
pub enum Answer {
    /// Terminates communication.
    Nothing,
    /// Sends a text message.
    SendMessage {
        /// Text to send, already escaped as valid `MarkdownV2`.
        text: String,
    },
    /// Initiates the download process.
    StartDownload {
        /// Message to send when download starts, already escaped as valid `MarkdownV2`.
        accept_message: String,
        /// URL to download.
        url: String,
        /// Whether to enable fallback mode.
        enable_fallback: bool,
        /// Token for the tentatively accepted task.
        token: TentativeToken,
        /// Permit that gates acceptance message sending order.
        accept_permit: AcceptPermit,
    },
    /// Initiates the video upload process.
    SendVideo {
        /// Contents of the video message.
        contents: Box<TaskOutput>,
        /// Optional caption for the video.
        maybe_caption: Option<String>,
    },
}

/// Answers a plaintext message by wrapping it in [`Command::Yeet`].
pub async fn answer_plaintext(
    bot: Bot,
    msg: Message,
    task_manager: TaskManager,
) -> anyhow::Result<()> {
    let maybe_msg_text = msg.text();
    let msg_text = maybe_msg_text.unwrap_or_default().to_owned();
    answer_command(bot, msg, Command::Yeet(msg_text), task_manager).await
}

/// Generates a short request ID for tracing.
fn generate_request_id() -> String {
    petname::petname(2, "-").unwrap_or_else(|| utils::random_string(8))
}

/// Answers a [`Command`] by routing it through [`answer_entrypoint`].
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
        let result = crate::messaging::handle_answer(
            &bot,
            &msg,
            &task_manager,
            answer_entrypoint(&msg, &cmd, &task_manager),
        )
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

/// Routes a [`Command`] to an [`Answer`].
pub fn answer_entrypoint(msg: &Message, cmd: &Command, task_manager: &TaskManager) -> Answer {
    let chat_id = msg.chat.id.0;

    // 1. do not react to pins, polls, or other service messages
    // 2. bail if forwarded to a non-private chat
    if !matches!(msg.kind, MessageKind::Common(_))
        || (!msg.chat.is_private() && msg.forward_date().is_some())
    {
        debug!(chat_id, "ignoring non-common or forwarded message");
        return Answer::Nothing;
    }

    // pre-escaped MarkdownV2 code list; ALLOWLIST is validated non-empty at startup
    let allowlist_str = {
        let mut entries = env::ALLOWLIST.iter().collect::<Vec<_>>();
        entries.sort();
        entries
            .into_iter()
            .map(|x| markdown::code_inline(&markdown::escape_code(x)))
            .collect::<Vec<_>>()
            .join(", ")
    };

    // basic commands
    let maybe_response = match cmd {
        Command::Help => {
            debug!(chat_id, "received /help command");
            Some(markdown::escape(&Command::descriptions().to_string()))
        }
        Command::Status => {
            let queue_size = task_manager.get_queue_size();
            debug!(chat_id, queue_size, "received /status command");
            Some(markdown::escape(&format!(
                "Number of active tasks: {queue_size}."
            )))
        }
        Command::Allowlist => {
            debug!(chat_id, "received /allowlist command");
            Some(format!("Supported websites:\n{allowlist_str}\\."))
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
                let suffix = markdown::escape(&format!(
                    "\n\nFor more information, please contact {maintainer}."
                ));
                format!("{error_msg}{suffix}")
            }
            None => error_msg,
        };

        return Answer::SendMessage { text: final_msg };
    }

    // proceed to download routine by registering a tentative task
    // the returned token keeps the tentative counter incremented
    // it is either committed via enqueue_task() or auto-cancelled on drop
    let (token, queue_position, accept_permit) = task_manager.tentative_enqueue();
    debug!(chat_id, queue_position, "task tentatively enqueued");

    let accept_message = if queue_position == 0 {
        markdown::escape("Request accepted.\nThe queue is empty, downloading now.")
    } else {
        markdown::escape(&format!(
            "Request accepted.\nYour position in the queue: {queue_position}."
        ))
    };

    let URLsFound::One { url, .. } = urls_found else {
        unreachable!()
    };

    Answer::StartDownload {
        accept_message,
        url,
        enable_fallback: fallback_enabled,
        token,
        accept_permit,
    }
}

/// Checks whether the URL extraction result is an error condition.
///
/// Returns [`Some`] with a `MarkdownV2`-escaped error message if the download
/// should not proceed, [`None`] otherwise. `allowlist_str` must already be
/// valid `MarkdownV2`.
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
                "Provide a URL after the command, or reply to a message containing one."
            } else {
                "No URLs found."
            };
            Some(markdown::escape(text))
        }
        URLsFound::Multiple => {
            debug!(chat_id, "multiple URLs found, rejecting");
            Some(markdown::escape(
                "Downloading more than one video at a time is unsupported.",
            ))
        }
        URLsFound::One { url, supported } => {
            debug!(chat_id, url = %url, supported = *supported, "URL extracted");
            if *supported || fallback_enabled {
                None
            } else {
                Some(format!(
                    "URL is unsupported\\.\n\nSupported websites: {allowlist_str}\\."
                ))
            }
        }
    }
}

/// Searches for URLs in a message, optionally falling back to one it's replying to.
fn extract_urls(msg: &Message, msg_text: &str) -> URLsFound {
    let maybe_original_msg_url = utils::get_url_info(msg_text, &env::ALLOWLIST);

    // case 1: the original message settles it
    // a single URL proceeds; multiple URLs reject without consulting the reply
    if !matches!(maybe_original_msg_url, URLsFound::None) {
        return maybe_original_msg_url;
    }

    // case 2: look at the replied-to message
    // will not work if the bot is in privacy mode: https://core.telegram.org/bots/features#privacy-mode
    let in_reply_to = if let MessageKind::Common(mc) = &msg.kind {
        mc.reply_to_message
            .as_ref()
            .and_then(|reply| reply.text())
            .map(ToString::to_string)
    } else {
        // don't unwrap other message kinds
        return maybe_original_msg_url;
    };

    // no reply, bail
    let Some(reply) = in_reply_to else {
        return maybe_original_msg_url;
    };

    let maybe_reply_url = utils::get_url_info(&reply, &env::ALLOWLIST);

    if matches!(maybe_reply_url, URLsFound::One { .. }) {
        maybe_reply_url
    } else {
        maybe_original_msg_url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod url_error {
        use super::*;

        const ALLOWLIST_STR: &str = "`example.com`";

        #[test]
        fn none_with_empty_msg_and_no_reply_prompts_usage() {
            let result = url_error(&URLsFound::None, "", false, false, ALLOWLIST_STR, 0);
            assert!(result.is_some());
            assert!(result.unwrap().contains("Provide a URL"));
        }

        #[test]
        fn none_with_nonempty_msg_says_no_urls_found() {
            let result = url_error(&URLsFound::None, "hello", false, false, ALLOWLIST_STR, 0);
            assert_eq!(result.unwrap(), r"No URLs found\.");
        }

        #[test]
        fn none_with_empty_msg_but_has_reply_says_no_urls_found() {
            // has_reply=true means we already looked at the replied-to message - just say no URLs
            let result = url_error(&URLsFound::None, "", true, false, ALLOWLIST_STR, 0);
            assert_eq!(result.unwrap(), r"No URLs found\.");
        }

        #[test]
        fn multiple_is_always_rejected() {
            let result = url_error(&URLsFound::Multiple, "", false, false, ALLOWLIST_STR, 0);
            assert!(result.is_some());
            assert!(result.unwrap().to_lowercase().contains("unsupported"));
        }

        #[test]
        fn supported_url_proceeds() {
            let found = URLsFound::One {
                url: "https://example.com/v".to_string(),
                supported: true,
            };
            assert!(url_error(&found, "", false, false, ALLOWLIST_STR, 0).is_none());
        }

        #[test]
        fn unsupported_url_without_fallback_is_rejected() {
            let found = URLsFound::One {
                url: "https://unsupported.com/v".to_string(),
                supported: false,
            };
            let result = url_error(&found, "", false, false, ALLOWLIST_STR, 0);
            assert!(result.is_some());
            assert!(result.unwrap().contains("unsupported"));
        }

        #[test]
        fn unsupported_url_with_fallback_proceeds() {
            let found = URLsFound::One {
                url: "https://unsupported.com/v".to_string(),
                supported: false,
            };
            assert!(url_error(&found, "", false, true, ALLOWLIST_STR, 0).is_none());
        }

        #[test]
        fn supported_url_with_fallback_proceeds() {
            let found = URLsFound::One {
                url: "https://example.com/v".to_string(),
                supported: true,
            };
            assert!(url_error(&found, "", false, true, ALLOWLIST_STR, 0).is_none());
        }
    }
}
