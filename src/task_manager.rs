//! A wrapper for enqueueing tasks.

use crate::{
    task::Task,
    worker::{QueuePosition, TentativeToken, Worker},
};

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::info;

/// Manager for `Task`s.
pub struct TaskManagerInner {
    /// Manager for download tasks.
    worker: Worker,
    /// A cancellation token for the inner `Worker`.
    cancellation_token: CancellationToken,
}

impl TaskManagerInner {
    /// Start the inner worker.
    pub fn start(&self) {
        self.worker.start(self.cancellation_token.child_token());
        info!("task manager started");
    }

    /// Stop the inner worker.
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

/// Public interface for `TaskManagerInner`.
#[derive(Clone)]
pub struct TaskManager {
    /// Private `TaskManager` API object.
    inner: Arc<TaskManagerInner>,
}

impl TaskManager {
    /// Get the current queue size.
    pub fn get_queue_size(&self) -> usize {
        self.inner.worker.queue_size()
    }

    /// Tentatively accept a new task, returning a token and the current queue position.
    ///
    /// The token keeps the tentative counter incremented. Dropping it cancels
    /// the tentative task automatically. Pass it to [`Self::enqueue_task`] to
    /// commit.
    pub fn tentative_enqueue(&self) -> (TentativeToken, QueuePosition) {
        self.inner.worker.tentative_enqueue()
    }

    /// Add a specified task to the queue, consuming the tentative token.
    pub fn enqueue_task(&self, task: Task, token: TentativeToken) {
        self.inner.worker.push(task, token);
    }
}

impl From<Arc<TaskManagerInner>> for TaskManager {
    fn from(inner: Arc<TaskManagerInner>) -> Self {
        Self { inner }
    }
}
