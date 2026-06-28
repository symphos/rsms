// ============================================================================
// SMGP 客户端参考实现（统一模型 / 窄腰架构版）
//
// 与旧版的根本区别：业务代码不再直接接触 SMGP 裸 codec 类型（Submit/Deliver/Login…），
// 全程只用协议无关的 `rsms_model::UnifiedMessage` + `SmgpAdapter`（实现 ProtocolAdapter）：
//   - 收包：SmgpAdapter.decode(frame) -> UnifiedMessage，业务按统一枚举分支处理
//   - 发包：构造 UnifiedMessage -> SmgpAdapter.encode(msg, Sequence) -> 字节
// 切换协议只需换 adapter（CmppAdapter/SmppAdapter/SgipAdapter）与 Decoder，业务逻辑零改。
//
// 功能：连接 SMGP 服务端 + MD5 认证(Login) + 发送短信 + 收回执/MO + 长短信拆分/合包
// 连接：默认连本机 smgp_server 示例（127.0.0.1:8890），login_mode=2
//
// SMGP 方言说明：
//   - 鉴权须 MD5：用 codec 的 compute_login_auth(account, password, timestamp) 算 16B 摘要，
//     塞进 UnifiedBind.authenticator（adapter 不算 MD5，按已算好的摘要直接填）。
//   - login_mode=Some(2) 表示 DUPLEX（既 submit 又收 MO/报告）。
//   - 计费/service_id/fee/msg_type/priority 等方言字段落 ProtocolExtra::Smgp(SmgpExtra)。
//     本 example 全取默认（与旧版 Submit::new() 默认值一致），仅设 want_report。
//   - 长短信：走窄腰 concat——发端传 Concat + 纯载荷，SmgpAdapter 自动前置 UDH 并置
//     SMGP TP_UDHI 可选参数 TLV(0x0002,[1])；收端 adapter 剥 UDH 还原为 concat + 纯载荷，
//     业务侧据 concat 重建含 UDH 段交 UdhParser 合包。
// ============================================================================

use async_trait::async_trait;
use rsms_business::{MessageContext, MessageHandler};
use rsms_codec_smgp::adapter::SmgpAdapter;
use rsms_codec_smgp::compute_login_auth;
use rsms_connector::{ClientBuilder, MessageItem, MessageSource, NoopClientHandler, SmgpDecoder};
use rsms_core::{EncodedPdu, EndpointConfig, Protocol, RawPdu, Result};
use rsms_longmsg::{
    split::SmsAlphabet, LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser,
};
use rsms_model::{
    Address, BindMode, Concat, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, Sequence,
    SmgpExtra, UnifiedBind, UnifiedMessage, UnifiedSubmit,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

// 连接配置：默认连本机的 smgp_server 示例（监听 8890，账号见其 accounts.conf）。
// SMGP 登录 login_mode=2（DUPLEX，下方 main 里设置）。
const ACCOUNT: &str = "900001";
const PASSWORD: &str = "password123";
const SERVER_ADDR: &str = "127.0.0.1:8890";

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

fn load_messages(path: &str) -> Vec<(String, String)> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("无法读取 {}: {}, 使用默认消息", path, e);
            return vec![("13800138000".to_string(), "Hello from SMGP Client".to_string())];
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
        return vec![("13800138000".to_string(), "Hello from SMGP Client".to_string())];
    }
    messages
}

/// 把文本按目标编码转为 wire 字节：UCS2 须为 UTF-16BE（每字符 2 字节大端），
/// 否则按 ASCII/原字节。**关键**：LongMessageSplitter 只按字节分段、不转码，
/// 若直接传 content.as_bytes()（UTF-8）却标 msg_fmt=8(UCS2)，对端会按 UTF-16BE 解 UTF-8 → 全乱码
/// （联调实测：cmos 对 SMGP UCS2 严格 UTF-16BE 解，中文/echo 前缀全乱、echo 不触发）。
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

/// 构造一条 SMGP 提交的统一消息。
/// SMGP 计费/service_id/msg_type 等方言字段落 ProtocolExtra::Smgp，本 example 全取默认。
/// **长短信走 concat（窄腰）**：传 concat + 纯载荷，adapter 自动前置 UDH 并置 SMGP TP_UDHI 可选参数
/// TLV(0x0002,[1])——缺它对端不知道正文含 UDH、不会重组（联调实测 cmos 会把 UDH 字节当正文、echo 失败）。
fn build_submit(
    phone: &str,
    content: &[u8],
    encoding: Encoding,
    concat: Option<Concat>,
) -> UnifiedMessage {
    UnifiedMessage::Submit(UnifiedSubmit {
        src: Address::plain(ACCOUNT),
        dests: vec![Address::plain(phone)],
        content: content.to_vec(),
        encoding,
        want_report: true,
        concat,
        extra: ProtocolExtra::Smgp(SmgpExtra::default()),
        tlvs: vec![],
    })
}

/// 消费侧：把窄腰 (concat, 纯载荷) 重建为含 UDH 的字节，供 UdhParser 合包逻辑复用。
fn seg_with_udh(concat: &Option<Concat>, content: &[u8]) -> Vec<u8> {
    match concat {
        Some(c) => {
            let mut v = c.to_udh_prefix();
            v.extend_from_slice(content);
            v
        }
        None => content.to_vec(),
    }
}

/// 把 splitter 的分段帧转为窄腰 (concat, 纯载荷)：has_udhi 段剥掉 UDH、concat 承载分段信息。
fn frame_to_concat(f: &LongMessageFrame) -> (Option<Concat>, Vec<u8>) {
    if f.has_udhi {
        (
            Some(Concat {
                reference: f.reference_id,
                total: f.total_segments,
                sequence: f.segment_number,
            }),
            UdhParser::strip_udh(&f.content),
        )
    } else {
        (None, f.content.clone())
    }
}

// ============================================================================
// MessageSource：把待发短信编码为 SMGP 字节（经 SmgpAdapter.encode）
// ============================================================================

struct ClientMessageSource {
    queue: Arc<Mutex<VecDeque<MessageItem>>>,
    authenticated: Arc<AtomicBool>,
    seq: AtomicU32,
}

impl ClientMessageSource {
    fn from_messages(messages: &[(String, String)], authenticated: Arc<AtomicBool>) -> Self {
        let seq = AtomicU32::new(1000);
        let next_seq = || seq.fetch_add(1, Ordering::Relaxed);
        let mut queue = VecDeque::new();
        let mut splitter = LongMessageSplitter::new();

        for (phone, content) in messages {
            let alphabet = detect_alphabet(content);
            let encoding = encoding_of(alphabet);
            let wire = to_wire_bytes(content, alphabet);
            let frames = splitter.split(&wire, alphabet);

            if frames.len() == 1 && !frames[0].has_udhi {
                // 单条短信：无级联信息（concat=None）。
                let msg = build_submit(phone, &frames[0].content, encoding, None);
                let bytes = SmgpAdapter
                    .encode(&msg, Sequence::Plain(next_seq()))
                    .expect("encode submit");
                queue.push_back(MessageItem::Single(
                    Arc::new(RawPdu::from(bytes)) as Arc<dyn EncodedPdu>
                ));
            } else {
                // 长短信：每段传 concat + 纯载荷，adapter 自动建 UDH + 置 TP_UDHI 可选参数，整组顺序发出。
                let items: Vec<Arc<dyn EncodedPdu>> = frames
                    .iter()
                    .map(|frame| {
                        let (concat, payload) = frame_to_concat(frame);
                        let msg = build_submit(phone, &payload, encoding, concat);
                        let bytes = SmgpAdapter
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
// ClientHandler：统一模型分支处理服务端下发的所有帧
// ============================================================================

struct SmgpClientHandler {
    authenticated: Arc<AtomicBool>,
    submit_count: AtomicU32,
    report_count: AtomicU32,
    mo_merger: Mutex<LongMessageMerger>,
}

impl SmgpClientHandler {
    fn new(authenticated: Arc<AtomicBool>) -> Self {
        Self {
            authenticated,
            submit_count: AtomicU32::new(0),
            report_count: AtomicU32::new(0),
            mo_merger: Mutex::new(LongMessageMerger::new()),
        }
    }

    /// 处理上行短信内容：含 UDH 则合包，否则直接呈现。
    fn handle_mo(&self, src: &str, dest: &str, content: Vec<u8>, encoding: Encoding) {
        if let Some((udh, _)) = UdhParser::extract_udh(&content) {
            let frame = LongMessageFrame::new(
                udh.reference_id,
                udh.total_segments,
                udh.segment_number,
                content,
                true,
                Some(udh.clone()),
            );
            let mut merger = self.mo_merger.lock().unwrap();
            match merger.add_frame(src, frame) {
                Ok(Some(merged)) => tracing::info!(
                    "长短信 MO 合包完成: src={}, dest={}, content={}",
                    src,
                    dest,
                    decode_text(&merged, encoding)
                ),
                Ok(None) => tracing::debug!(
                    "长短信 MO 分段 {}/{} 等待更多分段",
                    udh.segment_number,
                    udh.total_segments
                ),
                Err(e) => tracing::warn!("长短信 MO 合包失败: {}", e),
            }
        } else {
            tracing::info!(
                "上行短信: src={}, dest={}, content={}",
                src,
                dest,
                decode_text(&content, encoding)
            );
        }
    }
}

#[async_trait]
impl MessageHandler for SmgpClientHandler {
    fn name(&self) -> &'static str {
        "smgp-client"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        // 框架已按协议解码为统一消息，业务直接按枚举分支处理。
        match msg {
            UnifiedMessage::BindResp(resp) => {
                if resp.status == 0 {
                    tracing::info!("✓ SMGP 认证成功");
                    self.authenticated.store(true, Ordering::Relaxed);
                } else {
                    tracing::error!("SMGP 认证失败: status={}", resp.status);
                }
            }
            UnifiedMessage::SubmitResp(resp) => {
                let count = self.submit_count.fetch_add(1, Ordering::Relaxed) + 1;
                // msg: &UnifiedMessage，resp 为借用，msg_id 须 borrow 匹配。
                let id = match &resp.msg_id {
                    MessageId::Text(t) => t.clone(),
                    MessageId::Binary(b) => b.iter().map(|x| format!("{x:02x}")).collect(),
                };
                tracing::info!("[{}] SubmitResp: msg_id={}, status={}", count, id, resp.status);
            }
            UnifiedMessage::Report(report) => {
                let count = self.report_count.fetch_add(1, Ordering::Relaxed) + 1;
                let msg_id_hex = match &report.msg_id {
                    MessageId::Binary(b) => b.iter().map(|x| format!("{x:02x}")).collect(),
                    MessageId::Text(t) => t.clone(),
                };
                tracing::info!(
                    "[{}] 状态报告: msg_id={}, status={:?}, src={}, raw={}",
                    count,
                    msg_id_hex,
                    report.status,
                    report.src.number,
                    String::from_utf8_lossy(&report.raw)
                );
                // 一步回执（窄腰）：框架按请求帧序列编码 DeliverResp 并写回。
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            UnifiedMessage::Deliver(deliver) => {
                let enc = deliver.encoding;
                // 窄腰：adapter 已剥 UDH → deliver.concat + 纯载荷，据 concat 重建含 UDH 段供合包逻辑复用。
                let seg = seg_with_udh(&deliver.concat, &deliver.content);
                self.handle_mo(&deliver.src.number, &deliver.dest.number, seg, enc);
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            UnifiedMessage::UnbindResp => tracing::info!("收到 ExitResp，连接将关闭"),
            UnifiedMessage::PingResp => tracing::debug!("收到心跳响应"),
            other => tracing::debug!("收到未处理统一消息: {:?}", other),
        }
        Ok(())
    }
}

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
    let msg_source = Arc::new(ClientMessageSource::from_messages(&messages, authenticated.clone()));
    let handler = Arc::new(SmgpClientHandler::new(authenticated.clone()));

    let (host, port) = SERVER_ADDR
        .rsplit_once(':')
        .map(|(h, p)| (h.to_string(), p.parse().unwrap_or(19890)))
        .unwrap_or_else(|| ("127.0.0.1".to_string(), 19890));

    let endpoint = Arc::new(
        EndpointConfig::new(ACCOUNT, host, port, 100, 60)
            .with_protocol(Protocol::Smgp)
            .with_window_size(2048)
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!("正在连接 SMGP 服务端 {}...", SERVER_ADDR);
    // 窄腰主路径：业务处理器经 with_message_handler 注入；new 第二参用 NoopClientHandler 占位。
    let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), SmgpDecoder)
        .with_message_handler(handler)
        .message_source(msg_source as Arc<dyn MessageSource>)
        .connect()
        .await?;
    tracing::info!("TCP 连接已建立 (conn_id={})", conn.id);

    // 认证：构造统一 Bind，经 adapter 编码。
    // SMGP 鉴权须 MD5：用 compute_login_auth 算 16B 摘要装入 authenticator，并设 timestamp。
    // login_mode=Some(2) 表示 DUPLEX（既发 submit 又收 MO/报告）。
    let timestamp = smgp_timestamp();
    let authenticator = compute_login_auth(ACCOUNT, PASSWORD, timestamp).to_vec();
    let bind = UnifiedMessage::Bind(UnifiedBind {
        client_id: ACCOUNT.to_string(),
        authenticator,
        timestamp,
        version: 0x30,
        system_type: None,
        mode: BindMode::default(),
        login_mode: Some(2),
    });
    let bind_bytes = SmgpAdapter.encode(&bind, Sequence::Plain(1))?;
    conn.send_request(bind_bytes).await?;
    tracing::info!("已发送 Login 认证请求（DUPLEX，统一模型）");

    tracing::info!("认证成功后 MessageSource 将自动发送短信，等待回执/MO...");

    tracing::info!("已连接并开始收发，按 Ctrl+C 退出...");
    tokio::signal::ctrl_c().await?;
    tracing::info!("收到退出信号，发送 SMGP Exit 优雅关闭...");
    let unbind_bytes = SmgpAdapter.encode(&UnifiedMessage::Unbind, Sequence::Plain(2))?;
    conn.write_frame(&unbind_bytes).await?;
    Ok(())
}

/// 生成 SMGP 时间戳（MMDDHHMMSS 十进制，用于 MD5 鉴权与协议头）。
fn smgp_timestamp() -> u32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let month = ((secs / 86400 / 30) % 12 + 1) as u32;
    let day = ((secs / 86400) % 30 + 1) as u32;
    let h = (((secs / 3600) % 24 + 8) % 24) as u32;
    let m = ((secs / 60) % 60) as u32;
    let s = (secs % 60) as u32;
    month * 100000000 + day * 1000000 + h * 10000 + m * 100 + s
}
