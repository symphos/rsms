use async_trait::async_trait;
use rsms_connector::client::{ClientContext, ClientHandler, ClientConfig};
use rsms_connector::connect;
use rsms_connector::{MessageSource, MessageItem, ClientEventHandler, ProtocolConnection};
use rsms_connector::SgipDecoder;
use rsms_core::{RawPdu, EndpointConfig, Frame, Result};
use std::env;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use rsms_codec_sgip::{CommandId, Encodable, Bind, Unbind};

static OUTBOUND_QUEUE: std::sync::LazyLock<Arc<Mutex<Vec<RawPdu>>>> = 
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

struct MyClientHandler {
    connected: AtomicBool,
    submit_ok: AtomicBool,
    report_received: AtomicBool,
    mo_received: AtomicBool,
    seq: AtomicU64,
}

impl MyClientHandler {
    fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            submit_ok: AtomicBool::new(false),
            report_received: AtomicBool::new(false),
            mo_received: AtomicBool::new(false),
            seq: AtomicU64::new(1),
        }
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    fn build_bind_pdu(&self) -> Vec<u8> {
        let bind = Bind {
            login_type: 1,
            login_name: "106900".to_string(),
            login_password: "password".to_string(),
            reserve: [0u8; 8],
        };

        let body_bytes = {
            let mut buf = bytes::BytesMut::new();
            bind.encode(&mut buf).unwrap();
            buf.to_vec()
        };

        let total_len = (20 + body_bytes.len()) as u32;
        let seq_num = self.next_seq();

        let mut pdu = Vec::new();
        pdu.extend_from_slice(&total_len.to_be_bytes());
        pdu.extend_from_slice(&(CommandId::Bind as u32).to_be_bytes());
        pdu.extend_from_slice(&1u32.to_be_bytes());
        pdu.extend_from_slice(&0x04051200u32.to_be_bytes());
        pdu.extend_from_slice(&(seq_num as u32).to_be_bytes());
        pdu.extend(body_bytes);

        pdu
    }

    fn build_unbind_pdu(&self) -> Vec<u8> {
        let unbind = Unbind;

        let body_bytes = {
            let mut buf = bytes::BytesMut::new();
            unbind.encode(&mut buf).unwrap();
            buf.to_vec()
        };

        let total_len = (20 + body_bytes.len()) as u32;
        let seq_num = self.next_seq();

        let mut pdu = Vec::new();
        pdu.extend_from_slice(&total_len.to_be_bytes());
        pdu.extend_from_slice(&(CommandId::Unbind as u32).to_be_bytes());
        pdu.extend_from_slice(&1u32.to_be_bytes());
        pdu.extend_from_slice(&0x04051200u32.to_be_bytes());
        pdu.extend_from_slice(&(seq_num as u32).to_be_bytes());
        pdu.extend(body_bytes);

        pdu
    }
}

#[async_trait]
impl ClientHandler for MyClientHandler {
    fn name(&self) -> &'static str {
        "sgip-client"
    }

    async fn on_inbound(&self, _ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        let cmd_id = frame.command_id;
        
        if pdu.len() < 20 {
            return Ok(());
        }
        match cmd_id {
            x if x == CommandId::BindResp as u32 => {
                let status = u32::from_be_bytes([pdu[20], pdu[21], pdu[22], pdu[23]]);
                if status == 0 {
                    println!("[客户端] 收到 BindResp: status=0 成功");
                    self.connected.store(true, Ordering::Relaxed);
                } else {
                    println!("[客户端] 收到 BindResp: status={}", status);
                }
            }
            x if x == CommandId::SubmitResp as u32 => {
                let status = u32::from_be_bytes([pdu[20], pdu[21], pdu[22], pdu[23]]);
                if status == 0 {
                    println!("[客户端] 收到 SubmitResp: status=0 成功");
                    self.submit_ok.store(true, Ordering::Relaxed);
                } else {
                    println!("[客户端] 收到 SubmitResp: status={}", status);
                }
            }
            x if x == CommandId::Report as u32 => {
                println!("[客户端] 收到 Report (状态报告)");
                self.report_received.store(true, Ordering::Relaxed);
            }
            x if x == CommandId::Deliver as u32 => {
                println!("[客户端] 收到 Deliver (上行短信)");
                self.mo_received.store(true, Ordering::Relaxed);
            }
            x if x == CommandId::UnbindResp as u32 => {
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

    let server_addr = env::var("SGIP_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:7891".to_string());
    let (host, port) = if let Some((h, p)) = server_addr.rsplit_once(':') {
        (h.to_string(), p.parse().unwrap_or(7891))
    } else {
        ("127.0.0.1".to_string(), 7891)
    };

    let endpoint = Arc::new(EndpointConfig::new("sgip-client", host, port, 1, 60));
    let handler = Arc::new(MyClientHandler::new());

    println!("[客户端] 连接服务器 {}...", server_addr);

    push_test_messages();

    let client_config = Some(ClientConfig::new());
    let message_source = Some(Arc::new(MockMessageSource) as Arc<dyn MessageSource>);
    let event_handler = Some(Arc::new(MockClientEventHandler) as Arc<dyn ClientEventHandler>);

    let conn = connect(endpoint, handler.clone(), SgipDecoder, client_config, message_source, event_handler).await?;
    println!("[客户端] 已连接! conn_id={}", conn.id);

    // Step 1: SGIP Bind
    println!("\n========== 步骤1: SGIP Bind ==========");
    let bind_pdu = handler.build_bind_pdu();
    conn.write_frame(&bind_pdu).await?;
    println!("[客户端] 发送 Bind");
    
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
        if handler.report_received.load(Ordering::Relaxed) || handler.mo_received.load(Ordering::Relaxed) {
            println!("[客户端] 收到状态报告/上行短信!");
            break;
        }
        if i == 19 {
            println!("[客户端] 等待 Report/Deliver 消息超时");
        }
    }

    // Step 4: Unbind
    println!("\n========== 步骤4: SGIP 退出 ==========");
    let unbind_pdu = handler.build_unbind_pdu();
    conn.write_frame(&unbind_pdu).await?;
    println!("[客户端] 发送 Unbind");
    
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    println!("\n========== 所有测试完成! ==========");
    Ok(())
}

fn push_test_messages() {
    if let Ok(mut queue) = OUTBOUND_QUEUE.lock() {
        let mut submit = rsms_codec_sgip::datatypes::Submit::new();
        submit.sp_number = "106900".to_string();
        submit.user_count = 1;
        submit.user_numbers = vec!["13800138001".to_string()];
        submit.report_flag = 1;
        submit.msg_fmt = 0;
        submit.message_content = b"Hello SMS via fetch_outbound".to_vec();

        let body_bytes = {
            let mut buf = bytes::BytesMut::new();
            submit.encode(&mut buf).unwrap();
            buf.to_vec()
        };

        let total_len = (20 + body_bytes.len()) as u32;
        let seq_num = 1001u64;

        let mut pdu = Vec::new();
        pdu.extend_from_slice(&total_len.to_be_bytes());
        pdu.extend_from_slice(&(rsms_codec_sgip::CommandId::Submit as u32).to_be_bytes());
        pdu.extend_from_slice(&1u32.to_be_bytes());
        pdu.extend_from_slice(&0x04051200u32.to_be_bytes());
        pdu.extend_from_slice(&(seq_num as u32).to_be_bytes());
        pdu.extend(body_bytes);

        queue.push(RawPdu::from_vec(pdu));
        
        println!("[MessageSource] 预填充了 {} 条测试消息 (将在 Login 后自动发送)", queue.len());
    }
}

struct MockMessageSource;

#[async_trait]
impl MessageSource for MockMessageSource {
    async fn fetch(&self, _account: &str, _batch_size: usize) -> Result<Vec<MessageItem>> {
        let mut items = vec![];
        if let Ok(mut queue) = OUTBOUND_QUEUE.lock() {
            while let Some(pdu) = queue.pop() {
                items.push(MessageItem::Single(Arc::new(pdu)));
            }
        }
        if !items.is_empty() {
            println!("[MessageSource] 拉取到 {} 条待发送消息", items.len());
        }
        Ok(items)
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
