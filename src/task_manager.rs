//! A wrapper for enqueueing tasks.

use crate::{task::Task, worker::Worker};

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

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
        tracing::debug!("task manager started");
    }

    /// Stop the inner worker.
    pub fn stop(&self) {
        self.cancellation_token.cancel();
        tracing::debug!("task manager stopped");
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

    /// Get the current queue size, and tentatively accept a new task.
    ///
    /// This is used to not lose track of the number of `Task`s between sending
    /// an acceptance message to the user and putting the `Task` itself into the queue.
    pub fn tentative_enqueue(&self) -> usize {
        self.inner.worker.tentative_enqueue()
    }

    /// Add a specified task to the queue, taking place of any tentative task.
    pub fn enqueue_task(&self, task: Task) {
        self.inner.worker.push(task);
    }
}

impl From<Arc<TaskManagerInner>> for TaskManager {
    fn from(inner: Arc<TaskManagerInner>) -> Self {
        Self { inner }
    }
}
