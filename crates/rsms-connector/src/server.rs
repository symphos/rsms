use crate::connection::{run_connection, Connection};
use crate::pool::{ConnectionPool, AccountPool};
use crate::protocol::{
    AccountConfigProvider, AuthHandler, AccountConfig, AccountPoolConfig,
    MessageSource, ServerEventHandler,
};
use rsms_business::{BusinessHandler, MessageHandler};
use rsms_core::{Metrics, NoopMetrics, Result};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tracing::{error, info};

pub struct BoundServer {
    pub local_addr: SocketAddr,
    config: Arc<rsms_core::EndpointConfig>,
    handlers: Vec<Arc<dyn BusinessHandler>>,
    message_handlers: Vec<Arc<dyn MessageHandler>>,
    pool: Arc<ConnectionPool>,
    account_pool: Arc<AccountPool>,
    account_config_provider: Option<Arc<dyn AccountConfigProvider>>,
    listener: Arc<TcpListener>,
    message_source: Option<Arc<dyn MessageSource>>,
    event_handler: Option<Arc<dyn ServerEventHandler>>,
    auth_handler: Option<Arc<dyn AuthHandler>>,
    metrics: Arc<dyn Metrics>,
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
    message_handlers: Vec<Arc<dyn MessageHandler>>,
    auth_handler: Option<Arc<dyn AuthHandler>>,
    message_source: Option<Arc<dyn MessageSource>>,
    account_config_provider: Option<Arc<dyn AccountConfigProvider>>,
    event_handler: Option<Arc<dyn ServerEventHandler>>,
    metrics: Option<Arc<dyn Metrics>>,
    account_pool_config: Option<AccountPoolConfig>,
}

impl ServerBuilder {
    pub fn new(config: Arc<rsms_core::EndpointConfig>) -> Self {
        Self {
            config,
            handlers: Vec::new(),
            message_handlers: Vec::new(),
            auth_handler: None,
            message_source: None,
            account_config_provider: None,
            event_handler: None,
            metrics: None,
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

    /// 追加一个窄腰统一消息处理器（重塑后主路径）。设置任意一个即让本连接走
    /// 「解码→UnifiedMessage→on_message」链，旧 `BusinessHandler` 列表被忽略。
    pub fn message_handler(mut self, handler: Arc<dyn MessageHandler>) -> Self {
        self.message_handlers.push(handler);
        self
    }

    /// 一次性设置窄腰处理器列表（覆盖已有）。
    pub fn message_handlers(mut self, handlers: Vec<Arc<dyn MessageHandler>>) -> Self {
        self.message_handlers = handlers;
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

    /// 注入指标记录器（可观测性）。不设置时使用 `NoopMetrics`（不记录）。
    pub fn metrics(mut self, metrics: Arc<dyn Metrics>) -> Self {
        self.metrics = Some(metrics);
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
            message_handlers: self.message_handlers,
            pool,
            account_pool,
            account_config_provider: self.account_config_provider,
            listener: Arc::new(listener),
            message_source: self.message_source,
            event_handler: self.event_handler,
            auth_handler: self.auth_handler,
            metrics: self.metrics.unwrap_or_else(|| Arc::new(NoopMetrics)),
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
            let metrics_clone = Arc::clone(&self.metrics);
            let shutdown_clone = Arc::clone(&self.shutdown_flag);

            tokio::spawn(async move {
                inbound_fetcher_task(source_clone, account_pool2, account_config, metrics_clone, shutdown_clone).await;
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
            let mh = self.message_handlers.clone();
            let pool2 = Arc::clone(&self.pool);
            let account_pool2 = Arc::clone(&self.account_pool);
            let account_config_provider = account_config_provider.clone();
            let auth_handler_clone = self.auth_handler.clone();
            let event_handler_clone = self.event_handler.clone();
            let metrics_clone = Arc::clone(&self.metrics);
            let shutdown_clone = Arc::clone(&self.shutdown_flag);
            let id = conn.id;
            let protocol = self.config.protocol;

            tokio::spawn(async move {
                // 先把会话状态机推进到 Connecting，否则认证时无法转到 Authenticated/Logined，
                // 服务端 MessageSource 的 MO/回执将永不下发（见 Connection::mark_connected 注释）。
                conn.mark_connected().await;
                conn.mark_ready().await;
                pool2.add(Arc::clone(&conn)).await;
                run_connection(read, Arc::clone(&conn), h, mh, Some(account_pool2), account_config_provider, auth_handler_clone, protocol, event_handler_clone, metrics_clone, shutdown_clone).await;
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

    /// 取一个可在 `run()` 之后（`run` 取走 self）调用的优雅停机句柄。
    /// 典型用法：`let sd = server.shutdown_handle(); spawn(server.run()); … sd.shutdown(t).await;`
    pub fn shutdown_handle(&self) -> ServerShutdown {
        ServerShutdown {
            shutdown_flag: Arc::clone(&self.shutdown_flag),
            account_pool: Arc::clone(&self.account_pool),
            pool: Arc::clone(&self.pool),
        }
    }

    /// 优雅停机（`run()` 之前持有 self 时可用；之后请用 [`BoundServer::shutdown_handle`]）。
    /// 默认 drain 上限 10s。
    pub async fn shutdown(&self) {
        self.shutdown_handle().shutdown(Duration::from_secs(10)).await;
    }
}

/// 服务端优雅停机句柄。在 `run()` 取走 `BoundServer` 之前由 [`BoundServer::shutdown_handle`] 取得，
/// 之后可独立触发停机。
#[derive(Clone)]
pub struct ServerShutdown {
    shutdown_flag: Arc<AtomicBool>,
    account_pool: Arc<AccountPool>,
    pool: Arc<ConnectionPool>,
}

impl ServerShutdown {
    /// 是否已进入停机。
    pub fn is_shutting_down(&self) -> bool {
        self.shutdown_flag.load(Ordering::Acquire)
    }

    /// 优雅停机（零丢，尽力）：① 置停机标志（accept 循环不再接新连、各连接读循环 ≤1s 收尾、
    /// inbound fetcher 停取）→ ② 停健康检查 → ③ drain：等各账号出站在途（inflight）降为 0
    /// （上限 `timeout`）→ ④ 关闭全部连接（发协议关闭包 + shutdown 写半边）。
    pub async fn shutdown(&self, timeout: Duration) {
        self.shutdown_flag.store(true, Ordering::Release);
        // 快照全部连接：即便读循环随后因标志退出而从池移除，仍能逐个关闭。
        let conns = self.pool.all().await;

        self.account_pool.stop_health_check().await;

        let deadline = Instant::now() + timeout;
        loop {
            let mut inflight = 0usize;
            for account in self.account_pool.all_accounts().await {
                if let Some(acc) = self.account_pool.get(&account).await {
                    inflight += acc.inflight().await;
                }
            }
            if inflight == 0 || Instant::now() >= deadline {
                if inflight > 0 {
                    tracing::warn!(inflight, "server shutdown drain 超时：仍有出站在途");
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        for conn in conns {
            conn.close().await;
        }
        info!("server graceful shutdown complete");
    }
}

#[cfg(test)]
mod wp4_bridge_tests {
    use super::*;
    use async_trait::async_trait;
    use rsms_business::{MessageContext, MessageHandler};
    use rsms_model::UnifiedMessage;

    struct DummyMh;
    #[async_trait]
    impl MessageHandler for DummyMh {
        fn name(&self) -> &'static str { "dummy" }
        async fn on_message(&self, _ctx: &MessageContext, _msg: &UnifiedMessage) -> rsms_core::Result<()> { Ok(()) }
    }

    #[test]
    fn message_handlers_setter_stores_handlers() {
        let cfg = Arc::new(rsms_core::EndpointConfig::new("ep", "127.0.0.1", 0, 8, 60));
        let b = ServerBuilder::new(cfg)
            .message_handler(Arc::new(DummyMh))
            .message_handler(Arc::new(DummyMh));
        assert_eq!(b.message_handlers.len(), 2, "message_handler 应累加进列表");
    }
}

async fn inbound_fetcher_task(
    source: Arc<dyn MessageSource>,
    account_pool: Arc<AccountPool>,
    account_config_provider: Option<Arc<dyn AccountConfigProvider>>,
    metrics: Arc<dyn Metrics>,
    shutdown: Arc<AtomicBool>,
) {
    let mut account_thread_counts: std::collections::HashMap<String, u8> = std::collections::HashMap::new();

    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
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
                let metrics_clone = Arc::clone(&metrics);
                let shutdown_clone = Arc::clone(&shutdown);

                tokio::spawn(async move {
                    inbound_fetch_loop(account_clone, source_clone, account_pool_clone, interval_ms, metrics_clone, shutdown_clone).await;
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
    metrics: Arc<dyn Metrics>,
    shutdown: Arc<AtomicBool>,
) {
    let interval = Duration::from_millis(interval_ms as u64);
    let account_str = account.as_str();

    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
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
                        match conn.write_frames(&slices).await {
                            Ok(()) => {
                                for _ in 0..slices.len() {
                                    metrics.outbound_frame();
                                }
                            }
                            Err(e) => {
                                // 写失败可能造成流错位；标记连接断开，避免后续 fetch 复用这条流。
                                error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), account = %account, "send failed, marking connection disconnected: {}", e);
                                conn.mark_disconnected().await;
                            }
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
