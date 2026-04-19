use async_trait::async_trait;
use rsms_connector::client::{ClientContext, ClientHandler, ClientConfig};
use rsms_connector::{connect, MessageSource, MessageItem, ClientEventHandler, ProtocolConnection};
use rsms_connector::CmppDecoder;
use rsms_core::{EncodedPdu, RawPdu, EndpointConfig, Frame, Result};
use std::env;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use rsms_codec_cmpp::{
    Pdu, Connect, DeliverResp, ActiveTest, Terminate, CommandId,
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

    fn build_connect_pdu(&self) -> RawPdu {
        let timestamp = 0u32;
        let authenticator = rsms_codec_cmpp::compute_connect_auth(CLIENT_ID, CLIENT_PASSWORD, timestamp);
        
        let connect = Connect {
            source_addr: CLIENT_ID.to_string(),
            authenticator_source: authenticator,
            version: 0x30,
            timestamp,
        };
        
        let pdu: Pdu = connect.into();
        pdu.to_pdu_bytes(self.next_seq())
    }

    fn build_active_test_pdu(&self) -> RawPdu {
        let active_test = ActiveTest;
        let pdu: Pdu = active_test.into();
        pdu.to_pdu_bytes(self.next_seq())
    }

    fn build_terminate_pdu(&self) -> RawPdu {
        let terminate = Terminate;
        let pdu: Pdu = terminate.into();
        pdu.to_pdu_bytes(self.next_seq())
    }
}

#[async_trait]
impl ClientHandler for MyClientHandler {
    fn name(&self) -> &'static str {
        "cmpp-client"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        let cmd_id = frame.command_id;
        let seq_id = frame.sequence_id;

        if cmd_id == CommandId::ConnectResp as u32 {
            if pdu.len() >= 13 && pdu[12] == 0 {
                println!("[客户端] 收到连接响应: status=0 成功");
                self.connected.store(true, Ordering::Relaxed);
            } else {
                println!("[客户端] 收到连接响应: status 非0 失败");
            }
        } else if cmd_id == CommandId::SubmitResp as u32 {
            if pdu.len() >= 13 && pdu[12] == 0 {
                println!("[客户端] 收到提交响应: result=0 成功");
                self.submit_ok.store(true, Ordering::Relaxed);
            } else {
                println!("[客户端] 收到提交响应: result 非0 失败");
            }
        } else if cmd_id == CommandId::Deliver as u32 {
            println!("[客户端] 收到 Deliver");
            self.deliver_received.store(true, Ordering::Relaxed);

            let resp = DeliverResp {
                msg_id: [0u8; 8],
                result: 0,
            };
            let resp_pdu: Pdu = resp.into();
            ctx.conn.write_frame(resp_pdu.to_pdu_bytes(seq_id).as_bytes_ref()).await?;
            println!("[客户端] 发送 Deliver 响应");
        } else if cmd_id == CommandId::ActiveTestResp as u32 {
            println!("[客户端] 收到活性检测响应");
            self.active_test_resp_received.store(true, Ordering::Relaxed);
        }

        Ok(())
    }
}

fn push_test_messages() {
    if let Ok(mut queue) = OUTBOUND_QUEUE.lock() {
        let mut submit = rsms_codec_cmpp::datatypes::Submit::new();
        submit.src_id = "106900".to_string();
        submit.fee_terminal_id = "13800138001".to_string();
        submit.dest_usr_tl = 1;
        submit.dest_terminal_ids = vec!["13800138001".to_string()];
        submit.msg_content = "Hello SMS via fetch_outbound".as_bytes().to_vec();
        submit.registered_delivery = 1;
        
        let pdu: Pdu = submit.into();
        queue.push(pdu.to_pdu_bytes(1001));
        
        println!("[MessageSource] 预填充了 {} 条测试消息 (将在 Login 后自动发送)", queue.len());
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

    let endpoint = Arc::new(EndpointConfig::new("cmpp-client", host, port, 100, 60));
    let handler = Arc::new(MyClientHandler::new());

    println!("[客户端] 连接服务器 {}...", server_addr);

    push_test_messages();

    let client_config = Some(ClientConfig::new());
    let message_source = Some(Arc::new(MockMessageSource) as Arc<dyn MessageSource>);
    let event_handler = Some(Arc::new(MockClientEventHandler) as Arc<dyn ClientEventHandler>);

    let conn = connect(endpoint, handler.clone(), CmppDecoder, client_config, message_source, event_handler).await?;
    println!("[客户端] 已连接! conn_id={}", conn.id);

    println!("\n========== 步骤1: CMPP 连接 ==========");
    let connect_pdu = handler.build_connect_pdu();
    conn.send_request(connect_pdu).await?;
    println!("[客户端] 发送连接请求");
    
    for _ in 0..10 {
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        if handler.connected.load(Ordering::Relaxed) { break; }
    }

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

    println!("\n========== 步骤4: CMPP 活性检测 ==========");
    let at_pdu = handler.build_active_test_pdu();
    conn.send_request(at_pdu).await?;
    println!("[客户端] 发送活性检测");
    
    for _ in 0..10 {
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        if handler.active_test_resp_received.load(Ordering::Relaxed) {
            println!("[客户端] 收到活性检测响应!");
            break;
        }
    }

    println!("\n========== 步骤5: CMPP 退出 ==========");
    let term_pdu = handler.build_terminate_pdu();
    conn.send_request(term_pdu).await?;
    println!("[客户端] 发送退出请求");
    
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    println!("\n========== 所有测试完成! ==========");
    Ok(())
}

struct MockMessageSource;

#[async_trait]
impl MessageSource for MockMessageSource {
    async fn fetch(&self, _account: &str, _batch_size: usize) -> Result<Vec<MessageItem>> {
        let mut messages: Vec<RawPdu> = vec![];
        if let Ok(mut queue) = OUTBOUND_QUEUE.lock() {
            while let Some(sms) = queue.pop() {
                messages.push(sms);
            }
        }
        if !messages.is_empty() {
            println!("[MessageSource] 拉取到 {} 条待发送消息", messages.len());
        }
        Ok(messages.into_iter().map(|pdu| MessageItem::Single(Arc::new(pdu) as Arc<dyn EncodedPdu>)).collect())
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