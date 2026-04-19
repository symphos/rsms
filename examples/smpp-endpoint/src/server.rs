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
use rsms_codec_smpp::{
    decode_message, SmppMessage, CommandId, Encodable,
    DeliverSm,
};

static INBOUND_QUEUE: std::sync::LazyLock<Arc<Mutex<Vec<String>>>> = 
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

const SERVER_ID: &str = "SMSC";

#[derive(Clone)]
struct SmppServerHandler;

#[async_trait]
impl BusinessHandler for SmppServerHandler {
    fn name(&self) -> &'static str {
        "smpp-server"
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
            SmppMessage::BindTransmitter(b) => {
                println!("[Server] Received BindTransmitter: system_id={}", b.system_id);
            }
            SmppMessage::SubmitSm(s) => {
                let content = String::from_utf8_lossy(&s.short_message);
                println!("[Server] === Received SubmitSm ===");
                println!("[Server] Source: {}", s.source_addr);
                println!("[Server] Destination: {}", s.destination_addr);
                println!("[Server] Content: {}", content);
                println!("[Server] Registered Delivery: {}", s.registered_delivery);
                println!("[Server] ========================");
                
                if let Ok(mut queue) = INBOUND_QUEUE.lock() {
                    queue.push("submit_received".to_string());
                }
            }
            SmppMessage::DeliverSmResp(_) => {
                println!("[Server] Received DeliverSmResp");
            }
            SmppMessage::EnquireLink(_) => {
                println!("[Server] Received EnquireLink");
            }
            SmppMessage::Unbind(_) => {
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

        let deliver_pdu = build_deliver_sm(account);

        println!("[MessageSource] Fetched 1 messages for account={}", account);
        Ok(vec![MessageItem::Single(Arc::new(deliver_pdu))])
    }
}

fn build_deliver_sm(_account: &str) -> RawPdu {
    let deliver = DeliverSm {
        service_type: String::new(),
        source_addr_ton: 1,
        source_addr_npi: 1,
        source_addr: "13800138001".to_string(),
        dest_addr_ton: 0,
        dest_addr_npi: 0,
        destination_addr: "13800138000".to_string(),
        esm_class: 0x04,
        protocol_id: 0,
        priority_flag: 0,
        schedule_delivery_time: String::new(),
        validity_period: String::new(),
        registered_delivery: 1,
        replace_if_present_flag: 0,
        data_coding: 0x03,
        sm_default_msg_id: 0,
        short_message: b"id:MSG00001 sub:001 dlvrd:001 submit_dt:0601021200 done_dt:0601021205 stat:DELIVRD err:000".to_vec(),
        tlvs: Vec::new(),
    };

    let mut buf = bytes::BytesMut::new();
    deliver.encode(&mut buf).unwrap();

    let body_len = buf.len() as u32;
    let total_len = 16 + body_len;
    let sequence_id = 1u32;

    let mut pdu = Vec::new();
    pdu.extend_from_slice(&total_len.to_be_bytes());
    pdu.extend_from_slice(&(CommandId::DELIVER_SM as u32).to_be_bytes());
    pdu.extend_from_slice(&sequence_id.to_be_bytes());
    pdu.extend_from_slice(&buf);

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

    let server_addr = env::var("SMPP_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:7893".to_string());
    let (host, port) = if let Some((h, p)) = server_addr.rsplit_once(':') {
        (h.to_string(), p.parse().unwrap_or(7893))
    } else {
        ("127.0.0.1".to_string(), 7893)
    };

    let config = Arc::new(EndpointConfig::new("smpp-gateway", host, port, 100, 60).with_protocol("smpp"));

    println!("[Server] SMPP gateway starting at {}:{}...", config.host, config.port);
    println!("[Server] Gateway ID: {}", SERVER_ID);

    let auth_handler = Arc::new(MockAuthHandler) as Arc<dyn AuthHandler>;
    let message_source = Arc::new(MockMessageSource) as Arc<dyn MessageSource>;
    let account_config_provider = Arc::new(MockAccountConfigProvider) as Arc<dyn AccountConfigProvider>;
    let event_handler = Arc::new(MockServerEventHandler) as Arc<dyn ServerEventHandler>;
    let account_pool_config = Some(AccountPoolConfig::new());

    let server = serve(
        config.clone(),
        vec![Arc::new(SmppServerHandler) as Arc<dyn BusinessHandler>],
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
