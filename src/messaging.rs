//! Bot reply execution: sending acceptance messages, videos, and error replies.

use crate::{
    commands::Answer,
    task::{Task, TaskOutput, TaskResult},
    task_manager::TaskManager,
};

use anyhow::Context;
use teloxide::{prelude::*, sugar::request::RequestReplyExt, types::ParseMode, utils::markdown};
use tokio::sync::oneshot;
use tracing::{debug, error, info};

/// Sends a video to the user.
///
/// `maybe_caption` must already be valid `MarkdownV2`.
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
        bot.send_message(msg.chat.id, caption)
            .reply_to(video_msg_id)
            .parse_mode(ParseMode::MarkdownV2)
            .await
            .context("failed to send caption")?;
    }

    Ok(())
}

/// Enqueues a download task and awaits the result.
async fn download(
    task_manager: &TaskManager,
    url: &str,
    enable_fallback: bool,
    token: crate::queue::TentativeToken,
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
            anyhow::bail!("processing error: {e}");
        }
        Ok(contents) => {
            let maybe_caption = contents.reduced_bitrate.map(|fallback_bitrate| {
                let ratio = f64::from(fallback_bitrate) / f64::from(contents.metadata.bitrate);
                let reduction_percentage = (1.0 - ratio) * 100.0;

                markdown::escape(&format!(
                    "Warning: the bitrate of the video has been reduced \
                    from {} kbps to {} kbps ({:.1}% reduction) to meet \
                    Telegram's file size limit.",
                    contents.metadata.bitrate, fallback_bitrate, reduction_percentage,
                ))
            });

            Ok(Answer::SendVideo {
                contents,
                maybe_caption,
            })
        }
    }
}

/// Processes an [`Answer`] by making the appropriate Telegram API calls.
///
/// All message text is expected to already be valid `MarkdownV2`; composers
/// escape their content via [`teloxide::utils::markdown`].
pub async fn handle_answer(
    bot: &Bot,
    msg: &Message,
    task_manager: &TaskManager,
    answer: Answer,
) -> anyhow::Result<()> {
    let chat_id = msg.chat.id.0;

    let send_msg = async |text: String| -> anyhow::Result<()> {
        bot.send_message(msg.chat.id, text)
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
            mut accept_permit,
        } => {
            info!(chat_id, url = %url, fallback = enable_fallback, "starting download");

            // 1. wait for the predecessor's acceptance message to be sent
            // 2. send ours
            // 3. immediately release the successor
            // if send_msg fails, accept_permit is dropped, unblocking the chain
            accept_permit.wait().await;
            send_msg(accept_message).await?;
            accept_permit.signal();

            match download(task_manager, &url, enable_fallback, token).await {
                Ok(dl_ok) => {
                    debug!(chat_id, url = %url, "download completed, sending video");
                    Box::pin(handle_answer(bot, msg, task_manager, dl_ok)).await
                }
                Err(e) => {
                    error!(chat_id, url = %url, error = %e, "download failed");
                    send_msg(markdown::escape(&format!(
                        "Failed to download video ({e})."
                    )))
                    .await
                }
            }
        }
        Answer::SendVideo {
            contents,
            maybe_caption,
        } => send_video(bot, msg, contents, maybe_caption).await,
    }
}
