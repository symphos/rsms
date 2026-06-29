// ============================================================================
// SMPP 客户端参考实现（统一模型 / 窄腰架构版）
//
// 与旧版的根本区别：业务代码不再直接接触 SMPP 裸 codec 类型（SubmitSm/DeliverSm…），
// 全程只用协议无关的 `rsms_model::UnifiedMessage` + `SmppAdapter`（实现 ProtocolAdapter）：
//   - 收包：SmppAdapter.decode(frame) -> UnifiedMessage，业务按统一枚举分支处理
//   - 发包：构造 UnifiedMessage -> SmppAdapter.encode(msg, Sequence) -> 字节
// 切换协议只需换 adapter（CmppAdapter/SmgpAdapter/SgipAdapter）与 Decoder，业务逻辑零改。
//
// 功能：连接 SMPP 服务端 + 明文认证(BindTransceiver) + 发送短信 + 收回执/MO + 长短信拆分/合包
// 连接：默认连本机 smpp_server 示例（127.0.0.1:7893），BindTransceiver
// ============================================================================

use async_trait::async_trait;
use rsms_business::{MessageContext, MessageHandler};
use rsms_codec_smpp::adapter::SmppAdapter;
use rsms_connector::{ClientBuilder, MessageItem, MessageSource, SmppDecoder};
use rsms_core::{EncodedPdu, EndpointConfig, Protocol, RawPdu, Result};
use rsms_longmsg::{
    split::SmsAlphabet, LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser,
};
use rsms_model::{
    Address, BindMode, Concat, Encoding, ProtocolAdapter, ProtocolExtra, Sequence, SmppExtra,
    UnifiedBind, UnifiedMessage, UnifiedSubmit,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

// 连接配置：默认连本机的 smpp_server 示例（监听 7893，账号见其 accounts.conf）。
// SMPP DUPLEX 端点须 BindTransceiver（下方 main 里构造）。
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

/// 把统一 SmsAlphabet 翻译为统一模型 Encoding。
fn encoding_of(alphabet: SmsAlphabet) -> Encoding {
    match alphabet {
        SmsAlphabet::ASCII | SmsAlphabet::GSM7 => Encoding::Ascii,
        _ => Encoding::Ucs2,
    }
}

/// 把文本按目标编码转为 wire 字节：UCS2 须为 UTF-16BE（每字符 2 字节大端）。
/// **关键**：LongMessageSplitter 只按字节分段、不转码，若直接传 content.as_bytes()（UTF-8）
/// 却标 dcs=UCS2，对端会按 UTF-16BE 解 UTF-8 → 中文乱码（联调实测）。
fn to_wire_bytes(content: &str, alphabet: SmsAlphabet) -> Vec<u8> {
    match alphabet {
        SmsAlphabet::UCS2 => content.encode_utf16().flat_map(|u| u.to_be_bytes()).collect(),
        _ => content.as_bytes().to_vec(),
    }
}

/// 按编码解码 wire 字节用于显示：UCS2 按 UTF-16BE 还原，否则按 UTF-8。
fn decode_text(bytes: &[u8], encoding: Encoding) -> String {
    match encoding {
        Encoding::Ucs2 => {
            let u16s: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&u16s)
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn load_messages(path: &str) -> Vec<(String, String)> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("无法读取 {}: {}, 使用默认消息", path, e);
            return vec![("13800138000".to_string(), "Hello from SMPP Client".to_string())];
        }
    };
    let messages: Vec<(String, String)> = content
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
        .filter_map(|line| {
            line.trim()
                .split_once(' ')
                .map(|(phone, content)| (phone.to_string(), content.to_string()))
        })
        .collect();
    if messages.is_empty() {
        tracing::warn!("{} 中没有有效消息，使用默认消息", path);
        return vec![("13800138000".to_string(), "Hello from SMPP Client".to_string())];
    }
    messages
}

/// 构造一条 SMPP 提交的统一消息。
/// SMPP 的 registered_delivery / esm_class 是协议方言，落在 ProtocolExtra::Smpp 里；
/// 注意 SMPP adapter 的 encode 用 extra.registered_delivery 驱动回执标志（非 want_report）。
fn build_submit(phone: &str, content: &[u8], encoding: Encoding, concat: Option<Concat>) -> UnifiedMessage {
    UnifiedMessage::Submit(UnifiedSubmit {
        src: Address::plain(ACCOUNT),
        dests: vec![Address::plain(phone)],
        content: content.to_vec(),
        encoding,
        want_report: true,
        // 传 concat（Some=长短信分段），adapter 据此重建 UDH 并置 esm_class TP-UDHI(0x40)。
        concat,
        extra: ProtocolExtra::Smpp(SmppExtra {
            registered_delivery: 1, // 需要状态报告
            ..Default::default()
        }),
        tlvs: vec![],
    })
}

// ============================================================================
// MessageSource：把待发短信编码为 SMPP 字节（经 SmppAdapter.encode）
// ============================================================================

struct ClientMessageSource {
    queue: Arc<Mutex<VecDeque<MessageItem>>>,
    authenticated: Arc<AtomicBool>,
    seq: AtomicU32,
}

impl ClientMessageSource {
    fn from_messages(messages: &[(String, String)], authenticated: Arc<AtomicBool>) -> Self {
        let seq = AtomicU32::new(1);
        let next_seq = || seq.fetch_add(1, Ordering::Relaxed);
        let mut queue = VecDeque::new();
        let mut splitter = LongMessageSplitter::new();

        for (phone, content) in messages {
            let alphabet = detect_alphabet(content);
            let encoding = encoding_of(alphabet);
            let wire = to_wire_bytes(content, alphabet);
            let frames = splitter.split(&wire, alphabet);

            if frames.len() == 1 && !frames[0].has_udhi {
                let msg = build_submit(phone, &frames[0].content, encoding, None);
                let bytes = SmppAdapter
                    .encode(&msg, Sequence::Plain(next_seq()))
                    .expect("encode submit");
                queue.push_back(MessageItem::Single(
                    Arc::new(RawPdu::from(bytes)) as Arc<dyn EncodedPdu>
                ));
            } else {
                // 长短信：每段传 concat+纯载荷，adapter 重建 UDH 并置 TP-UDHI，同组顺序发出
                let items: Vec<Arc<dyn EncodedPdu>> = frames
                    .into_iter()
                    .map(|frame| {
                        let concat = Some(Concat {
                            reference: frame.reference_id,
                            total: frame.total_segments,
                            sequence: frame.segment_number,
                        });
                        let payload = UdhParser::strip_udh(&frame.content);
                        let msg = build_submit(phone, &payload, encoding, concat);
                        let bytes = SmppAdapter
                            .encode(&msg, Sequence::Plain(next_seq()))
                            .expect("encode submit segment");
                        Arc::new(RawPdu::from(bytes)) as Arc<dyn EncodedPdu>
                    })
                    .collect();
                queue.push_back(MessageItem::Group { items });
            }
        }

        tracing::info!("MessageSource: 已加载 {} 条待发送消息", queue.len());

        Self {
            queue: Arc::new(Mutex::new(queue)),
            authenticated,
            seq,
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
            tracing::info!("MessageSource: 发送 {} 条消息 (account={})", result.len(), account);
        }
        Ok(result)
    }
}

// ============================================================================
// MessageHandler：统一模型分支处理服务端下发的所有帧
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

    /// 处理上行短信内容：有 concat 则合包，否则直接呈现（按编码正确解码显示）。
    /// adapter 已把 UDH 剥成 concat、content 为纯载荷；据 concat 重建含 UDH 段交 merger。
    fn handle_mo(&self, src: &str, dest: &str, concat: Option<&Concat>, content: Vec<u8>, encoding: Encoding) {
        if let Some(c) = concat {
            let mut seg = c.to_udh_prefix();
            seg.extend_from_slice(&content);
            let udh = UdhParser::extract_udh(&seg).map(|(h, _)| h);
            let frame =
                LongMessageFrame::new(c.reference, c.total, c.sequence, seg, true, udh);
            let mut merger = self.mo_merger.lock().unwrap();
            match merger.add_frame(src, frame) {
                Ok(Some(merged)) => tracing::info!(
                    "长短信 MO 合包完成: src={}, dest={}, content={}",
                    src, dest, decode_text(&merged, encoding)
                ),
                Ok(None) => tracing::debug!("长短信 MO 分段 {}/{} 等待更多分段", c.sequence, c.total),
                Err(e) => tracing::warn!("长短信 MO 合包失败: {}", e),
            }
        } else {
            tracing::info!("上行短信: src={}, dest={}, content={}", src, dest, decode_text(&content, encoding));
        }
    }
}

#[async_trait]
impl MessageHandler for SmppClientHandler {
    fn name(&self) -> &'static str {
        "smpp-client"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        // 框架已按协议解码为统一消息，业务直接按枚举分支处理。
        match msg {
            UnifiedMessage::BindResp(resp) => {
                if resp.status == 0 {
                    tracing::info!("✓ SMPP 认证成功");
                    self.authenticated.store(true, Ordering::Relaxed);
                } else {
                    tracing::error!("SMPP 认证失败: status={}", resp.status);
                }
            }
            UnifiedMessage::SubmitResp(resp) => {
                let count = self.submit_count.fetch_add(1, Ordering::Relaxed) + 1;
                // msg: &UnifiedMessage，resp 为借用，msg_id 须 borrow 匹配。
                let id = match &resp.msg_id {
                    rsms_model::MessageId::Text(t) => t.clone(),
                    rsms_model::MessageId::Binary(b) => String::from_utf8_lossy(b).into_owned(),
                };
                tracing::info!("[{}] SubmitResp: message_id={}", count, id);
            }
            UnifiedMessage::Report(report) => {
                let count = self.report_count.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::info!(
                    "[{}] 状态报告: src={}, dest={}, status={:?}, content={}",
                    count, report.src.number, report.dest.number, report.status,
                    String::from_utf8_lossy(&report.raw)
                );
                // 一步回执（窄腰）：框架按请求帧序列编码 DeliverResp 并写回。
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            UnifiedMessage::Deliver(deliver) => {
                let enc = deliver.encoding;
                // msg: &UnifiedMessage，deliver.content 须 clone（handle_mo 取 Vec<u8>）。
                self.handle_mo(&deliver.src.number, &deliver.dest.number, deliver.concat.as_ref(), deliver.content.clone(), enc);
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            UnifiedMessage::UnbindResp => tracing::info!("收到 UnbindResp，连接将关闭"),
            UnifiedMessage::PingResp => tracing::debug!("收到心跳响应"),
            other => tracing::warn!("收到未处理统一消息: {:?}", other),
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let messages_path = std::path::Path::new(manifest_dir).join("messages.conf");
    let messages = load_messages(&messages_path.to_string_lossy());
    tracing::info!("加载了 {} 条待发送消息", messages.len());

    let authenticated = Arc::new(AtomicBool::new(false));
    let msg_source = Arc::new(ClientMessageSource::from_messages(&messages, authenticated.clone()));
    let handler = Arc::new(SmppClientHandler::new(authenticated.clone()));

    let (host, port) = SERVER_ADDR
        .rsplit_once(':')
        .map(|(h, p)| (h.to_string(), p.parse().unwrap_or(18890)))
        .unwrap_or_else(|| ("127.0.0.1".to_string(), 18890));

    let endpoint = Arc::new(
        EndpointConfig::new(ACCOUNT, host, port, 100, 60)
            .with_protocol(Protocol::Smpp)
            .with_window_size(2048)
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!("正在连接 SMPP 服务端 {}...", SERVER_ADDR);
    let conn = ClientBuilder::new(endpoint, handler, SmppDecoder)
        .message_source(msg_source as Arc<dyn MessageSource>)
        .connect()
        .await?;
    tracing::info!("TCP 连接已建立 (conn_id={})", conn.id);

    // 认证：构造统一 Bind（SMPP DUPLEX 端点须 Transceiver），经 adapter 编码。
    // SMPP 鉴权明文：authenticator 直接装口令字节（非 MD5）。
    let bind = UnifiedMessage::Bind(UnifiedBind {
        client_id: ACCOUNT.to_string(),
        authenticator: PASSWORD.as_bytes().to_vec(),
        timestamp: 0,
        version: 0x34,
        system_type: Some("CMT".to_string()),
        mode: BindMode::Transceiver,
        login_mode: None,
    });
    let bind_bytes = SmppAdapter.encode(&bind, Sequence::Plain(1))?;
    conn.send_request(bind_bytes).await?;
    tracing::info!("已发送 BindTransceiver 认证请求（双工，统一模型）");

    tracing::info!("认证成功后 MessageSource 将自动发送短信，等待回执/MO...");

    tracing::info!("已连接并开始收发，按 Ctrl+C 退出...");
    tokio::signal::ctrl_c().await?;
    tracing::info!("收到退出信号，发送 SMPP Unbind 优雅关闭...");
    let unbind_bytes = SmppAdapter.encode(&UnifiedMessage::Unbind, Sequence::Plain(2))?;
    conn.write_frame(&unbind_bytes).await?;
    Ok(())
}
