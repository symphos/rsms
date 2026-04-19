//! Sliding Window implementation

use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::Mutex;

use rsms_core::ShutdownHandle;

use crate::error::{WindowError, WindowResult};
use crate::future::WindowFuture;

/// Window configuration
#[derive(Debug, Clone)]
pub struct WindowConfig {
    /// Maximum number of concurrent requests
    pub max_size: usize,
    /// Default timeout for each request
    pub default_timeout: Duration,
    /// Monitor interval for checking expired requests
    pub monitor_interval: Duration,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            max_size: 16,
            default_timeout: Duration::from_secs(10),
            monitor_interval: Duration::from_millis(100),
        }
    }
}

impl WindowConfig {
    pub fn new(max_size: usize, timeout: Duration) -> Self {
        Self {
            max_size,
            default_timeout: timeout,
            ..Default::default()
        }
    }
    
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }
    
    pub fn with_monitor_interval(mut self, interval: Duration) -> Self {
        self.monitor_interval = interval;
        self
    }
}

/// Sliding window for async request/response handling
pub struct Window<K, R, P> {
    config: WindowConfig,
    pending: Arc<Mutex<HashMap<K, WindowFuture<K, R, P>>>>,
    event_sender: Option<mpsc::Sender<WindowEvent<K>>>,
    shutdown_flag: Arc<AtomicBool>,
}

impl<K, R, P> Default for Window<K, R, P>
where
    K: Eq + std::hash::Hash + Clone + Copy + std::fmt::Debug + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    P: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new(WindowConfig::default())
    }
}

pub enum WindowEvent<K> {
    Expired(K),
    Completed(K),
    Failed(K, #[allow(dead_code)] WindowError),
}

/// ShutdownHandle implementation for Window monitor task.
pub struct SimpleWindowShutdownHandle<K, R, P>
where
    K: Eq + std::hash::Hash + Clone + Copy + std::fmt::Debug + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    P: Clone + Send + Sync + 'static,
{
    shutdown_flag: Arc<AtomicBool>,
    shutdown_tx: StdMutex<Option<oneshot::Sender<()>>>,
    wait_rx: StdMutex<Option<oneshot::Receiver<()>>>,
    _phantom: PhantomData<(K, R, P)>,
}

impl<K, R, P> SimpleWindowShutdownHandle<K, R, P>
where
    K: Eq + std::hash::Hash + Clone + Copy + std::fmt::Debug + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    P: Clone + Send + Sync + 'static,
{
    pub fn new() -> Self {
        let (tx, rx) = oneshot::channel();
        Self {
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            shutdown_tx: StdMutex::new(Some(tx)),
            wait_rx: StdMutex::new(Some(rx)),
            _phantom: PhantomData,
        }
    }

    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        self.shutdown_flag.clone()
    }

    pub fn take_wait_rx(&self) -> oneshot::Receiver<()> {
        self.wait_rx
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| {
                let (_tx, rx) = oneshot::channel();
                rx
            })
    }
}

impl<K, R, P> Default for SimpleWindowShutdownHandle<K, R, P>
where
    K: Eq + std::hash::Hash + Clone + Copy + std::fmt::Debug + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    P: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, R, P> Drop for SimpleWindowShutdownHandle<K, R, P>
where
    K: Eq + std::hash::Hash + Clone + Copy + std::fmt::Debug + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    P: Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        if let Ok(mut guard) = self.wait_rx.try_lock()
            && let Some(mut rx) = guard.take() {
                rx.close();
            }
    }
}

impl<K, R, P> ShutdownHandle for SimpleWindowShutdownHandle<K, R, P>
where
    K: Eq + std::hash::Hash + Clone + Copy + std::fmt::Debug + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    P: Clone + Send + Sync + 'static,
{
    fn shutdown(&self) {
        self.shutdown_flag.store(true, Ordering::Release);
        if let Ok(mut guard) = self.shutdown_tx.try_lock()
            && let Some(tx) = guard.take() {
                let _ = tx.send(());
            }
    }

    fn wait(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let rx = self.wait_rx.lock().unwrap().take();
        Box::pin(async move {
            if let Some(rx) = rx {
                let _ = rx.await;
            }
        })
    }
}

impl<K, R, P> Window<K, R, P>
where
    K: Eq + std::hash::Hash + Clone + Copy + std::fmt::Debug + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    P: Clone + Send + Sync + 'static,
{
    /// Create a new window
    pub fn new(config: WindowConfig) -> Self {
        Self {
            config,
            pending: Arc::new(Mutex::new(HashMap::new())),
            event_sender: None,
            shutdown_flag: Arc::new(AtomicBool::new(false)),
        }
    }
    
    /// Create a window with event channel
    pub fn with_channel(config: WindowConfig, channel_size: usize) -> (Self, mpsc::Receiver<WindowEvent<K>>) {
        let (tx, rx) = mpsc::channel(channel_size);
        let mut window = Self::new(config);
        window.event_sender = Some(tx);
        (window, rx)
    }
    
    /// Try to offer a request without blocking (async)
    pub async fn try_offer(&self, key: K, params: P) -> WindowResult<WindowFuture<K, R, P>> {
        let mut pending = self.pending.lock().await;
        
        if pending.contains_key(&key) {
            return Err(WindowError::DuplicateKey(format!("{:?}", key)));
        }
        
        if pending.len() >= self.config.max_size {
            return Err(WindowError::WindowFull);
        }
        
        let future = WindowFuture::new(key, params, self.config.default_timeout);
        let result = future.clone();
        pending.insert(key, future);
        
        Ok(result)
    }
    
    /// Offer a request, blocking until there's space
    pub async fn offer(&self, key: K, params: P, timeout: Duration) -> WindowResult<WindowFuture<K, R, P>> {
        let start = Instant::now();
        
        loop {
            if let Ok(future) = self.try_offer(key, params.clone()).await {
                return Ok(future);
            }
            if start.elapsed() >= timeout {
                break;
            }
            time::sleep(Duration::from_millis(50)).await;
        }
        
        Err(WindowError::OfferTimeout)
    }
    
    /// Complete a pending request
    pub async fn complete(&self, key: &K, response: R) -> Option<WindowFuture<K, R, P>> {
        let mut pending = self.pending.lock().await;
        
        if let Some(future) = pending.remove(key) {
            future.complete(response).await;
            self.send_event(WindowEvent::<K>::Completed(*key));
            return Some(future);
        }
        
        None
    }
    
    /// Fail a pending request
    pub async fn fail(&self, key: &K, error: WindowError) -> Option<WindowFuture<K, R, P>> {
        let mut pending = self.pending.lock().await;
        
        if let Some(future) = pending.remove(key) {
            future.fail(error.clone()).await;
            self.send_event(WindowEvent::<K>::Failed(*key, error));
            return Some(future);
        }
        
        None
    }
    
    /// Remove and cancel a pending request
    pub async fn cancel(&self, key: &K) -> Option<WindowFuture<K, R, P>> {
        let mut pending = self.pending.lock().await;
        
        if let Some(future) = pending.remove(key) {
            future.cancel().await;
            return Some(future);
        }
        
        None
    }
    
    /// Get current window size
    pub async fn size(&self) -> usize {
        let pending = self.pending.lock().await;
        pending.len()
    }
    
    /// Get available slots
    pub async fn available(&self) -> usize {
        self.config.max_size - self.size().await
    }

    /// Get window occupancy ratio (0.0 to 1.0)
    pub async fn occupancy_ratio(&self) -> f64 {
        self.size().await as f64 / self.config.max_size as f64
    }
    
    /// Check if key exists
    pub async fn contains(&self, key: &K) -> bool {
        let pending = self.pending.lock().await;
        pending.contains_key(key)
    }
    
    /// Start background monitor for expired requests
    ///
    /// Returns a `ShutdownHandle` that can be used to stop the monitor.
    pub fn start_monitor(&self) -> Arc<SimpleWindowShutdownHandle<K, R, P>> {
        let handle = Arc::new(SimpleWindowShutdownHandle::new());
        let shutdown_flag = handle.shutdown_flag();
        let pending = self.pending.clone();
        let config = self.config.clone();
        let sender = self.event_sender.clone();
        let wait_rx = handle.take_wait_rx();
        
        tokio::spawn(async move {
            let mut interval = time::interval(config.monitor_interval);
            let mut wait_rx = Some(wait_rx);
            
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let mut expired_keys = Vec::new();
                        let mut pending = pending.lock().await;
                        
                        for (key, future) in pending.iter() {
                            if future.is_expired() {
                                expired_keys.push(*key);
                            }
                        }
                        
                        for key in expired_keys {
                            if let Some(future) = pending.remove(&key) {
                                future.fail(WindowError::Timeout).await;
                                if let Some(tx) = &sender {
                                    let _ = tx.send(WindowEvent::Expired(key)).await;
                                }
                            }
                        }
                    }
                    _ = async {
                        if let Some(rx) = wait_rx.take() {
                            rx.await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        break;
                    }
                }
                
                if shutdown_flag.load(Ordering::Acquire) {
                    break;
                }
            }
        });
        
        handle
    }
    
    fn send_event(&self, event: WindowEvent<K>) {
        if let Some(tx) = &self.event_sender {
            let _ = tx.try_send(event);
        }
    }
}

impl<K, R, P> Clone for Window<K, R, P> {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            pending: self.pending.clone(),
            event_sender: None, // Don't clone sender
            shutdown_flag: self.shutdown_flag.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_window_basic() {
        let config = WindowConfig::new(4, Duration::from_secs(10));
        let window: Window<u32, &str, String> = Window::new(config);
        
        let _f1 = window.try_offer(1u32, "req1".to_string()).await.unwrap();
        let _f2 = window.try_offer(2u32, "req2".to_string()).await.unwrap();
        
        assert_eq!(window.size().await, 2);
        assert_eq!(window.available().await, 2);
        
        window.complete(&1, "resp1").await;
        
        assert_eq!(window.size().await, 1);
        assert!(!window.contains(&1).await);
        assert!(window.contains(&2).await);
    }
    
    #[tokio::test]
    async fn test_window_full() {
        let config = WindowConfig::new(2, Duration::from_secs(10));
        let window: Window<u32, &str, String> = Window::new(config);
        
        window.try_offer(1u32, "req1".to_string()).await.unwrap();
        window.try_offer(2u32, "req2".to_string()).await.unwrap();
        
        let result = window.try_offer(3u32, "req3".to_string()).await;
        assert!(matches!(result, Err(WindowError::WindowFull)));
    }
    
    #[tokio::test]
    async fn test_window_duplicate() {
        let config = WindowConfig::new(4, Duration::from_secs(10));
        let window: Window<u32, &str, String> = Window::new(config);
        
        window.try_offer(1u32, "req1".to_string()).await.unwrap();
        
        let result = window.try_offer(1u32, "req1".to_string()).await;
        assert!(matches!(result, Err(WindowError::DuplicateKey(_))));
    }
}