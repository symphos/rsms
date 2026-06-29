use async_trait::async_trait;
use rsms_connector::transaction::{MessageCallback, SubmitInfo, ReportInfo, MoInfo, TransactionManager};
use rsms_connector::{ClientBuilder, CmppDecoder, AccountConnections, AccountConfig, NoopClientHandler};
use rsms_core::{ConnectionInfo, EndpointConfig, Protocol, Result};
use rsms_business::{MessageContext, MessageHandler};
// 窄腰统一模型：编解码经 CmppAdapter；compute_connect_auth 是 MD5 鉴权工具，保留。
use rsms_codec_cmpp::adapter::CmppAdapter;
use rsms_codec_cmpp::auth::compute_connect_auth;
use rsms_model::{
    Address, MessageId, ProtocolAdapter, ProtocolExtra, Sequence, UnifiedBind, UnifiedMessage,
    UnifiedSubmit, UnifiedSubmitResp,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const TEST_ACCOUNT: &str = "900001";
const TEST_PASSWORD: &str = "pwd";

#[allow(dead_code)]
struct TransactionTestHandler {
    /// 事务匹配驱动器：持有对 AccountConnections 同一 TM 的引用，在 on_message 里手动驱动。
    tm: Option<Arc<TransactionManager>>,
    seq: AtomicU64,
    pending_seq: tokio::sync::Mutex<Option<u32>>,
    connected: std::sync::atomic::AtomicBool,
    submit_ok_count: AtomicU64,
    submit_error_count: AtomicU64,
    report_count: AtomicU64,
    mo_count: AtomicU64,
}

impl TransactionTestHandler {
    fn new() -> Self {
        Self {
            tm: None,
            seq: AtomicU64::new(1),
            pending_seq: tokio::sync::Mutex::new(None),
            connected: std::sync::atomic::AtomicBool::new(false),
            submit_ok_count: Default::default(),
            submit_error_count: Default::default(),
            report_count: Default::default(),
            mo_count: Default::default(),
        }
    }

    /// 注入 TM 引用（与测试里 acc_connections.transaction_manager() 同一实例）。
    fn with_tm(mut self, tm: Arc<TransactionManager>) -> Self {
        self.tm = Some(tm);
        self
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u32
    }
}

#[async_trait]
impl MessageHandler for TransactionTestHandler {
    fn name(&self) -> &'static str {
        "transaction-test-handler"
    }

    async fn on_message(&self, _ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        // 窄腰路径：框架已 decode，直接 match 统一消息枚举。
        // BindResp(status=0) 置 connected；SubmitResp 取 msg_id/status 驱动事务匹配。
        match msg {
            UnifiedMessage::BindResp(resp) if resp.status == 0 => {
                self.connected.store(true, Ordering::Relaxed);
            }
            UnifiedMessage::SubmitResp(resp) => {
                // 事务匹配仍用 8 字节二进制 MsgId 的 hex 串（与原断言一致）。
                let msg_id: String = match &resp.msg_id {
                    MessageId::Binary(b) => b.iter().map(|x| format!("{:02x}", x)).collect(),
                    MessageId::Text(t) => t.clone(),
                };
                if let Some(tm) = &self.tm {
                    let pending = self.pending_seq.lock().await.take();
                    if let Some(pending_seq) = pending {
                        tm.on_submit_resp(pending_seq, Some(msg_id), resp.status).await;
                    }
                }
            }
            _ => {}
        }

        Ok(())
    }
}

#[derive(Default)]
struct TestTransactionCallback {
    submit_ok_count: AtomicU64,
    submit_error_count: AtomicU64,
    report_count: AtomicU64,
    mo_count: AtomicU64,
}

#[async_trait::async_trait]
impl MessageCallback for TestTransactionCallback {
    async fn on_submit_resp_ok(&self, info: &SubmitInfo) {
        self.submit_ok_count.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            "[Callback] SubmitResp OK: seq={} msg_id={:?} dest={} src={}",
            info.sequence_id, info.msg_id, info.dest_id, info.src_id
        );
    }

    async fn on_submit_resp_error(&self, info: &SubmitInfo, result: u32) {
        self.submit_error_count.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            "[Callback] SubmitResp ERROR: seq={} result={} dest={} src={}",
            info.sequence_id, result, info.dest_id, info.src_id
        );
    }

    async fn on_submit_resp_timeout(&self, info: &SubmitInfo) {
        tracing::warn!("[Callback] SubmitResp TIMEOUT: seq={}", info.sequence_id);
    }

    async fn on_send_failed(&self, sequence_id: u32, error: &str) {
        tracing::error!("[Callback] SendFailed: seq={} error={}", sequence_id, error);
    }

    async fn on_report(&self, report: &ReportInfo, original_submit: &SubmitInfo) {
        self.report_count.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            "[Callback] Report: msg_id={} stat={} original_seq={} dest={}",
            report.msg_id, report.stat, original_submit.sequence_id, original_submit.dest_id
        );
    }

    async fn on_mo(&self, mo: &MoInfo) {
        self.mo_count.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            "[Callback] MO: dest={} src={}",
            mo.dest_id, mo.src_terminal_id
        );
    }
}

use rsms_connector::ServerBuilder;
use rsms_connector::AuthHandler;

struct TestBusinessHandler;

impl TestBusinessHandler {
    fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl MessageHandler for TestBusinessHandler {
    fn name(&self) -> &'static str {
        "test-business"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        // 窄腰路径：框架已 decode，直接 match 统一消息枚举。
        // 收到 Submit 即回 SubmitResp（已认证 status=0，否则 3）。msg_id 置全 0（与原一致）。
        if let UnifiedMessage::Submit(_) = msg {
            let status = if ctx.conn.authenticated_account().await.is_some() { 0 } else { 3 };
            ctx.reply(UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                msg_id: MessageId::Binary(vec![0u8; 8]),
                status,
            })).await?;
        }
        Ok(())
    }
}

pub struct TestAuthHandler {
    accounts: std::collections::HashMap<String, String>,
}

impl TestAuthHandler {
    fn new() -> Self {
        let mut h = Self { accounts: Default::default() };
        h.accounts.insert(TEST_ACCOUNT.to_string(), TEST_PASSWORD.to_string());
        h
    }
}

#[async_trait::async_trait]
impl AuthHandler for TestAuthHandler {
    fn name(&self) -> &'static str {
        "test-auth"
    }

    async fn authenticate(&self, _client_id: &str, credentials: rsms_connector::AuthCredentials, _conn_info: &ConnectionInfo) -> Result<rsms_connector::AuthResult> {
        if let rsms_connector::AuthCredentials::Cmpp {
            source_addr,
            authenticator_source,
            timestamp,
            ..
        } = credentials
        {
            if let Some(password) = self.accounts.get(&source_addr) {
                let expected = compute_connect_auth(&source_addr, password, timestamp);
                if expected == authenticator_source {
                    return Ok(rsms_connector::AuthResult::success(&source_addr));
                }
            }
        }
        Ok(rsms_connector::AuthResult::failure(1, "Invalid"))
    }
}

fn get_test_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static COUNTER: AtomicU16 = AtomicU16::new(0);
    30000 + COUNTER.fetch_add(1, Ordering::Relaxed)
}

async fn start_test_server(port: u16) -> tokio::task::JoinHandle<()> {
    let config = Arc::new(
        EndpointConfig::new("cmpp-test", "127.0.0.1", port, 100, 60).with_protocol(Protocol::Cmpp),
    );

    let auth_handler: Arc<dyn AuthHandler> = Arc::new(TestAuthHandler::new());
    let biz_handler: Arc<dyn MessageHandler> = Arc::new(TestBusinessHandler::new());

    let server = ServerBuilder::new(config)
        .message_handlers(vec![biz_handler])
        .auth_handler(auth_handler)
        .serve()
        .await
        .unwrap();

    tokio::spawn(async move {
        let _ = server.run().await;
    })
}

/// 4B.1 回归：连接断开时应立即 drain 所有在途 send_request 请求并返回 Err，
/// 而非让它们各自挂到 endpoint.timeout（断开-重连场景的延迟尖峰）。
#[tokio::test]
async fn test_pending_requests_fail_immediately_on_disconnect() {
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    // 假 server：accept 后只读不回，使请求永远收不到响应。
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while sock.read(&mut buf).await.unwrap_or(0) > 0 {}
            });
        }
    });

    // timeout 设 30s，以区分「断开立即失败」与「挂到超时失败」。
    let endpoint = Arc::new(
        EndpointConfig::new("test-4b1", "127.0.0.1", port, 100, 60)
            .with_timeout(Duration::from_secs(30)),
    );
    // 此测试无需 TM 驱动，handler 不传 tm。
    let handler = Arc::new(TransactionTestHandler::new());
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(handler)
        .client_config(Default::default())
        .connect()
        .await
        .unwrap();

    // 发一个请求，server 永不回复 → 进入 pending_responses 等待。
    let connect = UnifiedMessage::Bind(UnifiedBind {
        client_id: TEST_ACCOUNT.to_string(),
        authenticator: vec![0u8; 16],
        timestamp: 0,
        version: 0x30,
        system_type: None,
        mode: rsms_model::BindMode::default(),
        login_mode: None,
    });
    let connect_bytes = CmppAdapter.encode(&connect, Sequence::Plain(1)).unwrap();
    let fut = conn
        .send_request(rsms_core::RawPdu::from_vec(connect_bytes))
        .await
        .expect("send_request 应成功入队");

    // 断开连接：应立即 drain 在途请求并使其失败。
    conn.mark_disconnected().await;

    // future 应在远短于 timeout(30s) 内返回 Err(ConnectionClosed)。
    match tokio::time::timeout(Duration::from_secs(2), fut).await {
        Ok(Err(rsms_core::RsmsError::ConnectionClosed(_))) => {}
        Ok(other) => panic!("期望 ConnectionClosed Err，实得 {:?}", other),
        Err(_) => panic!("future 未在 2s 内返回——断开未 drain 在途请求（回归）"),
    }
}

#[tokio::test]
async fn test_transaction_manager_integration() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let port = get_test_port();
    let server_handle = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let acc_config = AccountConfig::new();
    let acc_connections = AccountConnections::new(TEST_ACCOUNT.to_string(), acc_config);

    let tm = acc_connections.transaction_manager();
    let callback = Arc::new(TestTransactionCallback::default());
    tm.set_callback(Some(callback.clone())).await;
    let tm_clone = Arc::clone(&tm);
    tm_clone.start_timeout_checker().await;

    let endpoint = Arc::new(EndpointConfig::new("test", "127.0.0.1", port, 100, 60));
    // 把 TM 注入 handler，on_message 里直接用 self.tm 驱动事务匹配（与 acc_connections 同一实例）。
    let handler = Arc::new(TransactionTestHandler::new().with_tm(Arc::clone(&tm)));

    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(handler.clone())
        .client_config(Default::default())
        .connect()
        .await
        .unwrap();

    conn.set_account_connections(Some(acc_connections.clone())).await;

    let timestamp = 0u32;
    let authenticator = compute_connect_auth(TEST_ACCOUNT, TEST_PASSWORD, timestamp);
    let connect = UnifiedMessage::Bind(UnifiedBind {
        client_id: TEST_ACCOUNT.to_string(),
        authenticator: authenticator.to_vec(),
        timestamp,
        version: 0x30,
        system_type: None,
        mode: rsms_model::BindMode::default(),
        login_mode: None,
    });
    let connect_bytes = CmppAdapter.encode(&connect, Sequence::Plain(handler.next_seq())).unwrap();
    conn.send_request(rsms_core::RawPdu::from_vec(connect_bytes)).await.unwrap();

    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if handler.connected.load(Ordering::Relaxed) {
            break;
        }
    }
    assert!(handler.connected.load(Ordering::Relaxed), "应该连接成功");

    let seq = handler.next_seq();
    *handler.pending_seq.lock().await = Some(seq);

    // 统一 Submit（want_report=true 对应 registered_delivery=1）。
    let submit = UnifiedMessage::Submit(UnifiedSubmit {
        src: Address::plain(TEST_ACCOUNT),
        dests: vec![Address::plain("13800138000")],
        content: b"Test Transaction".to_vec(),
        encoding: rsms_model::Encoding::Ascii,
        want_report: true,
        concat: None,
        extra: ProtocolExtra::Cmpp(Default::default()),
        tlvs: vec![],
    });
    let submit_bytes = CmppAdapter.encode(&submit, Sequence::Plain(seq)).unwrap();

    let info = SubmitInfo::new(seq, "13800138000".to_string(), TEST_ACCOUNT.to_string(), b"Test Transaction".to_vec(), "CMPP");
    tm.add_submit_transaction(info).await;

    conn.send_request(rsms_core::RawPdu::from_vec(submit_bytes)).await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(callback.submit_ok_count.load(Ordering::Relaxed), 1, "应该收到 SubmitResp OK");
    assert_eq!(callback.submit_error_count.load(Ordering::Relaxed), 0, "不应该有错误");

    drop(conn);
    tokio::time::sleep(Duration::from_millis(50)).await;
    tm.stop_timeout_checker().await;
    server_handle.abort();
}

#[tokio::test]
async fn test_transaction_manager_multiple_submits() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let port = get_test_port();
    let server_handle = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let acc_config = AccountConfig::new();
    let acc_connections = AccountConnections::new(TEST_ACCOUNT.to_string(), acc_config);

    let tm = acc_connections.transaction_manager();
    let callback = Arc::new(TestTransactionCallback::default());
    tm.set_callback(Some(callback.clone())).await;

    let endpoint = Arc::new(EndpointConfig::new("test", "127.0.0.1", port, 100, 60));
    // 把 TM 注入 handler，on_message 里直接用 self.tm 驱动事务匹配（与 acc_connections 同一实例）。
    let handler = Arc::new(TransactionTestHandler::new().with_tm(Arc::clone(&tm)));

    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), CmppDecoder)
        .with_message_handler(handler.clone())
        .client_config(Default::default())
        .connect()
        .await
        .unwrap();

    conn.set_account_connections(Some(acc_connections.clone())).await;

    let timestamp = 0u32;
    let authenticator = compute_connect_auth(TEST_ACCOUNT, TEST_PASSWORD, timestamp);
    let connect = UnifiedMessage::Bind(UnifiedBind {
        client_id: TEST_ACCOUNT.to_string(),
        authenticator: authenticator.to_vec(),
        timestamp,
        version: 0x30,
        system_type: None,
        mode: rsms_model::BindMode::default(),
        login_mode: None,
    });
    let connect_bytes = CmppAdapter.encode(&connect, Sequence::Plain(handler.next_seq())).unwrap();
    conn.send_request(rsms_core::RawPdu::from_vec(connect_bytes)).await.unwrap();

    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if handler.connected.load(Ordering::Relaxed) {
            break;
        }
    }
    assert!(handler.connected.load(Ordering::Relaxed), "应该连接成功");

    for i in 0..5 {
        let seq = handler.next_seq();
        *handler.pending_seq.lock().await = Some(seq);

        let submit = UnifiedMessage::Submit(UnifiedSubmit {
            src: Address::plain(TEST_ACCOUNT),
            dests: vec![Address::plain(format!("138{:08}", i))],
            content: format!("Test {}", i).into_bytes(),
            encoding: rsms_model::Encoding::Ascii,
            want_report: true,
            concat: None,
            extra: ProtocolExtra::Cmpp(Default::default()),
            tlvs: vec![],
        });
        let submit_bytes = CmppAdapter.encode(&submit, Sequence::Plain(seq)).unwrap();

        let info = SubmitInfo::new(seq, format!("138{:08}", i), TEST_ACCOUNT.to_string(), format!("Test {}", i).into_bytes(), "CMPP");
        tm.add_submit_transaction(info).await;

        conn.send_request(rsms_core::RawPdu::from_vec(submit_bytes)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(callback.submit_ok_count.load(Ordering::Relaxed), 5, "应该收到 5 个 SubmitResp OK");

    drop(conn);
    tokio::time::sleep(Duration::from_millis(50)).await;
    server_handle.abort();
}
