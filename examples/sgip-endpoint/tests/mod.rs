use rsms_connector::{serve, SgipDecoder};
use rsms_connector::client::ClientHandler;
use rsms_connector::{AuthHandler, AuthCredentials, AuthResult, ServerEventHandler, ClientEventHandler, MessageSource, MessageItem, AccountConfigProvider, ProtocolConnection, AccountConfig};
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_core::{ConnectionInfo, EncodedPdu, RawPdu, EndpointConfig, Frame, Result};
use rsms_codec_sgip::Encodable;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use tokio::time::Duration;
use std::collections::HashMap;

#[allow(dead_code)]
fn get_test_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static COUNTER: AtomicU16 = AtomicU16::new(0);
    30000 + COUNTER.fetch_add(1, Ordering::Relaxed)
}

pub struct PasswordAuthHandler {
    accounts: HashMap<String, String>,
    auth_count: Arc<AtomicUsize>,
    auth_success: Arc<AtomicUsize>,
    auth_fail: Arc<AtomicUsize>,
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

    pub fn add_account(mut self, login_name: &str, password: &str) -> Self {
        self.accounts.insert(login_name.to_string(), password.to_string());
        self
    }

    pub fn auth_count(&self) -> usize {
        self.auth_count.load(Ordering::Relaxed)
    }

    pub fn auth_success_count(&self) -> usize {
        self.auth_success.load(Ordering::Relaxed)
    }

    pub fn auth_fail_count(&self) -> usize {
        self.auth_fail.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl AuthHandler for PasswordAuthHandler {
    fn name(&self) -> &'static str {
        "sgip-password-auth"
    }

    async fn authenticate(&self, _client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        self.auth_count.fetch_add(1, Ordering::Relaxed);

        if let AuthCredentials::Sgip { login_name, login_password } = credentials {
            if let Some(expected_password) = self.accounts.get(&login_name) {
                if *expected_password == login_password {
                    self.auth_success.fetch_add(1, Ordering::Relaxed);
                    return Ok(AuthResult::success(&login_name));
                }
            }
            self.auth_fail.fetch_add(1, Ordering::Relaxed);
            Ok(AuthResult::failure(1, "Invalid password"))
        } else {
            self.auth_fail.fetch_add(1, Ordering::Relaxed);
            Ok(AuthResult::failure(1, "Invalid credentials"))
        }
    }
}

pub struct TestBusinessHandler {
    pub submit_count: Arc<AtomicUsize>,
    pub messages: Arc<Mutex<Vec<String>>>,
    pub mo_messages: Arc<Mutex<Vec<(String, String)>>>,
    pub reports: Arc<Mutex<Vec<String>>>,
}

impl TestBusinessHandler {
    pub fn new() -> Self {
        Self {
            submit_count: Arc::new(AtomicUsize::new(0)),
            messages: Arc::new(Mutex::new(Vec::new())),
            mo_messages: Arc::new(Mutex::new(Vec::new())),
            reports: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn submit_count(&self) -> usize {
        self.submit_count.load(Ordering::Relaxed)
    }

    pub fn get_messages(&self) -> Vec<String> {
        self.messages.lock().unwrap().clone()
    }

    pub fn get_mo_messages(&self) -> Vec<(String, String)> {
        self.mo_messages.lock().unwrap().clone()
    }

    pub fn get_reports(&self) -> Vec<String> {
        self.reports.lock().unwrap().clone()
    }
}

#[async_trait]
impl BusinessHandler for TestBusinessHandler {
    fn name(&self) -> &'static str {
        "sgip-test-biz"
    }

    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        if let Ok(msg) = rsms_codec_sgip::decode_message(frame.data_as_slice()) {
            match msg {
                rsms_codec_sgip::SgipMessage::Submit(s) => {
                    self.submit_count.fetch_add(1, Ordering::Relaxed);
                    let content = String::from_utf8_lossy(&s.message_content).to_string();
                    self.messages.lock().unwrap().push(content);

                    let resp = rsms_codec_sgip::SubmitResp { result: 0 };
                    let body_bytes = {
                        let mut buf = bytes::BytesMut::new();
                        resp.encode(&mut buf).unwrap();
                        buf.to_vec()
                    };
                    let total_len = (20 + body_bytes.len()) as u32;
                    let pdu_bytes = frame.data_as_slice();
                    let mut resp_pdu = Vec::new();
                    resp_pdu.extend_from_slice(&total_len.to_be_bytes());
                    resp_pdu.extend_from_slice(&(rsms_codec_sgip::CommandId::SubmitResp as u32).to_be_bytes());
                    if pdu_bytes.len() >= 20 {
                        resp_pdu.extend_from_slice(&pdu_bytes[8..20]);
                    } else {
                        resp_pdu.extend_from_slice(&1u32.to_be_bytes());
                        resp_pdu.extend_from_slice(&0x04051200u32.to_be_bytes());
                        resp_pdu.extend_from_slice(&1u32.to_be_bytes());
                    }
                    resp_pdu.extend(body_bytes);
                    ctx.conn.write_frame(resp_pdu.as_slice()).await?;
                }
                rsms_codec_sgip::SgipMessage::Deliver(d) => {
                    if d.message_content.len() > 20 && String::from_utf8_lossy(&d.message_content).contains("id:") {
                        self.reports.lock().unwrap().push(String::from_utf8_lossy(&d.message_content).to_string());
                    } else {
                        self.mo_messages.lock().unwrap().push((
                            d.user_number.clone(),
                            String::from_utf8_lossy(&d.message_content).to_string(),
                        ));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

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

pub struct TestClientHandler {
    pub connected: AtomicBool,
    pub bind_resp_status: Mutex<Option<u32>>,
    pub submit_resp_status: Mutex<Option<u32>>,
    pub deliver_count: AtomicUsize,
    pub report_count: AtomicUsize,
    pub unbind_resp_received: AtomicBool,
    pub seq: AtomicUsize,
}

impl TestClientHandler {
    pub fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            bind_resp_status: Mutex::new(None),
            submit_resp_status: Mutex::new(None),
            deliver_count: AtomicUsize::new(0),
            report_count: AtomicUsize::new(0),
            unbind_resp_received: AtomicBool::new(false),
            seq: AtomicUsize::new(1),
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u32
    }

    pub fn build_bind_pdu(&self, login_name: &str, login_password: &str) -> RawPdu {
        let bind = rsms_codec_sgip::Bind {
            login_type: 1,
            login_name: login_name.to_string(),
            login_password: login_password.to_string(),
            reserve: [0u8; 8],
        };

        let body_bytes = {
            let mut buf = bytes::BytesMut::new();
            bind.encode(&mut buf).unwrap();
            buf.to_vec()
        };

        let total_len = (20 + body_bytes.len()) as u32;
        let seq_num = self.next_seq();

        let mut pdu = Vec::new();
        pdu.extend_from_slice(&total_len.to_be_bytes());
        pdu.extend_from_slice(&(rsms_codec_sgip::CommandId::Bind as u32).to_be_bytes());
        pdu.extend_from_slice(&1u32.to_be_bytes());
        pdu.extend_from_slice(&0x04051200u32.to_be_bytes());
        pdu.extend_from_slice(&seq_num.to_be_bytes());
        pdu.extend(body_bytes);

        pdu.into()
    }

    pub fn build_submit_pdu(&self, sp_number: &str, dest_number: &str, content: &str) -> RawPdu {
        let mut submit = rsms_codec_sgip::Submit::new();
        submit.sp_number = sp_number.to_string();
        submit.charge_number = sp_number.to_string();
        submit.user_count = 1;
        submit.user_numbers = vec![dest_number.to_string()];
        submit.corp_id = "".to_string();
        submit.service_type = "SMS".to_string();
        submit.fee_type = 2;
        submit.fee_value = "000000".to_string();
        submit.given_value = "000000".to_string();
        submit.agent_flag = 0;
        submit.morelate_to_mt_flag = 0;
        submit.priority = 0;
        submit.expire_time = "".to_string();
        submit.schedule_time = "".to_string();
        submit.report_flag = 1;
        submit.tppid = 0;
        submit.tpudhi = 0;
        submit.msg_fmt = 15;
        submit.message_type = 0;
        submit.message_content = content.as_bytes().to_vec();
        submit.reserve = [0u8; 8];

        let body_bytes = {
            let mut buf = bytes::BytesMut::new();
            submit.encode(&mut buf).unwrap();
            buf.to_vec()
        };

        let total_len = (20 + body_bytes.len()) as u32;
        let seq_num = self.next_seq();

        let mut pdu = Vec::new();
        pdu.extend_from_slice(&total_len.to_be_bytes());
        pdu.extend_from_slice(&(rsms_codec_sgip::CommandId::Submit as u32).to_be_bytes());
        pdu.extend_from_slice(&1u32.to_be_bytes());
        pdu.extend_from_slice(&0x04051200u32.to_be_bytes());
        pdu.extend_from_slice(&seq_num.to_be_bytes());
        pdu.extend(body_bytes);

        pdu.into()
    }

    pub fn build_unbind_pdu(&self) -> RawPdu {
        let unbind = rsms_codec_sgip::Unbind;

        let body_bytes = {
            let mut buf = bytes::BytesMut::new();
            unbind.encode(&mut buf).unwrap();
            buf.to_vec()
        };

        let total_len = (20 + body_bytes.len()) as u32;
        let seq_num = self.next_seq();

        let mut pdu = Vec::new();
        pdu.extend_from_slice(&total_len.to_be_bytes());
        pdu.extend_from_slice(&(rsms_codec_sgip::CommandId::Unbind as u32).to_be_bytes());
        pdu.extend_from_slice(&1u32.to_be_bytes());
        pdu.extend_from_slice(&0x04051200u32.to_be_bytes());
        pdu.extend_from_slice(&seq_num.to_be_bytes());
        pdu.extend(body_bytes);

        pdu.into()
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn get_bind_status(&self) -> Option<u32> {
        self.bind_resp_status.lock().unwrap().clone()
    }

    pub fn get_submit_status(&self) -> Option<u32> {
        self.submit_resp_status.lock().unwrap().clone()
    }

    pub fn deliver_count(&self) -> usize {
        self.deliver_count.load(Ordering::Relaxed)
    }

    pub fn report_count(&self) -> usize {
        self.report_count.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl ClientHandler for TestClientHandler {
    fn name(&self) -> &'static str {
        "sgip-test-client"
    }

    async fn on_inbound(&self, ctx: &rsms_connector::client::ClientContext<'_>, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        if pdu.len() < 20 {
            return Ok(());
        }

        let cmd_id = frame.command_id;

        match cmd_id {
            x if x == rsms_codec_sgip::CommandId::BindResp as u32 => {
                let result = u32::from_be_bytes([pdu[20], pdu[21], pdu[22], pdu[23]]);
                *self.bind_resp_status.lock().unwrap() = Some(result);
                if result == 0 {
                    self.connected.store(true, Ordering::Relaxed);
                }
            }
            x if x == rsms_codec_sgip::CommandId::SubmitResp as u32 => {
                let result = u32::from_be_bytes([pdu[20], pdu[21], pdu[22], pdu[23]]);
                *self.submit_resp_status.lock().unwrap() = Some(result);
            }
            x if x == rsms_codec_sgip::CommandId::Deliver as u32 => {
                self.deliver_count.fetch_add(1, Ordering::Relaxed);
                if let Ok(msg) = rsms_codec_sgip::decode_message(pdu) {
                    if let rsms_codec_sgip::SgipMessage::Deliver(d) = msg {
                        let content = String::from_utf8_lossy(&d.message_content);
                        if content.contains("id:") {
                            self.report_count.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                let resp = rsms_codec_sgip::DeliverResp { result: 0 };
                let body_bytes = {
                    let mut buf = bytes::BytesMut::new();
                    resp.encode(&mut buf).unwrap();
                    buf.to_vec()
                };
                let total_len = (20 + body_bytes.len()) as u32;
                let mut resp_pdu = Vec::new();
                resp_pdu.extend_from_slice(&total_len.to_be_bytes());
                resp_pdu.extend_from_slice(&(rsms_codec_sgip::CommandId::DeliverResp as u32).to_be_bytes());
                resp_pdu.extend_from_slice(&1u32.to_be_bytes());
                resp_pdu.extend_from_slice(&0x04051200u32.to_be_bytes());
                resp_pdu.extend_from_slice(&frame.sequence_id.to_be_bytes());
                resp_pdu.extend(body_bytes);
                ctx.conn.write_frame(resp_pdu.as_slice()).await?;
            }
            x if x == rsms_codec_sgip::CommandId::UnbindResp as u32 => {
                self.unbind_resp_received.store(true, Ordering::Relaxed);
            }
            _ => {}
        }

        Ok(())
    }
}

pub struct TestClientEventHandler {
    pub disconnected_count: AtomicUsize,
}

impl TestClientEventHandler {
    pub fn new() -> Self {
        Self {
            disconnected_count: AtomicUsize::new(0),
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

pub async fn start_test_server(
    auth_handler: Arc<dyn AuthHandler>,
    biz_handler: Arc<dyn BusinessHandler>,
    event_handler: Arc<dyn ServerEventHandler>,
    idle_timeout_secs: u32,
) -> Result<(u16, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "sgip-test-server",
        "127.0.0.1",
        0,
        8,
        idle_timeout_secs as u16,
    ).with_protocol("sgip"));
    let server = serve(
        cfg,
        vec![biz_handler],
        Some(auth_handler),
        None,
        Some(Arc::new(MockAccountConfigProvider) as Arc<dyn AccountConfigProvider>),
        Some(event_handler),
        None,
    )
    .await
    .expect("bind");
    let port = server.local_addr.port();
    let handle = tokio::spawn(async move {
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    Ok((port, handle))
}

pub async fn start_test_server_with_pool(
    auth_handler: Arc<dyn AuthHandler>,
    biz_handler: Arc<dyn BusinessHandler>,
    event_handler: Arc<dyn ServerEventHandler>,
    idle_timeout_secs: u32,
) -> Result<(u16, Arc<rsms_connector::ConnectionPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "sgip-test-server",
        "127.0.0.1",
        0,
        8,
        idle_timeout_secs as u16,
    ).with_protocol("sgip"));
    let server = serve(
        cfg,
        vec![biz_handler],
        Some(auth_handler),
        None,
        Some(Arc::new(MockAccountConfigProvider) as Arc<dyn AccountConfigProvider>),
        Some(event_handler),
        None,
    )
    .await
    .expect("bind");
    let port = server.local_addr.port();
    let pool = server.pool();
    let pool_clone = pool.clone();
    let handle = tokio::spawn(async move {
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    Ok((port, pool_clone, handle))
}

pub fn build_deliver_mo(seq_id: u32, user_number: &str, sp_number: &str, content: &str) -> RawPdu {
    let mut deliver = rsms_codec_sgip::Deliver::new();
    deliver.user_number = user_number.to_string();
    deliver.sp_number = sp_number.to_string();
    deliver.tppid = 0;
    deliver.tpudhi = 0;
    deliver.msg_fmt = 15;
    deliver.message_content = content.as_bytes().to_vec();
    deliver.reserve = [0u8; 8];

    let body_bytes = {
        let mut buf = bytes::BytesMut::new();
        deliver.encode(&mut buf).unwrap();
        buf.to_vec()
    };

    let total_len = (20 + body_bytes.len()) as u32;

    let mut pdu = Vec::new();
    pdu.extend_from_slice(&total_len.to_be_bytes());
    pdu.extend_from_slice(&(rsms_codec_sgip::CommandId::Deliver as u32).to_be_bytes());
    pdu.extend_from_slice(&1u32.to_be_bytes());
    pdu.extend_from_slice(&0x04051200u32.to_be_bytes());
    pdu.extend_from_slice(&seq_id.to_be_bytes());
    pdu.extend(body_bytes);

    pdu.into()
}

pub fn build_deliver_report(seq_id: u32, _submit_seq: &rsms_codec_sgip::SgipSequence, dest_id: &str) -> RawPdu {
    let report_content = format!(
        "id:{:02x?}{:02x?}{:02x?}{:02x?}{:02x?}{:02x?}{:02x?}{:02x?} sub:001 dlvrd:001 submit date:26010100 done date:26010100 stat:DELIVRD err:000",
        0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 1u8
    );

    let mut deliver = rsms_codec_sgip::Deliver::new();
    deliver.user_number = "".to_string();
    deliver.sp_number = dest_id.to_string();
    deliver.tppid = 0;
    deliver.tpudhi = 0;
    deliver.msg_fmt = 15;
    deliver.message_content = report_content.as_bytes().to_vec();
    deliver.reserve = [0u8; 8];

    let body_bytes = {
        let mut buf = bytes::BytesMut::new();
        deliver.encode(&mut buf).unwrap();
        buf.to_vec()
    };

    let total_len = (20 + body_bytes.len()) as u32;

    let mut pdu = Vec::new();
    pdu.extend_from_slice(&total_len.to_be_bytes());
    pdu.extend_from_slice(&(rsms_codec_sgip::CommandId::Deliver as u32).to_be_bytes());
    pdu.extend_from_slice(&1u32.to_be_bytes());
    pdu.extend_from_slice(&0x04051200u32.to_be_bytes());
    pdu.extend_from_slice(&seq_id.to_be_bytes());
    pdu.extend(body_bytes);

    pdu.into()
}

async fn get_conn_from_pool(pool: &Arc<rsms_connector::ConnectionPool>) -> Option<Arc<rsms_connector::Connection>> {
    pool.first().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsms_connector::connect;
    use rsms_connector::client::ClientConfig;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn test_bind_with_valid_login() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let account = "106900";
        let password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(account, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("sgip-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SgipDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_pdu(account, password);
        conn.write_frame(bind_pdu.as_bytes()).await.expect("send bind");

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if client_handler.is_connected() {
                break;
            }
        }

        assert_eq!(client_handler.get_bind_status(), Some(0), "认证成功状态码应为0");
        assert!(client_handler.is_connected(), "应该已连接");
        assert_eq!(auth.auth_success_count(), 1, "应该认证成功1次");

        handle.abort();
    }

    #[tokio::test]
    async fn test_bind_with_wrong_password() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let account = "106900";
        let correct_password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(account, correct_password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("sgip-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SgipDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_pdu(account, "wrongpassword");
        conn.write_frame(bind_pdu.as_bytes()).await.expect("send bind");

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if client_handler.get_bind_status().is_some() {
                break;
            }
        }

        assert!(client_handler.get_bind_status().is_some(), "应该收到响应");
        assert!(client_handler.get_bind_status() != Some(0), "认证失败状态码不应为0");
        assert!(!client_handler.is_connected(), "不应该已连接");
        assert_eq!(auth.auth_fail_count(), 1, "应该认证失败1次");

        handle.abort();
    }

    #[tokio::test]
    async fn test_bind_with_unknown_login() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let auth = Arc::new(PasswordAuthHandler::new());
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("sgip-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SgipDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_pdu("unknown", "password");
        conn.write_frame(bind_pdu.as_bytes()).await.expect("send bind");

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if client_handler.get_bind_status().is_some() {
                break;
            }
        }

        assert!(client_handler.get_bind_status().is_some(), "应该收到响应");
        assert!(client_handler.get_bind_status() != Some(0), "未知账号认证失败状态码不应为0");

        handle.abort();
    }

    #[tokio::test]
    async fn test_submit_message() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let account = "106900";
        let password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(account, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("sgip-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SgipDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_pdu(account, password);
        conn.write_frame(bind_pdu.as_bytes()).await.expect("send bind");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        let submit_pdu = client_handler.build_submit_pdu(account, "13800138000", "Hello SMS");
        conn.write_frame(submit_pdu.as_bytes()).await.expect("send submit");

        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(client_handler.get_submit_status(), Some(0), "提交成功状态码应为0");
        assert_eq!(biz.submit_count(), 1, "服务器应该收到1条Submit");
        assert_eq!(biz.get_messages(), vec!["Hello SMS"], "服务器应该收到正确内容");

        handle.abort();
    }

    #[tokio::test]
    async fn test_unbind() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let account = "106900";
        let password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(account, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("sgip-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SgipDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_pdu(account, password);
        conn.write_frame(bind_pdu.as_bytes()).await.expect("send bind");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        let unbind_pdu = client_handler.build_unbind_pdu();
        conn.write_frame(unbind_pdu.as_bytes()).await.expect("send unbind");

        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(client_handler.unbind_resp_received.load(Ordering::Relaxed), "应该收到UnbindResp");

        handle.abort();
    }

    #[tokio::test]
    async fn test_no_heartbeat_disconnect() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let account = "106900";
        let password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(account, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let client_evt = Arc::new(TestClientEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 2).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("sgip-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SgipDecoder,
            Some(ClientConfig::new()),
            None,
            Some(client_evt.clone()),
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_pdu(account, password);
        conn.write_frame(bind_pdu.as_bytes()).await.expect("send bind");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        tokio::time::sleep(Duration::from_secs(3)).await;

        assert_eq!(client_evt.disconnected_count(), 1, "无心跳时应该触发断开事件");

        handle.abort();
    }

    #[tokio::test]
    async fn test_deliver_mo() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let account = "106900";
        let password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(account, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, pool, handle) = start_test_server_with_pool(auth.clone(), biz.clone(), evt.clone(), 30)
            .await
            .unwrap();

        let endpoint = Arc::new(EndpointConfig::new("sgip-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SgipDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_pdu(account, password);
        conn.write_frame(bind_pdu.as_bytes()).await.expect("send bind");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        tokio::time::sleep(Duration::from_millis(100)).await;

        if let Some(server_conn) = get_conn_from_pool(&pool).await {
            let deliver_pdu = build_deliver_mo(100, "13800138000", account, "Hello MO SMS");
            server_conn.write_frame(deliver_pdu.as_bytes()).await.expect("send deliver");
        }

        tokio::time::sleep(Duration::from_millis(300)).await;

        assert_eq!(client_handler.deliver_count(), 1, "应该收到1个Deliver");
        assert_eq!(client_handler.report_count(), 0, "不应该收到状态报告");

        handle.abort();
    }

    #[tokio::test]
    async fn test_deliver_report() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let account = "106900";
        let password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(account, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, pool, handle) = start_test_server_with_pool(auth.clone(), biz.clone(), evt.clone(), 30)
            .await
            .unwrap();

        let endpoint = Arc::new(EndpointConfig::new("sgip-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SgipDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_pdu(account, password);
        conn.write_frame(bind_pdu.as_bytes()).await.expect("send bind");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        tokio::time::sleep(Duration::from_millis(100)).await;

        let submit_seq = rsms_codec_sgip::SgipSequence::new(1, 0x04051200, 1);
        if let Some(server_conn) = get_conn_from_pool(&pool).await {
            let deliver_pdu = build_deliver_report(101, &submit_seq, account);
            server_conn.write_frame(deliver_pdu.as_bytes()).await.expect("send deliver report");
        }

        tokio::time::sleep(Duration::from_millis(300)).await;

        assert_eq!(client_handler.deliver_count(), 1, "应该收到1个Deliver");
        assert_eq!(client_handler.report_count(), 1, "应该收到1个状态报告");

        handle.abort();
    }
}
