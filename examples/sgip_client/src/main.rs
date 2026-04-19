// ============================================================================
// SGIP 客户端完整参考实现
//
// 功能：连接 SGIP 服务端 + 明文认证 + 发送短信（含长短信） + 接收状态报告
// 运行：cargo run
// 配置：messages.conf（待发送短信列表）
//
// SGIP 与 CMPP 的关键差异：
//   1. 明文认证（Bind.login_name + Bind.login_password），无 MD5
//   2. 20 字节 Header（12 字节 SgipSequence）
//   3. 独立 Report 命令（不通过 Deliver 承载）
//   4. 必须用 write_frame() 发送所有 PDU（不能用 send_request）
//   5. to_pdu_bytes(node_id, timestamp, number) 三个参数
//
// 核心流程：
//   1. 从 messages.conf 读取待发送短信
//   2. 连接服务端 → 框架启动读循环
//   3. 发送 Bind（明文认证）→ 等待 BindResp
//   4. 认证成功后 MessageSource 自动发送 Submit
//   5. ClientHandler 接收 SubmitResp、Report 和 Deliver
//
// 重要：SGIP 客户端必须用 write_frame() 而非 send_request()
//       因为 send_request 中 sequence_id 提取偏移量硬编码为 CMPP 的 bytes 8-11
//       而 SGIP 的 sequence 是 SgipSequence(node_id, timestamp, number) 在 bytes 8-19
// ============================================================================

use async_trait::async_trait;
use rsms_codec_sgip::{
    Bind, CommandId, DeliverResp, Encodable, ReportResp, SgipSequence, Submit,
};
use rsms_connector::client::{ClientContext, ClientHandler};
use rsms_connector::{connect, MessageItem, MessageSource, SgipDecoder};
use rsms_core::{EncodedPdu, EndpointConfig, Frame, RawPdu, Result};
use rsms_longmsg::split::SmsAlphabet;
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const SP_NUMBER: &str = "106900";
const PASSWORD: &str = "password123";
const SERVER_ADDR: &str = "127.0.0.1:7891";
const NODE_ID: u32 = 1;

fn detect_alphabet(content: &[u8]) -> SmsAlphabet {
    if content.iter().all(|&b| b < 128) {
        SmsAlphabet::ASCII
    } else {
        SmsAlphabet::UCS2
    }
}

fn load_messages(path: &str) -> Vec<(String, String)> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("无法读取 {}: {}, 使用默认消息", path, e);
            return vec![(
                "13800138000".to_string(),
                "Hello from SGIP Client".to_string(),
            )];
        }
    };
    let messages: Vec<(String, String)> = content
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed
                .split_once(' ')
                .map(|(phone, content)| (phone.to_string(), content.to_string()))
        })
        .collect();
    if messages.is_empty() {
        tracing::warn!("{} 中没有有效消息，使用默认消息", path);
        return vec![(
            "13800138000".to_string(),
            "Hello from SGIP Client".to_string(),
        )];
    }
    messages
}

// ============================================================================
// MessageSource：客户端用来发送 Submit
//
// 框架的 run_outbound_fetcher 会循环调用 fetch(endpoint.id, 16)，
// 取出的消息通过 write_frame 直接发出（不走 window 机制）。
//
// 关键：fetch 的 account 参数就是 EndpointConfig.id，
//       必须和 connect() 时设置的 endpoint id 一致。
// ============================================================================

struct ClientMessageSource {
    queue: Arc<Mutex<VecDeque<MessageItem>>>,
    authenticated: Arc<AtomicBool>,
}

impl ClientMessageSource {
    fn from_messages(messages: &[(String, String)], authenticated: Arc<AtomicBool>) -> Self {
        let mut queue = VecDeque::new();
        let mut splitter = LongMessageSplitter::new();
        let mut seq = 1000u32;
        let ts = sgip_timestamp();

        for (phone, content) in messages {
            let content_bytes = content.as_bytes();
            let alphabet = detect_alphabet(content_bytes);
            let single_max = match alphabet {
                SmsAlphabet::GSM7 | SmsAlphabet::ASCII => 160,
                _ => 70,
            };

            if content_bytes.len() > single_max {
                let frames = splitter.split(content_bytes, alphabet);
                let mut items = Vec::new();
                let total = frames.len();

                for frame in frames.into_iter() {
                    let mut submit = Submit::new()
                        .with_message(SP_NUMBER, phone, &frame.content);
                    submit.report_flag = 1;
                    submit.msg_fmt = if alphabet == SmsAlphabet::ASCII { 0 } else { 15 };
                    if frame.has_udhi {
                        submit.tpudhi = 1;
                    }

                    let bytes = submit.to_pdu_bytes(NODE_ID, ts, seq);
                    items.push(Arc::new(RawPdu::from(bytes.to_vec())) as Arc<dyn EncodedPdu>);
                    seq += 1;
                }

                tracing::info!(
                    "长短信拆分: {} 字节 → {} 段 (phone={})",
                    content_bytes.len(), total, phone
                );
                queue.push_back(MessageItem::Group { items });
            } else {
                let mut submit = Submit::new()
                    .with_message(SP_NUMBER, phone, content_bytes);
                submit.report_flag = 1;
                submit.msg_fmt = 15;

                let bytes = submit.to_pdu_bytes(NODE_ID, ts, seq);
                queue.push_back(MessageItem::Single(Arc::new(RawPdu::from(bytes.to_vec())) as Arc<dyn EncodedPdu>));
                seq += 1;
            }
        }

        tracing::info!("MessageSource: 已加载 {} 条待发送消息", queue.len());

        Self {
            queue: Arc::new(Mutex::new(queue)),
            authenticated,
        }
    }
}

#[async_trait]
impl MessageSource for ClientMessageSource {
    async fn fetch(&self, account: &str, batch_size: usize) -> Result<Vec<MessageItem>> {
        if !self.authenticated.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }

        let mut result = Vec::new();
        let mut queue = self.queue.lock().unwrap();
        while result.len() < batch_size {
            match queue.pop_front() {
                Some(item) => result.push(item),
                None => break,
            }
        }

        if !result.is_empty() {
            tracing::info!(
                "MessageSource: 发送 {} 条消息 (account={})",
                result.len(),
                account
            );
        }

        Ok(result)
    }
}

// ============================================================================
// ClientHandler：处理服务端下发的所有帧
//
// 框架读循环收到帧后调用 on_inbound，业务方在这里处理：
// - BindResp：认证结果
// - SubmitResp：提交结果
// - Report：独立状态报告（SGIP 特有，不通过 Deliver 承载）
// - Deliver：MO 上行短信（不承载报告，含长短信合包）
// - UnbindResp：断连响应
// ============================================================================

struct SgipClientHandler {
    authenticated: Arc<AtomicBool>,
    submit_count: AtomicU32,
    report_count: AtomicU32,
    mo_merger: std::sync::Mutex<LongMessageMerger>,
}

impl SgipClientHandler {
    fn new(authenticated: Arc<AtomicBool>) -> Self {
        Self {
            authenticated,
            submit_count: AtomicU32::new(0),
            report_count: AtomicU32::new(0),
            mo_merger: std::sync::Mutex::new(LongMessageMerger::new()),
        }
    }
}

fn extract_sgip_sequence(data: &[u8]) -> Option<SgipSequence> {
    if data.len() < 20 {
        return None;
    }
    let node_id = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let timestamp = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
    let number = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    Some(SgipSequence::new(node_id, timestamp, number))
}

#[async_trait]
impl ClientHandler for SgipClientHandler {
    fn name(&self) -> &'static str {
        "sgip-client"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let data = frame.data_as_slice();

        match frame.command_id {
            cmd if cmd == CommandId::BindResp as u32 => {
                if data.len() >= 21 {
                    let result = data[20];
                    if result == 0 {
                        tracing::info!("SGIP 认证成功");
                        self.authenticated.store(true, Ordering::Relaxed);
                    } else {
                        tracing::error!("SGIP 认证失败: result={}", result);
                    }
                }
            }

            cmd if cmd == CommandId::SubmitResp as u32 => {
                if data.len() >= 24 {
                    let result = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
                    let count = self.submit_count.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::info!("[{}] SubmitResp: result={}", count, result);
                }
            }

            cmd if cmd == CommandId::Report as u32 => {
                let count = self.report_count.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::info!("[{}] 收到 Report（状态报告）", count);

                if let Some(seq) = extract_sgip_sequence(data) {
                    let resp = ReportResp { result: 0 };
                    ctx.conn
                        .write_frame(&resp.to_pdu_bytes(seq.node_id, seq.timestamp, seq.number))
                        .await?;
                }
            }

            cmd if cmd == CommandId::Deliver as u32 => {
                if let Some(seq) = extract_sgip_sequence(data) {
                    let resp = DeliverResp { result: 0 };
                    ctx.conn
                        .write_frame(&resp.to_pdu_bytes(seq.node_id, seq.timestamp, seq.number))
                        .await?;
                }

                if let Ok(rsms_codec_sgip::SgipMessage::Deliver(deliver)) = rsms_codec_sgip::decode_message(data) {
                    if deliver.tpudhi == 1 {
                        if let Some((udh, _)) = UdhParser::extract_udh(&deliver.message_content) {
                            let seg_num = udh.segment_number;
                            let seg_total = udh.total_segments;
                            let frame = LongMessageFrame::new(
                                udh.reference_id,
                                seg_total,
                                seg_num,
                                deliver.message_content.clone(),
                                true,
                                Some(udh),
                            );
                            let mut merger = self.mo_merger.lock().unwrap();
                            match merger.add_frame(frame) {
                                Ok(Some(complete)) => {
                                    let content = String::from_utf8_lossy(&complete);
                                    tracing::info!(
                                        "长短信 MO 合包完成: src={}, 内容={}",
                                        deliver.user_number,
                                        content
                                    );
                                }
                                Ok(None) => {
                                    tracing::info!(
                                        "长短信 MO 分段 {}/{} 等待更多分段",
                                        seg_num,
                                        seg_total
                                    );
                                }
                                Err(e) => tracing::warn!("长短信 MO 合包错误: {}", e),
                            }
                        }
                    } else {
                        let content = String::from_utf8_lossy(&deliver.message_content);
                        tracing::info!(
                            "收到 Deliver（MO 上行短信）: src={}, content={}",
                            deliver.user_number,
                            content
                        );
                    }
                }
            }

            cmd if cmd == CommandId::UnbindResp as u32 => {
                tracing::debug!("收到 UnbindResp");
            }

            _ => {
                tracing::debug!("收到未知命令: command_id=0x{:08x}", frame.command_id);
            }
        }

        Ok(())
    }
}

fn sgip_timestamp() -> u32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let month = ((secs / 86400 / 30) % 12 + 1) as u32;
    let day = ((secs / 86400) % 30 + 1) as u32;
    let h = ((secs / 3600) % 24 + 8) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    month * 1000000 + day * 10000 + h as u32 * 100 + m as u32 * 10 + (s % 10) as u32
}

// ============================================================================
// main：组装各组件并启动
//
// 启动顺序：
//   1. connect() 建立 TCP 连接，启动读循环、keepalive、outbound fetcher
//   2. write_frame() 发送 Bind 认证（不能用 send_request，SGIP 偏移量不同）
//   3. MessageSource.fetch() 被 outbound fetcher 循环调用，认证后自动发出 Submit
//   4. ClientHandler.on_inbound() 处理所有服务端响应
// ============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let messages_path = std::path::Path::new(manifest_dir).join("messages.conf");
    let messages = load_messages(&messages_path.to_string_lossy());
    tracing::info!("加载了 {} 条待发送消息", messages.len());

    let authenticated = Arc::new(AtomicBool::new(false));
    let msg_source = Arc::new(ClientMessageSource::from_messages(
        &messages,
        authenticated.clone(),
    ));
    let handler = Arc::new(SgipClientHandler::new(authenticated.clone()));

    let (host, port) = if let Some((h, p)) = SERVER_ADDR.rsplit_once(':') {
        (h.to_string(), p.parse().unwrap_or(7891))
    } else {
        ("127.0.0.1".to_string(), 7891)
    };

    let endpoint = Arc::new(
        EndpointConfig::new(SP_NUMBER, host, port, 100, 60)
            .with_protocol("sgip")
            .with_window_size(2048)
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!("正在连接 SGIP 服务端 {}...", SERVER_ADDR);

    let conn = connect(
        endpoint,
        handler,
        SgipDecoder,
        None,
        Some(msg_source as Arc<dyn MessageSource>),
        None,
    )
    .await?;

    tracing::info!("TCP 连接已建立 (conn_id={})", conn.id);

    let bind = Bind {
        login_type: 1,
        login_name: SP_NUMBER.to_string(),
        login_password: PASSWORD.to_string(),
        reserve: [0u8; 8],
    };
    let bind_bytes = bind.to_pdu_bytes(NODE_ID, sgip_timestamp(), 1);
    conn.write_frame(&bind_bytes).await?;
    tracing::info!("已发送 Bind 认证请求（明文）");

    tracing::info!("认证成功后 MessageSource 将自动发送短信...");
    tracing::info!("按 Ctrl+C 退出");

    tokio::signal::ctrl_c().await?;
    tracing::info!("收到退出信号，正在关闭...");

    Ok(())
}
