//! Synchronization utilities for use in asynchronous contexts.

use std::sync::Arc;

use tokio::sync::{Mutex, MutexGuard, Notify};

/// A combination of [`Mutex`] and [`Notify`], where only task can await the notify at a time.
///
/// This is useful for cases where multiple tasks queue up work which is processed in a single central location that they want to await the completion of, and where there is no way for this central location to distinguish between these units of work. Here each task can first acquire the notify, put the work in the queue, and then await the notify, while the central location can just send a notify event every time it processed one of these units of work.
pub struct LockableNotify {
    lock: Mutex<()>,
    notify: Arc<Notify>,
}
impl LockableNotify {
    pub fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn notify(&self) {
        self.notify.notify_one();
    }

    pub async fn lock(&self) -> LockableNotifyGuard<'_> {
        let guard = self.lock.lock().await;
        LockableNotifyGuard {
            notify: self.notify.clone(),
            _guard: guard,
        }
    }
}
pub struct LockableNotifyGuard<'a> {
    notify: Arc<Notify>,
    _guard: MutexGuard<'a, ()>,
}
impl<'a> LockableNotifyGuard<'a> {
    pub async fn notified(&self) {
        self.notify.notified().await;
    }
}
