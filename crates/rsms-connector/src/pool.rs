use crate::connection::Connection;
use crate::id_generator::SimpleIdGenerator;
use crate::protocol::{AccountConfig, AccountPoolConfig, SubmitLimiter};
use crate::transaction::TransactionManager;
use rsms_core::{IdGenerator, Result, RsmsError, SessionState, ShutdownHandle, SimpleShutdownHandle};
use rsms_ratelimit::SmoothRateLimiter;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, AtomicU16, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::instrument;

pub struct ConnectionPool {
    inner: RwLock<Vec<Arc<Connection>>>,
    cursor: AtomicUsize,
}

impl ConnectionPool {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(Vec::new()),
            cursor: AtomicUsize::new(0),
        })
    }

    pub async fn add(&self, c: Arc<Connection>) {
        let mut g = self.inner.write().await;
        g.push(c);
    }

    pub async fn remove(&self, id: u64) {
        let mut g = self.inner.write().await;
        g.retain(|c| c.id != id);
    }

    pub async fn get(&self, id: u64) -> Option<Arc<Connection>> {
        let g = self.inner.read().await;
        g.iter().find(|c| c.id == id).cloned()
    }

    pub async fn first(&self) -> Option<Arc<Connection>> {
        let g = self.inner.read().await;
        g.first().cloned()
    }

    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    #[instrument(skip(self))]
    pub async fn fetch(&self) -> Result<Arc<Connection>> {
        let g = self.inner.read().await;
        let n = g.len();
        if n == 0 {
            return Err(RsmsError::NoReadyConnection);
        }
        let start = self.cursor.fetch_add(1, Ordering::Relaxed);
        for i in 0..n {
            let idx = (start + i) % n;
            let c = &g[idx];
            if c.ready_for_fetch() && c.session_state().await == SessionState::Logined {
                return Ok(Arc::clone(c));
            }
        }
        Err(RsmsError::NoReadyConnection)
    }
}

impl Default for ConnectionPool {
    fn default() -> Self {
        Self {
            inner: RwLock::new(Vec::new()),
            cursor: AtomicUsize::new(0),
        }
    }
}

pub struct AccountConnections {
    pub account: String,
    pub config: RwLock<AccountConfig>,
    connections: RwLock<Vec<Arc<Connection>>>,
    cursor: AtomicUsize,
    inflight: AtomicU16,
    rate_limiter: SmoothRateLimiter,
    pub transaction_manager: Arc<TransactionManager>,
    id_generator: Arc<dyn IdGenerator>,
}

impl AccountConnections {
    pub fn new(account: String, config: AccountConfig) -> Arc<Self> {
        let rate_limiter = SmoothRateLimiter::new(config.max_qps);
        let transaction_manager = Arc::new(TransactionManager::new(config.submit_resp_timeout_secs));
        
        Arc::new(Self {
            account,
            config: RwLock::new(config),
            connections: RwLock::new(Vec::new()),
            cursor: AtomicUsize::new(0),
            inflight: AtomicU16::new(0),
            rate_limiter,
            transaction_manager,
            id_generator: Arc::new(SimpleIdGenerator::new()),
        })
    }

    pub fn id_generator(&self) -> &Arc<dyn IdGenerator> {
        &self.id_generator
    }

    pub async fn add_connection(&self, conn: Arc<Connection>) {
        self.connections.write().await.push(conn);
    }

    pub async fn remove_connection(&self, conn_id: u64) {
        self.connections.write().await.retain(|c| c.id != conn_id);
    }

    pub async fn connection_count(&self) -> usize {
        self.connections.read().await.len()
    }

    pub async fn first_connection(&self) -> Option<Arc<Connection>> {
        self.connections.read().await.first().cloned()
    }

    pub async fn config(&self) -> AccountConfig {
        self.config.read().await.clone()
    }

    pub async fn update_config(&self, config: AccountConfig) {
        let old_config = self.config.read().await.clone();
        *self.config.write().await = config.clone();

        if old_config.max_qps != config.max_qps || old_config.window_size_ms != config.window_size_ms {
            self.rate_limiter.set_rate(config.max_qps).await;
            tracing::info!(
                account = %self.account,
                old_qps = old_config.max_qps,
                new_qps = config.max_qps,
                old_window_ms = old_config.window_size_ms,
                new_window_ms = config.window_size_ms,
                "submit rate limiter reconfigured"
            );
        }

        if config.max_connections > 0 && old_config.max_connections != config.max_connections {
            self.evict_excess_connections().await;
        }
    }

    pub async fn evict_excess_connections(&self) -> Vec<u64> {
        let config = self.config.read().await.clone();
        let max = config.max_connections as usize;
        if max == 0 {
            return vec![];
        }

        let current = self.connection_count().await;
        if current <= max {
            return vec![];
        }

        let excess = current - max;

        let conns = self.connections.read().await.clone();
        let mut scored = Vec::with_capacity(conns.len());
        for c in &conns {
            let ready = c.ready_for_fetch();
            let last_active = c.last_active().await;
            scored.push((Arc::clone(c), ready, last_active));
        }

        scored.sort_by(|a, b| {
            match (a.1, b.1) {
                (false, true) => std::cmp::Ordering::Less,
                (true, false) => std::cmp::Ordering::Greater,
                _ => b.2.cmp(&a.2),
            }
        });

        let mut evicted = Vec::with_capacity(excess);
        for (conn, _, _) in scored.into_iter().take(excess) {
            let conn_id = conn.id;
            tracing::info!(
                account = %self.account,
                conn_id,
                remote_ip = %conn.remote_ip(),
                remote_port = conn.remote_port(),
                max_connections = max,
                "evicting excess connection"
            );
            conn.close().await;
            self.remove_connection(conn_id).await;
            evicted.push(conn_id);
        }

        let remaining = self.connection_count().await;

        tracing::info!(
            account = %self.account,
            evicted = evicted.len(),
            remaining,
            max_connections = max,
            "connection eviction completed"
        );

        evicted
    }

    pub async fn try_acquire_submit(&self) -> bool {
        self.rate_limiter.try_acquire().await
    }

    pub async fn acquire_submit(&self, timeout: std::time::Duration) -> bool {
        self.rate_limiter.acquire(timeout).await
    }

    pub async fn inflight(&self) -> usize {
        self.inflight.load(Ordering::Acquire) as usize
    }

    pub async fn increment_inflight(&self) {
        self.inflight.fetch_add(1, Ordering::Release);
    }

    pub async fn decrement_inflight(&self) {
        self.inflight.fetch_sub(1, Ordering::Release);
    }

    pub async fn fetch_available_connection(&self) -> Result<Arc<Connection>> {
        let conns = self.connections.read().await;
        let n = conns.len();
        if n == 0 {
            return Err(RsmsError::NoReadyConnection);
        }

        let start = self.cursor.fetch_add(1, Ordering::Relaxed);
        for i in 0..n {
            let idx = (start + i) % n;
            let conn = &conns[idx];
            if conn.ready_for_fetch() && conn.session_state().await == SessionState::Logined
                && conn.writable().await {
                    return Ok(Arc::clone(conn));
                }
        }

        Err(RsmsError::NoReadyConnection)
    }

    pub async fn fetch_for_inbound(&self) -> Option<Arc<Connection>> {
        self.fetch_available_connection().await.ok()
    }

    pub async fn get_connections_for_check(&self) -> Vec<Arc<Connection>> {
        self.connections.read().await.clone()
    }

    pub async fn get_connection_by_id(&self, conn_id: u64) -> Option<Arc<Connection>> {
        let connections = self.connections.read().await.clone();
        connections.into_iter().find(|c| c.id == conn_id)
    }

    pub async fn get_least_used_connections(&self, count: usize) -> Vec<Arc<Connection>> {
        let conns = self.connections.read().await.clone();
        let mut pairs: Vec<(Arc<Connection>, Instant)> = Vec::new();

        for conn in &conns {
            pairs.push((Arc::clone(conn), conn.last_active().await));
        }

        pairs.sort_by(|a, b| b.1.cmp(&a.1));

        pairs.into_iter()
            .take(count)
            .map(|(c, _)| c)
            .collect()
    }

    pub async fn start_transaction_timeout_checker(&self) {
        let tm = Arc::clone(&self.transaction_manager);
        tm.start_timeout_checker().await;
    }
}

impl AccountConnections {
    pub fn transaction_manager(&self) -> Arc<TransactionManager> {
        Arc::clone(&self.transaction_manager)
    }
}

#[async_trait::async_trait]
impl SubmitLimiter for AccountConnections {
    async fn try_acquire_submit(&self) -> bool {
        self.try_acquire_submit().await
    }

    async fn acquire_submit(&self, timeout: std::time::Duration) -> bool {
        self.acquire_submit(timeout).await
    }
}

pub struct AccountPool {
    inner: RwLock<HashMap<String, Arc<AccountConnections>>>,
    default_config: AccountConfig,
    config: AccountPoolConfig,
    health_checker: RwLock<Option<Arc<HealthChecker>>>,
    health_check_shutdown: RwLock<Option<Arc<SimpleShutdownHandle>>>,
}

impl AccountPool {
    pub fn new(default_config: AccountConfig, config: AccountPoolConfig) -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(HashMap::new()),
            default_config,
            config,
            health_checker: RwLock::new(None),
            health_check_shutdown: RwLock::new(None),
        })
    }

    pub async fn get_or_create(&self, account: &str) -> Arc<AccountConnections> {
        {
            let g = self.inner.read().await;
            if let Some(acc) = g.get(account) {
                return Arc::clone(acc);
            }
        }

        let acc = AccountConnections::new(account.to_string(), self.default_config.clone());
        self.inner.write().await.insert(account.to_string(), Arc::clone(&acc));
        
        let _ = acc.clone();
        acc
    }

    pub async fn start_health_check(self: &Arc<Self>) -> Arc<SimpleShutdownHandle> {
        {
            let hc_guard = self.health_checker.read().await;
            if let Some(ref hc) = *hc_guard {
                return hc.clone().start();
            }
        }
        
        let health_checker = HealthChecker::new(
            Arc::clone(self),
            Duration::from_secs(30),
            Duration::from_secs(10),
            Arc::new(|conn_id, account| {
                tracing::warn!(conn_id, account, "connection marked unhealthy");
            }),
        );  // HealthChecker::new already returns Arc<Self>
        
        let shutdown_handle = health_checker.clone().start();
        
        let mut hc_guard = self.health_checker.write().await;
        let mut shutdown_guard = self.health_check_shutdown.write().await;
        *hc_guard = Some(Arc::clone(&health_checker));
        *shutdown_guard = Some(shutdown_handle.clone());
        
        shutdown_handle
    }

    pub async fn get(&self, account: &str) -> Option<Arc<AccountConnections>> {
        let g = self.inner.read().await;
        g.get(account).map(Arc::clone)
    }

    pub async fn remove(&self, account: &str) {
        self.inner.write().await.remove(account);
    }

    pub async fn connection_count(&self, account: &str) -> usize {
        if let Some(acc) = self.get(account).await {
            acc.connection_count().await
        } else {
            0
        }
    }

    pub async fn update_config(&self, account: &str, config: AccountConfig) -> Result<()> {
        if let Some(acc) = self.get(account).await {
            acc.update_config(config).await;
            Ok(())
        } else {
            Err(RsmsError::Other(format!("account {} not found", account)))
        }
    }

    pub async fn all_accounts(&self) -> Vec<String> {
        self.inner.read().await.keys().cloned().collect()
    }

    pub fn check_interval(&self) -> Duration {
        Duration::from_millis(self.config.check_interval_ms as u64)
    }

    pub async fn stop_health_check(&self) {
        let mut shutdown_guard = self.health_check_shutdown.write().await;
        if let Some(shutdown) = shutdown_guard.take() {
            shutdown.shutdown();
        }
        let mut hc_guard = self.health_checker.write().await;
        hc_guard.take();
    }
}

/// HealthChecker monitors connection liveness via heartbeat responses.
///
/// When a connection fails to respond to heartbeats within the timeout,
/// HealthChecker marks it as unhealthy and triggers reconnection via the callback.
pub struct HealthChecker {
    account_pool: Arc<AccountPool>,
    check_interval: Duration,
    heartbeat_timeout: Duration,
    on_unhealthy: Arc<dyn Fn(u64, &str) + Send + Sync>,
}

impl HealthChecker {
    pub fn new(
        account_pool: Arc<AccountPool>,
        check_interval: Duration,
        heartbeat_timeout: Duration,
        on_unhealthy: Arc<dyn Fn(u64, &str) + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self {
            account_pool,
            check_interval,
            heartbeat_timeout,
            on_unhealthy,
        })
    }

    pub fn start(self: Arc<Self>) -> Arc<SimpleShutdownHandle> {
        let shutdown_handle = Arc::new(SimpleShutdownHandle::new());
        let wait_rx = shutdown_handle.take_wait_rx();
        let shutdown_flag = shutdown_handle.is_shutdown_signaled();

        let account_pool = self.account_pool.clone();
        let check_interval = self.check_interval;
        let heartbeat_timeout = self.heartbeat_timeout;
        let on_unhealthy = self.on_unhealthy.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(check_interval);
            let mut wait_rx = Some(wait_rx);

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if shutdown_flag.load(Ordering::Acquire) {
                            break;
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

                Self::check_connections(&account_pool, heartbeat_timeout, &on_unhealthy).await;
            }
        });

        shutdown_handle
    }

    async fn check_connections(
        account_pool: &Arc<AccountPool>,
        heartbeat_timeout: Duration,
        on_unhealthy: &Arc<dyn Fn(u64, &str) + Send + Sync>,
    ) {
        let accounts = account_pool.all_accounts().await;

        for account in accounts {
            if let Some(acc) = account_pool.get(&account).await {
                let connections = acc.get_connections_for_check().await;

                for conn in connections {
                    let last_active = conn.last_active().await;
                    let now = Instant::now();

                    if now.duration_since(last_active) > heartbeat_timeout
                        && conn.is_healthy().await {
                            on_unhealthy(conn.id, &account);
                        }
                }
            }
        }
    }
}
