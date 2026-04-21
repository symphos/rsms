use async_trait::async_trait;
use rsms_connector::{ClientEventHandler, ServerEventHandler, ProtocolConnection};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub struct TestEventHandler {
    pub connected: Arc<AtomicUsize>,
    pub authenticated: Arc<AtomicUsize>,
    pub disconnected: Arc<AtomicUsize>,
}

impl TestEventHandler {
    pub fn new() -> Self {
        Self {
            connected: Arc::new(AtomicUsize::new(0)),
            authenticated: Arc::new(AtomicUsize::new(0)),
            disconnected: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn connected_count(&self) -> usize {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn authenticated_count(&self) -> usize {
        self.authenticated.load(Ordering::Relaxed)
    }

    pub fn disconnected_count(&self) -> usize {
        self.disconnected.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl ServerEventHandler for TestEventHandler {
    async fn on_connected(&self, _conn: &Arc<dyn ProtocolConnection>) {
        self.connected.fetch_add(1, Ordering::Relaxed);
    }

    async fn on_disconnected(&self, _conn_id: u64, _account: Option<&str>) {
        self.disconnected.fetch_add(1, Ordering::Relaxed);
    }

    async fn on_authenticated(&self, _conn: &Arc<dyn ProtocolConnection>, _account: &str) {
        self.authenticated.fetch_add(1, Ordering::Relaxed);
    }
}

pub struct TestClientEventHandler {
    pub disconnected_count: Arc<AtomicUsize>,
}

impl TestClientEventHandler {
    pub fn new() -> Self {
        Self {
            disconnected_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn disconnected_count(&self) -> usize {
        self.disconnected_count.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl ClientEventHandler for TestClientEventHandler {
    async fn on_connected(&self, _conn: &Arc<dyn ProtocolConnection>) {}
    async fn on_disconnected(&self, _conn_id: u64) {
        self.disconnected_count.fetch_add(1, Ordering::Relaxed);
    }
}
