//! A wrapper for enqueueing tasks.

use crate::{
    queue::{AcceptPermit, QueuePosition, TentativeToken},
    task::Task,
    worker::Worker,
};

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::info;

/// Manages [`Task`] lifecycle and the underlying [`Worker`].
pub struct TaskManagerInner {
    /// The [`Worker`] that processes download tasks.
    worker: Worker,
    /// Cancellation token forwarded to the [`Worker`] on start.
    cancellation_token: CancellationToken,
}

impl TaskManagerInner {
    /// Starts the inner worker.
    pub fn start(&self) {
        self.worker.start(self.cancellation_token.child_token());
        info!("task manager started");
    }

    /// Stops the inner worker.
    pub fn stop(&self) {
        self.cancellation_token.cancel();
        info!("task manager stopped");
    }
}

impl Default for TaskManagerInner {
    fn default() -> Self {
        Self {
            worker: Worker::new(),
            cancellation_token: CancellationToken::new(),
        }
    }
}

/// Public, cloneable handle to a [`TaskManagerInner`].
#[derive(Clone)]
pub struct TaskManager {
    /// Shared inner state.
    inner: Arc<TaskManagerInner>,
}

impl TaskManager {
    /// Returns the current queue size.
    pub fn get_queue_size(&self) -> usize {
        self.inner.worker.queue_size()
    }

    /// Tentatively accepts a new task, returning a token, its queue position,
    /// and an accept permit.
    ///
    /// The token keeps the tentative counter incremented. Dropping it cancels
    /// the tentative task automatically. Pass it to [`Self::enqueue_task`] to
    /// commit.
    ///
    /// The permit must be used to send the acceptance message in order; see
    /// [`AcceptPermit`].
    pub fn tentative_enqueue(&self) -> (TentativeToken, QueuePosition, AcceptPermit) {
        self.inner.worker.tentative_enqueue()
    }

    /// Adds a task to the queue, consuming the tentative token.
    pub fn enqueue_task(&self, task: Task, token: TentativeToken) {
        self.inner.worker.push(task, token);
    }
}

impl From<Arc<TaskManagerInner>> for TaskManager {
    fn from(inner: Arc<TaskManagerInner>) -> Self {
        Self { inner }
    }
}
