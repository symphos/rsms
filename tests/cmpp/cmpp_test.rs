use async_trait::async_trait;
use rsms_connector::{
    ClientBuilder, AuthCredentials, AuthHandler, AuthResult,
    ClientEventHandler, MessageSource, MessageItem, ProtocolConnection,
    ServerEventHandler, AccountConfig, AccountConfigProvider,
    CmppDecoder, NoopClientHandler,
    MessageCallback, SubmitInfo, ReportInfo, MoInfo,
    ServerShutdown,
};
use rsms_connector::client::ClientConfig;
use rsms_business::{MessageContext, MessageHandler};
use rsms_core::{ConnectionInfo, EncodedPdu, Metrics, Protocol, RawPdu, EndpointConfig, Frame, Result};
// 窄腰统一模型：编解码全程经 CmppAdapter（decode→UnifiedMessage / encode UnifiedMessage→字节）。
// compute_connect_auth 是 MD5 鉴权工具（非裸消息构造），保留。
use rsms_codec_cmpp::adapter::CmppAdapter;
use rsms_codec_cmpp::auth::compute_connect_auth;
use rsms_model::{
    Address, CmppExtra, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra,
    Sequence, UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit,
    UnifiedSubmitResp,
};
use rsms_test_common::{TestEventHandler, TestClientEventHandler, TestMessageSource, MockAccountConfigProvider};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Duration;

/// 真实密码验证的认证处理器
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
        "password-auth-handler"
    }

    async fn authenticate(&self, client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        self.auth_count.fetch_add(1, Ordering::Relaxed);

        // 检查账号长度
        if client_id.len() > 6 {
            self.auth_fail.fetch_add(1, Ordering::Relaxed);
            return Ok(AuthResult::failure(1, "Account too long"));
        }

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

/// 测试用的业务处理器
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
        "test-biz-handler"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        // 窄腰路径：框架已 decode，直接 match 统一消息枚举。
        match msg {
            UnifiedMessage::Submit(s) => {
                let result = if ctx.conn.authenticated_account().await.is_some() {
                    if let Some(limiter) = ctx.conn.rate_limiter().await {
                        if !limiter.try_acquire().await {
                            8
                        } else {
                            self.submit_count.fetch_add(1, Ordering::Relaxed);
                            let content = String::from_utf8_lossy(&s.content).to_string();
                            self.messages.lock().unwrap().push(content);
                            0
                        }
                    } else {
                        self.submit_count.fetch_add(1, Ordering::Relaxed);
                        let content = String::from_utf8_lossy(&s.content).to_string();
                        self.messages.lock().unwrap().push(content);
                        0
                    }
                } else {
                    3
                };
                ctx.reply(UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                    msg_id: MessageId::Binary(vec![0u8; 8]),
                    status: result,
                })).await?;
            }
            // 状态报告（CMPP Deliver registered_delivery=1）→ Report，记原始正文。
            UnifiedMessage::Report(r) => {
                let report = String::from_utf8_lossy(&r.raw).to_string();
                self.reports.lock().unwrap().push(report);
            }
            // MO 上行（registered_delivery=0）→ Deliver，记 (src, content)。
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

/// 测试用客户端处理器
pub struct TestClientHandler {
    pub connected: Arc<AtomicBool>,
    pub connect_resp_status: Arc<Mutex<Option<u32>>>,
    pub submit_resp_status: Arc<Mutex<Option<u32>>>,
    pub deliver_count: Arc<AtomicUsize>,
    pub report_count: Arc<AtomicUsize>,
    pub active_test_resp_count: Arc<AtomicUsize>,
    pub terminate_resp_received: Arc<AtomicBool>,
    seq: AtomicUsize,
}

impl TestClientHandler {
    pub fn new() -> Self {
        Self {
            connected: Arc::new(AtomicBool::new(false)),
            connect_resp_status: Arc::new(Mutex::new(None)),
            submit_resp_status: Arc::new(Mutex::new(None)),
            deliver_count: Arc::new(AtomicUsize::new(0)),
            report_count: Arc::new(AtomicUsize::new(0)),
            active_test_resp_count: Arc::new(AtomicUsize::new(0)),
            terminate_resp_received: Arc::new(AtomicBool::new(false)),
            seq: AtomicUsize::new(1),
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u32
    }

    pub fn build_connect_pdu(&self, account: &str, password: &str) -> RawPdu {
        let timestamp = 0u32;
        let auth = compute_connect_auth(account, password, timestamp);
        // 统一 Bind：MD5 authenticator 原样填入，adapter 不重算。
        let bind = UnifiedMessage::Bind(UnifiedBind {
            client_id: account.to_string(),
            authenticator: auth.to_vec(),
            timestamp,
            version: 0x30,
            system_type: None,
            mode: rsms_model::BindMode::default(),
            login_mode: None,
        });
        let bytes = CmppAdapter.encode(&bind, Sequence::Plain(self.next_seq())).expect("encode bind");
        RawPdu::from_vec(bytes)
    }

    pub fn build_submit_pdu(&self, src: &str, dst: &str, content: &str) -> RawPdu {
        // 统一 Submit：want_report=true 对应 registered_delivery=1。
        let submit = UnifiedMessage::Submit(UnifiedSubmit {
            src: Address::plain(src),
            dests: vec![Address::plain(dst)],
            content: content.as_bytes().to_vec(),
            encoding: Encoding::Ascii,
            want_report: true,
            concat: None,
            extra: ProtocolExtra::Cmpp(Default::default()),
            tlvs: vec![],
        });
        let bytes = CmppAdapter.encode(&submit, Sequence::Plain(self.next_seq())).expect("encode submit");
        RawPdu::from_vec(bytes)
    }

    pub fn build_active_test_pdu(&self) -> RawPdu {
        // 心跳 ActiveTest → 统一 Ping。
        let bytes = CmppAdapter.encode(&UnifiedMessage::Ping, Sequence::Plain(self.next_seq())).expect("encode ping");
        RawPdu::from_vec(bytes)
    }

    pub fn build_terminate_pdu(&self) -> RawPdu {
        // Terminate → 统一 Unbind。
        let bytes = CmppAdapter.encode(&UnifiedMessage::Unbind, Sequence::Plain(self.next_seq())).expect("encode unbind");
        RawPdu::from_vec(bytes)
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

    pub fn deliver_count(&self) -> usize {
        self.deliver_count.load(Ordering::Relaxed)
    }

    pub fn report_count(&self) -> usize {
        self.report_count.load(Ordering::Relaxed)
    }

    pub fn active_test_resp_count(&self) -> usize {
        self.active_test_resp_count.load(Ordering::Relaxed)
    }

    pub fn reset(&self) {
        self.connected.store(false, Ordering::Relaxed);
        *self.connect_resp_status.lock().unwrap() = None;
        *self.submit_resp_status.lock().unwrap() = None;
        self.deliver_count.store(0, Ordering::Relaxed);
        self.report_count.store(0, Ordering::Relaxed);
        self.active_test_resp_count.store(0, Ordering::Relaxed);
        self.terminate_resp_received.store(false, Ordering::Relaxed);
    }
}

#[async_trait]
impl MessageHandler for TestClientHandler {
    fn name(&self) -> &'static str {
        "test-client-handler"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        // 窄腰路径：框架已 decode，直接 match 统一消息枚举。
        match msg {
            UnifiedMessage::BindResp(resp) => {
                *self.connect_resp_status.lock().unwrap() = Some(resp.status);
                if resp.status == 0 {
                    self.connected.store(true, Ordering::Relaxed);
                }
            }
            UnifiedMessage::SubmitResp(resp) => {
                *self.submit_resp_status.lock().unwrap() = Some(resp.status);
            }
            // 状态报告（Deliver registered_delivery=1）：既计 deliver 又计 report，并回 DeliverResp。
            UnifiedMessage::Report(_) => {
                self.deliver_count.fetch_add(1, Ordering::Relaxed);
                self.report_count.fetch_add(1, Ordering::Relaxed);
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            // MO 上行（registered_delivery=0）：仅计 deliver，并回 DeliverResp。
            UnifiedMessage::Deliver(_) => {
                self.deliver_count.fetch_add(1, Ordering::Relaxed);
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            UnifiedMessage::PingResp => {
                self.active_test_resp_count.fetch_add(1, Ordering::Relaxed);
            }
            UnifiedMessage::UnbindResp => {
                self.terminate_resp_received.store(true, Ordering::Relaxed);
            }
            _ => {}
        }

        Ok(())
    }
}

use rsms_connector::ServerBuilder;

/// 启动测试服务器
async fn start_test_server(
    auth_handler: Arc<dyn AuthHandler>,
    biz_handler: Arc<dyn MessageHandler>,
    event_handler: Arc<dyn ServerEventHandler>,
    idle_timeout_secs: u32,
) -> Result<(u16, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "test-server",
        "127.0.0.1",
        0,
        8,
        idle_timeout_secs as u16,
    ));
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

/// 启动测试服务器，返回server引用以便访问连接池
async fn start_test_server_with_pool(
    auth_handler: Arc<dyn AuthHandler>,
    biz_handler: Arc<dyn MessageHandler>,
    event_handler: Arc<dyn ServerEventHandler>,
    idle_timeout_secs: u32,
) -> Result<(u16, Arc<rsms_connector::ConnectionPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "test-server",
        "127.0.0.1",
        0,
        8,
        idle_timeout_secs as u16,
    ));
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

#[tokio::test]
async fn test_connect_with_valid_password() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let evt = Arc::new(TestEventHandler::new());
    let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(client_handler.clone())
        .client_config(ClientConfig::new())
        .connect()
        .await
        .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(client_handler.get_connect_status(), Some(0), "认证成功状态码应为0");
    assert!(client_handler.is_connected(), "应该已连接");
    assert_eq!(auth.auth_success_count(), 1, "应该认证成功1次");

    handle.abort();
}

/// 计数型 Metrics 测试桩：用原子计数验证框架在关键点回调了指标钩子。
#[derive(Default)]
struct CountingMetrics {
    opened: AtomicUsize,
    authenticated: AtomicUsize,
    inbound: AtomicUsize,
    outbound: AtomicUsize,
    decode_error: AtomicUsize,
}

impl Metrics for CountingMetrics {
    fn connection_opened(&self) {
        self.opened.fetch_add(1, Ordering::Relaxed);
    }
    fn connection_authenticated(&self, _account: &str) {
        self.authenticated.fetch_add(1, Ordering::Relaxed);
    }
    fn inbound_frame(&self, _protocol: Protocol, _command_id: u32) {
        self.inbound.fetch_add(1, Ordering::Relaxed);
    }
    fn outbound_frame(&self) {
        self.outbound.fetch_add(1, Ordering::Relaxed);
    }
    fn decode_error(&self, _protocol: Protocol) {
        self.decode_error.fetch_add(1, Ordering::Relaxed);
    }
}

/// 单发 MessageSource 测试桩:首次 fetch 返回一个 CMPP ActiveTest PDU,之后空。用于验证出站埋点。
struct OneShotSource {
    sent: AtomicBool,
}

#[async_trait]
impl MessageSource for OneShotSource {
    async fn fetch(&self, _account: &str, _batch: usize) -> Result<Vec<MessageItem>> {
        if self.sent.swap(true, Ordering::Relaxed) {
            return Ok(vec![]);
        }
        // CMPP ActiveTest(12B):total_len=0x0C, command_id=0x08, sequence=1。
        let pdu = vec![0, 0, 0, 0x0C, 0, 0, 0, 0x08, 0, 0, 0, 1];
        Ok(vec![MessageItem::Single(
            Arc::new(RawPdu::from_vec(pdu)) as Arc<dyn EncodedPdu>,
        )])
    }
}

async fn start_test_server_with_metrics(
    auth_handler: Arc<dyn AuthHandler>,
    biz_handler: Arc<dyn MessageHandler>,
    metrics: Arc<dyn Metrics>,
) -> Result<(u16, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new("metrics-server", "127.0.0.1", 0, 8, 30));
    let server = ServerBuilder::new(cfg)
        .message_handlers(vec![biz_handler])
        .auth_handler(auth_handler)
        .account_config_provider(Arc::new(MockAccountConfigProvider::new()) as Arc<dyn AccountConfigProvider>)
        .metrics(metrics)
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

#[tokio::test]
async fn test_metrics_hooks_fired() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let metrics = Arc::new(CountingMetrics::default());
    let (port, handle) =
        start_test_server_with_metrics(auth.clone(), biz.clone(), metrics.clone() as Arc<dyn Metrics>)
            .await
            .unwrap();

    let endpoint = Arc::new(EndpointConfig::new("metrics-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(client_handler.clone())
        .client_config(ClientConfig::new())
        .connect()
        .await
        .expect("connect");

    // 发一帧 Connect：服务端 accept → connection_opened；解码该帧 → inbound_frame；
    // 认证成功并注册账号池 → connection_authenticated。
    let connect_pdu = client_handler.build_connect_pdu("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(metrics.opened.load(Ordering::Relaxed) >= 1, "connection_opened 应被调用");
    assert!(metrics.inbound.load(Ordering::Relaxed) >= 1, "inbound_frame 应被调用");
    assert_eq!(metrics.authenticated.load(Ordering::Relaxed), 1, "connection_authenticated 应被调用 1 次");
    assert_eq!(metrics.decode_error.load(Ordering::Relaxed), 0, "正常帧不应触发 decode_error");
    // connection_closed 与 connection_opened 在 run_connection 同处对称埋点（收尾必调），
    // 客户端无公开 close API、整测断连时序不稳，故其覆盖交由 rsms-core 的 Metrics 单测 +
    // 接线对称性保证，此处不做时序敏感断言。

    handle.abort();
}

#[tokio::test]
async fn test_client_metrics_and_outbound() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    // 服务端(普通,不关心其指标)
    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let evt = Arc::new(TestEventHandler::new());
    let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

    // 客户端:注入指标 + 单发 MessageSource(出站一帧)
    let client_metrics = Arc::new(CountingMetrics::default());
    let endpoint = Arc::new(EndpointConfig::new("metrics-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(client_handler.clone())
        .client_config(ClientConfig::new())
        .metrics(client_metrics.clone() as Arc<dyn Metrics>)
        .message_source(Arc::new(OneShotSource { sent: AtomicBool::new(false) }) as Arc<dyn MessageSource>)
        .connect()
        .await
        .expect("connect");

    // 发 Connect 触发收到 ConnectResp(入站埋点);出站 fetcher 会把单发 PDU 写出(出站埋点)。
    let connect_pdu = client_handler.build_connect_pdu("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");
    tokio::time::sleep(Duration::from_millis(400)).await;

    assert!(client_metrics.opened.load(Ordering::Relaxed) >= 1, "客户端 connection_opened 应被调用");
    assert!(client_metrics.inbound.load(Ordering::Relaxed) >= 1, "客户端 inbound_frame 应被调用(收到 ConnectResp)");
    assert!(client_metrics.outbound.load(Ordering::Relaxed) >= 1, "客户端 outbound_frame 应被调用(出站 fetcher 发送)");

    handle.abort();
}

#[tokio::test]
async fn test_connect_with_invalid_password() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let evt = Arc::new(TestEventHandler::new());
    let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(client_handler.clone())
        .client_config(ClientConfig::new())
        .connect()
        .await
        .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu("106900", "wrongpassword");
    conn.send_request(connect_pdu).await.expect("send connect");

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(client_handler.get_connect_status().is_some(), "应该收到响应");
    assert!(client_handler.get_connect_status() != Some(0), "认证失败状态码不应为0");
    assert!(!client_handler.is_connected(), "不应该已连接");
    assert_eq!(auth.auth_fail_count(), 1, "应该认证失败1次");

    handle.abort();
}

#[tokio::test]
async fn test_submit_and_response() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let evt = Arc::new(TestEventHandler::new());
    let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(client_handler.clone())
        .client_config(ClientConfig::new())
        .connect()
        .await
        .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client_handler.is_connected(), "连接失败");

    let submit_pdu = client_handler.build_submit_pdu("13800138000", "106900", "Hello SMS");
    conn.send_request(submit_pdu).await.expect("send submit");

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(client_handler.get_submit_status().is_some(), "应该收到SubmitResp");
    assert_eq!(client_handler.get_submit_status(), Some(0), "提交成功状态码应为0");
    assert_eq!(biz.submit_count(), 1, "服务器应该收到1条Submit");
    assert_eq!(biz.get_messages(), vec!["Hello SMS"], "服务器应该收到正确内容");

    handle.abort();
}

#[tokio::test]
async fn test_terminate() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let evt = Arc::new(TestEventHandler::new());
    let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(client_handler.clone())
        .client_config(ClientConfig::new())
        .connect()
        .await
        .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client_handler.is_connected(), "连接失败");

    let term_pdu = client_handler.build_terminate_pdu();
    conn.send_request(term_pdu).await.expect("send terminate");

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(client_handler.terminate_resp_received.load(Ordering::Relaxed), "应该收到TerminateResp");

    handle.abort();
}

#[tokio::test]
async fn test_no_heartbeat_disconnect() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let evt = Arc::new(TestEventHandler::new());
    let client_evt = Arc::new(TestClientEventHandler::new());
    let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 2).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(client_handler.clone())
        .client_config(ClientConfig::new())
        .event_handler(client_evt.clone())
        .connect()
        .await
        .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client_handler.is_connected(), "连接失败");

    tokio::time::sleep(Duration::from_secs(3)).await;

    assert_eq!(client_evt.disconnected_count(), 1, "无心跳时应该触发断开事件");

    handle.abort();
}

#[tokio::test]
async fn test_submit_without_auth_returns_error() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let evt = Arc::new(TestEventHandler::new());
    let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(client_handler.clone())
        .client_config(ClientConfig::new())
        .connect()
        .await
        .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu("106900", "wrongpassword");
    conn.send_request(connect_pdu).await.expect("send connect");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!client_handler.is_connected(), "认证失败，连接不应成功");

    let submit_pdu = client_handler.build_submit_pdu("13800138000", "106900", "Hello SMS");
    conn.send_request(submit_pdu).await.expect("send submit");

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(client_handler.get_submit_status().is_some(), "应该收到SubmitResp");
    assert_eq!(client_handler.get_submit_status(), Some(3), "未认证时提交应返回result=3");

    handle.abort();
}

/// 辅助函数：从连接池获取一个已连接的连接
async fn get_conn_from_pool(pool: &Arc<rsms_connector::ConnectionPool>) -> Option<Arc<rsms_connector::Connection>> {
    pool.first().await
}

/// 辅助函数：构建 CMPP Deliver PDU (MO - 上行短信)。
/// 统一 Deliver（registered_delivery=0），encoding=Gbk 对应 msg_fmt=15。
fn build_deliver_mo(seq_id: u32, src_terminal: &str, dest_id: &str, content: &str) -> RawPdu {
    let deliver = UnifiedMessage::Deliver(UnifiedDeliver {
        src: Address::plain(src_terminal),
        dest: Address::plain(dest_id),
        content: content.as_bytes().to_vec(),
        encoding: Encoding::Gbk,
        concat: None,
        extra: ProtocolExtra::Cmpp(CmppExtra::default()),
        tlvs: vec![],
    });
    let bytes = CmppAdapter.encode(&deliver, Sequence::Plain(seq_id)).expect("encode deliver(mo)");
    RawPdu::from_vec(bytes)
}

/// 辅助函数：构建 CMPP Deliver PDU (Report - 状态报告)。
/// 统一 Report（adapter 编码为 Deliver registered_delivery=1）；msg_id 用 8 字节二进制。
fn build_deliver_report(seq_id: u32, msg_id: &[u8; 8], dest_id: &str) -> RawPdu {
    // CMPP 状态报告正文为**定长二进制**：Msg_Id(8) + Stat(7) + Submit_time(10) + Done_time(10) + ...
    // 关联键是正文体 [0..8] 的原 submit Msg_Id（真机 cmos 即此格式）。此前用文本 fixture 非规范，
    // 旧 decode 取 Deliver 信封 id 才碰巧能过；现 decode 按规范取正文体 [0..8]。
    let mut report_content = Vec::new();
    report_content.extend_from_slice(msg_id); // [0..8] 原 submit Msg_Id（关联键）
    report_content.extend_from_slice(b"DELIVRD"); // [8..15] stat
    report_content.extend_from_slice(b"2601010000"); // [15..25] submit_time
    report_content.extend_from_slice(b"2601010001"); // [25..35] done_time
    let report = UnifiedMessage::Report(UnifiedReport {
        msg_id: MessageId::Binary(msg_id.to_vec()),
        status: DeliveryStatus::Delivered,
        src: Address::plain(""),
        dest: Address::plain(dest_id),
        raw: report_content,
    });
    let bytes = CmppAdapter.encode(&report, Sequence::Plain(seq_id)).expect("encode report");
    RawPdu::from_vec(bytes)
}

#[tokio::test]
async fn test_deliver_mo() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let evt = Arc::new(TestEventHandler::new());
    let (port, pool, handle) = start_test_server_with_pool(auth.clone(), biz.clone(), evt.clone(), 30)
        .await
        .unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(client_handler.clone())
        .client_config(ClientConfig::new())
        .connect()
        .await
        .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client_handler.is_connected(), "连接失败");

    tokio::time::sleep(Duration::from_millis(100)).await;
    
    if let Some(server_conn) = get_conn_from_pool(&pool).await {
        let deliver_pdu = build_deliver_mo(100, "13800138000", "106900", "Hello MO SMS");
        server_conn.write_frame(deliver_pdu.as_slice()).await.expect("send deliver");
        tracing::info!("服务器发送Deliver MO成功");
    }

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(client_handler.deliver_count(), 1, "应该收到1个Deliver");
    assert_eq!(biz.mo_messages.lock().unwrap().len(), 0, "业务处理器不应收到MO (Deliver由客户端处理)");

    handle.abort();
}

#[tokio::test]
async fn test_deliver_report() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let evt = Arc::new(TestEventHandler::new());
    let (port, pool, handle) = start_test_server_with_pool(auth.clone(), biz.clone(), evt.clone(), 30)
        .await
        .unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(client_handler.clone())
        .client_config(ClientConfig::new())
        .connect()
        .await
        .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client_handler.is_connected(), "连接失败");

    tokio::time::sleep(Duration::from_millis(100)).await;
    
    let msg_id: [u8; 8] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01];
    if let Some(server_conn) = get_conn_from_pool(&pool).await {
        let deliver_pdu = build_deliver_report(101, &msg_id, "106900");
        server_conn.write_frame(deliver_pdu.as_slice()).await.expect("send deliver report");
        tracing::info!("服务器发送Deliver Report成功");
    }

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(client_handler.deliver_count(), 1, "应该收到1个Deliver Report");
    assert_eq!(client_handler.report_count(), 1, "应该收到1个状态报告");

    handle.abort();
}

/// 启动测试服务器并返回优雅停机句柄 + 连接池（用于 B2 服务端停机测试）。
async fn start_test_server_with_shutdown(
    auth_handler: Arc<dyn AuthHandler>,
    biz_handler: Arc<dyn MessageHandler>,
    event_handler: Arc<dyn ServerEventHandler>,
    idle_timeout_secs: u32,
) -> Result<(u16, ServerShutdown, Arc<rsms_connector::ConnectionPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new("test-server", "127.0.0.1", 0, 8, idle_timeout_secs as u16));
    let server = ServerBuilder::new(cfg)
        .message_handlers(vec![biz_handler])
        .auth_handler(auth_handler)
        .account_config_provider(Arc::new(MockAccountConfigProvider::new()) as Arc<dyn AccountConfigProvider>)
        .event_handler(event_handler)
        .serve()
        .await
        .expect("bind");
    let port = server.local_addr.port();
    let sd = server.shutdown_handle();
    let pool = server.pool();
    let handle = tokio::spawn(async move {
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    Ok((port, sd, pool, handle))
}

/// 服务端优雅停机（Phase B2）：`shutdown_handle().shutdown()` 应置停机标志，
/// 并使各连接读循环及时收尾、从连接池注销（pool 归零）。
#[tokio::test]
async fn test_server_graceful_shutdown() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let evt = Arc::new(TestEventHandler::new());
    let (port, sd, pool, handle) =
        start_test_server_with_shutdown(auth.clone(), biz.clone(), evt.clone(), 30)
            .await
            .unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(client_handler.clone())
        .client_config(ClientConfig::new())
        .connect()
        .await
        .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client_handler.is_connected(), "连接失败");
    assert!(pool.len().await >= 1, "服务端应至少有 1 条连接");

    // 优雅停机：无出站在途，drain 立即过，应快速返回并置标志。
    let started = std::time::Instant::now();
    sd.shutdown(Duration::from_secs(2)).await;
    assert!(sd.is_shutting_down(), "应已进入停机");
    assert!(started.elapsed() < Duration::from_secs(2), "无在途时停机应快速返回");

    // 读循环因停机标志退出 → 连接从池注销，pool 归零（≤1s poll + 余量）。
    let mut drained = false;
    for _ in 0..30 {
        if pool.len().await == 0 {
            drained = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(drained, "停机后服务端连接应全部注销（pool 归零）");

    handle.abort();
}

/// 客户端优雅停机（Phase B1）：在途 Resp 已追平时 `shutdown` 应快速返回，并停发停收。
#[tokio::test]
async fn test_client_graceful_shutdown() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let evt = Arc::new(TestEventHandler::new());
    let (port, handle) = start_test_server(auth.clone(), biz.clone(), evt.clone(), 30)
        .await
        .unwrap();

    let spy = Arc::new(DeliverySpy::default());
    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(client_handler.clone())
        .client_config(ClientConfig::new())
        .message_callback(spy.clone() as Arc<dyn MessageCallback>)
        .connect()
        .await
        .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client_handler.is_connected(), "连接失败");

    // 发一条 Submit 并等其 Resp 回来 → 在途 Pending 追平为 0。
    let submit_pdu = client_handler.build_submit_pdu("13800138000", "106900", "Hello");
    conn.send_request(submit_pdu).await.expect("send submit");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(spy.resp_ok.load(Ordering::Relaxed), 1, "Resp 应已追平");

    // 在途已清空，优雅停机应快速返回（drain 不阻塞），并停发停收。
    let started = std::time::Instant::now();
    conn.shutdown(Duration::from_secs(5)).await;
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(3), "在途为 0 时停机应快速返回，实际 {:?}", elapsed);
    assert!(!conn.is_sending(), "停机后应停止出站发送");
    assert!(!conn.ready_for_fetch(), "停机后应停止收发就绪");

    handle.abort();
}

struct RateLimitConfigProvider {
    max_qps: u64,
    window_size_ms: u64,
}

impl RateLimitConfigProvider {
    fn new(max_qps: u64, window_size_ms: u64) -> Self {
        Self { max_qps, window_size_ms }
    }
}

#[async_trait]
impl AccountConfigProvider for RateLimitConfigProvider {
    async fn get_config(&self, _account: &str) -> Result<AccountConfig> {
        Ok(AccountConfig::new()
            .with_max_qps(self.max_qps)
            .with_window_size_ms(self.window_size_ms))
    }
}

async fn start_test_server_with_rate_limit(
    auth_handler: Arc<dyn AuthHandler>,
    biz_handler: Arc<dyn MessageHandler>,
    event_handler: Arc<dyn ServerEventHandler>,
    max_qps: u64,
    window_size_ms: u64,
) -> Result<(u16, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "test-server",
        "127.0.0.1",
        0,
        8,
        30,
    ));
    let config_provider = Arc::new(RateLimitConfigProvider::new(max_qps, window_size_ms));
    let server = ServerBuilder::new(cfg)
        .message_handlers(vec![biz_handler])
        .auth_handler(auth_handler)
        .account_config_provider(config_provider)
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

#[tokio::test]
async fn test_submit_rate_limit_exceeded() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let evt = Arc::new(TestEventHandler::new());
    let (port, handle) = start_test_server_with_rate_limit(auth.clone(), biz.clone(), evt.clone(), 3, 1000)
        .await
        .unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(client_handler.clone())
        .client_config(ClientConfig::new())
        .connect()
        .await
        .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client_handler.is_connected(), "连接失败");

    let mut submit_statuses = Vec::new();
    for i in 0..5 {
        let submit_pdu = client_handler.build_submit_pdu("13800138000", "106900", &format!("Test SMS {}", i));
        let _ = conn.send_request(submit_pdu).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        submit_statuses.push(client_handler.get_submit_status());
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    tracing::info!(
        submit_statuses = ?submit_statuses,
        biz_submit_count = biz.submit_count(),
        "submit results"
    );

    let ok_count = submit_statuses.iter().filter(|s| **s == Some(0)).count();
    let rate_limited_count = submit_statuses.iter().filter(|s| **s == Some(8)).count();

    assert_eq!(ok_count, 3, "前3条应该成功 (result=0)");
    assert_eq!(rate_limited_count, 2, "第4、5条应该被限流 (result=8)");

    handle.abort();
}

/// 使用原始 TCP Socket 测试限流功能
/// 验证服务器端限流是否正常工作
#[tokio::test]
async fn test_submit_rate_limit_with_raw_socket() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let evt = Arc::new(TestEventHandler::new());
    let (port, handle) = start_test_server_with_rate_limit(auth.clone(), biz.clone(), evt.clone(), 3, 1000)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let addr = format!("127.0.0.1:{}", port);
    let mut stream = TcpStream::connect(&addr).await.expect("connect to server");
    tracing::info!("已连接到服务器 port={}", port);

    // 统一 Bind 经 adapter 编码（裸 socket 发送）。
    let bind = UnifiedMessage::Bind(UnifiedBind {
        client_id: "106900".to_string(),
        authenticator: compute_connect_auth("106900", "password123", 0).to_vec(),
        timestamp: 0,
        version: 0x30,
        system_type: None,
        mode: rsms_model::BindMode::default(),
        login_mode: None,
    });
    let connect_bytes = CmppAdapter.encode(&bind, Sequence::Plain(1)).expect("encode bind");
    stream.write_all(&connect_bytes).await.expect("send connect");
    tracing::info!("已发送 Connect PDU");

    let mut resp_buf = [0u8; 1024];
    let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut resp_buf)).await
        .expect("read timeout")
        .expect("read connect resp");
    tracing::info!("收到 ConnectResp: {} bytes", n);

    // 经 adapter 解码 ConnectResp → BindResp，断言 status=0。
    let connect_resp = CmppAdapter
        .decode(&Frame::from(RawPdu::from_vec(resp_buf[..n].to_vec())))
        .expect("decode connect resp");
    match connect_resp {
        UnifiedMessage::BindResp(resp) => {
            assert_eq!(resp.status, 0, "连接应该成功");
            tracing::info!("连接成功");
        }
        _ => panic!("expected BindResp"),
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut submit_results = Vec::new();
    for i in 0..5 {
        // 统一 Submit 经 adapter 编码（裸 socket 发送）。
        let submit = UnifiedMessage::Submit(UnifiedSubmit {
            src: Address::plain("106900"),
            dests: vec![Address::plain("13800138000")],
            content: format!("Test SMS {}", i).into_bytes(),
            encoding: Encoding::Ascii,
            want_report: false,
            concat: None,
            extra: ProtocolExtra::Cmpp(Default::default()),
            tlvs: vec![],
        });
        let seq_id = 100 + i as u32;
        let submit_bytes = CmppAdapter.encode(&submit, Sequence::Plain(seq_id)).expect("encode submit");

        stream.write_all(&submit_bytes).await.expect("send submit");
        tracing::info!("已发送 Submit #{} (seq={})", i + 1, seq_id);

        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut resp_buf = [0u8; 1024];
        match tokio::time::timeout(Duration::from_secs(2), stream.read(&mut resp_buf)).await {
            Ok(Ok(n)) if n > 0 => {
                // 经 adapter 解码响应；命中 SubmitResp 取 status，否则记 999。
                match CmppAdapter.decode(&Frame::from(RawPdu::from_vec(resp_buf[..n].to_vec()))) {
                    Ok(UnifiedMessage::SubmitResp(resp)) => {
                        tracing::info!("收到 SubmitResp #{}: result={}", i + 1, resp.status);
                        submit_results.push(resp.status);
                    }
                    other => {
                        tracing::warn!("收到非 SubmitResp 消息: {:?}", other);
                        submit_results.push(999);
                    }
                }
            }
            Ok(Ok(_)) | Err(_) => {
                tracing::warn!("Submit #{} 超时或无响应", i + 1);
                submit_results.push(999);
            }
            Ok(Err(e)) => {
                tracing::warn!("读取错误: {}", e);
                submit_results.push(999);
            }
        }
    }

    let ok_count = submit_results.iter().filter(|r| **r == 0).count();
    let rate_limited_count = submit_results.iter().filter(|r| **r == 8).count();

    tracing::info!(
        submit_results = ?submit_results,
        ok_count,
        rate_limited_count,
        "限流测试结果"
    );

    assert_eq!(ok_count, 3, "前3条应该成功 (result=0)");
    assert_eq!(rate_limited_count, 2, "第4、5条应该被限流 (result=8)");
    assert_eq!(biz.submit_count(), 3, "服务器应该只处理了3条消息");

    drop(stream);
    handle.abort();
}

#[tokio::test]
async fn test_auth_correct_password() {
    let auth_handler = Arc::new(PasswordAuthHandler::new().add_account("900001", "testpass"));
    let biz_handler = Arc::new(TestBusinessHandler::new());
    let evt_handler = Arc::new(TestEventHandler::new());
    let (port, _guard) = start_test_server(auth_handler.clone(), biz_handler, evt_handler, 30).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test", "127.0.0.1", port, 100, 60));
    let handler = Arc::new(TestClientHandler::new());
    let ts = Arc::new(TestClientEventHandler::new());
    let ms = Arc::new(TestMessageSource);

    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(handler.clone())
        .client_config(ClientConfig::new())
        .message_source(ms)
        .event_handler(ts)
        .connect()
        .await
        .unwrap();

    let connect_pdu = handler.build_connect_pdu("900001", "testpass");
    conn.send_request(connect_pdu).await.unwrap();

    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if handler.is_connected() {
            break;
        }
    }

    assert_eq!(handler.get_connect_status(), Some(0), "正确密码应该返回status=0");
    assert_eq!(auth_handler.auth_success_count(), 1);
}

#[tokio::test]
async fn test_auth_wrong_password() {
    let auth_handler = Arc::new(PasswordAuthHandler::new().add_account("900001", "correct"));
    let biz_handler = Arc::new(TestBusinessHandler::new());
    let evt_handler = Arc::new(TestEventHandler::new());
    let (port, _guard) = start_test_server(auth_handler.clone(), biz_handler, evt_handler, 30).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test", "127.0.0.1", port, 100, 60));
    let handler = Arc::new(TestClientHandler::new());
    let ts = Arc::new(TestClientEventHandler::new());
    let ms = Arc::new(TestMessageSource);

    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(handler.clone())
        .client_config(ClientConfig::new())
        .message_source(ms)
        .event_handler(ts)
        .connect()
        .await
        .unwrap();

    let connect_pdu = handler.build_connect_pdu("900001", "wrong");
    conn.send_request(connect_pdu).await.unwrap();

    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Some(status) = handler.get_connect_status() {
            if status != 0 {
                break;
            }
        }
    }

    assert_ne!(handler.get_connect_status(), Some(0), "错误密码应该返回非0状态");
    assert_eq!(auth_handler.auth_fail_count(), 1);
}

#[tokio::test]
async fn test_auth_unknown_account() {
    let auth_handler = Arc::new(PasswordAuthHandler::new());
    let biz_handler = Arc::new(TestBusinessHandler::new());
    let evt_handler = Arc::new(TestEventHandler::new());
    let (port, _guard) = start_test_server(auth_handler.clone(), biz_handler, evt_handler, 30).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test", "127.0.0.1", port, 100, 60));
    let handler = Arc::new(TestClientHandler::new());
    let ts = Arc::new(TestClientEventHandler::new());
    let ms = Arc::new(TestMessageSource);

    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(handler.clone())
        .client_config(ClientConfig::new())
        .message_source(ms)
        .event_handler(ts)
        .connect()
        .await
        .unwrap();

    let connect_pdu = handler.build_connect_pdu("123456", "pwd");
    conn.send_request(connect_pdu).await.unwrap();

    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Some(status) = handler.get_connect_status() {
            if status != 0 {
                break;
            }
        }
    }

    assert_ne!(handler.get_connect_status(), Some(0), "未知账号应该失败");
}

#[tokio::test]
async fn test_submit_message() {
    let auth_handler = Arc::new(PasswordAuthHandler::new().add_account("900001", "pwd"));
    let biz_handler = Arc::new(TestBusinessHandler::new());
    let evt_handler = Arc::new(TestEventHandler::new());
    let (port, _guard) = start_test_server(auth_handler, biz_handler.clone(), evt_handler, 30).await.unwrap();

    let endpoint = Arc::new(EndpointConfig::new("test", "127.0.0.1", port, 100, 60));
    let handler = Arc::new(TestClientHandler::new());
    let ts = Arc::new(TestClientEventHandler::new());
    let ms = Arc::new(TestMessageSource);

    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(handler.clone())
        .client_config(ClientConfig::new())
        .message_source(ms)
        .event_handler(ts)
        .connect()
        .await
        .unwrap();

    let connect_pdu = handler.build_connect_pdu("900001", "pwd");
    conn.send_request(connect_pdu).await.unwrap();

    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if handler.is_connected() {
            break;
        }
    }

    assert!(handler.is_connected(), "应该连接成功");

    tokio::time::sleep(Duration::from_millis(50)).await;

    let submit_pdu = handler.build_submit_pdu("900001", "13800138000", "Hello Test");
    conn.send_request(submit_pdu).await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(handler.get_submit_status(), Some(0), "短信应该发送成功");
}

#[test]
fn test_compute_auth() {
use rsms_codec_cmpp::auth::compute_connect_auth;
    let auth = compute_connect_auth("900001", "pwd", 0);
    assert_eq!(auth.len(), 16);
}

/// 交付链路回调测试桩：原子计数验证框架自动驱动了对应回调。
#[derive(Default)]
struct DeliverySpy {
    resp_ok: AtomicUsize,
    resp_err: AtomicUsize,
    reports: AtomicUsize,
    send_failed: AtomicUsize,
}

#[async_trait]
impl MessageCallback for DeliverySpy {
    async fn on_submit_resp_ok(&self, _: &SubmitInfo) {
        self.resp_ok.fetch_add(1, Ordering::Relaxed);
    }
    async fn on_submit_resp_error(&self, _: &SubmitInfo, _: u32) {
        self.resp_err.fetch_add(1, Ordering::Relaxed);
    }
    async fn on_submit_resp_timeout(&self, _: &SubmitInfo) {}
    async fn on_send_failed(&self, _: u32, _: &str) {
        self.send_failed.fetch_add(1, Ordering::Relaxed);
    }
    async fn on_report(&self, _: &ReportInfo, _: &SubmitInfo) {
        self.reports.fetch_add(1, Ordering::Relaxed);
    }
    async fn on_mo(&self, _: &MoInfo) {}
}

/// 交付链路自动驱动（Phase A 核心）：客户端只 `impl MessageCallback`，框架即自动
/// 登记出站 Submit、收到 SubmitResp/Report 自动回调，无需在 ClientHandler 里手工解析驱动 TM。
#[tokio::test]
async fn test_delivery_callback_autowire() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let auth = Arc::new(PasswordAuthHandler::new().add_account("106900", "password123"));
    let biz = Arc::new(TestBusinessHandler::new());
    let evt = Arc::new(TestEventHandler::new());
    let (port, pool, handle) = start_test_server_with_pool(auth.clone(), biz.clone(), evt.clone(), 30)
        .await
        .unwrap();

    let spy = Arc::new(DeliverySpy::default());
    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let client_handler = Arc::new(TestClientHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(client_handler.clone())
        .client_config(ClientConfig::new())
        .message_callback(spy.clone() as Arc<dyn MessageCallback>)
        .connect()
        .await
        .expect("connect");

    let connect_pdu = client_handler.build_connect_pdu("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client_handler.is_connected(), "连接失败");

    // 发 Submit：框架自动 register_outbound；服务端回 SubmitResp(msg_id=[0;8], status=0)，
    // 读循环 drive_inbound → on_submit_resp_ok（并把事务键 seq 改写为 msg_id）。
    let submit_pdu = client_handler.build_submit_pdu("13800138000", "106900", "Hello");
    conn.send_request(submit_pdu).await.expect("send submit");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(spy.resp_ok.load(Ordering::Relaxed), 1, "应自动回调 on_submit_resp_ok 一次");
    assert_eq!(spy.send_failed.load(Ordering::Relaxed), 0, "正常路径不应触发 on_send_failed");

    // 服务端主动发状态报告（msg_id 与 SubmitResp 一致=[0;8]）→ drive_inbound 按 msg_id 关联 → on_report。
    let msg_id: [u8; 8] = [0u8; 8];
    if let Some(server_conn) = get_conn_from_pool(&pool).await {
        let report_pdu = build_deliver_report(200, &msg_id, "106900");
        server_conn.write_frame(report_pdu.as_slice()).await.expect("send report");
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(spy.reports.load(Ordering::Relaxed), 1, "应自动回调 on_report 并按 msg_id 关联到原 Submit");

    handle.abort();
}