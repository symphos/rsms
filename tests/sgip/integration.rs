use rsms_connector::{ServerBuilder, SgipDecoder};
use rsms_connector::client::ClientHandler;
use rsms_connector::{AuthHandler, AuthCredentials, AuthResult, ServerEventHandler, AccountConfigProvider};
use rsms_test_common::{TestEventHandler, TestClientEventHandler, MockAccountConfigProvider};
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_core::{ConnectionInfo, EncodedPdu, RawPdu, EndpointConfig, Protocol, Frame, Result};
// 窄腰统一模型：业务/客户端不再直接接触 SGIP 裸 codec，统一走 SgipAdapter + UnifiedMessage。
use rsms_codec_sgip::adapter::SgipAdapter;
use rsms_model::{
    Address, MessageId, ProtocolAdapter, ProtocolExtra, Sequence, SgipExtra,
    UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedSubmit, UnifiedSubmitResp,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use tokio::time::Duration;
use std::collections::HashMap;

// SGIP 复合序列分量：发起方固定 node_id/timestamp，number 自增（保留原测试约定）。
const SGIP_NODE_ID: u32 = 1;
const SGIP_TIMESTAMP: u32 = 0x04051200;

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
        // 统一模型解码：裸 decode_message + 手剥字节全部由 SgipAdapter 吸收。
        if let Ok(unified) = SgipAdapter.decode(frame) {
            match unified {
                UnifiedMessage::Submit(s) => {
                    self.submit_count.fetch_add(1, Ordering::Relaxed);
                    let content = String::from_utf8_lossy(&s.content).to_string();
                    self.messages.lock().unwrap().push(content);

                    // 回 SubmitResp：SGIP 无 msg_id，msg_id 给空 Text；序列用 sequence_of 回显请求复合序列。
                    let resp = UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                        msg_id: MessageId::Text(String::new()),
                        status: 0,
                    });
                    let resp_bytes = SgipAdapter.encode(&resp, SgipAdapter.sequence_of(frame))?;
                    ctx.conn.write_frame(resp_bytes.as_slice()).await?;
                }
                UnifiedMessage::Deliver(d) => {
                    // 状态报告（旧版以 "id:" 文本承载在 Deliver 里）与普通 MO 区分语义保持不变。
                    if d.content.len() > 20 && String::from_utf8_lossy(&d.content).contains("id:") {
                        self.reports.lock().unwrap().push(String::from_utf8_lossy(&d.content).to_string());
                    } else {
                        self.mo_messages.lock().unwrap().push((
                            d.dest.number.clone(),
                            String::from_utf8_lossy(&d.content).to_string(),
                        ));
                    }
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

    // 复合序列工厂：node_id/timestamp 固定，number 走原 next_seq 自增。
    fn next_sgip_seq(&self) -> Sequence {
        Sequence::Sgip {
            node_id: SGIP_NODE_ID,
            timestamp: SGIP_TIMESTAMP,
            number: self.next_seq(),
        }
    }

    pub fn build_bind_pdu(&self, login_name: &str, login_password: &str) -> RawPdu {
        // 明文认证：authenticator 直接装口令字节（无 MD5）；version 承载 login_type=1。
        let bind = UnifiedMessage::Bind(UnifiedBind {
            client_id: login_name.to_string(),
            authenticator: login_password.as_bytes().to_vec(),
            timestamp: 0,
            version: 1,
            system_type: None,
            mode: rsms_model::BindMode::default(),
            login_mode: None,
        });
        let bytes = SgipAdapter.encode(&bind, self.next_sgip_seq()).expect("encode bind");
        RawPdu::from(bytes)
    }

    pub fn build_submit_pdu(&self, sp_number: &str, dest_number: &str, content: &str) -> RawPdu {
        // SGIP 方言字段（charge_number/service_type/fee_* 等）经 SgipExtra 传递。
        let submit = UnifiedMessage::Submit(UnifiedSubmit {
            src: Address::plain(sp_number),
            dests: vec![Address::plain(dest_number)],
            content: content.as_bytes().to_vec(),
            // msg_fmt=15(GBK)：旧版用 GBK 发 ASCII 文本，内容仍是原始字节。
            encoding: rsms_model::Encoding::Gbk,
            want_report: true, // report_flag=1
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
        let bytes = SgipAdapter.encode(&submit, self.next_sgip_seq()).expect("encode submit");
        RawPdu::from(bytes)
    }

    pub fn build_unbind_pdu(&self) -> RawPdu {
        let bytes = SgipAdapter
            .encode(&UnifiedMessage::Unbind, self.next_sgip_seq())
            .expect("encode unbind");
        RawPdu::from(bytes)
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
        let unified = match SgipAdapter.decode(frame) {
            Ok(m) => m,
            Err(_) => return Ok(()),
        };

        match unified {
            UnifiedMessage::BindResp(resp) => {
                *self.bind_resp_status.lock().unwrap() = Some(resp.status);
                if resp.status == 0 {
                    self.connected.store(true, Ordering::Relaxed);
                }
            }
            UnifiedMessage::SubmitResp(resp) => {
                *self.submit_resp_status.lock().unwrap() = Some(resp.status);
            }
            UnifiedMessage::Deliver(d) => {
                self.deliver_count.fetch_add(1, Ordering::Relaxed);
                // 旧版状态报告以 "id:" 文本承载在 Deliver 内容里，语义保持不变。
                let content = String::from_utf8_lossy(&d.content);
                if content.contains("id:") {
                    self.report_count.fetch_add(1, Ordering::Relaxed);
                }
                // 回 DeliverResp：序列用 sequence_of 回显请求复合序列。
                let bytes =
                    SgipAdapter.encode(&UnifiedMessage::DeliverResp, SgipAdapter.sequence_of(frame))?;
                ctx.conn.write_frame(bytes.as_slice()).await?;
            }
            UnifiedMessage::UnbindResp => {
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
        "sgip-test-server",
        "127.0.0.1",
        0,
        8,
        idle_timeout_secs as u16,
    ).with_protocol(Protocol::Sgip));
    let server = ServerBuilder::new(cfg)
        .handlers(vec![biz_handler])
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
    ).with_protocol(Protocol::Sgip));
    let server = ServerBuilder::new(cfg)
        .handlers(vec![biz_handler])
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

/// 构造 MO 上行 Deliver（统一模型）。decode 对称：src=sp_number, dest=user_number。
pub fn build_deliver_mo(seq_id: u32, user_number: &str, sp_number: &str, content: &str) -> RawPdu {
    let deliver = UnifiedMessage::Deliver(UnifiedDeliver {
        src: Address::plain(sp_number),
        dest: Address::plain(user_number),
        content: content.as_bytes().to_vec(),
        // msg_fmt=15(GBK)：内容是 ASCII 文本字节，用 GBK 保字节一致（与原测试相同）。
        encoding: rsms_model::Encoding::Gbk,
        concat: None,
        extra: ProtocolExtra::Sgip(SgipExtra::default()),
        tlvs: vec![],
    });
    let seq = Sequence::Sgip { node_id: SGIP_NODE_ID, timestamp: SGIP_TIMESTAMP, number: seq_id };
    RawPdu::from(SgipAdapter.encode(&deliver, seq).expect("encode deliver(MO)"))
}

/// 构造「以 Deliver 承载的状态报告」（旧测试约定：内容含 "id:" 文本）。
/// 注意：这并非 SGIP 独立 Report 命令，而是测试用文本约定，仍走 Deliver。
/// （迁移后移除了原 `_submit_seq: &SgipSequence` 死参数——它从未被使用，且是最后一处裸 codec 引用。）
pub fn build_deliver_report(seq_id: u32, dest_id: &str) -> RawPdu {
    let report_content = format!(
        "id:{:02x?}{:02x?}{:02x?}{:02x?}{:02x?}{:02x?}{:02x?}{:02x?} sub:001 dlvrd:001 submit date:26010100 done date:26010100 stat:DELIVRD err:000",
        0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 1u8
    );

    let deliver = UnifiedMessage::Deliver(UnifiedDeliver {
        src: Address::plain(dest_id),
        dest: Address::plain(String::new()),
        content: report_content.as_bytes().to_vec(),
        encoding: rsms_model::Encoding::Gbk,
        concat: None,
        extra: ProtocolExtra::Sgip(SgipExtra::default()),
        tlvs: vec![],
    });
    let seq = Sequence::Sgip { node_id: SGIP_NODE_ID, timestamp: SGIP_TIMESTAMP, number: seq_id };
    RawPdu::from(SgipAdapter.encode(&deliver, seq).expect("encode deliver(report)"))
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
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SgipDecoder)
            .client_config(ClientConfig::new())
            .connect()
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
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SgipDecoder)
            .client_config(ClientConfig::new())
            .connect()
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
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SgipDecoder)
            .client_config(ClientConfig::new())
            .connect()
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
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SgipDecoder)
            .client_config(ClientConfig::new())
            .connect()
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
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SgipDecoder)
            .client_config(ClientConfig::new())
            .connect()
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
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SgipDecoder)
            .client_config(ClientConfig::new())
            .event_handler(client_evt.clone())
            .connect()
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
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SgipDecoder)
            .client_config(ClientConfig::new())
            .connect()
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
        let conn = ClientBuilder::new(endpoint, client_handler.clone(), SgipDecoder)
            .client_config(ClientConfig::new())
            .connect()
            .await
            .expect("connect");

        let bind_pdu = client_handler.build_bind_pdu(account, password);
        conn.write_frame(bind_pdu.as_bytes()).await.expect("send bind");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(client_handler.is_connected(), "连接失败");

        tokio::time::sleep(Duration::from_millis(100)).await;

        if let Some(server_conn) = get_conn_from_pool(&pool).await {
            let deliver_pdu = build_deliver_report(101, account);
            server_conn.write_frame(deliver_pdu.as_bytes()).await.expect("send deliver report");
        }

        tokio::time::sleep(Duration::from_millis(300)).await;

        assert_eq!(client_handler.deliver_count(), 1, "应该收到1个Deliver");
        assert_eq!(client_handler.report_count(), 1, "应该收到1个状态报告");

        handle.abort();
    }
}
