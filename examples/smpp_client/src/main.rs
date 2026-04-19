// ============================================================================
// SMPP 客户端完整参考实现
//
// 功能：连接 SMPP 服务端 + 明文认证 + 发送短信 + 接收状态报告 + 长短信拆分/合包
// 运行：cargo run
// 配置：messages.conf（待发送短信列表）
//
// 核心流程：
//   1. 从 messages.conf 读取待发送短信
//   2. 连接服务端 → 框架启动读循环
//   3. 发送 BindTransmitter（明文认证）→ 等待 BindTransmitterResp
//   4. 认证成功后 MessageSource 自动发送 SubmitSm
//   5. ClientHandler 接收 SubmitSmResp 和 DeliverSm（Report/MO）
//
// SMPP 与 CMPP 的关键差异：
//   - 认证是明文：BindTransmitter::new(system_id, password, system_type, interface_version)
//   - EndpointConfig 必须加 .with_protocol("smpp")（sequence_id 在 bytes 12-15）
//   - Report 通过 DeliverSm(esm_class & 0x04) 判断
//   - MsgId 是 String 类型
//   - EnquireLink 替代 ActiveTest
//   - 长短信用 esm_class bit 6 (0x40) 表示 TP-UDHI（无独立 tpudhi 字段）
// ============================================================================

use async_trait::async_trait;
use rsms_codec_smpp::{
    decode_message, BindTransmitter, CommandId, DeliverSmResp, Pdu, SmppMessage, SubmitSm,
};
use rsms_connector::client::{ClientContext, ClientHandler};
use rsms_connector::{connect, MessageItem, MessageSource, SmppDecoder};
use rsms_core::{EncodedPdu, EndpointConfig, Frame, Result};
use rsms_longmsg::{
    LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser,
    split::SmsAlphabet,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const ACCOUNT: &str = "900001";
const PASSWORD: &str = "passwd12";
const SERVER_ADDR: &str = "127.0.0.1:7893";

fn detect_alphabet(content: &str) -> SmsAlphabet {
    if content.is_ascii() {
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
                "Hello from SMPP Client".to_string(),
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
            "Hello from SMPP Client".to_string(),
        )];
    }
    messages
}

// ============================================================================
// MessageSource：客户端用来发送 SubmitSm
//
// 框架的 run_outbound_fetcher 会循环调用 fetch(endpoint.id, 16)，
// 取出的消息通过 write_frame 直接发出（不走 window 机制）。
//
// 关键：fetch 的 account 参数就是 EndpointConfig.id，
//       必须和 connect() 时设置的 endpoint id 一致。
//
// 长短信支持：
//   - 超过单条限制的消息自动拆分为多个 SubmitSm
//   - 每个分段 esm_class |= 0x40（TP-UDHI）
//   - 使用 MessageItem::Group 保证同组帧走同一连接顺序发出
// ============================================================================

struct ClientMessageSource {
    queue: Arc<Mutex<VecDeque<MessageItem>>>,
    authenticated: Arc<AtomicBool>,
}

impl ClientMessageSource {
    fn from_messages(messages: &[(String, String)], authenticated: Arc<AtomicBool>) -> Self {
        let mut queue = VecDeque::new();
        let mut splitter = LongMessageSplitter::new();

        for (phone, content) in messages {
            let alphabet = detect_alphabet(content);
            let frames = splitter.split(content.as_bytes(), alphabet);

            if frames.len() == 1 && !frames[0].has_udhi {
                let mut submit =
                    SubmitSm::new().with_message(phone, ACCOUNT, &frames[0].content);
                submit.registered_delivery = 1;
                let pdu: Pdu = submit.into();
                queue.push_back(MessageItem::Single(
                    Arc::new(pdu.to_pdu_bytes(0)) as Arc<dyn EncodedPdu>
                ));
            } else {
                let items: Vec<Arc<dyn EncodedPdu>> = frames
                    .into_iter()
                    .map(|frame| {
                        let mut submit =
                            SubmitSm::new().with_message(phone, ACCOUNT, &frame.content);
                        submit.esm_class = 0x40;
                        submit.registered_delivery = 1;
                        let pdu: Pdu = submit.into();
                        Arc::new(pdu.to_pdu_bytes(0)) as Arc<dyn EncodedPdu>
                    })
                    .collect();
                queue.push_back(MessageItem::Group { items });
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
// - BindTransmitterResp：认证结果
// - SubmitSmResp：提交结果
// - DeliverSm：
//   - esm_class & 0x04 → 状态报告
//   - esm_class & 0x40 → 长短信 MO 分段（用 merger 合包）
//   - 其他 → 普通短消息 MO
// - EnquireLinkResp：心跳响应
// ============================================================================

struct SmppClientHandler {
    authenticated: Arc<AtomicBool>,
    submit_count: AtomicU32,
    report_count: AtomicU32,
    mo_merger: Mutex<LongMessageMerger>,
}

impl SmppClientHandler {
    fn new(authenticated: Arc<AtomicBool>) -> Self {
        Self {
            authenticated,
            submit_count: AtomicU32::new(0),
            report_count: AtomicU32::new(0),
            mo_merger: Mutex::new(LongMessageMerger::new()),
        }
    }
}

#[async_trait]
impl ClientHandler for SmppClientHandler {
    fn name(&self) -> &'static str {
        "smpp-client"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let data = frame.data_as_slice();
        let seq_id = frame.sequence_id;

        match frame.command_id {
            cmd if cmd == CommandId::BIND_TRANSMITTER_RESP as u32 => match decode_message(data) {
                Ok(SmppMessage::BindTransmitterResp(resp)) => {
                    tracing::info!("✓ SMPP 认证成功 (system_id={})", resp.system_id);
                    self.authenticated.store(true, Ordering::Relaxed);
                }
                _ => tracing::warn!("无法解析 BindTransmitterResp"),
            },

            cmd if cmd == CommandId::SUBMIT_SM_RESP as u32 => match decode_message(data) {
                Ok(SmppMessage::SubmitSmResp(resp)) => {
                    let count = self.submit_count.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::info!(
                        "[{}] SubmitSmResp: message_id={}",
                        count,
                        resp.message_id
                    );
                }
                _ => tracing::warn!("无法解析 SubmitSmResp"),
            },

            cmd if cmd == CommandId::DELIVER_SM as u32 => match decode_message(data) {
                Ok(SmppMessage::DeliverSm(deliver)) => {
                    let esm_class = deliver.esm_class;

                    if esm_class & 0x04 != 0 {
                        let content = String::from_utf8_lossy(&deliver.short_message);
                        let count = self.report_count.fetch_add(1, Ordering::Relaxed) + 1;
                        tracing::info!(
                            "[{}] 状态报告: src={}, content={}",
                            count,
                            deliver.source_addr,
                            content
                        );
                    } else if esm_class & 0x40 != 0 {
                        if let Some((udh, _)) = UdhParser::extract_udh(&deliver.short_message) {
                            let seg_num = udh.segment_number;
                            let total = udh.total_segments;
                            let frame = LongMessageFrame::new(
                                udh.reference_id,
                                total,
                                seg_num,
                                deliver.short_message.clone(),
                                true,
                                Some(udh),
                            );
                            let mut merger = self.mo_merger.lock().unwrap();
                            match merger.add_frame(frame) {
                                Ok(Some(merged)) => {
                                    let content = String::from_utf8_lossy(&merged);
                                    tracing::info!(
                                        "长短信 MO 合包完成: src={}, dest={}, content={}",
                                        deliver.source_addr,
                                        deliver.destination_addr,
                                        content
                                    );
                                }
                                Ok(None) => {
                                    tracing::debug!(
                                        "长短信 MO 分段 {}/{} 等待更多分段",
                                        seg_num,
                                        total
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!("长短信 MO 合包失败: {}", e);
                                }
                            }
                        }
                    } else {
                        let content = String::from_utf8_lossy(&deliver.short_message);
                        tracing::info!(
                            "上行短信: src={}, dest={}, content={}",
                            deliver.source_addr,
                            deliver.destination_addr,
                            content
                        );
                    }

                    let resp = DeliverSmResp {
                        message_id: String::new(),
                    };
                    let resp_pdu: Pdu = resp.into();
                    ctx.conn
                        .write_frame(resp_pdu.to_pdu_bytes(seq_id).as_bytes_ref())
                        .await?;
                }
                _ => tracing::warn!("无法解析 DeliverSm"),
            },

            cmd if cmd == CommandId::ENQUIRE_LINK_RESP as u32 => {
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
//   2. write_frame() 发送 BindTransmitter 认证
//   3. MessageSource.fetch() 被 outbound fetcher 循环调用，认证后自动发出 SubmitSm
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
    let handler = Arc::new(SmppClientHandler::new(authenticated.clone()));

    let (host, port) = if let Some((h, p)) = SERVER_ADDR.rsplit_once(':') {
        (h.to_string(), p.parse().unwrap_or(7893))
    } else {
        ("127.0.0.1".to_string(), 7893)
    };

    let endpoint = Arc::new(
        EndpointConfig::new(ACCOUNT, host, port, 100, 60)
            .with_protocol("smpp")
            .with_window_size(2048)
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!("正在连接 SMPP 服务端 {}...", SERVER_ADDR);

    let conn = connect(
        endpoint,
        handler,
        SmppDecoder,
        None,
        Some(msg_source as Arc<dyn MessageSource>),
        None,
    )
    .await?;

    tracing::info!("TCP 连接已建立 (conn_id={})", conn.id);

    let bind = BindTransmitter::new(ACCOUNT, PASSWORD, "CMT", 0x34);
    let pdu: Pdu = bind.into();
    conn.send_request(pdu.to_pdu_bytes(1)).await?;
    tracing::info!("已发送 BindTransmitter 认证请求");

    tracing::info!("认证成功后 MessageSource 将自动发送短信...");
    tracing::info!("按 Ctrl+C 退出");

    tokio::signal::ctrl_c().await?;
    tracing::info!("收到退出信号，正在关闭...");

    Ok(())
}
