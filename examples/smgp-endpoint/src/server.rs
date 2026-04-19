use async_trait::async_trait;
use rsms_connector::serve;
use rsms_connector::{AuthHandler, AuthCredentials, AuthResult, AccountConfig, AccountPoolConfig, AccountConfigProvider, MessageSource, MessageItem, ServerEventHandler, ProtocolConnection};
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_core::{ConnectionInfo, EncodedPdu, RawPdu, EndpointConfig, Frame, Result};
use std::env;
use std::sync::{Arc, Mutex};
use rsms_codec_smgp::{decode_message, SmgpMessage, Pdu};
use rsms_codec_smgp::datatypes::Deliver;

static INBOUND_QUEUE: std::sync::LazyLock<Arc<Mutex<Vec<String>>>> = 
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

const SERVER_ID: &str = "106900";

#[derive(Clone)]
struct SmgpServerHandler;

#[async_trait]
impl BusinessHandler for SmgpServerHandler {
    fn name(&self) -> &'static str {
        "smgp-server"
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
            SmgpMessage::Login { sequence_id, login: l } => {
                let client_id = &l.client_id;
                println!("[服务端] 收到登录请求: client_id={}, seq={}", client_id, sequence_id);
                println!("[服务端] 发送登录响应: status=0");
            }
            SmgpMessage::Submit { sequence_id, submit: s } => {
                println!("[服务端] === 收到短信提交 ===");
                println!("[服务端] seq_id={}", sequence_id);
                println!("[服务端] 源号码: {}", s.src_term_id);
                if let Some(dest) = s.dest_term_ids.first() {
                    println!("[服务端] 目标号码: {}", dest);
                }
                println!("[服务端] 短信内容: {:?}", String::from_utf8_lossy(&s.msg_content));
                println!("[服务端] 需要状态报告: {}", if s.need_report == 1 { "是" } else { "否" });
                println!("[服务端] ====================");
                
                if let Ok(mut queue) = INBOUND_QUEUE.lock() {
                    queue.push("submit_received".to_string());
                }
            }
            SmgpMessage::ActiveTest { sequence_id } => {
                println!("[服务端] 收到活性检测: sequence_id={}", sequence_id);
            }
            SmgpMessage::Deliver { sequence_id, deliver: _ } => {
                println!("[服务端] 收到 Deliver 消息: sequence_id={}", sequence_id);
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

#[derive(Clone)]
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

        let mut messages = vec![];

        let report_pdu = build_deliver_report(account);
        messages.push(MessageItem::Single(Arc::new(report_pdu) as Arc<dyn EncodedPdu>));

        let mo_pdu = build_deliver_mo(account);
        messages.push(MessageItem::Single(Arc::new(mo_pdu) as Arc<dyn EncodedPdu>));

        println!("[MessageSource] 拉取到 {} 条消息 for account={}", messages.len(), account);
        Ok(messages)
    }
}

fn build_deliver_report(account: &str) -> RawPdu {
    let mut msg_id_bytes = [0u8; 10];
    msg_id_bytes[2..].copy_from_slice(&10001u64.to_be_bytes());
    
    let now = chrono_lite_timestamp();
    let report_content = format!("id:{} sub:001 dlvrd:001 submit date:{} done date:{} stat:DELIVRD err:000 text:Hello", 10001, &now, &now);
    
    let deliver = Deliver {
        msg_id: rsms_codec_smgp::datatypes::SmgpMsgId::new(msg_id_bytes),
        is_report: 1,
        msg_fmt: 15,
        recv_time: now.clone(),
        src_term_id: "13800138000".to_string(),
        dest_term_id: account.to_string(),
        msg_content: report_content.into_bytes(),
        reserve: [0u8; 8],
        optional_params: rsms_codec_smgp::datatypes::tlv::OptionalParameters::new(),
    };
    
    let pdu: Pdu = deliver.into();
    pdu.to_pdu_bytes(1)
}

fn build_deliver_mo(account: &str) -> RawPdu {
    let mut msg_id_bytes = [0u8; 10];
    msg_id_bytes[2..].copy_from_slice(&20001u64.to_be_bytes());
    
    let now = chrono_lite_timestamp();
    let mo_content = "测试上行短信".as_bytes().to_vec();
    
    let deliver = Deliver {
        msg_id: rsms_codec_smgp::datatypes::SmgpMsgId::new(msg_id_bytes),
        is_report: 0,
        msg_fmt: 15,
        recv_time: now.clone(),
        src_term_id: "13800138000".to_string(),
        dest_term_id: account.to_string(),
        msg_content: mo_content,
        reserve: [0u8; 8],
        optional_params: rsms_codec_smgp::datatypes::tlv::OptionalParameters::new(),
    };
    
    let pdu: Pdu = deliver.into();
    pdu.to_pdu_bytes(2)
}

fn chrono_lite_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ts = format!("{:010}", now);
    format!("{}{}", &ts[0..8], &ts[8..10])
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

    let server_addr = env::var("SMGP_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:7892".to_string());
    let (host, port) = if let Some((h, p)) = server_addr.rsplit_once(':') {
        (h.to_string(), p.parse().unwrap_or(7892))
    } else {
        ("127.0.0.1".to_string(), 7892)
    };

    let config = Arc::new(EndpointConfig::new("smgp-gateway", host, port, 100, 60).with_protocol("smgp"));

    println!("[服务端] SMGP 网关启动于 {}:{}...", config.host, config.port);
    println!("[服务端] 网关ID: {}", SERVER_ID);
    
    let auth_handler = Arc::new(MockAuthHandler) as Arc<dyn AuthHandler>;
    let message_source = Arc::new(MockMessageSource) as Arc<dyn MessageSource>;
    let account_config_provider = Arc::new(MockAccountConfigProvider) as Arc<dyn AccountConfigProvider>;
    let event_handler = Arc::new(MockServerEventHandler) as Arc<dyn ServerEventHandler>;
    let account_pool_config = Some(AccountPoolConfig::new());

    let server = serve(
        config.clone(),
        vec![Arc::new(SmgpServerHandler) as Arc<dyn BusinessHandler>],
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
