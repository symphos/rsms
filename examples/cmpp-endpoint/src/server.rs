use async_trait::async_trait;
use rsms_connector::serve;
use rsms_connector::{AuthHandler, AuthCredentials, AuthResult, AccountConfig, AccountPoolConfig, AccountConfigProvider, MessageSource, MessageItem, ServerEventHandler, ProtocolConnection};
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_core::{ConnectionInfo, EncodedPdu, RawPdu, EndpointConfig, Frame, Result};
use std::env;
use std::sync::{Arc, Mutex};
use rsms_codec_cmpp::{decode_message, CmppMessage, Pdu, Deliver};

static INBOUND_QUEUE: std::sync::LazyLock<Arc<Mutex<Vec<String>>>> = 
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

const SERVER_ID: &str = "106900";

#[derive(Clone)]
struct CmppServerHandler;

#[async_trait]
impl BusinessHandler for CmppServerHandler {
    fn name(&self) -> &'static str {
        "cmpp-server"
    }
    
    async fn on_inbound(&self, _ctx: &InboundContext, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        let msg = match decode_message(pdu) {
            Ok(m) => m,
            Err(e) => {
                println!("[服务端] 解析消息失败: {:?}", e);
                return Ok(());
            }
        };
        
        match msg {
            CmppMessage::Connect { version: _, sequence_id: _, connect: c } => {
                let addr = &c.source_addr;
                println!("[服务端] 收到连接请求: source_addr={}", addr);
            }
            CmppMessage::SubmitV20 { sequence_id, submit: s } => {
                println!("[服务端] === 收到短信提交 (V2.0) ===");
                println!("[服务端] seq_id={}", sequence_id);
                println!("[服务端] 源号码: {}", s.src_id);
                if let Some(dest) = s.dest_terminal_ids.first() {
                    println!("[服务端] 目标号码: {}", dest);
                }
                println!("[服务端] 短信内容: {:?}", String::from_utf8_lossy(&s.msg_content));
                println!("[服务端] 需要状态报告: {}", if s.registered_delivery == 1 { "是" } else { "否" });
                println!("[服务端] ====================");
                
                if let Ok(mut queue) = INBOUND_QUEUE.lock() {
                    queue.push("submit_received".to_string());
                }
            }
            CmppMessage::SubmitV30 { sequence_id, submit: s } => {
                println!("[服务端] === 收到短信提交 (V3.0) ===");
                println!("[服务端] seq_id={}", sequence_id);
                println!("[服务端] 源号码: {}", s.src_id);
                if let Some(dest) = s.dest_terminal_ids.first() {
                    println!("[服务端] 目标号码: {}", dest);
                }
                println!("[服务端] 短信内容: {:?}", String::from_utf8_lossy(&s.msg_content));
                println!("[服务端] 需要状态报告: {}", if s.registered_delivery == 1 { "是" } else { "否" });
                println!("[服务端] ====================");
                
                if let Ok(mut queue) = INBOUND_QUEUE.lock() {
                    queue.push("submit_received".to_string());
                }
            }
            CmppMessage::ActiveTest { version: _, sequence_id } => {
                println!("[服务端] 收到活性检测: sequence_id={}", sequence_id);
            }
            CmppMessage::DeliverV20 { sequence_id, deliver: d } => {
                println!("[服务端] === 收到Deliver消息 (V2.0) ===");
                println!("[服务端] seq_id={}", sequence_id);
                if d.registered_delivery == 1 {
                    let content = String::from_utf8_lossy(&d.msg_content);
                    println!("[服务端] === 收到状态报告 ===");
                    println!("[服务端] 状态报告内容: {}", content);
                    println!("[服务端] ==================");
                } else {
                    let content = String::from_utf8_lossy(&d.msg_content);
                    println!("[服务端] === 收到上行短信 ===");
                    println!("[服务端] 发送号码: {}", d.src_terminal_id);
                    println!("[服务端] 目标号码: {}", d.dest_id);
                    println!("[服务端] 上行内容: {}", content);
                    println!("[服务端] ==================");
                }
            }
            CmppMessage::DeliverV30 { sequence_id, deliver: d } => {
                println!("[服务端] === 收到Deliver消息 (V3.0) ===");
                println!("[服务端] seq_id={}", sequence_id);
                if d.registered_delivery == 1 {
                    let content = String::from_utf8_lossy(&d.msg_content);
                    println!("[服务端] === 收到状态报告 ===");
                    println!("[服务端] 状态报告内容: {}", content);
                    println!("[服务端] ==================");
                } else {
                    let content = String::from_utf8_lossy(&d.msg_content);
                    println!("[服务端] === 收到上行短信 ===");
                    println!("[服务端] 发送号码: {}", d.src_terminal_id);
                    println!("[服务端] 目标号码: {}", d.dest_id);
                    println!("[服务端] 上行内容: {}", content);
                    println!("[服务端] ==================");
                }
            }
            _ => {}
        }
        
        Ok(())
    }
}

struct MockAuthHandler;

#[async_trait]
impl AuthHandler for MockAuthHandler {
    fn name(&self) -> &'static str {
        "mock-auth"
    }

    async fn authenticate(&self, client_id: &str, _credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        println!("[Auth] 认证 client_id: {}", client_id);
        Ok(AuthResult::success(client_id))
    }
}

struct MockMessageSource;

#[async_trait]
impl MessageSource for MockMessageSource {
    async fn fetch(&self, account: &str, _batch_size: usize) -> Result<Vec<MessageItem>> {
        let has_event = {
            if let Ok(mut queue) = INBOUND_QUEUE.lock() {
                if !queue.is_empty() {
                    queue.pop();
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };

        if !has_event {
            return Ok(vec![]);
        }

        println!("[MessageSource] fetch called for account={}", account);

        let mut messages: Vec<RawPdu> = vec![];

        let report_pdu = build_deliver_report(account);
        messages.push(report_pdu);

        let mo_pdu = build_deliver_mo(account);
        messages.push(mo_pdu);

        println!("[MessageSource] 拉取到 {} 条消息 for account={}", messages.len(), account);
        Ok(messages.into_iter().map(|pdu| MessageItem::Single(Arc::new(pdu) as Arc<dyn EncodedPdu>)).collect())
    }
}

fn build_deliver_report(account: &str) -> RawPdu {
    let deliver = Deliver {
        msg_id: 10001u64.to_be_bytes(),
        dest_id: account.to_string(),
        service_id: "SMS".to_string(),
        tppid: 0,
        tpudhi: 0,
        msg_fmt: 15,
        src_terminal_id: "13800138000".to_string(),
        src_terminal_type: 0,
        registered_delivery: 1,
        msg_content: "id:10001 sub:001 dlvrd:001 submit date:20260101 done date:20260101 stat:DELIVRD err:000 text:Hello".as_bytes().to_vec(),
        link_id: "".to_string(),
    };
    
    let pdu: Pdu = deliver.into();
    pdu.to_pdu_bytes(1)
}

fn build_deliver_mo(account: &str) -> RawPdu {
    let deliver = Deliver {
        msg_id: 20001u64.to_be_bytes(),
        dest_id: account.to_string(),
        service_id: "SMS".to_string(),
        tppid: 0,
        tpudhi: 0,
        msg_fmt: 15,
        src_terminal_id: "13800138000".to_string(),
        src_terminal_type: 0,
        registered_delivery: 0,
        msg_content: "测试上行短信".as_bytes().to_vec(),
        link_id: "".to_string(),
    };
    
    let pdu: Pdu = deliver.into();
    pdu.to_pdu_bytes(2)
}

struct MockAccountConfigProvider;

#[async_trait]
impl AccountConfigProvider for MockAccountConfigProvider {
    async fn get_config(&self, _account: &str) -> Result<AccountConfig> {
        Ok(AccountConfig::new())
    }
}

struct MockServerEventHandler;

#[async_trait]
impl ServerEventHandler for MockServerEventHandler {
    async fn on_connected(&self, _conn: &Arc<dyn ProtocolConnection>) {
        println!("[Event] Server: client connected");
    }

    async fn on_disconnected(&self, _conn_id: u64, _account: Option<&str>) {
        println!("[Event] Server: client disconnected");
    }

    async fn on_authenticated(&self, _conn: &Arc<dyn ProtocolConnection>, account: &str) {
        println!("[Event] Server: client authenticated, account={}", account);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let server_addr = env::var("CMPP_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:7890".to_string());
    let (host, port) = if let Some((h, p)) = server_addr.rsplit_once(':') {
        (h.to_string(), p.parse().unwrap_or(7890))
    } else {
        ("127.0.0.1".to_string(), 7890)
    };

    let config = Arc::new(EndpointConfig::new("cmpp-gateway", host, port, 100, 60).with_protocol("cmpp"));

    println!("[服务端] CMPP 网关启动于 {}:{}...", config.host, config.port);
    println!("[服务端] 网关ID: {}", SERVER_ID);
    
    let auth_handler = Arc::new(MockAuthHandler) as Arc<dyn AuthHandler>;
    let message_source = Arc::new(MockMessageSource) as Arc<dyn MessageSource>;
    let account_config_provider = Arc::new(MockAccountConfigProvider) as Arc<dyn AccountConfigProvider>;
    let event_handler = Arc::new(MockServerEventHandler) as Arc<dyn ServerEventHandler>;
    let account_pool_config = Some(AccountPoolConfig::new());

    let server = serve(
        config.clone(),
        vec![Arc::new(CmppServerHandler) as Arc<dyn BusinessHandler>],
        Some(auth_handler),
        Some(message_source),
        Some(account_config_provider),
        Some(event_handler),
        account_pool_config,
    ).await?;
    println!("[服务端] 监听地址: {}", server.local_addr);

    server.run().await?;

    Ok(())
}
