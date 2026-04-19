//! WindowFuture - Async future for window requests

use std::task::{Waker};
use std::sync::Arc;
use tokio::sync::Mutex;
use std::time::Instant;

use crate::error::WindowError;

/// Future state
#[derive(Debug, Clone)]
enum FutureState<R> {
    Pending,
    Success(R),
    Failed(#[allow(dead_code)] WindowError),
    Cancelled,
}

/// WindowFuture - represents a pending request in the window
pub struct WindowFuture<K, R, P> {
    key: K,
    params: P,
    created_at: Instant,
    timeout: std::time::Duration,
    state: Arc<Mutex<FutureState<R>>>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl<K, R, P> Clone for WindowFuture<K, R, P>
where
    K: Clone,
    P: Clone,
{
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            params: self.params.clone(),
            created_at: self.created_at,
            timeout: self.timeout,
            state: Arc::clone(&self.state),
            waker: Arc::clone(&self.waker),
        }
    }
}

impl<K, R, P> WindowFuture<K, R, P>
where
    P: Clone,
{
    pub fn new(key: K, params: P, timeout: std::time::Duration) -> Self {
        Self {
            key,
            params,
            created_at: Instant::now(),
            timeout,
            state: Arc::new(Mutex::new(FutureState::Pending)),
            waker: Arc::new(Mutex::new(None)),
        }
    }
    
    pub fn key(&self) -> &K {
        &self.key
    }
    
    pub fn params(&self) -> &P {
        &self.params
    }
    
    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() > self.timeout
    }
    
    pub async fn is_done(&self) -> bool {
        matches!(
            *self.state.lock().await,
            FutureState::Success(_) | FutureState::Failed(_) | FutureState::Cancelled
        )
    }
    
    /// Complete the future with a successful response
    pub async fn complete(&self, response: R) {
        let mut state = self.state.lock().await;
        *state = FutureState::Success(response);
        let _ = self.wake().await;
    }
    
    /// Complete the future with an error
    pub async fn fail(&self, error: WindowError) {
        let mut state = self.state.lock().await;
        *state = FutureState::Failed(error);
        let _ = self.wake().await;
    }
    
    /// Cancel the future
    pub async fn cancel(&self) {
        let mut state = self.state.lock().await;
        *state = FutureState::Cancelled;
        let _ = self.wake().await;
    }
    
    #[allow(unused_must_use)]
    async fn wake(&self) {
        if let Some(waker) = self.waker.lock().await.take() {
            waker.wake();
        }
    }
}

impl<K, R, P> std::fmt::Debug for WindowFuture<K, R, P>
where
    K: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowFuture")
            .field("key", &self.key)
            .field("timeout", &self.timeout)
            .finish()
    }
}