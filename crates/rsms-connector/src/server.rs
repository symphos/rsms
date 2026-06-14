use crate::connection::{run_connection, Connection};
use crate::pool::{ConnectionPool, AccountPool};
use crate::protocol::{
    AccountConfigProvider, AuthHandler, AccountConfig, AccountPoolConfig,
    MessageSource, ServerEventHandler,
};
use rsms_business::BusinessHandler;
use rsms_core::Result;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tracing::{error, info};

pub struct BoundServer {
    pub local_addr: SocketAddr,
    config: Arc<rsms_core::EndpointConfig>,
    handlers: Vec<Arc<dyn BusinessHandler>>,
    pool: Arc<ConnectionPool>,
    account_pool: Arc<AccountPool>,
    account_config_provider: Option<Arc<dyn AccountConfigProvider>>,
    listener: Arc<TcpListener>,
    message_source: Option<Arc<dyn MessageSource>>,
    event_handler: Option<Arc<dyn ServerEventHandler>>,
    auth_handler: Option<Arc<dyn AuthHandler>>,
    shutdown_flag: Arc<AtomicBool>,
}

/// 服务端构建器：链式配置后调用 [`ServerBuilder::serve`] 绑定监听并返回 [`BoundServer`]。
///
/// ```ignore
/// let server = ServerBuilder::new(config)
///     .handler(Arc::new(MyBiz))
///     .auth_handler(Arc::new(MyAuth))
///     .serve()
///     .await?;
/// server.run().await
/// ```
pub struct ServerBuilder {
    config: Arc<rsms_core::EndpointConfig>,
    handlers: Vec<Arc<dyn BusinessHandler>>,
    auth_handler: Option<Arc<dyn AuthHandler>>,
    message_source: Option<Arc<dyn MessageSource>>,
    account_config_provider: Option<Arc<dyn AccountConfigProvider>>,
    event_handler: Option<Arc<dyn ServerEventHandler>>,
    account_pool_config: Option<AccountPoolConfig>,
}

impl ServerBuilder {
    pub fn new(config: Arc<rsms_core::EndpointConfig>) -> Self {
        Self {
            config,
            handlers: Vec::new(),
            auth_handler: None,
            message_source: None,
            account_config_provider: None,
            event_handler: None,
            account_pool_config: None,
        }
    }

    /// 追加一个业务处理器。
    pub fn handler(mut self, handler: Arc<dyn BusinessHandler>) -> Self {
        self.handlers.push(handler);
        self
    }

    /// 一次性设置业务处理器列表（覆盖已有）。
    pub fn handlers(mut self, handlers: Vec<Arc<dyn BusinessHandler>>) -> Self {
        self.handlers = handlers;
        self
    }

    pub fn auth_handler(mut self, auth_handler: Arc<dyn AuthHandler>) -> Self {
        self.auth_handler = Some(auth_handler);
        self
    }

    pub fn message_source(mut self, message_source: Arc<dyn MessageSource>) -> Self {
        self.message_source = Some(message_source);
        self
    }

    pub fn account_config_provider(mut self, provider: Arc<dyn AccountConfigProvider>) -> Self {
        self.account_config_provider = Some(provider);
        self
    }

    pub fn event_handler(mut self, event_handler: Arc<dyn ServerEventHandler>) -> Self {
        self.event_handler = Some(event_handler);
        self
    }

    pub fn account_pool_config(mut self, config: AccountPoolConfig) -> Self {
        self.account_pool_config = Some(config);
        self
    }

    /// 绑定监听端口并构建服务器实例。
    pub async fn serve(self) -> Result<BoundServer> {
        let addr = format!("{}:{}", self.config.host, self.config.port);
        let listener = TcpListener::bind(&addr).await?;
        let local_addr = listener.local_addr()?;
        let pool = ConnectionPool::new();

        let default_account_config = AccountConfig::new();
        let pool_config = self.account_pool_config.unwrap_or_default();
        let account_pool = AccountPool::new(default_account_config, pool_config);

        Ok(BoundServer {
            local_addr,
            config: self.config,
            handlers: self.handlers,
            pool,
            account_pool,
            account_config_provider: self.account_config_provider,
            listener: Arc::new(listener),
            message_source: self.message_source,
            event_handler: self.event_handler,
            auth_handler: self.auth_handler,
            shutdown_flag: Arc::new(AtomicBool::new(false)),
        })
    }
}

impl BoundServer {
    pub async fn run(self) -> Result<()> {
        let account_config_provider = self.account_config_provider.map(|p| Arc::clone(&p));
        
        if let Some(source) = &self.message_source {
            let source_clone = Arc::clone(source);
            let account_pool2 = Arc::clone(&self.account_pool);
            let account_config = account_config_provider.clone();
            
            tokio::spawn(async move {
                inbound_fetcher_task(source_clone, account_pool2, account_config).await;
            });
        }
        
        loop {
            if self.shutdown_flag.load(Ordering::Acquire) {
                break;
            }

            let listener = Arc::clone(&self.listener);

            let (socket, peer) = match listener.accept().await {
                Ok((socket, peer)) => (socket, peer),
                Err(e) => {
                    if self.shutdown_flag.load(Ordering::Acquire) {
                        break;
                    }
                    error!("accept error: {}", e);
                    continue;
                }
            };
            
            info!(?peer, "accepted");
            let current = self.pool.len().await;
            let max = self.config.max_channels;
            if max > 0 && current >= max as usize {
                info!(?peer, "max channels reached ({max}), closing");
                drop(socket);
                continue;
            }
            
            let (conn, read) = Connection::new_with_window(socket, self.config.clone(), self.config.window_size);
            let h = self.handlers.clone();
            let pool2 = Arc::clone(&self.pool);
            let account_pool2 = Arc::clone(&self.account_pool);
            let account_config_provider = account_config_provider.clone();
            let auth_handler_clone = self.auth_handler.clone();
            let event_handler_clone = self.event_handler.clone();
            let id = conn.id;
            let protocol = self.config.protocol;

            tokio::spawn(async move {
                // 先把会话状态机推进到 Connecting，否则认证时无法转到 Authenticated/Logined，
                // 服务端 MessageSource 的 MO/回执将永不下发（见 Connection::mark_connected 注释）。
                conn.mark_connected().await;
                conn.mark_ready().await;
                pool2.add(Arc::clone(&conn)).await;
                run_connection(read, Arc::clone(&conn), h, Some(account_pool2), account_config_provider, auth_handler_clone, protocol, event_handler_clone).await;
                pool2.remove(id).await;
            });
        }
        
        Ok(())
    }

    pub fn pool(&self) -> Arc<ConnectionPool> {
        Arc::clone(&self.pool)
    }

    pub fn account_pool(&self) -> Arc<AccountPool> {
        Arc::clone(&self.account_pool)
    }

    pub fn close(&self) {
        self.shutdown_flag.store(true, Ordering::Release);
    }

    pub async fn shutdown(&self) {
        self.close();
        
        let accounts = self.account_pool.all_accounts().await;
        for account in accounts {
            if let Some(acc) = self.account_pool.get(&account).await {
                let connections = acc.get_connections_for_check().await;
                for conn in connections {
                    conn.mark_disconnected().await;
                }
            }
        }
    }
}

async fn inbound_fetcher_task(
    source: Arc<dyn MessageSource>,
    account_pool: Arc<AccountPool>,
    account_config_provider: Option<Arc<dyn AccountConfigProvider>>,
) {
    let mut account_thread_counts: std::collections::HashMap<String, u8> = std::collections::HashMap::new();
    
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        
        let accounts = account_pool.all_accounts().await;
        let active_accounts: Vec<String> = accounts.clone();
        
        for account in &accounts {
            let config = if let Some(ref provider) = account_config_provider {
                provider.get_config(account).await.unwrap_or_else(|_| AccountConfig::new())
            } else {
                AccountConfig::new()
            };
            
            let max_threads = config.max_fetch_threads as usize;
            let current_threads = *account_thread_counts.get(account).unwrap_or(&0);
            
            if (current_threads as usize) < max_threads {
                let source_clone = Arc::clone(&source);
                let account_pool_clone = Arc::clone(&account_pool);
                let interval_ms = config.fetch_interval_ms;
                let account_clone = account.clone();
                
                tokio::spawn(async move {
                    inbound_fetch_loop(account_clone, source_clone, account_pool_clone, interval_ms).await;
                });
                
                account_thread_counts.insert(account.clone(), current_threads + 1);
            }
        }
        
        account_thread_counts.retain(|acc, count| {
            if !active_accounts.contains(acc) {
                false
            } else {
                *count > 0
            }
        });
    }
}

async fn inbound_fetch_loop(
    account: String,
    source: Arc<dyn MessageSource>,
    account_pool: Arc<AccountPool>,
    interval_ms: u32,
) {
    let interval = Duration::from_millis(interval_ms as u64);
    let account_str = account.as_str();
    
    loop {
        tokio::time::sleep(interval).await;
        
        if let Some(acc) = account_pool.get(account_str).await
            && let Ok(conn) = acc.fetch_available_connection().await {
                if !conn.ready_for_fetch() {
                    continue;
                }
                
                let config = acc.config().await;
                let window_size = config.window_size as usize;
                
                if window_size > 0 {
                    let inflight = acc.inflight().await;
                    if inflight >= window_size {
                        tracing::debug!(account = %account, inflight, window_size, "window full, skip");
                        continue;
                    }
                    acc.increment_inflight().await;
                }
                
                match source.fetch(account_str, 10).await {
                    Ok(messages) => {
                        // 合并本次 fetch 的所有 PDU（含分组按序）为一批，单次 flush 写出。
                        let mut pdus = Vec::new();
                        for item in messages {
                            match item {
                                crate::protocol::MessageItem::Single(pdu) => pdus.push(pdu),
                                crate::protocol::MessageItem::Group { items } => pdus.extend(items),
                            }
                        }
                        let slices: Vec<&[u8]> = pdus.iter().map(|p| p.as_bytes()).collect();
                        if let Err(e) = conn.write_frames(&slices).await {
                            // 写失败可能造成流错位；标记连接断开，避免后续 fetch 复用这条流。
                            error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), account = %account, "send failed, marking connection disconnected: {}", e);
                            conn.mark_disconnected().await;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(account = %account, "fetch failed: {}", e);
                    }
                }
                
                if window_size > 0 {
                    acc.decrement_inflight().await;
                }
            }
    }
}
