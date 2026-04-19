//! 客户端连接池：管理多个 ClientConnection，支持动态调整和自动重连。

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use rsms_core::EndpointConfig;

use crate::protocol::{ClientEventHandler, MessageSource};
use crate::{ClientConfig, ClientConnection, ClientHandler, ConnectionEvent};

struct ClientPoolInner {
    connections: Vec<Arc<ClientConnection>>,
    max_channels: u16,
}

pub struct ConnectionReadyCallback {
    inner: Arc<dyn Fn(Arc<ClientConnection>) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>,
}

impl ConnectionReadyCallback {
    fn new<F>(f: F) -> Self
    where
        F: Fn(Arc<ClientConnection>) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync + 'static,
    {
        Self { inner: Arc::new(f) }
    }
    
    fn call(&self, conn: Arc<ClientConnection>) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        (self.inner)(conn)
    }
}

impl Clone for ConnectionReadyCallback {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl ClientPoolInner {
    fn new(max_channels: u16) -> Self {
        Self {
            connections: Vec::new(),
            max_channels,
        }
    }

    fn max_channels(&self) -> u16 {
        self.max_channels
    }

    fn set_max_channels(&mut self, n: u16) {
        self.max_channels = n;
    }

    async fn add_connection(&mut self, conn: Arc<ClientConnection>) {
        self.connections.push(conn);
    }

    async fn remove_connection(&mut self, id: u64) -> Option<Arc<ClientConnection>> {
        if let Some(pos) = self.connections.iter().position(|c| c.id() == id) {
            return Some(self.connections.remove(pos));
        }
        None
    }

    async fn connection_count(&self) -> usize {
        self.connections.len()
    }

    async fn get_connections(&self) -> Vec<Arc<ClientConnection>> {
        self.connections.clone()
    }
}

pub struct ClientPool {
    inner: RwLock<ClientPoolInner>,
    endpoint: Arc<EndpointConfig>,
    client_handler: Arc<dyn ClientHandler>,
    client_config: ClientConfig,
    message_source: Option<Arc<dyn MessageSource>>,
    event_handler: Option<Arc<dyn ClientEventHandler>>,
    event_tx: mpsc::Sender<ConnectionEvent>,
    shutdown_flag: AtomicBool,
    on_connection_ready: RwLock<Option<Arc<ConnectionReadyCallback>>>,
}

impl ClientPool {
    pub async fn new(
        endpoint: Arc<EndpointConfig>,
        max_channels: u16,
        client_handler: Arc<dyn ClientHandler>,
        client_config: Option<ClientConfig>,
        message_source: Option<Arc<dyn MessageSource>>,
        event_handler: Option<Arc<dyn ClientEventHandler>>,
    ) -> Arc<Self> {
        let config = client_config.unwrap_or_default();
        let (event_tx, _event_rx) = mpsc::channel(100);

        Arc::new(Self {
            inner: RwLock::new(ClientPoolInner::new(max_channels)),
            endpoint,
            client_handler,
            client_config: config,
            message_source,
            event_handler,
            event_tx,
            shutdown_flag: AtomicBool::new(false),
            on_connection_ready: RwLock::new(None),
        })
    }

    pub async fn set_on_connection_ready<F>(&self, callback: F)
    where
        F: Fn(Arc<ClientConnection>) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync + 'static,
    {
        *self.on_connection_ready.write().await = Some(Arc::new(ConnectionReadyCallback::new(callback)));
    }

    pub async fn start(&self) {
        let max_channels = self.inner.read().await.max_channels();
        info!(max_channels, "starting client pool");

        for _ in 0..max_channels {
            self.create_and_add_connection().await;
        }
    }

    async fn create_and_add_connection(&self) -> Option<Arc<ClientConnection>> {
        let addr = format!("{}:{}", self.endpoint.host, self.endpoint.port);
        info!(%addr, "creating new connection");

        let stream = match tokio::net::TcpStream::connect(&addr).await {
            Ok(s) => s,
            Err(e) => {
                warn!("failed to connect: {}", e);
                return None;
            }
        };

        let conn = crate::connect_with_pool(
            stream,
            self.endpoint.clone(),
            self.client_handler.clone(),
            Some(self.client_config.clone()),
            self.message_source.clone(),
            self.event_handler.clone(),
            self.event_tx.clone(),
        )
        .await
        .ok()?;

        info!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), "connection created and added to pool");
        self.inner.write().await.add_connection(conn.clone()).await;
        
        if let Some(ref callback) = *self.on_connection_ready.read().await {
            let callback = callback.clone();
            let conn_clone = conn.clone();
            tokio::spawn(async move {
                callback.call(conn_clone).await;
            });
        }
        
        Some(conn)
    }

    pub async fn set_max_channels(&self, n: u16) {
        self.inner.write().await.set_max_channels(n);
    }

    pub async fn connection_count(&self) -> usize {
        self.inner.read().await.connection_count().await
    }

    pub async fn max_channels(&self) -> u16 {
        self.inner.read().await.max_channels()
    }

    pub async fn shutdown(&self) {
        self.shutdown_flag.store(true, Ordering::Release);
        let conns = self.inner.read().await.get_connections().await;
        for conn in conns {
            conn.mark_disconnected().await;
        }
    }

    pub async fn adjust_connections(&self) {
        let (current, max) = {
            let inner = self.inner.read().await;
            (inner.connection_count().await, inner.max_channels())
        };

        if current < max as usize {
            let needed = max as usize - current;
            info!(current, max, needed, "creating {} new connections", needed);
            for _ in 0..needed {
                self.create_and_add_connection().await;
            }
        } else if current > max as usize {
            let excess = current - max as usize;
            info!(current, max, excess, "closing {} excess connections", excess);
            let conns = self.inner.read().await.get_connections().await;
            for conn in conns.into_iter().take(excess) {
                conn.mark_disconnected().await;
                let _ = self.inner.write().await.remove_connection(conn.id()).await;
            }
        }
    }

    pub async fn on_connection_event(&self, event: ConnectionEvent) {
        match event {
            ConnectionEvent::Disconnected(id) => {
                warn!(conn_id = id, "connection disconnected");
                let _ = self.inner.write().await.remove_connection(id).await;
                if let Some(handler) = &self.event_handler {
                    handler.on_disconnected(id).await;
                }
            }
            ConnectionEvent::HeartbeatTimeout(id) => {
                warn!(conn_id = id, "connection heartbeat timeout");
                let _ = self.inner.write().await.remove_connection(id).await;
                if let Some(handler) = &self.event_handler {
                    handler.on_disconnected(id).await;
                }
            }
        }
    }
}
