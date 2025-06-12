//! Bot startup schema.

use crate::{
    commands::{self, Command},
    task_manager::{TaskManager, TaskManagerInner},
};

use std::{fmt::Debug, sync::Arc};

use futures::future::BoxFuture;
use teloxide::{dispatching::UpdateHandler, error_handlers::ErrorHandler, prelude::*};

/// Start the bot.
pub async fn start() {
    // create a task manager
    let task_manager_inner = Arc::new(TaskManagerInner::default());
    let task_manager_public: TaskManager = task_manager_inner.clone().into();

    // create the main bot instance
    let bot = Bot::from_env();

    let mut dispatcher = Dispatcher::builder(bot, schema())
        .distribution_function(|_| None::<std::convert::Infallible>) // always process in parallel
        .dependencies(dptree::deps![task_manager_public])
        .error_handler(TracingErrorHandler::new())
        .enable_ctrlc_handler()
        .build();

    // start the task manager, and the dispatcher
    task_manager_inner.start();

    tracing::debug!("dispatcher started");
    tokio::select! {
        () = dispatcher.dispatch() => {
            task_manager_inner.stop();
            tracing::debug!("dispatcher stopped");
        }
    }
}

/// Define routes for the bot.
fn schema() -> UpdateHandler<color_eyre::Report> {
    dptree::entry().chain(
        Update::filter_message()
            .branch(
                dptree::entry()
                    .filter_command::<Command>()
                    .endpoint(commands::answer_command),
            )
            .branch(dptree::endpoint(commands::answer_plaintext)),
    )
}

/// A logging error handler that utilises `tracing`.
struct TracingErrorHandler {}

impl TracingErrorHandler {
    fn new() -> Arc<Self> {
        Arc::new(Self {})
    }
}

impl<E> ErrorHandler<E> for TracingErrorHandler
where
    E: Debug,
{
    fn handle_error(self: Arc<Self>, error: E) -> BoxFuture<'static, ()> {
        tracing::error!("{error:#?}");
        Box::pin(async {})
    }
}
