//! Queue state management: task queue, counters, RAII tokens, and accept ordering.

use crate::task::Task;

use std::{
    future::Future,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use deadqueue::unlimited::Queue;
use tokio::sync::oneshot;
use tracing::debug;

/// Internal state shared between the queue handle and its RAII tokens.
struct InternalState {
    /// Counter for tentatively accepted tasks not yet in the queue.
    tentative: usize,
    /// Flag indicating whether the worker is currently processing a task.
    is_busy: bool,
    /// Receiver from the most recently issued [`AcceptPermit`].
    ///
    /// Each new permit chains off this, forming a linked list that ensures
    /// acceptance messages are sent in the order tasks were accepted.
    last_accept_rx: Option<oneshot::Receiver<()>>,
}

/// Locks the state mutex, recovering from poisoning.
///
/// Ignoring poison is safe here: every critical section is a couple of field
/// assignments that cannot leave the state inconsistent partway through, so a
/// panic under the lock must not cascade into panics at every later site.
fn lock_state(state: &Mutex<InternalState>) -> MutexGuard<'_, InternalState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// RAII guard that marks the worker as busy on creation and idle on drop.
pub struct BusyGuard(Arc<Mutex<InternalState>>);

impl BusyGuard {
    /// Acquires the busy guard, marking the worker as busy.
    fn acquire(state: &Arc<Mutex<InternalState>>) -> Self {
        lock_state(state).is_busy = true;
        Self(Arc::clone(state))
    }

    /// Executes an async closure while holding the guard, then releases it.
    pub async fn then<F, Fut, R>(self, f: F) -> R
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = R>,
    {
        let result = f().await;
        drop(self);
        result
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        lock_state(&self.0).is_busy = false;
    }
}

/// Position in the task queue at the time of enqueueing.
pub type QueuePosition = usize;

/// Token representing a tentatively accepted task.
///
/// Automatically decrements the tentative counter on drop (cancel path).
/// Call [`Worker::push`][crate::worker::Worker::push] to commit the task,
/// which consumes the token via [`TentativeToken::disarm`].
pub struct TentativeToken {
    /// Shared state; [`Some`] while active, [`None`] after disarming.
    state: Option<Arc<Mutex<InternalState>>>,
}

impl TentativeToken {
    /// Disarms the token, preventing automatic cancellation on drop.
    pub fn disarm(mut self) {
        self.state.take();
    }
}

impl Drop for TentativeToken {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            let mut st = lock_state(&state);
            let old_tentative = st.tentative;
            st.tentative = st.tentative.saturating_sub(1);
            debug!(
                old_tentative,
                new_tentative = st.tentative,
                "cancelled tentative task"
            );
        }
    }
}

/// Permit for sending an acceptance message in the correct order.
///
/// Tasks call [`wait`][`Self::wait`] before sending their acceptance message,
/// and [`signal`][`Self::signal`] immediately after, so that concurrent
/// requests always send acceptance messages in the order they were accepted.
///
/// If a task is cancelled or fails before signalling, dropping the permit
/// closes the channel, unblocking the successor automatically.
pub struct AcceptPermit {
    /// Receiver from the predecessor's permit; [`None`] if there is no predecessor.
    wait_for: Option<oneshot::Receiver<()>>,
    /// Sender used to unblock the successor's permit.
    done_tx: oneshot::Sender<()>,
}

impl AcceptPermit {
    /// Waits until the predecessor's acceptance message has been sent (or cancelled).
    pub async fn wait(&mut self) {
        if let Some(rx) = self.wait_for.take() {
            let _ = rx.await;
        }
    }

    /// Signals that this task's acceptance message has been sent.
    pub fn signal(self) {
        let _ = self.done_tx.send(());
    }
}

/// Shared task queue with counters and accept-ordering state.
///
/// Cheap to clone, all fields are [`Arc`][std::sync::Arc]-wrapped.
#[derive(Clone)]
pub struct TaskQueue {
    /// The underlying task queue.
    queue: Arc<Queue<Task>>,
    /// Counters and accept-ordering state.
    state: Arc<Mutex<InternalState>>,
}

impl TaskQueue {
    /// Creates a new, empty [`TaskQueue`].
    pub fn new() -> Self {
        Self {
            queue: Arc::new(Queue::new()),
            state: Arc::new(Mutex::new(InternalState {
                tentative: 0,
                is_busy: false,
                last_accept_rx: None,
            })),
        }
    }

    /// Returns the current effective queue size (queued + tentative + busy).
    pub fn queue_size(&self) -> usize {
        let st = lock_state(&self.state);
        self.queue.len() + st.tentative + usize::from(st.is_busy)
    }

    /// Tentatively accept a task, returning a token, its queue position, and
    /// an accept permit.
    ///
    /// The token keeps the tentative counter incremented. If dropped without
    /// being committed via [`Self::push`], it decrements the counter
    /// automatically.
    ///
    /// The permit must be used in [`crate::messaging::handle_answer`] to
    /// ensure acceptance messages are sent in acceptance order.
    pub fn tentative_enqueue(&self) -> (TentativeToken, QueuePosition, AcceptPermit) {
        let mut st = lock_state(&self.state);

        let qsize = self.queue.len() + st.tentative + usize::from(st.is_busy);
        st.tentative += 1;

        // chain this permit to the previous one so sends are ordered
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let wait_for = st.last_accept_rx.replace(done_rx);

        drop(st);

        let token = TentativeToken {
            state: Some(Arc::clone(&self.state)),
        };
        let permit = AcceptPermit { wait_for, done_tx };

        (token, qsize, permit)
    }

    /// Pushes a task onto the queue, consuming the tentative token.
    pub fn push(&self, item: Task, token: TentativeToken) {
        token.disarm();

        let mut st = lock_state(&self.state);
        st.tentative -= 1;
        let queue_len = self.queue.len() + 1; // +1 for the item being pushed
        debug!(
            url = %item.url,
            queue_len,
            tentative = st.tentative,
            "task pushed to queue"
        );
        self.queue.push(item);
    }

    /// Pops the next task from the queue, waiting asynchronously if empty.
    pub async fn pop(&self) -> Task {
        self.queue.pop().await
    }

    /// Returns the number of tasks currently in the queue, not counting tentative or busy.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Acquires a [`BusyGuard`], marking the worker as busy until it is dropped.
    pub fn acquire_busy_guard(&self) -> BusyGuard {
        BusyGuard::acquire(&self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    fn make_test_task(url: &str) -> Task {
        let (tx, _rx) = oneshot::channel();
        Task {
            url: url.to_string(),
            enable_fallback: false,
            return_channel: tx,
        }
    }

    #[test]
    fn new_queue_has_zero_size() {
        let q = TaskQueue::new();
        assert_eq!(q.queue_size(), 0);
    }

    #[test]
    fn tentative_enqueue_increments_size() {
        let q = TaskQueue::new();
        let (token, pos, _permit) = q.tentative_enqueue();
        assert_eq!(pos, 0);
        assert_eq!(q.queue_size(), 1);
        drop(token);
    }

    #[test]
    fn multiple_tentative_enqueues_stack() {
        let q = TaskQueue::new();
        let (t1, p1, _permit1) = q.tentative_enqueue();
        assert_eq!(p1, 0);
        let (t2, p2, _permit2) = q.tentative_enqueue();
        assert_eq!(p2, 1);
        let (t3, p3, _permit3) = q.tentative_enqueue();
        assert_eq!(p3, 2);
        assert_eq!(q.queue_size(), 3);
        drop((t1, t2, t3));
    }

    #[test]
    fn drop_token_decrements_size() {
        let q = TaskQueue::new();
        let (t1, _, _p1) = q.tentative_enqueue();
        let (t2, _, _p2) = q.tentative_enqueue();
        assert_eq!(q.queue_size(), 2);

        drop(t1);
        assert_eq!(q.queue_size(), 1);

        drop(t2);
        assert_eq!(q.queue_size(), 0);
    }

    #[test]
    fn push_moves_from_tentative_to_queue() {
        let q = TaskQueue::new();
        let (token, _, _permit) = q.tentative_enqueue();
        assert_eq!(q.queue_size(), 1);

        q.push(make_test_task("https://example.com"), token);
        // size should still be 1: tentative decremented, queue incremented
        assert_eq!(q.queue_size(), 1);
    }

    #[test]
    fn push_multiple_tasks() {
        let q = TaskQueue::new();
        let (t1, _, _p1) = q.tentative_enqueue();
        let (t2, _, _p2) = q.tentative_enqueue();
        let (t3, _, _p3) = q.tentative_enqueue();

        q.push(make_test_task("https://a.com"), t1);
        q.push(make_test_task("https://b.com"), t2);
        q.push(make_test_task("https://c.com"), t3);

        assert_eq!(q.queue_size(), 3);
    }

    #[test]
    fn busy_guard_is_reflected_in_queue_size() {
        let q = TaskQueue::new();
        assert_eq!(q.queue_size(), 0);

        let guard = q.acquire_busy_guard();
        assert_eq!(q.queue_size(), 1);

        drop(guard);
        assert_eq!(q.queue_size(), 0);
    }

    #[test]
    fn busy_guard_stacks_with_tentative_count() {
        let q = TaskQueue::new();
        let (token, _, _permit) = q.tentative_enqueue();
        assert_eq!(q.queue_size(), 1);

        let guard = q.acquire_busy_guard();
        assert_eq!(q.queue_size(), 2);

        drop(guard);
        assert_eq!(q.queue_size(), 1);

        drop(token);
        assert_eq!(q.queue_size(), 0);
    }

    #[test]
    fn recovers_from_poisoned_mutex() {
        let q = TaskQueue::new();

        // poison the state mutex by panicking while it is held
        let state = Arc::clone(&q.state);
        let handle = std::thread::spawn(move || {
            let _guard = state.lock().unwrap();
            panic!("deliberately poison the mutex");
        });
        assert!(handle.join().is_err());

        // every lock site must keep working after the poisoning panic
        assert_eq!(q.queue_size(), 0);

        let (token, pos, _permit) = q.tentative_enqueue();
        assert_eq!(pos, 0);
        q.push(make_test_task("https://example.com"), token);
        assert_eq!(q.queue_size(), 1);

        let guard = q.acquire_busy_guard();
        assert_eq!(q.queue_size(), 2);
        drop(guard);
        assert_eq!(q.queue_size(), 1);

        let (token, _, _permit) = q.tentative_enqueue();
        drop(token);
        assert_eq!(q.queue_size(), 1);
    }

    #[tokio::test]
    async fn accept_permit_drop_unblocks_successor() {
        let q = TaskQueue::new();
        let (t1, _, p1) = q.tentative_enqueue();
        let (t2, _, mut p2) = q.tentative_enqueue();
        drop((t1, t2));

        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_clone = done.clone();

        let handle = tokio::spawn(async move {
            p2.wait().await;
            done_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        // give the spawned task a chance to reach wait()
        tokio::task::yield_now().await;
        assert!(!done.load(std::sync::atomic::Ordering::SeqCst));

        // drop p1 without calling signal() - chain must still unblock
        drop(p1);
        handle.await.unwrap();
        assert!(done.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn accept_permits_chain_in_order() {
        let q = TaskQueue::new();
        let (t1, _, mut p1) = q.tentative_enqueue();
        let (t2, _, mut p2) = q.tentative_enqueue();
        drop((t1, t2));

        // p1 has no predecessor, should resolve immediately
        p1.wait().await;

        // p2 is waiting for p1; signal p1 first
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_clone = done.clone();

        let handle = tokio::spawn(async move {
            p2.wait().await;
            done_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        // p2 should not have resolved yet
        tokio::task::yield_now().await;
        assert!(!done.load(std::sync::atomic::Ordering::SeqCst));

        // signal p1, p2 should now resolve
        p1.signal();
        handle.await.unwrap();
        assert!(done.load(std::sync::atomic::Ordering::SeqCst));
    }
}
