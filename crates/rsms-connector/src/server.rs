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

pub async fn serve(
    config: Arc<rsms_core::EndpointConfig>,
    handlers: Vec<Arc<dyn BusinessHandler>>,
    auth_handler: Option<Arc<dyn AuthHandler>>,
    message_source: Option<Arc<dyn MessageSource>>,
    account_config_provider: Option<Arc<dyn AccountConfigProvider>>,
    event_handler: Option<Arc<dyn ServerEventHandler>>,
    account_pool_config: Option<AccountPoolConfig>,
) -> Result<BoundServer> {
    let addr = format!("{}:{}", config.host, config.port);
    let listener = TcpListener::bind(&addr).await?;
    let local_addr = listener.local_addr()?;
    let pool = ConnectionPool::new();

    let default_account_config = AccountConfig::new();
    let pool_config = account_pool_config.unwrap_or_default();
    let account_pool = AccountPool::new(default_account_config, pool_config);

    Ok(BoundServer {
        local_addr,
        config,
        handlers,
        pool,
        account_pool,
        account_config_provider,
        listener: Arc::new(listener),
        message_source,
        event_handler,
        auth_handler,
        shutdown_flag: Arc::new(AtomicBool::new(false)),
    })
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
            let protocol = self.config.protocol.clone();

            tokio::spawn(async move {
                conn.mark_pipeline_ready().await;
                pool2.add(Arc::clone(&conn)).await;
                run_connection(read, Arc::clone(&conn), h, Some(account_pool2), account_config_provider, auth_handler_clone, &protocol, event_handler_clone).await;
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
                        for item in messages {
                            let pdus = match item {
                                crate::protocol::MessageItem::Single(pdu) => vec![pdu],
                                crate::protocol::MessageItem::Group { items } => items,
                            };
                            for pdu in pdus {
                                if let Err(e) = conn.write_frame(pdu.as_bytes()).await {
                                    error!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), account = %account, "send failed: {}", e);
                                } else {
                                    info!(conn_id = conn.id, remote_ip = %conn.remote_ip(), remote_port = conn.remote_port(), account = %account, "pushed message");
                                }
                                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
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
