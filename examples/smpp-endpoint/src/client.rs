use async_trait::async_trait;
use rsms_connector::client::{ClientContext, ClientHandler, ClientConfig};
use rsms_connector::connect;
use rsms_connector::{MessageSource, MessageItem, ClientEventHandler, ProtocolConnection};
use rsms_connector::SmppDecoder;
use rsms_core::{RawPdu, EndpointConfig, Frame, Result};
use std::env;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use rsms_codec_smpp::{
    CommandId, BindTransmitter, EnquireLink, Unbind, Pdu,
};

static OUTBOUND_QUEUE: std::sync::LazyLock<Arc<Mutex<Vec<RawPdu>>>> = 
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

struct MyClientHandler {
    connected: AtomicBool,
    submit_ok: AtomicBool,
    report_received: AtomicBool,
    seq: AtomicU32,
}

impl MyClientHandler {
    fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            submit_ok: AtomicBool::new(false),
            report_received: AtomicBool::new(false),
            seq: AtomicU32::new(1),
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    fn build_bind_pdu(&self) -> Vec<u8> {
        let bind = BindTransmitter::new("sys_id", "password", "CMT", 0x34);
        let pdu: Pdu = bind.into();
        pdu.to_pdu_bytes(self.next_seq()).to_vec()
    }

    fn build_enquire_link_pdu(&self) -> Vec<u8> {
        let enquire_link = EnquireLink;
        let pdu: Pdu = enquire_link.into();
        pdu.to_pdu_bytes(self.next_seq()).to_vec()
    }

    fn build_unbind_pdu(&self) -> Vec<u8> {
        let unbind = Unbind;
        let pdu: Pdu = unbind.into();
        pdu.to_pdu_bytes(self.next_seq()).to_vec()
    }
}

#[async_trait]
impl ClientHandler for MyClientHandler {
    fn name(&self) -> &'static str {
        "smpp-client"
    }

    async fn on_inbound(&self, _ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        let cmd_id = frame.command_id;
        
        if pdu.len() < 12 {
            return Ok(());
        }
        
        let status = u32::from_be_bytes([pdu[8], pdu[9], pdu[10], pdu[11]]);
        match cmd_id {
            x if x == CommandId::BIND_TRANSMITTER_RESP as u32 => {
                if status == 0 {
                    println!("[客户端] 收到 BindTransmitterResp: status=0 成功");
                    self.connected.store(true, Ordering::Relaxed);
                } else {
                    println!("[客户端] 收到 BindTransmitterResp: status={}", status);
                }
            }
            x if x == CommandId::SUBMIT_SM_RESP as u32 => {
                if status == 0 {
                    println!("[客户端] 收到 SubmitSmResp: status=0 成功");
                    self.submit_ok.store(true, Ordering::Relaxed);
                } else {
                    println!("[客户端] 收到 SubmitSmResp: status={}", status);
                }
            }
            x if x == CommandId::DELIVER_SM as u32 => {
                println!("[客户端] 收到 DeliverSm (状态报告/上行短信)");
                self.report_received.store(true, Ordering::Relaxed);
            }
            x if x == CommandId::ENQUIRE_LINK_RESP as u32 => {
                println!("[客户端] 收到 EnquireLinkResp");
            }
            x if x == CommandId::UNBIND_RESP as u32 => {
                println!("[客户端] 收到 UnbindResp");
            }
            _ => {
                println!("[客户端] 收到未知 PDU: cmd_id={:#x}", cmd_id);
            }
        }
        Ok(())
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

    let endpoint = Arc::new(EndpointConfig::new("smpp-client", host, port, 1, 60));
    let handler = Arc::new(MyClientHandler::new());

    println!("[客户端] 连接服务器 {}...", server_addr);

    push_test_messages();

    let client_config = Some(ClientConfig::new());
    let message_source = Some(Arc::new(MockMessageSource) as Arc<dyn MessageSource>);
    let event_handler = Some(Arc::new(MockClientEventHandler) as Arc<dyn ClientEventHandler>);

    let conn = connect(endpoint, handler.clone(), SmppDecoder, client_config, message_source, event_handler).await?;
    println!("[客户端] 已连接! conn_id={}", conn.id);

    // Step 1: SMPP Bind
    println!("\n========== 步骤1: SMPP Bind ==========");
    let bind_pdu = handler.build_bind_pdu();
    conn.write_frame(&bind_pdu).await?;
    println!("[客户端] 发送 BindTransmitter");
    
    for _ in 0..10 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if handler.connected.load(Ordering::Relaxed) { break; }
    }

    // Step 2: 等待 fetch_outbound 自动发送短信
    println!("\n========== 步骤2: 等待 MessageSource 自动发送短信 ==========");
    println!("[客户端] fetch_outbound 将在 Login 后自动拉取并发送短信...");
    
    for i in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if handler.submit_ok.load(Ordering::Relaxed) {
            println!("[客户端] 收到短信提交响应!");
            break;
        }
        if i == 29 {
            println!("[客户端] 等待提交响应超时");
        }
    }

    // Step 3: 等待状态报告/上行短信
    println!("\n========== 步骤3: 等待服务端推送状态报告和上行短信 ==========");
    for i in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        if handler.report_received.load(Ordering::Relaxed) {
            println!("[客户端] 收到状态报告/上行短信!");
            break;
        }
        if i == 19 {
            println!("[客户端] 等待 Deliver 消息超时");
        }
    }

    // Step 4: EnquireLink (心跳)
    println!("\n========== 步骤4: SMPP 活性检测 ==========");
    let eq_pdu = handler.build_enquire_link_pdu();
    conn.write_frame(&eq_pdu).await?;
    println!("[客户端] 发送 EnquireLink");
    
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Step 5: Unbind
    println!("\n========== 步骤5: SMPP 退出 ==========");
    let unbind_pdu = handler.build_unbind_pdu();
    conn.write_frame(&unbind_pdu).await?;
    println!("[客户端] 发送 Unbind");
    
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    println!("\n========== 所有测试完成! ==========");
    Ok(())
}

fn push_test_messages() {
    if let Ok(mut queue) = OUTBOUND_QUEUE.lock() {
        let mut submit = rsms_codec_smpp::datatypes::SubmitSm::new();
        submit.source_addr_ton = 0;
        submit.source_addr_npi = 0;
        submit.source_addr = "13800138000".to_string();
        submit.dest_addr_ton = 1;
        submit.dest_addr_npi = 1;
        submit.destination_addr = "13800138001".to_string();
        submit.esm_class = 0x04;
        submit.registered_delivery = 1;
        submit.data_coding = 0x03;
        submit.short_message = b"Hello SMS via fetch_outbound".to_vec();

        let pdu: Pdu = submit.into();
        let encoded = pdu.to_pdu_bytes(1001);

        queue.push(encoded);
        
        println!("[MessageSource] 预填充了 {} 条测试消息 (将在 Login 后自动发送)", queue.len());
    }
}

struct MockMessageSource;

#[async_trait]
impl MessageSource for MockMessageSource {
    async fn fetch(&self, _account: &str, _batch_size: usize) -> Result<Vec<MessageItem>> {
        let mut messages = vec![];
        if let Ok(mut queue) = OUTBOUND_QUEUE.lock() {
            while let Some(sms) = queue.pop() {
                messages.push(MessageItem::Single(Arc::new(sms)));
            }
        }
        if !messages.is_empty() {
            println!("[MessageSource] 拉取到 {} 条待发送消息", messages.len());
        }
        Ok(messages)
    }
}

struct MockClientEventHandler;

#[async_trait]
impl ClientEventHandler for MockClientEventHandler {
    async fn on_connected(&self, _conn: &Arc<dyn ProtocolConnection>) {
        println!("[Event] Client: connected to server");
    }

    async fn on_disconnected(&self, _conn_id: u64) {
        println!("[Event] Client: disconnected from server");
    }
}
