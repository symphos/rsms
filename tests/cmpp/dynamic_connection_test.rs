use async_trait::async_trait;
use rsms_connector::{
    AuthCredentials, AuthHandler, AuthResult,
    AccountConfig, AccountConfigProvider, CmppDecoder, connect,
    AccountPool,
};
use rsms_connector::client::{ClientContext, ClientConfig, ClientHandler, ClientConnection};
use rsms_core::{ConnectionInfo, EncodedPdu, RawPdu, EndpointConfig, Frame, Result};
use rsms_codec_cmpp::{Pdu, Connect, Submit, CommandId};
use rsms_codec_cmpp::auth::compute_connect_auth;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::time::{Duration, Instant};

const TEST_ACCOUNT: &str = "900001";
const TEST_PASSWORD: &str = "pass001";
const CMPP_VERSION: u8 = 0x30;
const WINDOW_SIZE: u16 = 2048;

struct PasswordAuthHandler {
    accounts: std::collections::HashMap<String, String>,
}

impl PasswordAuthHandler {
    fn new() -> Self {
        Self { accounts: std::collections::HashMap::new() }
    }
    fn add_account(mut self, id: &str, pw: &str) -> Self {
        self.accounts.insert(id.to_string(), pw.to_string());
        self
    }
}

#[async_trait]
impl AuthHandler for PasswordAuthHandler {
    fn name(&self) -> &'static str { "password-auth" }
    async fn authenticate(&self, _client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        match credentials {
            AuthCredentials::Cmpp { source_addr, authenticator_source, version, timestamp } => {
                if let Some(pw) = self.accounts.get(&source_addr) {
                    let expected = compute_connect_auth(source_addr.as_str(), pw.as_str(), timestamp);
                    if authenticator_source == expected {
                        return Ok(AuthResult::success(source_addr));
                    }
                }
                Ok(AuthResult::failure(1, "auth failed"))
            }
            _ => Ok(AuthResult::failure(1, "unsupported")),
        }
    }
}

struct DynamicConfigProvider {
    config: tokio::sync::RwLock<AccountConfig>,
}

impl DynamicConfigProvider {
    fn new(config: AccountConfig) -> Self {
        Self { config: tokio::sync::RwLock::new(config) }
    }
}

#[async_trait]
impl AccountConfigProvider for DynamicConfigProvider {
    async fn get_config(&self, _account: &str) -> Result<AccountConfig> {
        Ok(self.config.read().await.clone())
    }
}

struct TestClientHandler {
    submit_resp_count: AtomicU64,
}

impl TestClientHandler {
    fn new() -> Self {
        Self { submit_resp_count: AtomicU64::new(0) }
    }
}

#[async_trait]
impl ClientHandler for TestClientHandler {
    fn name(&self) -> &'static str { "test-client" }
    async fn on_inbound(&self, _ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        if frame.command_id == CommandId::SubmitResp as u32 {
            self.submit_resp_count.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

fn build_connect_pdu(source_addr: &str, password: &str, version: u8) -> RawPdu {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let timestamp = (now % 100000) as u32;
    let authenticator = compute_connect_auth(source_addr, password, timestamp);
    let connect = Connect {
        source_addr: source_addr.to_string(),
        authenticator_source: authenticator,
        version,
        timestamp,
    };
    let pdu: Pdu = connect.into();
    RawPdu::from_vec(pdu.to_pdu_bytes(1).to_vec())
}

fn build_submit_pdu(src_id: &str, dest_id: &str, content: &str, sequence_id: u32) -> RawPdu {
    let submit = Submit {
        msg_id: [0u8; 8],
        pk_total: 1,
        pk_number: 1,
        registered_delivery: 1,
        msg_level: 0,
        service_id: "SMS".to_string(),
        fee_user_type: 0,
        fee_terminal_id: "".to_string(),
        fee_terminal_type: 0,
        tppid: 0,
        tpudhi: 0,
        msg_fmt: 15,
        msg_src: "900001".to_string(),
        fee_type: "01".to_string(),
        fee_code: "0".to_string(),
        valid_time: "".to_string(),
        at_time: "".to_string(),
        src_id: src_id.to_string(),
        dest_usr_tl: 1,
        dest_terminal_ids: vec![dest_id.to_string()],
        dest_terminal_type: 0,
        msg_content: content.as_bytes().to_vec(),
        link_id: "".to_string(),
    };
    let pdu: Pdu = submit.into();
    RawPdu::from_vec(pdu.to_pdu_bytes(sequence_id).to_vec())
}

async fn start_server(
    config_provider: Arc<DynamicConfigProvider>,
) -> Result<(u16, Arc<AccountPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "dynamic-conn-server",
        "127.0.0.1",
        0,
        500,
        60,
    ));
    let auth = Arc::new(PasswordAuthHandler::new().add_account(TEST_ACCOUNT, TEST_PASSWORD));
    let server = rsms_connector::serve(
        cfg,
        vec![],
        Some(auth),
        None,
        Some(config_provider as Arc<dyn AccountConfigProvider>),
        None,
        None,
    ).await?;
    let port = server.local_addr.port();
    let account_pool = server.account_pool();
    let handle = tokio::spawn(async move { let _ = server.run().await; });
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok((port, account_pool, handle))
}

async fn create_connections(port: u16, count: usize) -> Vec<Arc<ClientConnection>> {
    let mut connections = Vec::with_capacity(count);
    for _ in 0..count {
        let endpoint = Arc::new(EndpointConfig::new(
            "cmpp-client",
            "127.0.0.1",
            port,
            500,
            60,
        ).with_window_size(WINDOW_SIZE));

        let client_handler = Arc::new(TestClientHandler::new());
        let client_config = ClientConfig::default();

        let conn = connect(
            endpoint,
            client_handler,
            CmppDecoder,
            Some(client_config),
            None,
            None,
        ).await.expect("connect failed");

        let connect_pdu = build_connect_pdu(TEST_ACCOUNT, TEST_PASSWORD, CMPP_VERSION);
        conn.write_frame(connect_pdu.as_bytes()).await.expect("send connect failed");

        connections.push(conn);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    connections
}

fn alive_connections(conns: &[Arc<ClientConnection>]) -> Vec<Arc<ClientConnection>> {
    conns.iter().filter(|c| c.ready_for_fetch()).cloned().collect()
}

async fn send_submits_for_duration(
    connections: &[Arc<ClientConnection>],
    duration: Duration,
    rate_per_sec: u64,
) -> u64 {
    let sent = Arc::new(AtomicU64::new(0));
    let interval = Duration::from_secs_f64(1.0 / rate_per_sec as f64);
    let start = Instant::now();
    let mut seq: u32 = 1000;

    loop {
        if start.elapsed() >= duration {
            break;
        }
        for conn in connections {
            if !conn.ready_for_fetch() {
                continue;
            }
            if start.elapsed() >= duration {
                break;
            }
            seq = seq.wrapping_add(1);
            let submit = build_submit_pdu("10086", "13800138000", &format!("msg-{}", seq), seq);
            match conn.write_frame(submit.as_bytes()).await {
                Ok(_) => { sent.fetch_add(1, Ordering::Relaxed); }
                Err(_) => {}
            }
        }
        tokio::time::sleep(interval).await;
    }

    sent.load(Ordering::Relaxed)
}

#[tokio::test]
async fn test_dynamic_connection_adjust_5_to_3() {
    let initial_config = AccountConfig::new()
        .with_max_connections(5)
        .with_max_qps(1000)
        .with_window_size(2048);

    let provider = Arc::new(DynamicConfigProvider::new(initial_config));
    let (port, account_pool, server_handle) = start_server(provider.clone()).await.unwrap();

    let connections = create_connections(port, 5).await;
    let initial_count = account_pool.connection_count(TEST_ACCOUNT).await;
    println!("初始连接数: {}", initial_count);
    assert_eq!(initial_count, 5, "应该有5个连接");

    tokio::time::sleep(Duration::from_secs(2)).await;

    let new_config = AccountConfig::new()
        .with_max_connections(3)
        .with_max_qps(500)
        .with_window_size(2048);
    account_pool.update_config(TEST_ACCOUNT, new_config).await.unwrap();

    tokio::time::sleep(Duration::from_secs(2)).await;

    let after_count = account_pool.connection_count(TEST_ACCOUNT).await;
    println!("调整后服务端连接数: {}", after_count);
    assert_eq!(after_count, 3, "应该调整为3个连接");

    let alive = alive_connections(&connections);
    println!("客户端存活连接数: {}", alive.len());
    assert_eq!(alive.len(), 3, "客户端应该有3个连接存活");

    let sent = send_submits_for_duration(&alive, Duration::from_secs(5), 200).await;
    println!("调整后发送 Submit: {}", sent);
    assert!(sent > 0, "剩余连接应该能正常发送消息");

    server_handle.abort();
}

#[tokio::test]
async fn test_dynamic_connection_adjust_5_to_1() {
    let initial_config = AccountConfig::new()
        .with_max_connections(5)
        .with_max_qps(1000)
        .with_window_size(2048);

    let provider = Arc::new(DynamicConfigProvider::new(initial_config));
    let (port, account_pool, server_handle) = start_server(provider.clone()).await.unwrap();

    let connections = create_connections(port, 5).await;
    assert_eq!(account_pool.connection_count(TEST_ACCOUNT).await, 5);

    tokio::time::sleep(Duration::from_secs(2)).await;

    let new_config = AccountConfig::new()
        .with_max_connections(1)
        .with_max_qps(200)
        .with_window_size(2048);
    account_pool.update_config(TEST_ACCOUNT, new_config).await.unwrap();

    tokio::time::sleep(Duration::from_secs(2)).await;

    let after_count = account_pool.connection_count(TEST_ACCOUNT).await;
    println!("5→1 调整后服务端连接数: {}", after_count);
    assert_eq!(after_count, 1, "应该调整为1个连接");

    let alive = alive_connections(&connections);
    println!("客户端存活连接数: {}", alive.len());
    assert_eq!(alive.len(), 1, "客户端应该有1个连接存活");

    let sent = send_submits_for_duration(&alive, Duration::from_secs(5), 100).await;
    println!("5→1 调整后发送 Submit: {}", sent);
    assert!(sent > 0, "剩余连接应该能正常发送消息");

    server_handle.abort();
}

#[tokio::test]
async fn test_dynamic_connection_multi_step_adjust() {
    let initial_config = AccountConfig::new()
        .with_max_connections(5)
        .with_max_qps(1000)
        .with_window_size(2048);

    let provider = Arc::new(DynamicConfigProvider::new(initial_config));
    let (port, account_pool, server_handle) = start_server(provider.clone()).await.unwrap();

    let connections = create_connections(port, 5).await;
    assert_eq!(account_pool.connection_count(TEST_ACCOUNT).await, 5);

    // Step 1: 5 → 3
    tokio::time::sleep(Duration::from_secs(2)).await;
    account_pool.update_config(TEST_ACCOUNT, AccountConfig::new()
        .with_max_connections(3)
        .with_max_qps(500)
        .with_window_size(2048)
    ).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;

    let count_step1 = account_pool.connection_count(TEST_ACCOUNT).await;
    let alive_step1 = alive_connections(&connections);
    println!("Step 1 (5→3): 服务端连接数={}, 客户端存活数={}", count_step1, alive_step1.len());
    assert_eq!(count_step1, 3);
    assert_eq!(alive_step1.len(), 3);

    // Step 2: 验证剩余 3 个连接能正常发消息
    let sent_step2 = send_submits_for_duration(&alive_step1, Duration::from_secs(5), 200).await;
    println!("Step 2 (3连接): 发送 Submit={}", sent_step2);
    assert!(sent_step2 > 0);

    // Step 3: 3 → 1
    account_pool.update_config(TEST_ACCOUNT, AccountConfig::new()
        .with_max_connections(1)
        .with_max_qps(200)
        .with_window_size(2048)
    ).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;

    let count_step3 = account_pool.connection_count(TEST_ACCOUNT).await;
    let alive_step3 = alive_connections(&connections);
    println!("Step 3 (3→1): 服务端连接数={}, 客户端存活数={}", count_step3, alive_step3.len());
    assert_eq!(count_step3, 1);
    assert_eq!(alive_step3.len(), 1);

    // Step 4: 验证剩余 1 个连接能正常发消息
    let sent_step4 = send_submits_for_duration(&alive_step3, Duration::from_secs(5), 100).await;
    println!("Step 4 (1连接): 发送 Submit={}", sent_step4);
    assert!(sent_step4 > 0);

    // Step 5: 1 → 3（调高上限，不会自动创建连接）
    account_pool.update_config(TEST_ACCOUNT, AccountConfig::new()
        .with_max_connections(3)
        .with_max_qps(500)
        .with_window_size(2048)
    ).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    let count_step5 = account_pool.connection_count(TEST_ACCOUNT).await;
    println!("Step 5 (1→3上限): 服务端连接数={}", count_step5);
    assert_eq!(count_step5, 1, "调高上限不会自动创建连接");

    server_handle.abort();
}

#[tokio::test]
async fn test_dynamic_adjust_rate_limiter() {
    let initial_config = AccountConfig::new()
        .with_max_connections(3)
        .with_max_qps(1000)
        .with_window_size(2048);

    let provider = Arc::new(DynamicConfigProvider::new(initial_config));
    let (port, account_pool, server_handle) = start_server(provider.clone()).await.unwrap();

    let _connections = create_connections(port, 3).await;
    assert_eq!(account_pool.connection_count(TEST_ACCOUNT).await, 3);

    tokio::time::sleep(Duration::from_secs(1)).await;

    let acc = account_pool.get(TEST_ACCOUNT).await.unwrap();
    assert!(acc.try_acquire_submit().await, "初始 1000 QPS 应该能获取令牌");

    // 调低 QPS
    account_pool.update_config(TEST_ACCOUNT, AccountConfig::new()
        .with_max_connections(3)
        .with_max_qps(10)
        .with_window_size(2048)
    ).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut acquired = 0;
    for _ in 0..50 {
        if acc.try_acquire_submit().await {
            acquired += 1;
        }
    }
    println!("QPS=10 时连续 50 次获取令牌: 成功 {} 次", acquired);
    assert!(acquired < 20, "QPS=10 时不应该获取太多令牌, 实际获取 {}", acquired);

    // 调高 QPS
    account_pool.update_config(TEST_ACCOUNT, AccountConfig::new()
        .with_max_connections(3)
        .with_max_qps(5000)
        .with_window_size(2048)
    ).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut acquired_high = 0;
    for _ in 0..50 {
        if acc.try_acquire_submit().await {
            acquired_high += 1;
        }
    }
    println!("QPS=5000 时连续 50 次获取令牌: 成功 {} 次", acquired_high);
    assert!(acquired_high > acquired, "调高 QPS 后应获取更多令牌");

    server_handle.abort();
}
