use async_trait::async_trait;
use rsms_connector::client::{ClientContext, ClientHandler, ClientConfig};
use rsms_connector::{connect, MessageSource, MessageItem, ClientEventHandler, ProtocolConnection};
use rsms_connector::SmgpDecoder;
use rsms_core::{EncodedPdu, RawPdu, EndpointConfig, Frame, Result};
use std::env;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use rsms_codec_smgp::{
    CommandId, Pdu, PduHeader, Login, LoginResp, SubmitResp, ActiveTest, Exit, 
    DeliverResp, Decodable,
};

static OUTBOUND_QUEUE: std::sync::LazyLock<Arc<Mutex<Vec<RawPdu>>>> = 
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

const CLIENT_ID: &str = "106900";
const CLIENT_PASSWORD: &str = "password123";

struct MyClientHandler {
    connected: AtomicBool,
    submit_ok: AtomicBool,
    deliver_received: AtomicBool,
    active_test_resp_received: AtomicBool,
    seq: AtomicU32,
}

impl MyClientHandler {
    fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            submit_ok: AtomicBool::new(false),
            deliver_received: AtomicBool::new(false),
            active_test_resp_received: AtomicBool::new(false),
            seq: AtomicU32::new(1),
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    fn build_login_pdu(&self) -> RawPdu {
        let timestamp = 0u32;
        let authenticator = rsms_codec_smgp::compute_login_auth(CLIENT_ID, CLIENT_PASSWORD, timestamp);
        
        let login = Login {
            client_id: CLIENT_ID.to_string(),
            authenticator,
            login_mode: 0,
            timestamp,
            version: 0x30,
        };
        
        let pdu: Pdu = login.into();
        pdu.to_pdu_bytes(self.next_seq())
    }

    fn build_active_test_pdu(&self) -> RawPdu {
        let active_test = ActiveTest;
        let pdu: Pdu = active_test.into();
        pdu.to_pdu_bytes(self.next_seq())
    }

    fn build_exit_pdu(&self) -> RawPdu {
        let exit = Exit;
        let pdu: Pdu = exit.into();
        pdu.to_pdu_bytes(self.next_seq())
    }

    fn build_submit(&self, dest_terminal: &str, content: &str, registered_delivery: u8) -> RawPdu {
        let mut submit = rsms_codec_smgp::datatypes::Submit::new();
        submit.need_report = registered_delivery;
        submit.service_id = "SMS".to_string();
        submit.fee_type = "02".to_string();
        submit.fee_code = "000000".to_string();
        submit.fixed_fee = "000000".to_string();
        submit.msg_fmt = 15;
        submit.src_term_id = "106900".to_string();
        submit.charge_term_id = dest_terminal.to_string();
        submit.dest_term_id_count = 1;
        submit.dest_term_ids = vec![dest_terminal.to_string()];
        submit.msg_content = content.as_bytes().to_vec();
        
        let pdu: Pdu = submit.into();
        pdu.to_pdu_bytes(1001)
    }
}

#[async_trait]
impl ClientHandler for MyClientHandler {
    fn name(&self) -> &'static str {
        "smgp-client"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        if pdu.len() < 12 {
            return Ok(());
        }
        
        let header = match PduHeader::decode(&mut std::io::Cursor::new(pdu)) {
            Ok(h) => h,
            Err(_) => return Ok(()),
        };
        
        let body = &pdu[PduHeader::SIZE..];
        
        // LoginResp (0x80000001)
        if header.command_id == CommandId::LoginResp {
            let resp = match LoginResp::decode(header, &mut std::io::Cursor::new(body)) {
                Ok(r) => r,
                Err(_) => return Ok(()),
            };
            
            if resp.status == 0 {
                println!("[客户端] 收到登录响应: status=0 成功");
                self.connected.store(true, Ordering::Relaxed);
            } else {
                println!("[客户端] 收到登录响应: status={} 失败", resp.status);
            }
        }
        // SubmitResp (0x80000002)
        else if header.command_id == CommandId::SubmitResp {
            let resp = match SubmitResp::decode(header, &mut std::io::Cursor::new(body)) {
                Ok(r) => r,
                Err(_) => return Ok(()),
            };
            
            if resp.status == 0 {
                let msg_id = String::from_utf8_lossy(&resp.msg_id.bytes);
                println!("[客户端] 收到提交响应: msg_id={}, status=0 成功", msg_id);
                self.submit_ok.store(true, Ordering::Relaxed);
            } else {
                println!("[客户端] 收到提交响应: status={} 失败", resp.status);
            }
        }
        // Deliver (0x00000003)
        else if header.command_id == CommandId::Deliver {
            let (sequence_id, deliver) = match rsms_codec_smgp::decode_message(pdu) {
                Ok(rsms_codec_smgp::SmgpMessage::Deliver { sequence_id, deliver: d }) => (sequence_id, d),
                _ => return Ok(()),
            };
            
            if deliver.is_report == 1 {
                // This is a report
                println!("[客户端] 收到状态报告");
            } else {
                println!("[客户端] 收到上行短信");
            }
            self.deliver_received.store(true, Ordering::Relaxed);
            
            let resp = DeliverResp { status: 0 };
            let resp_pdu: Pdu = resp.into();
            ctx.conn.write_frame(resp_pdu.to_pdu_bytes(sequence_id).as_bytes_ref()).await?;
            println!("[客户端] 发送 Deliver 响应");
        }
        // ActiveTestResp (0x80000008)
        else if header.command_id == CommandId::ActiveTestResp {
            println!("[客户端] 收到活性检测响应");
            self.active_test_resp_received.store(true, Ordering::Relaxed);
        }
        
        Ok(())
    }
}



fn push_test_messages(handler: &MyClientHandler) {
    if let Ok(mut queue) = OUTBOUND_QUEUE.lock() {
        let pdu1 = handler.build_submit("13800138001", "Hello SMS via fetch_outbound", 1);
        queue.push(pdu1);
        
        println!("[MessageSource] 预填充了 {} 条测试消息 (将在 Login 后自动发送)", queue.len());
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

    let endpoint = Arc::new(EndpointConfig::new("smgp-client", host, port, 100, 60));
    let handler = Arc::new(MyClientHandler::new());

    println!("[客户端] 连接服务器 {}...", server_addr);

    push_test_messages(&handler);

    let client_config = Some(ClientConfig::new());
    let message_source = Some(Arc::new(MockMessageSource) as Arc<dyn MessageSource>);
    let event_handler = Some(Arc::new(MockClientEventHandler) as Arc<dyn ClientEventHandler>);

    let conn = connect(endpoint, handler.clone(), SmgpDecoder, client_config, message_source, event_handler).await?;
    println!("[客户端] 已连接! conn_id={}", conn.id);

    // Step 1: SMGP Login
    println!("\n========== 步骤1: SMGP 登录 ==========");
    
    let login_pdu = handler.build_login_pdu();
    conn.send_request(login_pdu).await?;
    println!("[客户端] 发送登录请求");
    
    for _ in 0..10 {
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        if handler.connected.load(Ordering::Relaxed) { break; }
    }

    // Step 2: 等待 fetch_outbound 自动发送短信并收到响应
    println!("\n========== 步骤2: 等待 MessageSource 自动发送短信 ==========");
    println!("[客户端] fetch_outbound 将在 Login 后自动拉取并发送短信...");
    
    for i in 0..30 {
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        if handler.submit_ok.load(Ordering::Relaxed) {
            println!("[客户端] 收到短信提交响应!");
            break;
        }
        if i == 29 {
            println!("[客户端] 等待提交响应超时");
        }
    }

    // Step 3: 等待状态报告和上行短信（由 fetch_inbound 推送）
    println!("\n========== 步骤3: 等待服务端推送状态报告和上行短信 ==========");
    for i in 0..20 {
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        if handler.deliver_received.load(Ordering::Relaxed) {
            println!("[客户端] 收到状态报告/上行短信!");
            break;
        }
        if i == 19 {
            println!("[客户端] 等待 Deliver 消息超时");
        }
    }

    // Step 4: ActiveTest
    println!("\n========== 步骤4: SMGP 活性检测 ==========");
    let active_test_pdu = handler.build_active_test_pdu();
    conn.send_request(active_test_pdu).await?;
    println!("[客户端] 发送活性检测");
    
    for _ in 0..10 {
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        if handler.active_test_resp_received.load(Ordering::Relaxed) {
            println!("[客户端] 收到活性检测响应!");
            break;
        }
    }

    // Step 5: Exit (Terminate)
    println!("\n========== 步骤5: SMGP 退出 ==========");
    let exit_pdu = handler.build_exit_pdu();
    conn.send_request(exit_pdu).await?;
    println!("[客户端] 发送退出请求");
    
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    println!("\n========== 所有测试完成! ==========");
    Ok(())
}

struct MockMessageSource;

#[async_trait]
impl MessageSource for MockMessageSource {
    async fn fetch(&self, _account: &str, _batch_size: usize) -> Result<Vec<MessageItem>> {
        let mut messages = vec![];
        if let Ok(mut queue) = OUTBOUND_QUEUE.lock() {
            while let Some(sms) = queue.pop() {
                messages.push(MessageItem::Single(Arc::new(sms) as Arc<dyn EncodedPdu>));
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