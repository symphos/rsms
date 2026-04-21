use async_trait::async_trait;
use rsms_connector::{
    AuthCredentials, AuthHandler, AuthResult,
    AccountConfig, AccountConfigProvider, SmppDecoder, connect,
    AccountPool,
};
use rsms_connector::client::{ClientContext, ClientConfig, ClientHandler, ClientConnection};
use rsms_core::{ConnectionInfo, EncodedPdu, RawPdu, EndpointConfig, Frame, Result};
use rsms_codec_smpp::{
    Pdu, CommandId, BindTransmitter, SubmitSm,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::time::{Duration, Instant};

const TEST_SYSTEM_ID: &str = "SMPP";
const TEST_PASSWORD: &str = "pwd12345";

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
            AuthCredentials::Smpp { system_id, password, interface_version: _ } => {
                if let Some(pw) = self.accounts.get(&system_id) {
                    if password == *pw {
                        return Ok(AuthResult::success(system_id));
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
        if frame.command_id == CommandId::SUBMIT_SM_RESP as u32 {
            self.submit_resp_count.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

fn build_bind_pdu(system_id: &str, password: &str) -> RawPdu {
    let bind = BindTransmitter::new(system_id, password, "CMT", 0x34);
    let pdu: Pdu = bind.into();
    pdu.to_pdu_bytes(1)
}

fn build_submit_sm_pdu(src: &str, dst: &str, content: &str, seq: u32) -> RawPdu {
    let mut submit = SubmitSm::new();
    submit.source_addr = src.to_string();
    submit.dest_addr_ton = 1;
    submit.dest_addr_npi = 1;
    submit.destination_addr = dst.to_string();
    submit.esm_class = 0;
    submit.registered_delivery = 1;
    submit.data_coding = 0x03;
    submit.short_message = content.as_bytes().to_vec();
    let pdu: Pdu = submit.into();
    pdu.to_pdu_bytes(seq)
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
    ).with_protocol("smpp"));
    let auth = Arc::new(PasswordAuthHandler::new().add_account(TEST_SYSTEM_ID, TEST_PASSWORD));
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
            "smpp-client",
            "127.0.0.1",
            port,
            500,
            60,
        ).with_protocol("smpp"));

        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler,
            SmppDecoder,
            Some(ClientConfig::default()),
            None,
            None,
        ).await.expect("connect failed");

        let bind_pdu = build_bind_pdu(TEST_SYSTEM_ID, TEST_PASSWORD);
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
            let submit = build_submit_sm_pdu("10086", "13800138000", &format!("msg-{}", seq), seq);
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
async fn test_smpp_dynamic_connection_adjust_5_to_3() {
    let provider = Arc::new(DynamicConfigProvider::new(
        AccountConfig::new().with_max_connections(5).with_max_qps(1000).with_window_size(2048)
    ));
    let (port, account_pool, server_handle) = start_server(provider).await.unwrap();

    let connections = create_connections(port, 5).await;
    assert_eq!(account_pool.connection_count(TEST_SYSTEM_ID).await, 5);

    tokio::time::sleep(Duration::from_secs(2)).await;
    account_pool.update_config(TEST_SYSTEM_ID, AccountConfig::new()
        .with_max_connections(3).with_max_qps(500).with_window_size(2048)
    ).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(account_pool.connection_count(TEST_SYSTEM_ID).await, 3);
    let alive = alive_connections(&connections);
    assert_eq!(alive.len(), 3);

    let sent = send_submits_for_duration(&alive, Duration::from_secs(5), 200).await;
    println!("SMPP 5→3 发送 Submit: {}", sent);
    assert!(sent > 0);
    server_handle.abort();
}

#[tokio::test]
async fn test_smpp_dynamic_connection_multi_step() {
    let provider = Arc::new(DynamicConfigProvider::new(
        AccountConfig::new().with_max_connections(5).with_max_qps(1000).with_window_size(2048)
    ));
    let (port, account_pool, server_handle) = start_server(provider).await.unwrap();

    let connections = create_connections(port, 5).await;
    assert_eq!(account_pool.connection_count(TEST_SYSTEM_ID).await, 5);

    // 5 → 3
    tokio::time::sleep(Duration::from_secs(2)).await;
    account_pool.update_config(TEST_SYSTEM_ID, AccountConfig::new()
        .with_max_connections(3).with_max_qps(500).with_window_size(2048)
    ).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(account_pool.connection_count(TEST_SYSTEM_ID).await, 3);
    assert_eq!(alive_connections(&connections).len(), 3);

    // 3 → 1
    account_pool.update_config(TEST_SYSTEM_ID, AccountConfig::new()
        .with_max_connections(1).with_max_qps(200).with_window_size(2048)
    ).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(account_pool.connection_count(TEST_SYSTEM_ID).await, 1);
    let alive = alive_connections(&connections);
    assert_eq!(alive.len(), 1);

    let sent = send_submits_for_duration(&alive, Duration::from_secs(5), 100).await;
    println!("SMPP 3→1 发送 Submit: {}", sent);
    assert!(sent > 0);
    server_handle.abort();
}
