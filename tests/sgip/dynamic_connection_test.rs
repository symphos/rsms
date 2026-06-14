use async_trait::async_trait;
use rsms_connector::{
    AuthCredentials, AuthHandler, AuthResult,
    AccountConfig, AccountConfigProvider, SgipDecoder, ClientBuilder,
    AccountPool,
};
use rsms_connector::client::{ClientContext, ClientConfig, ClientHandler, ClientConnection};
use rsms_core::{ConnectionInfo, EncodedPdu, RawPdu, EndpointConfig, Protocol, Frame, Result};
// 窄腰统一模型：收发一律走 SgipAdapter + UnifiedMessage，不再手构裸 codec / 手剥头部字节。
use rsms_codec_sgip::adapter::SgipAdapter;
use rsms_model::{
    Address, Encoding, ProtocolAdapter, ProtocolExtra, Sequence, SgipExtra, UnifiedBind,
    UnifiedMessage, UnifiedSubmit,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::time::{Duration, Instant};

const TEST_ACCOUNT: &str = "900001";
const TEST_PASSWORD: &str = "password123";
const SGIP_NODE_ID: u32 = 1;
const SGIP_TIMESTAMP: u32 = 0x04051200;

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
            AuthCredentials::Sgip { login_name, login_password } => {
                if let Some(pw) = self.accounts.get(&login_name) {
                    if login_password == *pw {
                        return Ok(AuthResult::success(login_name));
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
        if let Ok(UnifiedMessage::SubmitResp(_)) = SgipAdapter.decode(frame) {
            self.submit_resp_count.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

static SEQ: AtomicU64 = AtomicU64::new(1);

fn next_seq() -> u32 {
    SEQ.fetch_add(1, Ordering::Relaxed) as u32
}

fn build_bind_pdu() -> RawPdu {
    // 明文认证：authenticator 装口令字节；version 承载 login_type=1。
    let bind = UnifiedMessage::Bind(UnifiedBind {
        client_id: TEST_ACCOUNT.to_string(),
        authenticator: TEST_PASSWORD.as_bytes().to_vec(),
        timestamp: 0,
        version: 1,
        system_type: None,
        mode: rsms_model::BindMode::default(),
        login_mode: None,
    });
    let seq = Sequence::Sgip { node_id: SGIP_NODE_ID, timestamp: SGIP_TIMESTAMP, number: next_seq() };
    RawPdu::from(SgipAdapter.encode(&bind, seq).expect("encode bind"))
}

fn build_submit_pdu(sp_number: &str, dest_number: &str, content: &str, seq_num: u32) -> Vec<u8> {
    // 统一模型构造 Submit；SGIP 方言字段经 SgipExtra 传递；复合序列 number=seq_num。
    let submit = UnifiedMessage::Submit(UnifiedSubmit {
        src: Address::plain(sp_number),
        dests: vec![Address::plain(dest_number)],
        content: content.as_bytes().to_vec(),
        encoding: Encoding::Gbk, // msg_fmt=15(GBK)
        want_report: true,       // report_flag=1
        concat: None,
        extra: ProtocolExtra::Sgip(SgipExtra {
            charge_number: sp_number.to_string(),
            service_type: "SMS".to_string(),
            fee_type: 2,
            fee_value: "000000".to_string(),
            given_value: "000000".to_string(),
            ..Default::default()
        }),
        tlvs: vec![],
    });
    let seq = Sequence::Sgip { node_id: SGIP_NODE_ID, timestamp: SGIP_TIMESTAMP, number: seq_num };
    SgipAdapter.encode(&submit, seq).expect("encode submit")
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
    ).with_protocol(Protocol::Sgip));
    let auth = Arc::new(PasswordAuthHandler::new().add_account(TEST_ACCOUNT, TEST_PASSWORD));
    let server = rsms_connector::ServerBuilder::new(cfg)
        .handlers(vec![])
        .auth_handler(auth)
        .account_config_provider(config_provider as Arc<dyn AccountConfigProvider>)
        .serve()
        .await?;
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
            "sgip-client",
            "127.0.0.1",
            port,
            500,
            60,
        ).with_protocol(Protocol::Sgip));

        let client_handler = Arc::new(TestClientHandler::new());
        let conn = ClientBuilder::new(endpoint, client_handler, SgipDecoder)
            .client_config(ClientConfig::default())
            .connect()
            .await
            .expect("connect failed");

        let bind_pdu = build_bind_pdu();
        conn.write_frame(bind_pdu.as_bytes()).await.expect("send bind failed");
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
        if start.elapsed() >= duration { break; }
        for conn in connections {
            if !conn.ready_for_fetch() { continue; }
            if start.elapsed() >= duration { break; }
            seq = seq.wrapping_add(1);
            let submit = build_submit_pdu("106900", "13800138000", &format!("msg-{}", seq), seq);
            match conn.write_frame(&submit).await {
                Ok(_) => { sent.fetch_add(1, Ordering::Relaxed); }
                Err(_) => {}
            }
        }
        tokio::time::sleep(interval).await;
    }
    sent.load(Ordering::Relaxed)
}

#[tokio::test]
async fn test_sgip_dynamic_connection_adjust_5_to_3() {
    let provider = Arc::new(DynamicConfigProvider::new(
        AccountConfig::new().with_max_connections(5).with_max_qps(1000).with_window_size(2048)
    ));
    let (port, account_pool, server_handle) = start_server(provider).await.unwrap();

    let connections = create_connections(port, 5).await;
    assert_eq!(account_pool.connection_count(TEST_ACCOUNT).await, 5);

    tokio::time::sleep(Duration::from_secs(2)).await;
    account_pool.update_config(TEST_ACCOUNT, AccountConfig::new()
        .with_max_connections(3).with_max_qps(500).with_window_size(2048)
    ).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(account_pool.connection_count(TEST_ACCOUNT).await, 3);
    let alive = alive_connections(&connections);
    assert_eq!(alive.len(), 3);

    let sent = send_submits_for_duration(&alive, Duration::from_secs(5), 200).await;
    println!("SGIP 5→3 发送 Submit: {}", sent);
    assert!(sent > 0);
    server_handle.abort();
}

#[tokio::test]
async fn test_sgip_dynamic_connection_multi_step() {
    let provider = Arc::new(DynamicConfigProvider::new(
        AccountConfig::new().with_max_connections(5).with_max_qps(1000).with_window_size(2048)
    ));
    let (port, account_pool, server_handle) = start_server(provider).await.unwrap();

    let connections = create_connections(port, 5).await;
    assert_eq!(account_pool.connection_count(TEST_ACCOUNT).await, 5);

    // 5 → 3
    tokio::time::sleep(Duration::from_secs(2)).await;
    account_pool.update_config(TEST_ACCOUNT, AccountConfig::new()
        .with_max_connections(3).with_max_qps(500).with_window_size(2048)
    ).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(account_pool.connection_count(TEST_ACCOUNT).await, 3);
    assert_eq!(alive_connections(&connections).len(), 3);

    // 3 → 1
    account_pool.update_config(TEST_ACCOUNT, AccountConfig::new()
        .with_max_connections(1).with_max_qps(200).with_window_size(2048)
    ).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(account_pool.connection_count(TEST_ACCOUNT).await, 1);
    let alive = alive_connections(&connections);
    assert_eq!(alive.len(), 1);

    let sent = send_submits_for_duration(&alive, Duration::from_secs(5), 100).await;
    println!("SGIP 3→1 发送 Submit: {}", sent);
    assert!(sent > 0);
    server_handle.abort();
}
