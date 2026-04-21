use async_trait::async_trait;
use rsms_connector::{
    connect, AuthCredentials, AuthHandler, AuthResult,
    ClientEventHandler, ClientHandler, MessageSource, MessageItem, ProtocolConnection,
    ServerEventHandler, AccountConfig, AccountConfigProvider,
    CmppDecoder,
};
use rsms_connector::client::{ClientContext, ClientConfig};
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_core::{ConnectionInfo, RawPdu, EndpointConfig, Frame, Result};
use rsms_codec_cmpp::{decode_message, CmppMessage, Pdu, Connect, Deliver, DeliverResp, Terminate, ActiveTest, CommandId, Submit, SubmitResp, Decodable};
use rsms_codec_cmpp::codec::PduHeader;
use rsms_codec_cmpp::auth::compute_connect_auth;
use rsms_test_common::{TestEventHandler, TestClientEventHandler, TestMessageSource, MockAccountConfigProvider};
use std::collections::HashMap;
use std::io::Cursor;
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
impl BusinessHandler for TestBusinessHandler {
    fn name(&self) -> &'static str {
        "test-biz-handler"
    }

    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        if let Ok(msg) = decode_message(frame.data_as_slice()) {
            match msg {
                CmppMessage::SubmitV30 { submit: s, .. } => {
                    let result = if ctx.conn.authenticated_account().await.is_some() {
                        if let Some(limiter) = ctx.conn.rate_limiter().await {
                            if !limiter.try_acquire().await {
                                8
                            } else {
                                self.submit_count.fetch_add(1, Ordering::Relaxed);
                                let content = String::from_utf8_lossy(&s.msg_content).to_string();
                                self.messages.lock().unwrap().push(content);
                                0
                            }
                        } else {
                            self.submit_count.fetch_add(1, Ordering::Relaxed);
                            let content = String::from_utf8_lossy(&s.msg_content).to_string();
                            self.messages.lock().unwrap().push(content);
                            0
                        }
                    } else {
                        3
                    };
                    let resp = SubmitResp {
                        msg_id: [0u8; 8],
                        result,
                    };
                    let resp_pdu: Pdu = resp.into();
                    ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;
                }
                CmppMessage::SubmitV20 { submit: s, .. } => {
                    let result = if ctx.conn.authenticated_account().await.is_some() {
                        if let Some(limiter) = ctx.conn.rate_limiter().await {
                            if !limiter.try_acquire().await {
                                8
                            } else {
                                self.submit_count.fetch_add(1, Ordering::Relaxed);
                                let content = String::from_utf8_lossy(&s.msg_content).to_string();
                                self.messages.lock().unwrap().push(content);
                                0
                            }
                        } else {
                            self.submit_count.fetch_add(1, Ordering::Relaxed);
                            let content = String::from_utf8_lossy(&s.msg_content).to_string();
                            self.messages.lock().unwrap().push(content);
                            0
                        }
                    } else {
                        3
                    };
                    let resp = SubmitResp {
                        msg_id: [0u8; 8],
                        result,
                    };
                    let resp_pdu: Pdu = resp.into();
                    ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;
                }
                CmppMessage::DeliverV30 { deliver: d, .. } => {
                    if d.registered_delivery == 1 {
                        let report = String::from_utf8_lossy(&d.msg_content).to_string();
                        self.reports.lock().unwrap().push(report);
                    } else {
                        self.mo_messages.lock().unwrap().push((
                            d.src_terminal_id.clone(),
                            String::from_utf8_lossy(&d.msg_content).to_string(),
                        ));
                    }
                }
                _ => {}
            }
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

        let connect = Connect {
            source_addr: account.to_string(),
            authenticator_source: auth,
            version: 0x30,
            timestamp,
        };

        let pdu: Pdu = connect.into();
        pdu.to_pdu_bytes(self.next_seq())
    }

    pub fn build_submit_pdu(&self, src: &str, dst: &str, content: &str) -> RawPdu {
        let mut submit = rsms_codec_cmpp::Submit::new();
        submit.src_id = src.to_string();
        submit.dest_usr_tl = 1;
        submit.dest_terminal_ids = vec![dst.to_string()];
        submit.msg_content = content.as_bytes().to_vec();
        submit.registered_delivery = 1;

        let pdu: Pdu = submit.into();
        pdu.to_pdu_bytes(self.next_seq())
    }

    pub fn build_active_test_pdu(&self) -> RawPdu {
        let at = ActiveTest;
        let pdu: Pdu = at.into();
        pdu.to_pdu_bytes(self.next_seq())
    }

    pub fn build_terminate_pdu(&self) -> RawPdu {
        let term = Terminate;
        let pdu: Pdu = term.into();
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
            self.deliver_count.fetch_add(1, Ordering::Relaxed);
            if let Ok(msg) = decode_message(pdu) {
                if let CmppMessage::DeliverV30 { deliver: d, .. } = msg {
                    if d.registered_delivery == 1 {
                        self.report_count.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            let resp = DeliverResp {
                msg_id: [0u8; 8],
                result: 0,
            };
            let resp_pdu: Pdu = resp.into();
            ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;
        } else if cmd_id == CommandId::ActiveTestResp as u32 {
            self.active_test_resp_count.fetch_add(1, Ordering::Relaxed);
        } else if cmd_id == CommandId::TerminateResp as u32 {
            self.terminate_resp_received.store(true, Ordering::Relaxed);
        }

        Ok(())
    }
}

use rsms_connector::serve;

/// 启动测试服务器
async fn start_test_server(
    auth_handler: Arc<dyn AuthHandler>,
    biz_handler: Arc<dyn BusinessHandler>,
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

/// 启动测试服务器，返回server引用以便访问连接池
async fn start_test_server_with_pool(
    auth_handler: Arc<dyn AuthHandler>,
    biz_handler: Arc<dyn BusinessHandler>,
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

    let connect_pdu = client_handler.build_connect_pdu("106900", "password123");
    conn.send_request(connect_pdu).await.expect("send connect");

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(client_handler.get_connect_status(), Some(0), "认证成功状态码应为0");
    assert!(client_handler.is_connected(), "应该已连接");
    assert_eq!(auth.auth_success_count(), 1, "应该认证成功1次");

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
    let conn = connect(
        endpoint,
        client_handler.clone(),
        CmppDecoder,
        Some(ClientConfig::new()),
        None,
        Some(client_evt.clone()),
    )
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

/// 辅助函数：构建 CMPP Deliver PDU (MO - 上行短信)
fn build_deliver_mo(seq_id: u32, src_terminal: &str, dest_id: &str, content: &str) -> RawPdu {
    let pdu: Pdu = Deliver {
        msg_id: [0u8; 8],
        dest_id: dest_id.to_string(),
        service_id: "SMS".to_string(),
        tppid: 0,
        tpudhi: 0,
        msg_fmt: 15,
        src_terminal_id: src_terminal.to_string(),
        src_terminal_type: 0,
        registered_delivery: 0,
        msg_content: content.as_bytes().to_vec(),
        link_id: "".to_string(),
    }.into();
    pdu.to_pdu_bytes(seq_id)
}

/// 辅助函数：构建 CMPP Deliver PDU (Report - 状态报告)
fn build_deliver_report(seq_id: u32, msg_id: &[u8; 8], dest_id: &str) -> RawPdu {
    let report_content = format!(
        "id:{:02x?}{:02x?}{:02x?}{:02x?}{:02x?}{:02x?}{:02x?}{:02x?} sub:001 dlvrd:001 submit date:26010100 done date:26010100 stat:DELIVRD err:000",
        msg_id[0], msg_id[1], msg_id[2], msg_id[3], msg_id[4], msg_id[5], msg_id[6], msg_id[7]
    );
    let pdu: Pdu = Deliver {
        msg_id: *msg_id,
        dest_id: dest_id.to_string(),
        service_id: "SMS".to_string(),
        tppid: 0,
        tpudhi: 0,
        msg_fmt: 15,
        src_terminal_id: "".to_string(),
        src_terminal_type: 0,
        registered_delivery: 1,
        msg_content: report_content.as_bytes().to_vec(),
        link_id: "".to_string(),
    }.into();
    pdu.to_pdu_bytes(seq_id)
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
    biz_handler: Arc<dyn BusinessHandler>,
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
    let server = serve(
        cfg,
        vec![biz_handler],
        Some(auth_handler),
        None,
        Some(config_provider),
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

    let connect = Connect {
        source_addr: "106900".to_string(),
        authenticator_source: compute_connect_auth("106900", "password123", 0),
        version: 0x30,
        timestamp: 0,
    };
    let connect_pdu: Pdu = connect.into();
    let connect_bytes = connect_pdu.to_pdu_bytes(1);
    stream.write_all(connect_bytes.as_slice()).await.expect("send connect");
    tracing::info!("已发送 Connect PDU");

    let mut resp_buf = [0u8; 1024];
    let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut resp_buf)).await
        .expect("read timeout")
        .expect("read connect resp");
    tracing::info!("收到 ConnectResp: {} bytes", n);

    let connect_resp = decode_message(&resp_buf[..n]).expect("decode connect resp");
    match connect_resp {
        CmppMessage::ConnectResp { resp, .. } => {
            assert_eq!(resp.status, 0, "连接应该成功");
            tracing::info!("连接成功");
        }
        _ => panic!("expected ConnectResp"),
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut submit_results = Vec::new();
    for i in 0..5 {
        let submit = Submit::new()
            .with_message("106900", "13800138000", format!("Test SMS {}", i).as_bytes());
        let submit_pdu: Pdu = submit.into();
        let seq_id = 100 + i as u32;
        let submit_bytes = submit_pdu.to_pdu_bytes(seq_id);
        
        stream.write_all(submit_bytes.as_slice()).await.expect("send submit");
        tracing::info!("已发送 Submit #{} (seq={})", i + 1, seq_id);

        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut resp_buf = [0u8; 1024];
        match tokio::time::timeout(Duration::from_secs(2), stream.read(&mut resp_buf)).await {
            Ok(Ok(n)) if n > 0 => {
                let resp_bytes = &resp_buf[..n];
                let mut cursor = Cursor::new(resp_bytes);
                let header = PduHeader::decode(&mut cursor).expect("decode header");
                let body = &resp_bytes[PduHeader::SIZE..];
                
                if header.command_id == CommandId::SubmitResp {
                    let submit_resp = SubmitResp::decode(header, &mut Cursor::new(body))
                        .expect("decode submit resp");
                    tracing::info!("收到 SubmitResp #{}: result={}", i + 1, submit_resp.result);
                    submit_results.push(submit_resp.result);
                } else {
                    tracing::warn!("收到未知消息: command_id={:?}", header.command_id);
                    submit_results.push(999);
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

    let conn = connect(
        endpoint,
        handler.clone(),
        CmppDecoder,
        Some(ClientConfig::new()),
        Some(ms),
        Some(ts),
    )
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

    let conn = connect(
        endpoint,
        handler.clone(),
        CmppDecoder,
        Some(ClientConfig::new()),
        Some(ms),
        Some(ts),
    )
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

    let conn = connect(
        endpoint,
        handler.clone(),
        CmppDecoder,
        Some(ClientConfig::new()),
        Some(ms),
        Some(ts),
    )
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

    let conn = connect(
        endpoint,
        handler.clone(),
        CmppDecoder,
        Some(ClientConfig::new()),
        Some(ms),
        Some(ts),
    )
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
use std::io::Cursor;
    let auth = compute_connect_auth("900001", "pwd", 0);
    assert_eq!(auth.len(), 16);
}