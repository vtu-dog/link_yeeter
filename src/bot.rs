//! Bot startup schema.

use crate::{
    commands::{self, Command},
    task_manager::{TaskManager, TaskManagerInner},
};

use std::{fmt::Debug, sync::Arc};

use futures::future::BoxFuture;
use teloxide::{dispatching::UpdateHandler, error_handlers::ErrorHandler, prelude::*};
use tracing::{error, info};

/// Starts the bot.
pub async fn start() {
    // create a task manager
    let task_manager_inner = Arc::new(TaskManagerInner::default());
    let task_manager_public: TaskManager = task_manager_inner.clone().into();

    // create the main bot instance
    let bot = Bot::from_env();

    let mut dispatcher = Dispatcher::builder(bot.clone(), schema())
        .distribution_function(|_| None::<std::convert::Infallible>) // always process in parallel
        .dependencies(dptree::deps![task_manager_public])
        .error_handler(TracingErrorHandler::new())
        .default_handler(async |_| {})
        .enable_ctrlc_handler()
        .build();

    // verify connectivity before starting - hard crash if unreachable
    bot.get_me()
        .await
        .expect("Telegram API should be reachable (check TELOXIDE_TOKEN and network)");

    // start the task manager, and the dispatcher
    task_manager_inner.start();

    info!("dispatcher started, bot is ready to receive messages");
    dispatcher.dispatch().await;
    task_manager_inner.stop();
    info!("dispatcher stopped");
}

/// Defines routes for the bot.
fn schema() -> UpdateHandler<anyhow::Error> {
    dptree::entry().chain(
        Update::filter_message()
            .branch(
                dptree::entry()
                    .filter_command::<Command>()
                    .endpoint(commands::answer_command),
            )
            .branch(
                dptree::filter(|msg: Message| msg.chat.is_private())
                    .endpoint(commands::answer_plaintext),
            ),
    )
}

/// A logging error handler that uses [`tracing`] for output.
struct TracingErrorHandler {}

impl TracingErrorHandler {
    /// Creates a new [`TracingErrorHandler`].
    fn new() -> Arc<Self> {
        Arc::new(Self {})
    }
}

impl<E> ErrorHandler<E> for TracingErrorHandler
where
    E: Debug,
{
    fn handle_error(self: Arc<Self>, error: E) -> BoxFuture<'static, ()> {
        error!(error = ?error, "unhandled error in dispatcher");
        Box::pin(async {})
    }
}
