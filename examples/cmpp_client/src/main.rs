// ============================================================================
// CMPP 客户端完整参考实现
//
// 功能：连接 CMPP 服务端 + MD5 认证 + 发送短信（含长短信） + 接收状态报告
// 运行：cargo run
// 配置：messages.conf（待发送短信列表）
//
// 核心流程：
//   1. 从 messages.conf 读取待发送短信
//   2. 连接服务端 → 框架启动读循环
//   3. 发送 Connect（MD5 认证）→ 等待 ConnectResp
//   4. 认证成功后 MessageSource 自动发送 Submit
//   5. ClientHandler 接收 SubmitResp 和 Deliver（Report/MO）
// ============================================================================

use async_trait::async_trait;
use rsms_codec_cmpp::{
    decode_message, compute_connect_auth, CmppMessage, CmppReport, CommandId, Connect, DeliverResp,
    Pdu, Submit,
};
use rsms_connector::client::{ClientContext, ClientHandler};
use rsms_connector::{connect, MessageItem, MessageSource, CmppDecoder};
use rsms_core::{EncodedPdu, EndpointConfig, Frame, Result};
use rsms_longmsg::split::SmsAlphabet;
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const ACCOUNT: &str = "900001";
const PASSWORD: &str = "password123";
const SERVER_ADDR: &str = "127.0.0.1:7890";

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
                "Hello from CMPP Client".to_string(),
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
            "Hello from CMPP Client".to_string(),
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

                for (i, frame) in frames.into_iter().enumerate() {
                    let mut submit = Submit::new().with_message(ACCOUNT, phone, &frame.content);
                    submit.src_id = ACCOUNT.to_string();
                    submit.fee_terminal_id = phone.clone();
                    submit.registered_delivery = 1;
                    submit.pk_total = total as u8;
                    submit.pk_number = (i + 1) as u8;
                    submit.msg_fmt = if alphabet == SmsAlphabet::ASCII { 0 } else { 15 };
                    if frame.has_udhi {
                        submit.tpudhi = 1;
                    }
                    let pdu: Pdu = submit.into();
                    items.push(Arc::new(pdu.to_pdu_bytes(seq)) as Arc<dyn EncodedPdu>);
                    seq += 1;
                }

                tracing::info!(
                    "长短信拆分: {} 字节 → {} 段 (phone={})",
                    content_bytes.len(), total, phone
                );
                queue.push_back(MessageItem::Group { items });
            } else {
                let mut submit = Submit::new().with_message(ACCOUNT, phone, content_bytes);
                submit.src_id = ACCOUNT.to_string();
                submit.fee_terminal_id = phone.clone();
                submit.registered_delivery = 1;
                let pdu: Pdu = submit.into();
                queue.push_back(MessageItem::Single(Arc::new(pdu.to_pdu_bytes(seq))));
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
// - ConnectResp：认证结果
// - SubmitResp：提交结果
// - Deliver：状态报告（registered_delivery=1）或 MO 上行（=0）
// - ActiveTestResp：心跳响应
// ============================================================================

struct CmppClientHandler {
    authenticated: Arc<AtomicBool>,
    submit_count: AtomicU32,
    report_count: AtomicU32,
    mo_merger: std::sync::Mutex<LongMessageMerger>,
}

impl CmppClientHandler {
    fn new(authenticated: Arc<AtomicBool>) -> Self {
        Self {
            authenticated,
            submit_count: AtomicU32::new(0),
            report_count: AtomicU32::new(0),
            mo_merger: std::sync::Mutex::new(LongMessageMerger::new()),
        }
    }
}

#[async_trait]
impl ClientHandler for CmppClientHandler {
    fn name(&self) -> &'static str {
        "cmpp-client"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let data = frame.data_as_slice();
        let seq_id = frame.sequence_id;

        match frame.command_id {
            cmd if cmd == CommandId::ConnectResp as u32 => match decode_message(data) {
                Ok(CmppMessage::ConnectResp { resp, .. }) => {
                    if resp.status == 0 {
                        tracing::info!("✓ CMPP 认证成功 (version=0x{:02x})", resp.version);
                        self.authenticated.store(true, Ordering::Relaxed);
                    } else {
                        tracing::error!("✗ CMPP 认证失败: status={}", resp.status);
                    }
                }
                _ => tracing::warn!("无法解析 ConnectResp"),
            },

            cmd if cmd == CommandId::SubmitResp as u32 => match decode_message(data) {
                Ok(CmppMessage::SubmitResp { resp, .. }) => {
                    let count = self.submit_count.fetch_add(1, Ordering::Relaxed) + 1;
                    let msg_id: String = resp.msg_id.iter().map(|b| format!("{:02x}", b)).collect();
                    tracing::info!(
                        "[{}] SubmitResp: msg_id={}, result={}",
                        count,
                        msg_id,
                        resp.result
                    );
                }
                _ => tracing::warn!("无法解析 SubmitResp"),
            },

            cmd if cmd == CommandId::Deliver as u32 => match decode_message(data) {
                Ok(CmppMessage::DeliverV30 { deliver, .. }) => {
                    if deliver.registered_delivery == 1 {
                        let report_str = String::from_utf8_lossy(&deliver.msg_content);
                        if let Some(report) = CmppReport::parse(&report_str) {
                            let count = self.report_count.fetch_add(1, Ordering::Relaxed) + 1;
                            tracing::info!(
                                "[{}] 状态报告: msg_id={}, stat={}, dest={}",
                                count,
                                report.msg_id,
                                report.stat,
                                report.dest_terminal_id
                            );
                        }
                    } else if deliver.tpudhi == 1 {
                        if let Some((udh, _)) = UdhParser::extract_udh(&deliver.msg_content) {
                            let ref_id = udh.reference_id;
                            let seg_num = udh.segment_number;
                            let seg_total = udh.total_segments;
                            let frame = LongMessageFrame::new(
                                ref_id,
                                seg_total,
                                seg_num,
                                deliver.msg_content.clone(),
                                true,
                                Some(udh),
                            );
                            let mut merger = self.mo_merger.lock().unwrap();
                            match merger.add_frame(frame) {
                                Ok(Some(complete)) => {
                                    let content = String::from_utf8_lossy(&complete);
                                    tracing::info!(
                                        "长短信 MO 合包完成: src={}, 内容={}",
                                        deliver.src_terminal_id,
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
                        let content = String::from_utf8_lossy(&deliver.msg_content);
                        tracing::info!(
                            "上行短信: src={}, content={}",
                            deliver.src_terminal_id,
                            content
                        );
                    }

                    let resp = DeliverResp {
                        msg_id: deliver.msg_id,
                        result: 0,
                    };
                    let resp_pdu: Pdu = resp.into();
                    ctx.conn
                        .write_frame(resp_pdu.to_pdu_bytes(seq_id).as_bytes_ref())
                        .await?;
                }
                _ => tracing::warn!("无法解析 Deliver"),
            },

            cmd if cmd == CommandId::ActiveTestResp as u32 => {
                tracing::debug!("收到心跳响应");
            }

            _ => {
                tracing::debug!("收到未知命令: command_id=0x{:08x}", frame.command_id);
            }
        }

        Ok(())
    }
}

// ============================================================================
// main：组装各组件并启动
//
// 启动顺序：
//   1. connect() 建立 TCP 连接，启动读循环、keepalive、outbound fetcher
//   2. send_request() 发送 Connect 认证（通过 window 跟踪响应）
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
    let handler = Arc::new(CmppClientHandler::new(authenticated.clone()));

    let (host, port) = if let Some((h, p)) = SERVER_ADDR.rsplit_once(':') {
        (h.to_string(), p.parse().unwrap_or(7890))
    } else {
        ("127.0.0.1".to_string(), 7890)
    };

    // EndpointConfig.id 必须和 MessageSource 的 fetch key 一致
    // 因为 run_outbound_fetcher 用 conn.authenticated_account() 作为 key
    // 而 authenticated_account() 返回 endpoint.id
    let endpoint = Arc::new(
        EndpointConfig::new(ACCOUNT, host, port, 100, 60)
            .with_window_size(2048)
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!("正在连接 CMPP 服务端 {}...", SERVER_ADDR);

    let conn = connect(
        endpoint,
        handler,
        CmppDecoder,
        None,
        Some(msg_source as Arc<dyn MessageSource>),
        None,
    )
    .await?;

    tracing::info!("TCP 连接已建立 (conn_id={})", conn.id);

    // 发送 Connect 认证（MD5）
    let timestamp = 0u32;
    let authenticator = compute_connect_auth(ACCOUNT, PASSWORD, timestamp);
    let connect_pdu = Connect {
        source_addr: ACCOUNT.to_string(),
        authenticator_source: authenticator,
        version: 0x30,
        timestamp,
    };
    let pdu: Pdu = connect_pdu.into();
    conn.send_request(pdu.to_pdu_bytes(1)).await?;
    tracing::info!("已发送 Connect 认证请求");

    tracing::info!("认证成功后 MessageSource 将自动发送短信...");
    tracing::info!("按 Ctrl+C 退出");

    tokio::signal::ctrl_c().await?;
    tracing::info!("收到退出信号，正在关闭...");

    Ok(())
}
