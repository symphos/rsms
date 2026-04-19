use async_trait::async_trait;
use rsms_connector::serve;
use rsms_connector::{
    AuthHandler, AuthCredentials, AuthResult, AccountConfig, AccountPoolConfig,
    AccountConfigProvider, MessageSource, MessageItem, ServerEventHandler,
    ProtocolConnection,
};
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_core::{ConnectionInfo, RawPdu, EndpointConfig, Frame, Result};
use std::env;
use std::sync::{Arc, Mutex};
use rsms_codec_sgip::{
    decode_message, SgipMessage, CommandId, Encodable,
    Deliver, Report,
};

static INBOUND_QUEUE: std::sync::LazyLock<Arc<Mutex<Vec<String>>>> = 
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

const SERVER_ID: &str = "106900";

#[derive(Clone)]
struct SgipServerHandler;

#[async_trait]
impl BusinessHandler for SgipServerHandler {
    fn name(&self) -> &'static str {
        "sgip-server"
    }

    async fn on_inbound(&self, _ctx: &InboundContext, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        let msg = match decode_message(pdu) {
            Ok(m) => m,
            Err(e) => {
                println!("[Server] Decode error: {:?}", e);
                return Ok(());
            }
        };

        match msg {
            SgipMessage::Bind(b) => {
                println!("[Server] Received Bind: login_type={}, login_name={}", b.login_type, b.login_name);
            }
            SgipMessage::Submit(s) => {
                let content = String::from_utf8_lossy(&s.message_content);
                println!("[Server] === Received Submit ===");
                println!("[Server] SP Number: {}", s.sp_number);
                if let Some(user) = s.user_numbers.first() {
                    println!("[Server] User Number: {}", user);
                }
                println!("[Server] Content: {}", content);
                println!("[Server] Report Flag: {}", s.report_flag);
                println!("[Server] ========================");
                
                if let Ok(mut queue) = INBOUND_QUEUE.lock() {
                    queue.push("submit_received".to_string());
                }
            }
            SgipMessage::Deliver(d) => {
                let content = String::from_utf8_lossy(&d.message_content);
                println!("[Server] === Received Deliver (MO) ===");
                println!("[Server] User Number: {}", d.user_number);
                println!("[Server] SP Number: {}", d.sp_number);
                println!("[Server] Content: {}", content);
                println!("[Server] ============================");
            }
            SgipMessage::Report(r) => {
                println!("[Server] Received Report: state={}, error_code={}", r.state, r.error_code);
            }
            SgipMessage::Unbind(_) => {
                println!("[Server] Received Unbind");
            }
            _ => {
                println!("[Server] Unknown message type");
            }
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
        println!("[Auth] Authenticating client_id: {}", client_id);
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

        let mut messages: Vec<MessageItem> = vec![];

        let report_pdu = build_deliver_report(account);
        messages.push(MessageItem::Single(Arc::new(report_pdu)));

        let mo_pdu = build_deliver_mo(account);
        messages.push(MessageItem::Single(Arc::new(mo_pdu)));

        println!("[MessageSource] Fetched {} messages for account={}", messages.len(), account);
        Ok(messages)
    }
}

fn build_deliver_report(_account: &str) -> RawPdu {
    let report = Report {
        submit_sequence: rsms_codec_sgip::SgipSequence::new(1, 0x04051200, 42),
        report_type: 0,
        user_number: "13800138000".to_string(),
        state: 0,
        error_code: 0,
        reserve: [0u8; 8],
    };

    let mut body_buf = bytes::BytesMut::new();
    report.encode(&mut body_buf).unwrap();

    let body_len = body_buf.len() as u32;
    let total_len = 20 + body_len;
    let sequence = rsms_codec_sgip::SgipSequence::new(1, 0x04051200, 100);

    let mut seq_buf = bytes::BytesMut::with_capacity(12);
    sequence.encode(&mut seq_buf);

    let mut pdu = Vec::new();
    pdu.extend_from_slice(&total_len.to_be_bytes());
    pdu.extend_from_slice(&(CommandId::Report as u32).to_be_bytes());
    pdu.extend_from_slice(&seq_buf);
    pdu.extend_from_slice(&body_buf);

    pdu.into()
}

fn build_deliver_mo(_account: &str) -> RawPdu {
    let mut deliver = Deliver::new();
    deliver.user_number = "13800138001".to_string();
    deliver.sp_number = "106900".to_string();
    deliver.msg_fmt = 0;
    deliver.message_content = b"Test MO message".to_vec();

    let mut body_buf = bytes::BytesMut::new();
    deliver.encode(&mut body_buf).unwrap();

    let body_len = body_buf.len() as u32;
    let total_len = 20 + body_len;
    let sequence = rsms_codec_sgip::SgipSequence::new(1, 0x04051200, 101);

    let mut seq_buf = bytes::BytesMut::with_capacity(12);
    sequence.encode(&mut seq_buf);

    let mut pdu = Vec::new();
    pdu.extend_from_slice(&total_len.to_be_bytes());
    pdu.extend_from_slice(&(CommandId::Deliver as u32).to_be_bytes());
    pdu.extend_from_slice(&seq_buf);
    pdu.extend_from_slice(&body_buf);

    pdu.into()
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

    let server_addr = env::var("SGIP_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:7891".to_string());
    let (host, port) = if let Some((h, p)) = server_addr.rsplit_once(':') {
        (h.to_string(), p.parse().unwrap_or(7891))
    } else {
        ("127.0.0.1".to_string(), 7891)
    };

    let config = Arc::new(EndpointConfig::new("sgip-gateway", host, port, 100, 60).with_protocol("sgip"));

    println!("[Server] SGIP gateway starting at {}:{}...", config.host, config.port);
    println!("[Server] Gateway ID: {}", SERVER_ID);

    let auth_handler = Arc::new(MockAuthHandler) as Arc<dyn AuthHandler>;
    let message_source = Arc::new(MockMessageSource) as Arc<dyn MessageSource>;
    let account_config_provider = Arc::new(MockAccountConfigProvider) as Arc<dyn AccountConfigProvider>;
    let event_handler = Arc::new(MockServerEventHandler) as Arc<dyn ServerEventHandler>;
    let account_pool_config = Some(AccountPoolConfig::new());

    let server = serve(
        config.clone(),
        vec![Arc::new(SgipServerHandler) as Arc<dyn BusinessHandler>],
        Some(auth_handler),
        Some(message_source),
        Some(account_config_provider),
        Some(event_handler),
        account_pool_config,
    ).await?;
    println!("[Server] Listening on: {}", server.local_addr);

    server.run().await?;

    Ok(())
}
