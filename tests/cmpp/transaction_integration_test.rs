use async_trait::async_trait;
use rsms_connector::client::{ClientContext, ClientHandler};
use rsms_connector::transaction::{MessageCallback, SubmitInfo, ReportInfo, MoInfo};
use rsms_connector::{connect, CmppDecoder, AccountConnections, AccountConfig};
use rsms_core::{ConnectionInfo, EndpointConfig, Frame, Result};
use rsms_codec_cmpp::{Pdu, Connect, CommandId};
use rsms_codec_cmpp::auth::compute_connect_auth;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const TEST_ACCOUNT: &str = "900001";
const TEST_PASSWORD: &str = "pwd";

#[allow(dead_code)]
struct TransactionTestHandler {
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
            seq: AtomicU64::new(1),
            pending_seq: tokio::sync::Mutex::new(None),
            connected: std::sync::atomic::AtomicBool::new(false),
            submit_ok_count: Default::default(),
            submit_error_count: Default::default(),
            report_count: Default::default(),
            mo_count: Default::default(),
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u32
    }
}

#[async_trait]
impl ClientHandler for TransactionTestHandler {
    fn name(&self) -> &'static str {
        "transaction-test-handler"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        let cmd_id = frame.command_id;

        if cmd_id == CommandId::ConnectResp as u32 && pdu.len() >= 13 && pdu[12] == 0 {
            self.connected.store(true, Ordering::Relaxed);
        } else if cmd_id == CommandId::SubmitResp as u32 && pdu.len() >= 24 {
            let result = u32::from_be_bytes([pdu[20], pdu[21], pdu[22], pdu[23]]);
            let msg_id = format!(
                "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                pdu[12], pdu[13], pdu[14], pdu[15], pdu[16], pdu[17], pdu[18], pdu[19]
            );

            if let Some(tm) = ctx.conn.transaction_manager().await {
                let pending = self.pending_seq.lock().await.take();
                if let Some(pending_seq) = pending {
                    tm.on_submit_resp(pending_seq, Some(msg_id), result).await;
                }
            }
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

use rsms_connector::serve;
use rsms_connector::AuthHandler;
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;

struct TestBusinessHandler;

impl TestBusinessHandler {
    fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl BusinessHandler for TestBusinessHandler {
    fn name(&self) -> &'static str {
        "test-business"
    }

    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        if let Ok(msg) = rsms_codec_cmpp::decode_message(frame.data_as_slice()) {
            match msg {
                rsms_codec_cmpp::CmppMessage::SubmitV30 { submit: _s, .. } => {
                    let result = if ctx.conn.authenticated_account().await.is_some() { 0 } else { 3 };
                    let resp = rsms_codec_cmpp::SubmitResp {
                        msg_id: [0u8; 8],
                        result,
                    };
                    let resp_pdu: Pdu = resp.into();
                    ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;
                }
                rsms_codec_cmpp::CmppMessage::SubmitV20 { submit: _s, .. } => {
                    let result = if ctx.conn.authenticated_account().await.is_some() { 0 } else { 3 };
                    let resp = rsms_codec_cmpp::SubmitResp {
                        msg_id: [0u8; 8],
                        result,
                    };
                    let resp_pdu: Pdu = resp.into();
                    ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;
                }
                _ => {}
            }
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
        EndpointConfig::new("cmpp-test", "127.0.0.1", port, 100, 60).with_protocol("cmpp"),
    );

    let auth_handler: Arc<dyn AuthHandler> = Arc::new(TestAuthHandler::new());
    let biz_handler: Arc<dyn BusinessHandler> = Arc::new(TestBusinessHandler::new());
    
    let server = serve(
        config,
        vec![biz_handler],
        Some(auth_handler),
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    tokio::spawn(async move {
        let _ = server.run().await;
    })
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
    let handler = Arc::new(TransactionTestHandler::new());
    
    let conn = connect(
        endpoint,
        handler.clone(),
        CmppDecoder,
        Some(Default::default()),
        None,
        None,
    )
    .await
    .unwrap();

    conn.set_account_connections(Some(acc_connections.clone())).await;

    let timestamp = 0u32;
    let authenticator = compute_connect_auth(TEST_ACCOUNT, TEST_PASSWORD, timestamp);
    let connect = Connect {
        source_addr: TEST_ACCOUNT.to_string(),
        authenticator_source: authenticator,
        version: 0x30,
        timestamp,
    };
    let connect_pdu: Pdu = connect.into();
    let connect_bytes = connect_pdu.to_pdu_bytes(handler.next_seq());
    conn.send_request(connect_bytes).await.unwrap();

    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if handler.connected.load(Ordering::Relaxed) {
            break;
        }
    }
    assert!(handler.connected.load(Ordering::Relaxed), "应该连接成功");

    let seq = handler.next_seq();
    *handler.pending_seq.lock().await = Some(seq);

    let mut submit = rsms_codec_cmpp::datatypes::Submit::new();
    submit.src_id = TEST_ACCOUNT.to_string();
    submit.dest_usr_tl = 1;
    submit.dest_terminal_ids = vec!["13800138000".to_string()];
    submit.msg_content = b"Test Transaction".to_vec();
    submit.registered_delivery = 1;
    let submit_pdu: Pdu = submit.into();
    let submit_bytes = submit_pdu.to_pdu_bytes(seq);
    
    let info = SubmitInfo::new(seq, "13800138000".to_string(), TEST_ACCOUNT.to_string(), b"Test Transaction".to_vec(), "CMPP");
    tm.add_submit_transaction(info).await;
    
    conn.send_request(submit_bytes).await.unwrap();

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
    let handler = Arc::new(TransactionTestHandler::new());
    
    let conn = connect(
        endpoint,
        handler.clone(),
        CmppDecoder,
        Some(Default::default()),
        None,
        None,
    )
    .await
    .unwrap();

    conn.set_account_connections(Some(acc_connections.clone())).await;

    let timestamp = 0u32;
    let authenticator = compute_connect_auth(TEST_ACCOUNT, TEST_PASSWORD, timestamp);
    let connect = Connect {
        source_addr: TEST_ACCOUNT.to_string(),
        authenticator_source: authenticator,
        version: 0x30,
        timestamp,
    };
    let connect_pdu: Pdu = connect.into();
    let connect_bytes = connect_pdu.to_pdu_bytes(handler.next_seq());
    conn.send_request(connect_bytes).await.unwrap();

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

        let mut submit = rsms_codec_cmpp::datatypes::Submit::new();
        submit.src_id = TEST_ACCOUNT.to_string();
        submit.dest_usr_tl = 1;
        submit.dest_terminal_ids = vec![format!("138{:08}", i)];
        submit.msg_content = format!("Test {}", i).into_bytes();
        submit.registered_delivery = 1;
        let submit_pdu: Pdu = submit.into();
        let submit_bytes = submit_pdu.to_pdu_bytes(seq);
        
        let info = SubmitInfo::new(seq, format!("138{:08}", i), TEST_ACCOUNT.to_string(), format!("Test {}", i).into_bytes(), "CMPP");
        tm.add_submit_transaction(info).await;
        
        conn.send_request(submit_bytes).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(callback.submit_ok_count.load(Ordering::Relaxed), 5, "应该收到 5 个 SubmitResp OK");

    drop(conn);
    tokio::time::sleep(Duration::from_millis(50)).await;
    server_handle.abort();
}
