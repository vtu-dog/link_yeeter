//! `link_yeeter` Telegram bot entrypoint.

mod bot;
mod commands;
mod env;
mod task;
mod task_manager;
mod utils;
mod worker;

/// Start the application.
#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // load environment variables (.env file takes precedence)
    dotenvy::dotenv_override().ok();

    // initialise color support for tracing
    color_eyre::install().expect("color_eyre::install() should not be called multiple times");

    // initialise tracing itself
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        // change the timestamp format to something human-readable
        .with_timer({
            let time_format = time::format_description::parse_borrowed::<2>(
                "[year]-[month padding:zero]-[day padding:zero] [hour]:[minute]:[second]",
            )
            .expect("time format should be valid");

            let utc_offset = time::UtcOffset::from_hms(
                (chrono::Local::now().offset().local_minus_utc() / 3600)
                    .try_into()
                    .expect("local offset should not exceed i8 range"),
                0,
                0,
            )
            .expect("UTC offset should be valid");

            tracing_subscriber::fmt::time::OffsetTime::new(utc_offset, time_format)
        })
        .init();

    // now we can properly use tracing
    tracing::debug!("tracing initialised");

    // make sure that the process can access essential binaries
    for bin in ["ffmpeg", "ffprobe", "yt-dlp"] {
        assert!(which::which(bin).is_ok(), "{bin} should be in PATH");
    }

    // start the bot (pinned due to large future size)
    Box::pin(bot::start()).await;
}
