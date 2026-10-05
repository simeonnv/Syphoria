use std::future::Future;

use futures::FutureExt;
use tokio::task::JoinHandle;

pub struct Handle<T>(JoinHandle<T>);

impl<T> Drop for Handle<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub enum Task<T> {
    Unexecuted,
    Running(Handle<T>),
    Done(T),
    Panicked,
}

impl<T: Send + 'static> Task<T> {
    pub fn new() -> Self {
        Self::Unexecuted
    }

    pub fn new_spawn(future: impl Future<Output = T> + Send + 'static) -> Self {
        Task::Running(Handle(tokio::spawn(future)))
    }

    pub fn spawn(&mut self, future: impl Future<Output = T> + Send + 'static) {
        *self = Self::new_spawn(future)
    }

    pub fn poll(&mut self) -> &Self {
        if let Task::Running(handle) = self
            && let Some(result) = (&mut handle.0).now_or_never()
        {
            *self = match result {
                Ok(value) => Task::Done(value),
                Err(_) => Task::Panicked,
            };
        }
        self
    }
}

impl<T: Send + 'static> Default for Task<T> {
    fn default() -> Self {
        Self::new()
    }
}
