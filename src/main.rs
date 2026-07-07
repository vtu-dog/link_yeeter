//! `link_yeeter` Telegram bot entrypoint.

mod bot;
mod commands;
mod env;
mod media;
mod messaging;
mod queue;
mod task;
mod task_manager;
mod utils;
mod worker;

use tracing_forest::ForestLayer;
use tracing_subscriber::{
    EnvFilter, filter::LevelFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt,
};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // load environment variables (.env file takes precedence)
    dotenvy::dotenv_override().ok();

    // fail loudly on missing or malformed environment variables
    env::validate();

    // make sure that the process can access essential binaries
    for bin in ["ffmpeg", "ffprobe", "yt-dlp"] {
        assert!(which::which(bin).is_ok(), "{bin} should be in PATH");
    }

    // initialise tracing
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    match &*env::LOG_FORMAT {
        env::LogFormat::Json => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt::layer().json())
                .init();
        }
        env::LogFormat::Forest => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(ForestLayer::default())
                .init();
        }
    }

    tracing::info!("link_yeeter starting up");
    tracing::debug!("tracing initialised");

    // start the bot
    Box::pin(bot::start()).await;

    tracing::info!("link_yeeter shutting down");
}
