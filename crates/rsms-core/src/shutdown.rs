//! Unified shutdown handle for graceful task cancellation.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::oneshot;

/// Unified trait for graceful shutdown of background tasks.
///
/// All spawned background tasks (window monitor, periodic checker, health checker)
/// implement this trait to allow coordinated shutdown.
pub trait ShutdownHandle: Send + Sync {
    /// Signal the managed task to shutdown.
    fn shutdown(&self);

    /// Wait for task completion after shutdown signal.
    fn wait(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// Simple shutdown handle implementation using AtomicBool and oneshot channel.
pub struct SimpleShutdownHandle {
    shutdown_flag: Arc<AtomicBool>,
    shutdown_tx: std::sync::Mutex<Option<oneshot::Sender<()>>>,
    wait_rx: std::sync::Mutex<Option<oneshot::Receiver<()>>>,
}

impl SimpleShutdownHandle {
    /// Create a new handle.
    pub fn new() -> Self {
        let (tx, rx) = oneshot::channel();
        Self {
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            shutdown_tx: std::sync::Mutex::new(Some(tx)),
            wait_rx: std::sync::Mutex::new(Some(rx)),
        }
    }

    /// Create a new handle with separate sender for external coordination.
    /// Returns (handle, sender) where sender can be used to trigger wait completion.
    #[allow(dead_code)]
    pub fn with_sender() -> (Self, oneshot::Sender<()>) {
        let (tx, rx) = oneshot::channel();
        (
            Self {
                shutdown_flag: Arc::new(AtomicBool::new(false)),
                shutdown_tx: std::sync::Mutex::new(None),
                wait_rx: std::sync::Mutex::new(Some(rx)),
            },
            tx,
        )
    }

    /// Create a handle from existing components.
    #[allow(dead_code)]
    pub fn from_parts(
        shutdown_flag: Arc<AtomicBool>,
        wait_rx: oneshot::Receiver<()>,
    ) -> Self {
        Self {
            shutdown_flag,
            shutdown_tx: std::sync::Mutex::new(None),
            wait_rx: std::sync::Mutex::new(Some(wait_rx)),
        }
    }

    /// Check if shutdown has been signaled.
    pub fn is_shutdown_signaled(&self) -> Arc<AtomicBool> {
        self.shutdown_flag.clone()
    }

    /// Take the wait receiver for use in a task.
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

impl Default for SimpleShutdownHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for SimpleShutdownHandle {
    fn clone(&self) -> Self {
        Self {
            shutdown_flag: self.shutdown_flag.clone(),
            shutdown_tx: std::sync::Mutex::new(None),
            wait_rx: std::sync::Mutex::new(None),
        }
    }
}

impl Drop for SimpleShutdownHandle {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.wait_rx.try_lock()
            && let Some(mut rx) = guard.take() {
                rx.close();
            }
    }
}

impl ShutdownHandle for SimpleShutdownHandle {
    fn shutdown(&self) {
        self.shutdown_flag.store(true, Ordering::Release);
        if let Ok(mut guard) = self.shutdown_tx.lock()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_shutdown_handle() {
        let handle = SimpleShutdownHandle::new();
        assert!(!handle.is_shutdown_signaled().load(Ordering::Acquire));

        handle.shutdown();
        assert!(handle.is_shutdown_signaled().load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn test_shutdown_handle_wait() {
        let (mut handle, tx) = SimpleShutdownHandle::with_sender();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = tx.send(());
        });

        handle.wait().await;
    }
}
