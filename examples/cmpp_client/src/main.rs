// ============================================================================
// CMPP 客户端参考实现（统一模型 / 窄腰架构版）
//
// 与旧版的根本区别：业务代码不再直接接触 CMPP 裸 codec 类型（Submit/Deliver/Connect…），
// 全程只用协议无关的 `rsms_model::UnifiedMessage` + `CmppAdapter`（实现 ProtocolAdapter）：
//   - 收包：CmppAdapter.decode(frame) -> UnifiedMessage，业务按统一枚举分支处理
//   - 发包：构造 UnifiedMessage -> CmppAdapter.encode(msg, Sequence) -> 字节
// 切换协议只需换 adapter（SmppAdapter/SmgpAdapter/SgipAdapter）与 Decoder，业务逻辑零改。
//
// CMPP 方言字段（msg_src/service_id/fee_*/tppid/tpudhi/pk_total/pk_number 等）落在
// ProtocolExtra::Cmpp(CmppExtra{..})；长短信分段通过 CmppExtra.tpudhi=1 + pk_total/pk_number 标志。
//
// 唯一保留的裸 codec 调用是鉴权助手 compute_connect_auth（加密工具，非裸消息类型，指南允许）。
//
// 功能：连接 CMPP 服务端 + MD5 认证(Connect) + 发送短信 + 收回执/MO + 长短信拆分/合包
// 连接：默认连本机 cmpp_server 示例（127.0.0.1:7890），CMPP 3.0
// ============================================================================

use async_trait::async_trait;
use rsms_codec_cmpp::adapter::CmppAdapter;
use rsms_codec_cmpp::compute_connect_auth;
use rsms_connector::client::{ClientContext, ClientHandler};
use rsms_connector::{ClientBuilder, CmppDecoder, MessageItem, MessageSource};
use rsms_core::{EncodedPdu, EndpointConfig, Frame, Protocol, RawPdu, Result};
use rsms_longmsg::split::SmsAlphabet;
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser};
use rsms_model::{
    Address, CmppExtra, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, Sequence, UnifiedBind,
    UnifiedMessage, UnifiedSubmit,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

// 连接配置：默认连本机的 cmpp_server 示例（监听 7890，账号见其 accounts.conf）
const ACCOUNT: &str = "900001";
const PASSWORD: &str = "password123";
const SERVER_ADDR: &str = "127.0.0.1:7890";

// 长短信发送用例（ASCII，>160 触发拆分）
const LONG_SUBMIT_TEXT: &str = "This is a long ASCII test message sent from the rsms CMPP client to validate long message splitting over the wire. The rsms LongMessageSplitter must cut it into several UDH segments and the cmos CMPP long message handler on the simulator side must reassemble all of the segments back into this exact original text without any loss at all.";

// 长短信接收用例（echo 前缀剥离后由模拟器拆段，rsms LongMessageMerger 合包）
const LONG_MO_TEXT: &str = "This long uplink mo is echoed back by the cmos simulator after stripping the echo prefix, then split into multiple UDH segments which the rsms LongMessageMerger must merge back into this one complete original sentence for the cmpp test.";

fn detect_alphabet(content: &str) -> SmsAlphabet {
    if content.is_ascii() {
        SmsAlphabet::ASCII
    } else {
        SmsAlphabet::UCS2
    }
}

/// 把字母表翻译为统一模型 Encoding。
/// UCS2 → Encoding::Ucs2（msg_fmt=8），ASCII → Encoding::Ascii（msg_fmt=0）。
fn encoding_of(alphabet: SmsAlphabet) -> Encoding {
    match alphabet {
        SmsAlphabet::ASCII | SmsAlphabet::GSM7 => Encoding::Ascii,
        _ => Encoding::Ucs2,
    }
}

/// 把文本按目标编码转为 wire 字节：UCS2 须为 UTF-16BE（每字符 2 字节大端），
/// 否则按 ASCII/原字节。**关键**：LongMessageSplitter 只按字节分段、不转码，
/// 若直接传 content.as_bytes()（UTF-8）却标 msg_fmt=8(UCS2)，对端按 UTF-16BE 解 UTF-8 → 全乱码
/// （联调实测：CMPP 对端对 UCS2 严格 UTF-16BE 解码）。
fn to_wire_bytes(content: &str, alphabet: SmsAlphabet) -> Vec<u8> {
    match alphabet {
        SmsAlphabet::UCS2 => content.encode_utf16().flat_map(|u| u.to_be_bytes()).collect(),
        _ => content.as_bytes().to_vec(),
    }
}

/// 把 wire 字节按编码解码为显示字符串：UCS2 按 UTF-16BE 解，否则按 UTF-8 宽松解。
fn decode_text(bytes: &[u8], enc: Encoding) -> String {
    match enc {
        Encoding::Ucs2 => {
            // 两两组成 u16 大端再 from_utf16_lossy
            let u16s: Vec<u16> = bytes
                .chunks(2)
                .map(|c| {
                    if c.len() == 2 {
                        u16::from_be_bytes([c[0], c[1]])
                    } else {
                        0xFFFD // 奇数尾字节用替换字符
                    }
                })
                .collect();
            String::from_utf16_lossy(&u16s)
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

#[allow(dead_code)]
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
        return vec![(
            "13800138000".to_string(),
            "Hello from CMPP Client".to_string(),
        )];
    }
    messages
}

/// 构造一条 CMPP 提交的统一消息。
/// CMPP 方言（fee_terminal_id / 长短信 pk_total/pk_number/tpudhi）落在 ProtocolExtra::Cmpp。
/// 注意：registered_delivery 由统一 want_report 驱动；msg_fmt 由统一 encoding 驱动（见 adapter）。
fn build_submit(
    phone: &str,
    content: &[u8],
    encoding: Encoding,
    pk_total: u8,
    pk_number: u8,
    tpudhi: u8,
) -> UnifiedMessage {
    UnifiedMessage::Submit(UnifiedSubmit {
        src: Address::plain(ACCOUNT),
        dests: vec![Address::plain(phone)],
        content: content.to_vec(),
        encoding,
        want_report: true,
        concat: None,
        extra: ProtocolExtra::Cmpp(CmppExtra {
            fee_terminal_id: phone.to_string(),
            pk_total,
            pk_number,
            tpudhi,
            ..Default::default()
        }),
        tlvs: vec![],
    })
}

// ============================================================================
// MessageSource：把待发短信编码为 CMPP 字节（经 CmppAdapter.encode）
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
            let alphabet = detect_alphabet(content);
            let encoding = encoding_of(alphabet);
            // 先按目标编码转为 wire 字节（UCS2 → UTF-16BE），再按字节数决定是否拆分
            let wire = to_wire_bytes(content, alphabet);
            let single_max = match alphabet {
                SmsAlphabet::GSM7 | SmsAlphabet::ASCII => 160,
                _ => 70,
            };

            if wire.len() > single_max {
                let frames = splitter.split(&wire, alphabet);
                let total = frames.len();
                let mut items = Vec::new();

                for (i, frame) in frames.into_iter().enumerate() {
                    // 长短信：每段置 tpudhi=1（若分段含 UDH）+ pk_total/pk_number，同组顺序发出。
                    let tpudhi = if frame.has_udhi { 1 } else { 0 };
                    let msg = build_submit(
                        phone,
                        &frame.content,
                        encoding,
                        total as u8,
                        (i + 1) as u8,
                        tpudhi,
                    );
                    let bytes = CmppAdapter
                        .encode(&msg, Sequence::Plain(seq))
                        .expect("encode submit segment");
                    items.push(Arc::new(RawPdu::from(bytes)) as Arc<dyn EncodedPdu>);
                    seq += 1;
                }

                tracing::info!(
                    "长短信拆分: {} 字节 → {} 段 (phone={})",
                    wire.len(),
                    total,
                    phone
                );
                queue.push_back(MessageItem::Group { items });
            } else {
                let msg = build_submit(phone, &wire, encoding, 0, 0, 0);
                let bytes = CmppAdapter
                    .encode(&msg, Sequence::Plain(seq))
                    .expect("encode submit");
                queue.push_back(MessageItem::Single(
                    Arc::new(RawPdu::from(bytes)) as Arc<dyn EncodedPdu>
                ));
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
// ClientHandler：统一模型分支处理服务端下发的所有帧
// ============================================================================

struct CmppClientHandler {
    authenticated: Arc<AtomicBool>,
    submit_count: AtomicU32,
    report_count: AtomicU32,
    mo_merger: Mutex<LongMessageMerger>,
}

impl CmppClientHandler {
    fn new(authenticated: Arc<AtomicBool>) -> Self {
        Self {
            authenticated,
            submit_count: AtomicU32::new(0),
            report_count: AtomicU32::new(0),
            mo_merger: Mutex::new(LongMessageMerger::new()),
        }
    }

    /// 处理上行短信内容：含 UDH 则合包，否则直接呈现。
    /// encoding 用于将 wire 字节正确解码为文本（UCS2 → UTF-16BE，其余 → UTF-8）。
    fn handle_mo(&self, src: &str, content: Vec<u8>, encoding: Encoding) {
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
            match merger.add_frame(frame) {
                Ok(Some(merged)) => tracing::info!(
                    "长短信 MO 合包完成: src={}, 内容={}",
                    src,
                    decode_text(&merged, encoding)
                ),
                Ok(None) => tracing::info!(
                    "长短信 MO 分段 {}/{} 等待更多分段",
                    udh.segment_number,
                    udh.total_segments
                ),
                Err(e) => tracing::warn!("长短信 MO 合包错误: {}", e),
            }
        } else {
            tracing::info!(
                "上行短信: src={}, content={}",
                src,
                decode_text(&content, encoding)
            );
        }
    }
}

#[async_trait]
impl ClientHandler for CmppClientHandler {
    fn name(&self) -> &'static str {
        "cmpp-client"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let unified = match CmppAdapter.decode(frame) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("解码失败 cmd_id=0x{:08x}: {}", frame.command_id, e);
                return Ok(());
            }
        };

        match unified {
            UnifiedMessage::BindResp(resp) => {
                if resp.status == 0 {
                    tracing::info!("✓ CMPP 认证成功");
                    self.authenticated.store(true, Ordering::Relaxed);
                } else {
                    tracing::error!("✗ CMPP 认证失败: status={}", resp.status);
                }
            }
            UnifiedMessage::SubmitResp(resp) => {
                let count = self.submit_count.fetch_add(1, Ordering::Relaxed) + 1;
                let id = match resp.msg_id {
                    MessageId::Text(t) => t,
                    MessageId::Binary(b) => b.iter().map(|x| format!("{:02x}", x)).collect(),
                };
                tracing::info!("[{}] SubmitResp: msg_id={}, result={}", count, id, resp.status);
            }
            UnifiedMessage::Report(report) => {
                let count = self.report_count.fetch_add(1, Ordering::Relaxed) + 1;
                let msg_id = match &report.msg_id {
                    MessageId::Text(t) => t.clone(),
                    MessageId::Binary(b) => b.iter().map(|x| format!("{:02x}", x)).collect(),
                };
                tracing::info!(
                    "[{}] 状态报告: msg_id={}, src={}, dest={}, raw={}",
                    count,
                    msg_id,
                    report.src.number,
                    report.dest.number,
                    String::from_utf8_lossy(&report.raw)
                );
                reply_deliver_resp(ctx, frame).await?;
            }
            UnifiedMessage::Deliver(deliver) => {
                self.handle_mo(&deliver.src.number, deliver.content, deliver.encoding);
                reply_deliver_resp(ctx, frame).await?;
            }
            UnifiedMessage::PingResp => tracing::info!("✓ 收到心跳响应 (ActiveTestResp)"),
            UnifiedMessage::UnbindResp => tracing::info!("收到 Terminate 响应，连接将关闭"),
            other => tracing::debug!("收到未处理统一消息: {:?}", other),
        }

        Ok(())
    }
}

/// 回 DeliverResp（经 adapter 编码，sequence_of 自动回显请求序列——跨协议统一写法）。
async fn reply_deliver_resp(ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
    let bytes = CmppAdapter.encode(&UnifiedMessage::DeliverResp, CmppAdapter.sequence_of(frame))?;
    ctx.conn.write_frame(&bytes).await
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let _ = load_messages; // 联调期不读文件，用固定测试集

    // 【联调临时 / 跑完即弃】固定测试集，覆盖回执 + 上行 + 长短信双向
    let messages = vec![
        ("13800138000".to_string(), "rsms cmpp short status report test".to_string()),
        ("13800138001".to_string(), "echo cmpp short uplink mo".to_string()),
        ("13800138002".to_string(), LONG_SUBMIT_TEXT.to_string()),
        ("13800138003".to_string(), format!("echo {LONG_MO_TEXT}")),
    ];
    tracing::info!("加载了 {} 条待发送消息（含长短信双向）", messages.len());

    let authenticated = Arc::new(AtomicBool::new(false));
    let msg_source = Arc::new(ClientMessageSource::from_messages(
        &messages,
        authenticated.clone(),
    ));
    let handler = Arc::new(CmppClientHandler::new(authenticated.clone()));

    let (host, port) = if let Some((h, p)) = SERVER_ADDR.rsplit_once(':') {
        (h.to_string(), p.parse().unwrap_or(17890))
    } else {
        ("127.0.0.1".to_string(), 17890)
    };

    // idle=10 → keepalive 间隔 5s，跑动期内会触发框架自动心跳（验证 ActiveTest）
    let endpoint = Arc::new(
        EndpointConfig::new(ACCOUNT, host, port, 100, 10)
            .with_protocol(Protocol::Cmpp)
            .with_window_size(2048)
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!("正在连接 CMPP 服务端 {}...", SERVER_ADDR);

    let conn = ClientBuilder::new(endpoint, handler, CmppDecoder)
        .message_source(msg_source as Arc<dyn MessageSource>)
        .connect()
        .await?;

    tracing::info!("TCP 连接已建立 (conn_id={})", conn.id);

    // 认证：构造统一 Bind，经 adapter 编码。CMPP 鉴权须 MD5：
    // 用 compute_connect_auth 算出 16B authenticator 塞进 UnifiedBind.authenticator（adapter 不重算）。
    // CMPP 3.0（version=0x30），CMPP 无 system_type/login_mode，mode 取默认（adapter 忽略这三项）。
    let timestamp = 0u32;
    let authenticator = compute_connect_auth(ACCOUNT, PASSWORD, timestamp);
    let bind = UnifiedMessage::Bind(UnifiedBind {
        client_id: ACCOUNT.to_string(),
        authenticator: authenticator.to_vec(),
        timestamp,
        version: 0x30,
        system_type: None,
        mode: rsms_model::BindMode::default(),
        login_mode: None,
    });
    // 登录帧用 send_request（同现状）。
    let bind_bytes = CmppAdapter.encode(&bind, Sequence::Plain(1))?;
    conn.send_request(bind_bytes).await?;
    tracing::info!("已发送 Connect 认证请求（MD5，统一模型）");

    tracing::info!("认证成功后 MessageSource 自动发送，等待回执/上行/长短信/心跳...");

    tracing::info!("已连接并开始收发，按 Ctrl+C 退出...");
    tokio::signal::ctrl_c().await?;
    tracing::info!("收到退出信号，发送 CMPP Terminate 优雅关闭...");
    let term_bytes = CmppAdapter.encode(&UnifiedMessage::Unbind, Sequence::Plain(9999))?;
    conn.write_frame(&term_bytes).await?;

    Ok(())
}
