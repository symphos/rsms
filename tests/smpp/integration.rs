use rsms_connector::{serve, SmppDecoder};
use rsms_connector::client::ClientHandler;
use rsms_connector::{AuthHandler, AuthCredentials, AuthResult, ServerEventHandler, AccountConfigProvider};
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_core::{ConnectionInfo, RawPdu, EndpointConfig, Frame, Result};
use rsms_codec_smpp::Encodable;
use rsms_test_common::{TestEventHandler, TestClientEventHandler, MockAccountConfigProvider};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use tokio::time::Duration;
use std::collections::HashMap;

#[allow(dead_code)]
fn get_test_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static COUNTER: AtomicU16 = AtomicU16::new(0);
    50000 + COUNTER.fetch_add(1, Ordering::Relaxed)
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

    pub fn add_account(mut self, system_id: &str, password: &str) -> Self {
        self.accounts.insert(system_id.to_string(), password.to_string());
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
        "smpp-password-auth"
    }

    async fn authenticate(&self, _client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        self.auth_count.fetch_add(1, Ordering::Relaxed);

        if let AuthCredentials::Smpp { system_id, password, interface_version: _ } = credentials {
            if let Some(expected_password) = self.accounts.get(&system_id) {
                if *expected_password == password {
                    self.auth_success.fetch_add(1, Ordering::Relaxed);
                    return Ok(AuthResult::success(&system_id));
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
}

impl TestBusinessHandler {
    pub fn new() -> Self {
        Self {
            submit_count: Arc::new(AtomicUsize::new(0)),
            messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn submit_count(&self) -> usize {
        self.submit_count.load(Ordering::Relaxed)
    }

    pub fn get_messages(&self) -> Vec<String> {
        self.messages.lock().unwrap().clone()
    }
}

#[async_trait]
impl BusinessHandler for TestBusinessHandler {
    fn name(&self) -> &'static str {
        "smpp-test-biz"
    }

    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        if let Ok(msg) = rsms_codec_smpp::decode_message(frame.data_as_slice()) {
            match msg {
                rsms_codec_smpp::SmppMessage::SubmitSm(s) => {
                    self.submit_count.fetch_add(1, Ordering::Relaxed);
                    let content = String::from_utf8_lossy(&s.short_message).to_string();
                    self.messages.lock().unwrap().push(content);

                    let resp = rsms_codec_smpp::SubmitSmResp {
                        message_id: "000".to_string(),
                    };
                    let mut buf = bytes::BytesMut::new();
                    resp.encode(&mut buf).unwrap();
                    let body_len = buf.len() as u32;
                    let total_len = 16 + body_len;
                    let mut resp_pdu = Vec::new();
                    resp_pdu.extend_from_slice(&total_len.to_be_bytes());
                    resp_pdu.extend_from_slice(&(rsms_codec_smpp::CommandId::SUBMIT_SM_RESP as u32).to_be_bytes());
                    resp_pdu.extend_from_slice(&0u32.to_be_bytes());
                    resp_pdu.extend_from_slice(&frame.sequence_id.to_be_bytes());
                    resp_pdu.extend_from_slice(&buf);
                    ctx.conn.write_frame(resp_pdu.as_slice()).await?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

pub struct TestClientHandler {
    pub connected: AtomicBool,
    pub bind_resp_status: Mutex<Option<u32>>,
    pub submit_resp_status: Mutex<Option<u32>>,
    pub deliver_count: AtomicUsize,
    pub enquire_link_resp_count: AtomicUsize,
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
            enquire_link_resp_count: AtomicUsize::new(0),
            unbind_resp_received: AtomicBool::new(false),
            seq: AtomicUsize::new(1),
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u32
    }

    pub fn build_bind_transmitter_pdu(&self, system_id: &str, password: &str) -> RawPdu {
        let bind = rsms_codec_smpp::BindTransmitter::new(system_id, password, "CMT", 0x34);
        let pdu: rsms_codec_smpp::Pdu = bind.into();
        pdu.to_pdu_bytes(self.next_seq()).to_vec().into()
    }

    pub fn build_bind_transceiver_pdu(&self, system_id: &str, password: &str) -> RawPdu {
        let bind = rsms_codec_smpp::BindTransceiver::new(system_id, password, "CMT", 0x34);
        let pdu: rsms_codec_smpp::Pdu = bind.into();
        pdu.to_pdu_bytes(self.next_seq()).to_vec().into()
    }

    pub fn build_submit_sm_pdu(&self, src: &str, dest: &str, content: &str) -> RawPdu {
        let mut submit = rsms_codec_smpp::datatypes::SubmitSm::new();
        submit.source_addr_ton = 0;
        submit.source_addr_npi = 0;
        submit.source_addr = src.to_string();
        submit.dest_addr_ton = 1;
        submit.dest_addr_npi = 1;
        submit.destination_addr = dest.to_string();
        submit.esm_class = 0x04;
        submit.registered_delivery = 1;
        submit.data_coding = 0x03;
        submit.short_message = content.as_bytes().to_vec();

        let pdu: rsms_codec_smpp::Pdu = submit.into();
        pdu.to_pdu_bytes(self.next_seq()).to_vec().into()
    }

    pub fn build_enquire_link_pdu(&self) -> RawPdu {
        let enquire_link = rsms_codec_smpp::EnquireLink;
        let pdu: rsms_codec_smpp::Pdu = enquire_link.into();
        pdu.to_pdu_bytes(self.next_seq()).to_vec().into()
    }

    pub fn build_unbind_pdu(&self) -> RawPdu {
        let unbind = rsms_codec_smpp::Unbind;
        let pdu: rsms_codec_smpp::Pdu = unbind.into();
        pdu.to_pdu_bytes(self.next_seq()).to_vec().into()
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

    pub fn enquire_link_resp_count(&self) -> usize {
        self.enquire_link_resp_count.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl ClientHandler for TestClientHandler {
    fn name(&self) -> &'static str {
        "smpp-test-client"
    }

    async fn on_inbound(&self, ctx: &rsms_connector::client::ClientContext<'_>, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        if pdu.len() < 16 {
            return Ok(());
        }

        let cmd_id = frame.command_id;
        let status = u32::from_be_bytes([pdu[8], pdu[9], pdu[10], pdu[11]]);

        match cmd_id {
            x if x == rsms_codec_smpp::CommandId::BIND_TRANSMITTER_RESP as u32 => {
                *self.bind_resp_status.lock().unwrap() = Some(status);
                if status == 0 {
                    self.connected.store(true, Ordering::Relaxed);
                }
            }
            x if x == rsms_codec_smpp::CommandId::BIND_TRANSCEIVER_RESP as u32 => {
                *self.bind_resp_status.lock().unwrap() = Some(status);
                if status == 0 {
                    self.connected.store(true, Ordering::Relaxed);
                }
            }
            x if x == rsms_codec_smpp::CommandId::SUBMIT_SM_RESP as u32 => {
                *self.submit_resp_status.lock().unwrap() = Some(status);
            }
            x if x == rsms_codec_smpp::CommandId::DELIVER_SM as u32 => {
                self.deliver_count.fetch_add(1, Ordering::Relaxed);
                let resp = rsms_codec_smpp::DeliverSmResp {
                    message_id: "".to_string(),
                };
                let mut buf = bytes::BytesMut::new();
                resp.encode(&mut buf).unwrap();
                let body_len = buf.len() as u32;
                let total_len = 16 + body_len;
                let seq_id = u32::from_be_bytes([pdu[12], pdu[13], pdu[14], pdu[15]]);
                let mut resp_pdu = Vec::new();
                resp_pdu.extend_from_slice(&total_len.to_be_bytes());
                resp_pdu.extend_from_slice(&(rsms_codec_smpp::CommandId::DELIVER_SM_RESP as u32).to_be_bytes());
                resp_pdu.extend_from_slice(&0u32.to_be_bytes());
                resp_pdu.extend_from_slice(&seq_id.to_be_bytes());
                resp_pdu.extend_from_slice(&buf);
                ctx.conn.write_frame(resp_pdu.as_slice()).await?;
            }
            x if x == rsms_codec_smpp::CommandId::ENQUIRE_LINK_RESP as u32 => {
                self.enquire_link_resp_count.fetch_add(1, Ordering::Relaxed);
            }
            x if x == rsms_codec_smpp::CommandId::UNBIND_RESP as u32 => {
                self.unbind_resp_received.store(true, Ordering::Relaxed);
            }
            _ => {}
        }

        Ok(())
    }
}

pub async fn start_test_server(
    auth_handler: Arc<dyn AuthHandler>,
    biz_handler: Arc<dyn BusinessHandler>,
    event_handler: Arc<dyn ServerEventHandler>,
    idle_timeout_secs: u32,
) -> Result<(u16, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "smpp-test-server",
        "127.0.0.1",
        0,
        8,
        idle_timeout_secs as u16,
    ).with_protocol("smpp"));
    let server = serve(
        cfg,
        vec![biz_handler],
        Some(auth_handler),
        None,
        Some(Arc::new(MockAccountConfigProvider::new()) as Arc<dyn AccountConfigProvider>),
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
        "smpp-test-server",
        "127.0.0.1",
        0,
        8,
        idle_timeout_secs as u16,
    ).with_protocol("smpp"));
    let server = serve(
        cfg,
        vec![biz_handler],
        Some(auth_handler),
        None,
        Some(Arc::new(MockAccountConfigProvider::new()) as Arc<dyn AccountConfigProvider>),
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

pub fn build_deliver_sm(seq_id: u32, src: &str, dest: &str, content: &str) -> RawPdu {
    let deliver = rsms_codec_smpp::DeliverSm {
        service_type: String::new(),
        source_addr_ton: 1,
        source_addr_npi: 1,
        source_addr: src.to_string(),
        dest_addr_ton: 0,
        dest_addr_npi: 0,
        destination_addr: dest.to_string(),
        esm_class: 0x04,
        protocol_id: 0,
        priority_flag: 0,
        schedule_delivery_time: String::new(),
        validity_period: String::new(),
        registered_delivery: 1,
        replace_if_present_flag: 0,
        data_coding: 0x03,
        sm_default_msg_id: 0,
        short_message: content.as_bytes().to_vec(),
        tlvs: Vec::new(),
    };

    let mut buf = bytes::BytesMut::new();
    deliver.encode(&mut buf).unwrap();

    let body_len = buf.len() as u32;
    let total_len = 16 + body_len;

    let mut pdu = Vec::new();
    pdu.extend_from_slice(&total_len.to_be_bytes());
    pdu.extend_from_slice(&(rsms_codec_smpp::CommandId::DELIVER_SM as u32).to_be_bytes());
    pdu.extend_from_slice(&0u32.to_be_bytes());
    pdu.extend_from_slice(&seq_id.to_be_bytes());
    pdu.extend_from_slice(&buf);

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
    async fn test_bind_transmitter_valid() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let system_id = "smppcli";
        let password = "pwd12345";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(system_id, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smpp-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SmppDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_transmitter_pdu(system_id, password);
        conn.write_frame(bind_pdu.as_slice()).await.expect("send bind");

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
    async fn test_bind_transceiver_valid() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let system_id = "smppcli";
        let password = "pwd12345";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(system_id, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smpp-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SmppDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_transceiver_pdu(system_id, password);
        conn.write_frame(bind_pdu.as_slice()).await.expect("send bind");

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
    async fn test_bind_with_invalid_password() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let system_id = "smppcli";
        let correct_password = "pwd12345";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(system_id, correct_password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smpp-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SmppDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_transmitter_pdu(system_id, "wrongpwd");
        conn.write_frame(bind_pdu.as_slice()).await.expect("send bind");

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
    async fn test_bind_with_unknown_system_id() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let auth = Arc::new(PasswordAuthHandler::new());
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smpp-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SmppDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_transmitter_pdu("unknown", "password");
        conn.write_frame(bind_pdu.as_slice()).await.expect("send bind");

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
    async fn test_submit_sm() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let system_id = "smppcli";
        let password = "pwd12345";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(system_id, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smpp-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SmppDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_transmitter_pdu(system_id, password);
        conn.write_frame(bind_pdu.as_slice()).await.expect("send bind");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        let submit_pdu = client_handler.build_submit_sm_pdu("13800138000", "13800138001", "Hello SMS");
        conn.write_frame(submit_pdu.as_slice()).await.expect("send submit");

        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(client_handler.get_submit_status(), Some(0), "提交成功状态码应为0");
        assert_eq!(biz.submit_count(), 1, "服务器应该收到1条Submit");
        assert_eq!(biz.get_messages(), vec!["Hello SMS"], "服务器应该收到正确内容");

        handle.abort();
    }

    #[tokio::test]
    async fn test_deliver_sm() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let system_id = "smppcli";
        let password = "pwd12345";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(system_id, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, pool, handle) = start_test_server_with_pool(auth.clone(), biz.clone(), evt.clone(), 30)
            .await
            .unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smpp-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SmppDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_transceiver_pdu(system_id, password);
        conn.write_frame(bind_pdu.as_slice()).await.expect("send bind");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        tokio::time::sleep(Duration::from_millis(100)).await;

        if let Some(server_conn) = get_conn_from_pool(&pool).await {
            let deliver_pdu = build_deliver_sm(100, "13800138000", "13800138001", "Hello MO SMS");
            server_conn.write_frame(deliver_pdu.as_slice()).await.expect("send deliver");
        }

        tokio::time::sleep(Duration::from_millis(300)).await;

        assert_eq!(client_handler.deliver_count(), 1, "应该收到1个DeliverSm");

        handle.abort();
    }

    #[tokio::test]
    async fn test_enquire_link() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let system_id = "smppcli";
        let password = "pwd12345";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(system_id, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smpp-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SmppDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_transmitter_pdu(system_id, password);
        conn.write_frame(bind_pdu.as_slice()).await.expect("send bind");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        let enquire_link_pdu = client_handler.build_enquire_link_pdu();
        conn.write_frame(enquire_link_pdu.as_slice()).await.expect("send enquire link");

        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(client_handler.enquire_link_resp_count(), 1, "应该收到EnquireLinkResp");

        handle.abort();
    }

    #[tokio::test]
    async fn test_unbind() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let system_id = "smppcli";
        let password = "pwd12345";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(system_id, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smpp-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SmppDecoder,
            Some(ClientConfig::new()),
            None,
            None,
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_transmitter_pdu(system_id, password);
        conn.write_frame(bind_pdu.as_slice()).await.expect("send bind");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        let unbind_pdu = client_handler.build_unbind_pdu();
        conn.write_frame(unbind_pdu.as_slice()).await.expect("send unbind");

        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(client_handler.unbind_resp_received.load(Ordering::Relaxed), "应该收到UnbindResp");

        handle.abort();
    }

    #[tokio::test]
    async fn test_no_heartbeat_disconnect() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let system_id = "smppcli";
        let password = "pwd12345";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(system_id, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let client_evt = Arc::new(TestClientEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 2).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smpp-client", "127.0.0.1", port, 8, 30));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = connect(
            endpoint,
            client_handler.clone(),
            SmppDecoder,
            Some(ClientConfig::new()),
            None,
            Some(client_evt.clone()),
        )
        .await
        .expect("connect");

        let bind_pdu = client_handler.build_bind_transmitter_pdu(system_id, password);
        conn.write_frame(bind_pdu.as_slice()).await.expect("send bind");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        tokio::time::sleep(Duration::from_secs(3)).await;

        assert_eq!(client_evt.disconnected_count(), 1, "无心跳时应该触发断开事件");

        handle.abort();
    }
}
