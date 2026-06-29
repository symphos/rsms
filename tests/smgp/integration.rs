use rsms_connector::{ServerBuilder, SmgpDecoder};
use rsms_connector::{AuthHandler, AuthCredentials, AuthResult, ServerEventHandler, AccountConfigProvider};
use rsms_business::{MessageContext, MessageHandler};
use rsms_core::{ConnectionInfo, RawPdu, EndpointConfig, Protocol, Result};
// 窄腰统一模型：业务/对端代码不再直接用裸 codec PDU，统一走 SmgpAdapter + UnifiedMessage。
use rsms_codec_smgp::adapter::SmgpAdapter;
use rsms_codec_smgp::datatypes::SmgpReport;
use rsms_model::{
    Address, BindMode, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra,
    Sequence, UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit,
    UnifiedSubmitResp,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use tokio::time::Duration;
use std::collections::HashMap;
use rsms_test_common::{TestEventHandler, TestClientEventHandler, TestMessageSource, MockAccountConfigProvider};

#[allow(dead_code)]
fn get_test_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static COUNTER: AtomicU16 = AtomicU16::new(0);
    40000 + COUNTER.fetch_add(1, Ordering::Relaxed)
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

    pub fn add_account(mut self, client_id: &str, password: &str) -> Self {
        self.accounts.insert(client_id.to_string(), password.to_string());
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
        "smgp-password-auth"
    }

    async fn authenticate(&self, _client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        self.auth_count.fetch_add(1, Ordering::Relaxed);

        if let AuthCredentials::Smgp { client_id: cid, authenticator, version: _ } = credentials {
            if let Some(password) = self.accounts.get(&cid) {
                let expected_auth = rsms_codec_smgp::compute_login_auth(&cid, password, 0);
                if expected_auth == authenticator {
                    self.auth_success.fetch_add(1, Ordering::Relaxed);
                    return Ok(AuthResult::success(&cid));
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
impl MessageHandler for TestBusinessHandler {
    fn name(&self) -> &'static str {
        "smgp-test-biz"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        // 框架已按协议解码为统一消息，直接 match 统一枚举。
        match msg {
            UnifiedMessage::Submit(s) => {
                self.submit_count.fetch_add(1, Ordering::Relaxed);
                let content = String::from_utf8_lossy(&s.content).to_string();
                self.messages.lock().unwrap().push(content);

                // 一步回执（窄腰）：框架按请求帧序列编码 SubmitResp 并写回。
                ctx.reply(UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                    msg_id: MessageId::Binary(rsms_codec_smgp::SmgpMsgId::from_u64(1).bytes.to_vec()),
                    status: 0,
                })).await?;
            }
            // is_report=1 经 adapter 翻译为 Report，is_report=0 翻译为 Deliver。
            UnifiedMessage::Report(r) => {
                self.reports.lock().unwrap().push(String::from_utf8_lossy(&r.raw).to_string());
            }
            UnifiedMessage::Deliver(d) => {
                self.mo_messages.lock().unwrap().push((
                    d.src.number.clone(),
                    String::from_utf8_lossy(&d.content).to_string(),
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

pub struct TestClientHandler {
    pub connected: AtomicBool,
    pub login_resp_status: Mutex<Option<u32>>,
    pub submit_resp_status: Mutex<Option<u32>>,
    pub deliver_count: AtomicUsize,
    pub active_test_resp_count: AtomicUsize,
    pub exit_resp_received: AtomicBool,
    pub seq: AtomicUsize,
}

impl TestClientHandler {
    pub fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            login_resp_status: Mutex::new(None),
            submit_resp_status: Mutex::new(None),
            deliver_count: AtomicUsize::new(0),
            active_test_resp_count: AtomicUsize::new(0),
            exit_resp_received: AtomicBool::new(false),
            seq: AtomicUsize::new(1),
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u32
    }

    pub fn build_login_pdu(&self, client_id: &str, password: &str) -> RawPdu {
        let timestamp = 0u32;
        // compute_login_auth 保留：鉴权 MD5 加密非 codec 范畴，把算好的 16B 摘要塞进 UnifiedBind。
        let authenticator = rsms_codec_smgp::compute_login_auth(client_id, password, timestamp).to_vec();

        let bind = UnifiedMessage::Bind(UnifiedBind {
            client_id: client_id.to_string(),
            authenticator,
            timestamp,
            version: 0x30,
            system_type: None,
            mode: BindMode::default(),
            login_mode: Some(0),
        });
        let bytes = SmgpAdapter.encode(&bind, Sequence::Plain(self.next_seq())).expect("encode login");
        RawPdu::from(bytes)
    }

    pub fn build_submit_pdu(&self, src: &str, dest: &str, content: &str) -> RawPdu {
        // 原 msg_fmt=15(GBK)，对应 Encoding::Gbk；service_id/fee 等方言字段走 ProtocolExtra::Smgp。
        let submit = UnifiedMessage::Submit(UnifiedSubmit {
            src: Address::plain(src),
            dests: vec![Address::plain(dest)],
            content: content.as_bytes().to_vec(),
            encoding: Encoding::Gbk,
            want_report: true,
            concat: None,
            extra: ProtocolExtra::Smgp(rsms_model::SmgpExtra {
                service_id: "SMS".to_string(),
                fee_type: "02".to_string(),
                fee_code: "000000".to_string(),
                fixed_fee: "000000".to_string(),
                charge_term_id: dest.to_string(),
                ..Default::default()
            }),
            tlvs: vec![],
        });
        let bytes = SmgpAdapter.encode(&submit, Sequence::Plain(1001)).expect("encode submit");
        RawPdu::from(bytes)
    }

    pub fn build_active_test_pdu(&self) -> RawPdu {
        let bytes = SmgpAdapter
            .encode(&UnifiedMessage::Ping, Sequence::Plain(self.next_seq()))
            .expect("encode active_test");
        RawPdu::from(bytes)
    }

    pub fn build_exit_pdu(&self) -> RawPdu {
        let bytes = SmgpAdapter
            .encode(&UnifiedMessage::Unbind, Sequence::Plain(self.next_seq()))
            .expect("encode exit");
        RawPdu::from(bytes)
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn get_login_status(&self) -> Option<u32> {
        self.login_resp_status.lock().unwrap().clone()
    }

    pub fn get_submit_status(&self) -> Option<u32> {
        self.submit_resp_status.lock().unwrap().clone()
    }

    pub fn deliver_count(&self) -> usize {
        self.deliver_count.load(Ordering::Relaxed)
    }

    pub fn active_test_resp_count(&self) -> usize {
        self.active_test_resp_count.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl MessageHandler for TestClientHandler {
    fn name(&self) -> &'static str {
        "smgp-test-client"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        // 框架已按协议解码为统一消息，直接 match 统一枚举。
        match msg {
            UnifiedMessage::BindResp(resp) => {
                *self.login_resp_status.lock().unwrap() = Some(resp.status);
                if resp.status == 0 {
                    self.connected.store(true, Ordering::Relaxed);
                }
            }
            UnifiedMessage::SubmitResp(resp) => {
                *self.submit_resp_status.lock().unwrap() = Some(resp.status);
            }
            // is_report=1→Report，is_report=0→Deliver；两者都计入 deliver_count 并回 DeliverResp。
            UnifiedMessage::Report(_) => {
                self.deliver_count.fetch_add(1, Ordering::Relaxed);
                tracing::info!("[客户端] 收到状态报告");
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            UnifiedMessage::Deliver(_) => {
                self.deliver_count.fetch_add(1, Ordering::Relaxed);
                tracing::info!("[客户端] 收到上行短信");
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            UnifiedMessage::PingResp => {
                self.active_test_resp_count.fetch_add(1, Ordering::Relaxed);
            }
            UnifiedMessage::UnbindResp => {
                self.exit_resp_received.store(true, Ordering::Relaxed);
            }
            _ => {}
        }

        Ok(())
    }
}

pub async fn start_test_server(
    auth_handler: Arc<dyn AuthHandler>,
    biz_handler: Arc<dyn MessageHandler>,
    event_handler: Arc<dyn ServerEventHandler>,
    idle_timeout_secs: u32,
) -> Result<(u16, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "smgp-test-server",
        "127.0.0.1",
        0,
        8,
        idle_timeout_secs as u16,
    ).with_protocol(Protocol::Smgp));
    let server = ServerBuilder::new(cfg)
        .message_handlers(vec![biz_handler])
        .auth_handler(auth_handler)
        .account_config_provider(Arc::new(MockAccountConfigProvider::new()) as Arc<dyn AccountConfigProvider>)
        .event_handler(event_handler)
        .serve()
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
    biz_handler: Arc<dyn MessageHandler>,
    event_handler: Arc<dyn ServerEventHandler>,
    idle_timeout_secs: u32,
) -> Result<(u16, Arc<rsms_connector::ConnectionPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "smgp-test-server",
        "127.0.0.1",
        0,
        8,
        idle_timeout_secs as u16,
    ).with_protocol(Protocol::Smgp));
    let server = ServerBuilder::new(cfg)
        .message_handlers(vec![biz_handler])
        .auth_handler(auth_handler)
        .account_config_provider(Arc::new(MockAccountConfigProvider::new()) as Arc<dyn AccountConfigProvider>)
        .event_handler(event_handler)
        .serve()
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

fn chrono_lite_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ts = format!("{:010}", now);
    format!("{}{}", &ts[0..8], &ts[8..10])
}

pub fn build_deliver_mo(seq_id: u32, src_term: &str, dest_term: &str, content: &str) -> RawPdu {
    // 下行单条 MO：is_report=0 的 Deliver，原 msg_fmt=15→Encoding::Gbk。
    // 注：UnifiedDeliver 不携带 recv_time/msg_id 头字段，经 adapter 走 Deliver::new() 默认（语义无关）。
    let unified = UnifiedMessage::Deliver(UnifiedDeliver {
        src: Address::plain(src_term),
        dest: Address::plain(dest_term),
        content: content.as_bytes().to_vec(),
        encoding: Encoding::Gbk,
        concat: None,
        extra: ProtocolExtra::None,
        tlvs: vec![],
    });
    let bytes = SmgpAdapter.encode(&unified, Sequence::Plain(seq_id)).expect("encode MO");
    RawPdu::from(bytes)
}

pub fn build_deliver_report(seq_id: u32, dest_term: &str) -> RawPdu {
    let now = chrono_lite_timestamp();
    // 报告正文经 SmgpReport 序列化（SMGP 报告 payload 格式化助手，非裸 PDU 消息类型）。
    let report = SmgpReport {
        msg_id: rsms_codec_smgp::SmgpMsgId::from_u64(10001).bytes,
        sub: "001".to_string(),
        dlvrd: "001".to_string(),
        submit_time: now.clone(),
        done_time: now,
        stat: "DELIVRD".to_string(),
        err: "000".to_string(),
        txt: "Hello".to_string(),
    };
    // is_report=1 经 UnifiedMessage::Report → adapter 编码；msg_id 头取默认全零（匹配靠正文 id:）。
    let unified = UnifiedMessage::Report(UnifiedReport {
        msg_id: MessageId::Binary(vec![0u8; 10]),
        status: DeliveryStatus::Delivered,
        src: Address::plain("13800138000"),
        dest: Address::plain(dest_term),
        raw: report.to_bytes(),
    });
    let bytes = SmgpAdapter.encode(&unified, Sequence::Plain(seq_id)).expect("encode report");
    RawPdu::from(bytes)
}

async fn get_conn_from_pool(pool: &Arc<rsms_connector::ConnectionPool>) -> Option<Arc<rsms_connector::Connection>> {
    pool.first().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsms_connector::ClientBuilder;
    use rsms_connector::client::ClientConfig;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn test_login_with_valid_password() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let client_id = "106900";
        let password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(client_id, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smgp-client", "127.0.0.1", port, 8, 30).with_protocol(Protocol::Smgp));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SmgpDecoder)
            .client_config(ClientConfig::new())
            .connect()
            .await
            .expect("connect");

        let login_pdu = client_handler.build_login_pdu(client_id, password);
        conn.send_request(login_pdu).await.expect("send login");

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if client_handler.is_connected() {
                break;
            }
        }

        assert_eq!(client_handler.get_login_status(), Some(0), "认证成功状态码应为0");
        assert!(client_handler.is_connected(), "应该已连接");
        assert_eq!(auth.auth_success_count(), 1, "应该认证成功1次");

        handle.abort();
    }

    #[tokio::test]
    async fn test_login_with_wrong_password() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let client_id = "106900";
        let correct_password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(client_id, correct_password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smgp-client", "127.0.0.1", port, 8, 30).with_protocol(Protocol::Smgp));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SmgpDecoder)
            .client_config(ClientConfig::new())
            .connect()
            .await
            .expect("connect");

        let login_pdu = client_handler.build_login_pdu(client_id, "wrongpassword");
        conn.send_request(login_pdu).await.expect("send login");

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if client_handler.get_login_status().is_some() {
                break;
            }
        }

        assert!(client_handler.get_login_status().is_some(), "应该收到响应");
        assert!(client_handler.get_login_status() != Some(0), "认证失败状态码不应为0");
        assert!(!client_handler.is_connected(), "不应该已连接");
        assert_eq!(auth.auth_fail_count(), 1, "应该认证失败1次");

        handle.abort();
    }

    #[tokio::test]
    async fn test_login_with_unknown_client_id() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let auth = Arc::new(PasswordAuthHandler::new());
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smgp-client", "127.0.0.1", port, 8, 30).with_protocol(Protocol::Smgp));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SmgpDecoder)
            .client_config(ClientConfig::new())
            .connect()
            .await
            .expect("connect");

        let login_pdu = client_handler.build_login_pdu("unknown", "password");
        conn.send_request(login_pdu).await.expect("send login");

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if client_handler.get_login_status().is_some() {
                break;
            }
        }

        assert!(client_handler.get_login_status().is_some(), "应该收到响应");
        assert!(client_handler.get_login_status() != Some(0), "未知账号认证失败状态码不应为0");

        handle.abort();
    }

    #[tokio::test]
    async fn test_submit_message() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let client_id = "106900";
        let password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(client_id, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smgp-client", "127.0.0.1", port, 8, 30).with_protocol(Protocol::Smgp));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SmgpDecoder)
            .client_config(ClientConfig::new())
            .connect()
            .await
            .expect("connect");

        let login_pdu = client_handler.build_login_pdu(client_id, password);
        conn.send_request(login_pdu).await.expect("send login");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        let submit_pdu = client_handler.build_submit_pdu("13800138000", client_id, "Hello SMS");
        conn.send_request(submit_pdu).await.expect("send submit");

        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(client_handler.get_submit_status(), Some(0), "提交成功状态码应为0");
        assert_eq!(biz.submit_count(), 1, "服务器应该收到1条Submit");
        assert_eq!(biz.get_messages(), vec!["Hello SMS"], "服务器应该收到正确内容");

        handle.abort();
    }

    #[tokio::test]
    async fn test_active_test() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let client_id = "106900";
        let password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(client_id, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smgp-client", "127.0.0.1", port, 8, 30).with_protocol(Protocol::Smgp));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SmgpDecoder)
            .client_config(ClientConfig::new())
            .connect()
            .await
            .expect("connect");

        let login_pdu = client_handler.build_login_pdu(client_id, password);
        conn.send_request(login_pdu).await.expect("send login");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        let active_test_pdu = client_handler.build_active_test_pdu();
        conn.send_request(active_test_pdu).await.expect("send active test");

        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(client_handler.active_test_resp_count(), 1, "应该收到ActiveTestResp");

        handle.abort();
    }

    #[tokio::test]
    async fn test_exit() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let client_id = "106900";
        let password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(client_id, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smgp-client", "127.0.0.1", port, 8, 30).with_protocol(Protocol::Smgp));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SmgpDecoder)
            .client_config(ClientConfig::new())
            .connect()
            .await
            .expect("connect");

        let login_pdu = client_handler.build_login_pdu(client_id, password);
        conn.send_request(login_pdu).await.expect("send login");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        let exit_pdu = client_handler.build_exit_pdu();
        conn.send_request(exit_pdu).await.expect("send exit");

        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(client_handler.exit_resp_received.load(Ordering::Relaxed), "应该收到ExitResp");

        handle.abort();
    }

    #[tokio::test]
    async fn test_no_heartbeat_disconnect() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let client_id = "106900";
        let password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(client_id, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let client_evt = Arc::new(TestClientEventHandler::new());
        let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 2).await.unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smgp-client", "127.0.0.1", port, 8, 30).with_protocol(Protocol::Smgp));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SmgpDecoder)
            .client_config(ClientConfig::new())
            .event_handler(client_evt.clone())
            .connect()
            .await
            .expect("connect");

        let login_pdu = client_handler.build_login_pdu(client_id, password);
        conn.send_request(login_pdu).await.expect("send login");
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

        let client_id = "106900";
        let password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(client_id, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, pool, handle) = start_test_server_with_pool(auth.clone(), biz.clone(), evt.clone(), 30)
            .await
            .unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smgp-client", "127.0.0.1", port, 8, 30).with_protocol(Protocol::Smgp));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SmgpDecoder)
            .client_config(ClientConfig::new())
            .connect()
            .await
            .expect("connect");

        let login_pdu = client_handler.build_login_pdu(client_id, password);
        conn.send_request(login_pdu).await.expect("send login");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        tokio::time::sleep(Duration::from_millis(100)).await;

        if let Some(server_conn) = get_conn_from_pool(&pool).await {
            let deliver_pdu = build_deliver_mo(100, "13800138000", client_id, "Hello MO SMS");
            server_conn.write_frame(deliver_pdu.as_slice()).await.expect("send deliver");
        }

        tokio::time::sleep(Duration::from_millis(300)).await;

        assert_eq!(client_handler.deliver_count(), 1, "应该收到1个Deliver");

        handle.abort();
    }

    #[tokio::test]
    async fn test_deliver_report() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .try_init();

        let client_id = "106900";
        let password = "password123";

        let auth = Arc::new(PasswordAuthHandler::new().add_account(client_id, password));
        let biz = Arc::new(TestBusinessHandler::new());
        let evt = Arc::new(TestEventHandler::new());
        let (port, pool, handle) = start_test_server_with_pool(auth.clone(), biz.clone(), evt.clone(), 30)
            .await
            .unwrap();

        let endpoint = Arc::new(EndpointConfig::new("smgp-client", "127.0.0.1", port, 8, 30).with_protocol(Protocol::Smgp));
        let client_handler = Arc::new(TestClientHandler::new());
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SmgpDecoder)
            .client_config(ClientConfig::new())
            .connect()
            .await
            .expect("connect");

        let login_pdu = client_handler.build_login_pdu(client_id, password);
        conn.send_request(login_pdu).await.expect("send login");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        tokio::time::sleep(Duration::from_millis(100)).await;

        if let Some(server_conn) = get_conn_from_pool(&pool).await {
            let deliver_pdu = build_deliver_report(101, client_id);
            server_conn.write_frame(deliver_pdu.as_slice()).await.expect("send deliver report");
        }

        tokio::time::sleep(Duration::from_millis(300)).await;

        assert_eq!(client_handler.deliver_count(), 1, "应该收到1个Deliver");

        handle.abort();
    }
}
