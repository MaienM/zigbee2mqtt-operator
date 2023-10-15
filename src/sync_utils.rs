use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::{
    select,
    sync::{Mutex, MutexGuard, Notify, RwLock},
    time::timeout,
};

use crate::Error;

/// An wrapper for [`HashMap<String, T>`] which allows one to wait for a key to become available.
pub struct AwaitableMap<T> {
    name: String,
    map: RwLock<HashMap<String, Arc<AwaitableValue<T>>>>,
    notify: Notify,
}
impl<T> AwaitableMap<T>
where
    T: Clone,
{
    pub fn new(name: String) -> Self {
        Self {
            name,
            map: RwLock::new(HashMap::new()),
            notify: Notify::new(),
        }
    }

    pub async fn get(&self, key: &String, duration: Duration) -> Result<T, Error> {
        if let Some(value) = self.map.read().await.get(key) {
            return value.get(duration).await;
        }

        let timeout = self.create(key);
        loop {
            select! {
                result = timeout.get(duration) => {
                    return result;
                }
                _ = self.notify.notified() => {
                    match self.map.read().await.get(key) {
                        Some(value) => {
                            return value.get(duration).await
                        },
                        None => continue,
                    }
                }
            }
        }
    }

    pub async fn get_or_create(&self, key: String) -> Arc<AwaitableValue<T>> {
        self.map
            .write()
            .await
            .entry(key)
            .or_insert_with_key(|key| {
                self.notify.notify_one();
                Arc::new(self.create(key))
            })
            .clone()
    }

    pub async fn remove(&self, key: &String) -> Option<Arc<AwaitableValue<T>>> {
        self.map.write().await.remove(key)
    }

    fn create(&self, key: &String) -> AwaitableValue<T> {
        AwaitableValue::new(format!("{name} {key}", name = self.name))
    }
}

/// A wrapper for [`Option<Result<T, Error>>`] which allows one to wait for the option to become [`Option::Some`].
#[derive(Debug)]
pub struct AwaitableValue<T> {
    name: String,
    value: RwLock<Option<Result<T, Error>>>,
    notify: Notify,
}
impl<T> AwaitableValue<T>
where
    T: Clone,
{
    pub fn new(name: String) -> Self {
        return Self {
            name,
            value: RwLock::new(None),
            notify: Notify::new(),
        };
    }

    pub async fn set(&self, value: T) {
        *self.value.write().await = Some(Ok(value));
        self.notify.notify_waiters();
    }

    pub async fn invalidate(&self, error: Error) {
        *self.value.write().await = Some(Err(error));
        self.notify.notify_waiters();
    }

    pub async fn clear(&self) {
        *self.value.write().await = None;
    }

    pub async fn get(&self, duration: Duration) -> Result<T, Error> {
        let future = async {
            loop {
                if let Some(value) = &*self.value.read().await {
                    return value.clone();
                }
                self.notify.notified().await;
            }
        };
        match timeout(duration, future).await {
            Ok(result) => return result,
            Err(_) => {
                return Err(Error::ActionFailed(
                    format!(
                        "value for {name} took too long to resolve",
                        name = self.name
                    ),
                    None,
                ))
            }
        }
    }

    pub async fn get_immediate(&self) -> Option<Result<T, Error>> {
        return self.value.read().await.clone();
    }
}

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
