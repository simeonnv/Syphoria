use std::future::Future;

use futures::FutureExt;
use tokio::task::JoinHandle;

pub enum Task<T> {
    Running(JoinHandle<T>),
    Done(T),
    Panicked,
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn(future: impl Future<Output = T> + Send + 'static) -> Self {
        Task::Running(tokio::spawn(future))
    }

    pub fn poll(&mut self) -> &Self {
        if let Task::Running(handle) = self
            && let Some(result) = (&mut *handle).now_or_never()
        {
            *self = match result {
                Ok(value) => Task::Done(value),
                Err(_) => Task::Panicked,
            };
        }
        self
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        if let Task::Running(handle) = self {
            handle.abort();
        }
    }
}
