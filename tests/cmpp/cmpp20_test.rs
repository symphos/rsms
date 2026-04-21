use async_trait::async_trait;
use rsms_connector::{
    connect, AuthCredentials, AuthHandler, AuthResult,
    ClientEventHandler, ClientHandler, MessageSource, MessageItem, ProtocolConnection,
    AccountConfig, AccountConfigProvider,
    CmppDecoder,
};
use rsms_connector::client::{ClientContext, ClientConfig};
use rsms_core::{ConnectionInfo, RawPdu, EndpointConfig, Frame, Result};
use rsms_codec_cmpp::{
    decode_message_with_version, CmppMessage, Pdu, Connect, DeliverResp, 
    CommandId, Submit,
};
use rsms_codec_cmpp::codec::PduHeader;
use rsms_codec_cmpp::auth::compute_connect_auth;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::time::Duration;

pub struct PasswordAuthHandler {
    accounts: HashMap<String, String>,
    pub auth_count: Arc<AtomicUsize>,
    pub auth_success: Arc<AtomicUsize>,
    pub auth_fail: Arc<AtomicUsize>,
}

impl PasswordAuthHandler {
    pub fn new() -> Self {
        Self {
            accounts: HashMap::new(),
            auth_count: Arc::new(AtomicUsize::new(0)),
            auth_success: Arc::new(AtomicUsize::new(0)),
            auth_fail: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn add_account(mut self, account: &str, password: &str) -> Self {
        self.accounts.insert(account.to_string(), password.to_string());
        self
    }
}

#[async_trait]
impl AuthHandler for PasswordAuthHandler {
    fn name(&self) -> &'static str {
        "password-auth-handler"
    }

    async fn authenticate(&self, _client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        self.auth_count.fetch_add(1, Ordering::Relaxed);

        if let AuthCredentials::Cmpp {
            source_addr,
            authenticator_source,
            timestamp,
            ..
        } = credentials
        {
            if let Some(password) = self.accounts.get(&source_addr) {
                let expected = compute_connect_auth(&source_addr, password, timestamp);
                if expected == authenticator_source {
                    self.auth_success.fetch_add(1, Ordering::Relaxed);
                    return Ok(AuthResult::success(&source_addr));
                } else {
                    self.auth_fail.fetch_add(1, Ordering::Relaxed);
                    return Ok(AuthResult::failure(1, "Invalid password"));
                }
            }

            self.auth_fail.fetch_add(1, Ordering::Relaxed);
            Ok(AuthResult::failure(1, "Unknown account"))
        } else {
            self.auth_fail.fetch_add(1, Ordering::Relaxed);
            Ok(AuthResult::failure(1, "Invalid credentials"))
        }
    }
}

pub struct TestClientHandler {
    pub connected: Arc<AtomicBool>,
    pub connect_resp_status: Arc<Mutex<Option<u32>>>,
    pub submit_resp_status: Arc<Mutex<Option<u32>>>,
    pub version: u8,
    seq: AtomicUsize,
}

impl TestClientHandler {
    pub fn new_with_version(version: u8) -> Self {
        Self {
            connected: Arc::new(AtomicBool::new(false)),
            connect_resp_status: Arc::new(Mutex::new(None)),
            submit_resp_status: Arc::new(Mutex::new(None)),
            version,
            seq: AtomicUsize::new(1),
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u32
    }

    pub fn build_connect_pdu_v2(&self, account: &str, password: &str) -> RawPdu {
        let timestamp = 0u32;
        let auth = compute_connect_auth(account, password, timestamp);

        let connect = Connect {
            source_addr: account.to_string(),
            authenticator_source: auth,
            version: self.version,
            timestamp,
        };

        let pdu: Pdu = connect.into();
        pdu.to_pdu_bytes(self.next_seq())
    }

    pub fn build_submit_pdu_v2(&self, src: &str, dst: &str, content: &str) -> RawPdu {
        let mut submit = Submit::new();
        submit.src_id = src.to_string();
        submit.dest_usr_tl = 1;
        submit.dest_terminal_ids = vec![dst.to_string()];
        submit.msg_content = content.as_bytes().to_vec();
        submit.registered_delivery = 0;

        let pdu: Pdu = submit.into();
        pdu.to_pdu_bytes(self.next_seq())
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn get_connect_status(&self) -> Option<u32> {
        self.connect_resp_status.lock().unwrap().clone()
    }

    pub fn get_submit_status(&self) -> Option<u32> {
        self.submit_resp_status.lock().unwrap().clone()
    }

    pub fn reset(&self) {
        self.connected.store(false, Ordering::Relaxed);
        *self.connect_resp_status.lock().unwrap() = None;
        *self.submit_resp_status.lock().unwrap() = None;
    }
}

#[async_trait]
impl ClientHandler for TestClientHandler {
    fn name(&self) -> &'static str {
        "test-client-handler"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        if pdu.len() < 12 {
            return Ok(());
        }

        let cmd_id = u32::from_be_bytes([pdu[4], pdu[5], pdu[6], pdu[7]]);

        if cmd_id == CommandId::ConnectResp as u32 && pdu.len() >= 25 {
            let status = u32::from_be_bytes([pdu[12], pdu[13], pdu[14], pdu[15]]);
            *self.connect_resp_status.lock().unwrap() = Some(status);
            if status == 0 {
                self.connected.store(true, Ordering::Relaxed);
            }
        } else if cmd_id == CommandId::SubmitResp as u32 && pdu.len() >= 20 {
            let result = u32::from_be_bytes([pdu[20], pdu[21], pdu[22], pdu[23]]);
            *self.submit_resp_status.lock().unwrap() = Some(result);
        } else if cmd_id == CommandId::Deliver as u32 {
            let resp = DeliverResp {
                msg_id: [0u8; 8],
                result: 0,
            };
            let resp_pdu: Pdu = resp.into();
            ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;
        }

        Ok(())
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

pub struct TestMessageSource;

#[async_trait]
impl MessageSource for TestMessageSource {
    async fn fetch(&self, _account: &str, _batch_size: usize) -> Result<Vec<MessageItem>> {
        Ok(vec![])
    }
}

struct MockAccountConfigProvider;

#[async_trait]
impl AccountConfigProvider for MockAccountConfigProvider {
    async fn get_config(&self, _account: &str) -> Result<AccountConfig> {
        Ok(AccountConfig::new())
    }
}

use rsms_connector::serve;

async fn start_test_server(
    auth_handler: Arc<dyn AuthHandler>,
    idle_timeout_secs: u32,
) -> Result<(u16, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "test-server",
        "127.0.0.1",
        0,
        8,
        idle_timeout_secs as u16,
    ));
    let server = serve(
        cfg,
        vec![],
        Some(auth_handler),
        None,
        Some(Arc::new(MockAccountConfigProvider) as Arc<dyn AccountConfigProvider>),
        None,
        None,
    )
    .await
    .expect("bind");
    let port = server.local_addr.port();
    let handle = tokio::spawn(async move {
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok((port, handle))
}

#[allow(dead_code)]
fn get_test_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static COUNTER: AtomicU16 = AtomicU16::new(0);
    30000 + COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[tokio::test]
async fn test_connect_v20_version() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let (port, handle) = start_test_server(auth.clone(), 30).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new_with_version(0x20));
    let conn = connect(
        endpoint,
        client_handler.clone(),
        CmppDecoder,
        Some(ClientConfig::new()),
        None,
        None,
    )
    .await
    .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu_v2("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(client_handler.get_connect_status(), Some(0), "CMPP 2.0 认证成功状态码应为0");
    assert!(client_handler.is_connected(), "应该已连接");
    assert_eq!(auth.auth_success.load(Ordering::Relaxed), 1, "应该认证成功1次");

    handle.abort();
}

#[tokio::test]
async fn test_connect_v30_version() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let (port, handle) = start_test_server(auth.clone(), 30).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new_with_version(0x30));
    let conn = connect(
        endpoint,
        client_handler.clone(),
        CmppDecoder,
        Some(ClientConfig::new()),
        None,
        None,
    )
    .await
    .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu_v2("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(client_handler.get_connect_status(), Some(0), "CMPP 3.0 认证成功状态码应为0");
    assert!(client_handler.is_connected(), "应该已连接");
    assert_eq!(auth.auth_success.load(Ordering::Relaxed), 1, "应该认证成功1次");

    handle.abort();
}

#[tokio::test]
async fn test_connect_unsupported_version() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let (port, handle) = start_test_server(auth.clone(), 30).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new_with_version(0x50));
    let conn = connect(
        endpoint,
        client_handler.clone(),
        CmppDecoder,
        Some(ClientConfig::new()),
        None,
        None,
    )
    .await
    .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu_v2("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(!client_handler.is_connected(), "不支持的版本应该连接失败");
    assert_ne!(client_handler.get_connect_status(), Some(0), "应该返回错误状态");

    handle.abort();
}

#[tokio::test]
async fn test_submit_v20_after_connect() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let (port, handle) = start_test_server(auth.clone(), 30).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new_with_version(0x20));
    let conn = connect(
        endpoint,
        client_handler.clone(),
        CmppDecoder,
        Some(ClientConfig::new()),
        None,
        None,
    )
    .await
    .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu_v2("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client_handler.is_connected(), "连接失败");

    handle.abort();
}

#[tokio::test]
async fn test_submit_v30_after_connect() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let (port, handle) = start_test_server(auth.clone(), 30).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new_with_version(0x30));
    let conn = connect(
        endpoint,
        client_handler.clone(),
        CmppDecoder,
        Some(ClientConfig::new()),
        None,
        None,
    )
    .await
    .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu_v2("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client_handler.is_connected(), "连接失败");

    handle.abort();
}

#[tokio::test]
async fn test_decode_message_with_version_v20() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let mut submit = rsms_codec_cmpp::SubmitV20::new();
    submit.pk_total = 1;
    submit.pk_number = 1;
    submit.registered_delivery = 0;
    submit.msg_level = 1;
    submit.service_id = "SMS".to_string();
    submit.fee_user_type = 0;
    submit.fee_terminal_id = "13800138000".to_string();
    submit.tppid = 0;
    submit.tpudhi = 0;
    submit.msg_fmt = 15;
    submit.msg_src = "src".to_string();
    submit.fee_type = "01".to_string();
    submit.fee_code = "000000".to_string();
    submit.src_id = "10655000000".to_string();
    submit.dest_usr_tl = 1;
    submit.dest_terminal_ids = vec!["13800138000".to_string()];
    submit.msg_content = b"Test V2.0".to_vec();
    submit.reserve = [0u8; 8];

    let body_size = rsms_codec_cmpp::encoded_size_v20(&submit) - 8;
    let total_len = PduHeader::SIZE + body_size;
    let mut pdu = Vec::with_capacity(total_len);
    pdu.extend_from_slice(&(total_len as u32).to_be_bytes());
    pdu.extend_from_slice(&0x00000004u32.to_be_bytes());
    pdu.extend_from_slice(&12345u32.to_be_bytes());
    pdu.extend_from_slice(&submit.msg_id);
    pdu.push(submit.pk_total);
    pdu.push(submit.pk_number);
    pdu.push(submit.registered_delivery);
    pdu.push(submit.msg_level);
    
    let mut service_id_bytes = [0u8; 10];
    service_id_bytes[..submit.service_id.len()].copy_from_slice(submit.service_id.as_bytes());
    pdu.extend_from_slice(&service_id_bytes);
    
    pdu.push(submit.fee_user_type);
    
    let mut fee_terminal_bytes = [0u8; 21];
    fee_terminal_bytes[..submit.fee_terminal_id.len()].copy_from_slice(submit.fee_terminal_id.as_bytes());
    pdu.extend_from_slice(&fee_terminal_bytes);
    
    pdu.push(submit.tppid);
    pdu.push(submit.tpudhi);
    pdu.push(submit.msg_fmt);
    
    let mut msg_src_bytes = [0u8; 6];
    msg_src_bytes[..submit.msg_src.len()].copy_from_slice(submit.msg_src.as_bytes());
    pdu.extend_from_slice(&msg_src_bytes);
    
    let mut fee_type_bytes = [0u8; 2];
    fee_type_bytes[..submit.fee_type.len()].copy_from_slice(submit.fee_type.as_bytes());
    pdu.extend_from_slice(&fee_type_bytes);
    
    let mut fee_code_bytes = [0u8; 6];
    fee_code_bytes[..submit.fee_code.len()].copy_from_slice(submit.fee_code.as_bytes());
    pdu.extend_from_slice(&fee_code_bytes);
    
    pdu.extend_from_slice(&[0u8; 17]);
    pdu.extend_from_slice(&[0u8; 17]);
    
    let mut src_id_bytes = [0u8; 21];
    src_id_bytes[..submit.src_id.len()].copy_from_slice(submit.src_id.as_bytes());
    pdu.extend_from_slice(&src_id_bytes);
    
    pdu.push(submit.dest_usr_tl);
    
    for dest in &submit.dest_terminal_ids {
        let mut dest_bytes = [0u8; 21];
        dest_bytes[..dest.len()].copy_from_slice(dest.as_bytes());
        pdu.extend_from_slice(&dest_bytes);
    }
    
    pdu.push(submit.msg_content.len() as u8);
    pdu.extend_from_slice(&submit.msg_content);
    pdu.extend_from_slice(&submit.reserve);

    let msg = decode_message_with_version(&pdu, Some(0x20)).unwrap();
    match msg {
        CmppMessage::SubmitV20 { sequence_id, submit: s } => {
            assert_eq!(sequence_id, 12345);
            assert_eq!(s.msg_fmt, 15);
            assert_eq!(s.msg_content, b"Test V2.0");
            assert_eq!(s.src_id, "10655000000");
        }
        _ => panic!("expected SubmitV20 message"),
    }
}

#[tokio::test]
async fn test_decode_message_with_version_v30() {
    let submit = rsms_codec_cmpp::Submit::new();
    let pdu: Pdu = submit.into();
    let pdu_bytes: Vec<u8> = pdu.to_pdu_bytes(54321).to_vec();

    let msg = decode_message_with_version(&pdu_bytes, Some(0x30)).unwrap();
    match msg {
        CmppMessage::SubmitV30 { sequence_id, .. } => {
            assert_eq!(sequence_id, 54321);
        }
        _ => panic!("expected SubmitV30 message"),
    }
}

#[tokio::test]
async fn test_version_00_and_01_treated_as_v2() {
    for version in &[0x00u8, 0x01] {
        let mut submit = rsms_codec_cmpp::SubmitV20::new();
        submit.pk_total = 1;
        submit.pk_number = 1;
        submit.service_id = "SMS".to_string();
        submit.fee_terminal_id = "13800138000".to_string();
        submit.msg_src = "src".to_string();
        submit.fee_type = "01".to_string();
        submit.fee_code = "000000".to_string();
        submit.src_id = "10655000000".to_string();
        submit.dest_usr_tl = 1;
        submit.dest_terminal_ids = vec!["13800138000".to_string()];
        submit.msg_content = b"V2".to_vec();
        submit.reserve = [0u8; 8];

    let body_size = rsms_codec_cmpp::encoded_size_v20(&submit) - 8;
        let total_len = PduHeader::SIZE + body_size;
        let mut pdu = Vec::with_capacity(total_len);
        pdu.extend_from_slice(&(total_len as u32).to_be_bytes());
        pdu.extend_from_slice(&0x00000004u32.to_be_bytes());
        pdu.extend_from_slice(&1u32.to_be_bytes());
        pdu.extend_from_slice(&submit.msg_id);
        pdu.push(submit.pk_total);
        pdu.push(submit.pk_number);
        pdu.push(submit.registered_delivery);
        pdu.push(submit.msg_level);
        
        let mut service_id_bytes = [0u8; 10];
        service_id_bytes[..submit.service_id.len()].copy_from_slice(submit.service_id.as_bytes());
        pdu.extend_from_slice(&service_id_bytes);
        
        pdu.push(submit.fee_user_type);
        
        let mut fee_terminal_bytes = [0u8; 21];
        fee_terminal_bytes[..submit.fee_terminal_id.len()].copy_from_slice(submit.fee_terminal_id.as_bytes());
        pdu.extend_from_slice(&fee_terminal_bytes);
        
        pdu.push(submit.tppid);
        pdu.push(submit.tpudhi);
        pdu.push(submit.msg_fmt);
        
        let mut msg_src_bytes = [0u8; 6];
        msg_src_bytes[..submit.msg_src.len()].copy_from_slice(submit.msg_src.as_bytes());
        pdu.extend_from_slice(&msg_src_bytes);
        
        let mut fee_type_bytes = [0u8; 2];
        fee_type_bytes[..submit.fee_type.len()].copy_from_slice(submit.fee_type.as_bytes());
        pdu.extend_from_slice(&fee_type_bytes);
        
        let mut fee_code_bytes = [0u8; 6];
        fee_code_bytes[..submit.fee_code.len()].copy_from_slice(submit.fee_code.as_bytes());
        pdu.extend_from_slice(&fee_code_bytes);
        
        pdu.extend_from_slice(&[0u8; 17]);
        pdu.extend_from_slice(&[0u8; 17]);
        
        let mut src_id_bytes = [0u8; 21];
        src_id_bytes[..submit.src_id.len()].copy_from_slice(submit.src_id.as_bytes());
        pdu.extend_from_slice(&src_id_bytes);
        
        pdu.push(submit.dest_usr_tl);
        
        for dest in &submit.dest_terminal_ids {
            let mut dest_bytes = [0u8; 21];
            dest_bytes[..dest.len()].copy_from_slice(dest.as_bytes());
            pdu.extend_from_slice(&dest_bytes);
        }
        
        pdu.push(submit.msg_content.len() as u8);
        pdu.extend_from_slice(&submit.msg_content);
        pdu.extend_from_slice(&submit.reserve);

        let msg = decode_message_with_version(&pdu, Some(*version)).unwrap();
        match msg {
            CmppMessage::SubmitV20 { submit: s, .. } => {
                assert_eq!(s.dest_usr_tl, 1);
            }
            _ => panic!("expected Submit message for version {:02x}", version),
        }
    }
}