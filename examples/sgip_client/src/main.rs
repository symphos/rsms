// ============================================================================
// SGIP 客户端参考实现（统一模型 / 窄腰架构版）
//
// 与旧版的根本区别：业务代码不再直接接触 SGIP 裸 codec 类型（Submit/Deliver/Report…），
// 也不再手剥头部字节（旧版的 extract_sgip_sequence、data[20]/data[8..20] 全部删除）：
//   - 收包：SgipAdapter.decode(frame) -> UnifiedMessage，业务按统一枚举分支处理
//   - 发包：构造 UnifiedMessage -> SgipAdapter.encode(msg, Sequence) -> 字节
//   - 回执：一律用 SgipAdapter.sequence_of(frame) 取回显序列（自动解 12B 复合序列）
// 切换协议只需换 adapter（CmppAdapter/SmgpAdapter/SmppAdapter）与 Decoder，业务逻辑零改。
//
// 功能：连接 SGIP 服务端 + 明文认证 + 发送短信（含长短信拆分） + 收 SubmitResp/Report/MO
// 连接：默认连本机 sgip_server 示例（127.0.0.1:7891）
//
// SGIP 协议特有要点（最关键）：
//   1. 明文认证：UnifiedBind.authenticator 直接装口令字节（无 MD5），登录用 write_frame
//      （非 send_request——send_request 的 sequence_id 偏移按 CMPP 8..11 取，SGIP 复合序列在 8..19）
//   2. 20 字节 Header，序列号是 12 字节复合 SgipSequence(node_id,timestamp,number)
//   3. 发起方序列用 Sequence::Sgip{node_id:NODE_ID, timestamp:sgip_timestamp(), number:自增}
//   4. 回复方一律用 SgipAdapter.sequence_of(frame) 回显请求序列（不手剥字节）
//   5. 独立 Report 命令（不通过 Deliver 承载）——收到必须回 ReportResp（见下方注释）
// ============================================================================

use async_trait::async_trait;
use rsms_codec_sgip::adapter::SgipAdapter;
use rsms_connector::client::{ClientContext, ClientHandler};
use rsms_connector::{ClientBuilder, MessageItem, MessageSource, SgipDecoder};
use rsms_core::{EncodedPdu, EndpointConfig, Frame, Protocol, RawPdu, Result};
use rsms_longmsg::split::SmsAlphabet;
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser};
use rsms_model::{
    Address, BindMode, Concat, Encoding, ProtocolAdapter, ProtocolExtra, Sequence, SgipExtra,
    UnifiedBind, UnifiedMessage, UnifiedSubmit,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

// 连接配置：默认连本机的 sgip_server 示例（监听 7891，账号见其 accounts.conf）。
// SGIP 登录只校验 login_name/login_password，login_type=1（SP→SMG）。
const LOGIN_NAME: &str = "106900";
const LOGIN_PASSWORD: &str = "password123";
const SP_NUMBER: &str = "106900"; // Submit 的 spnumber 字段，与登录账号无关
const SERVER_ADDR: &str = "127.0.0.1:7891";
const NODE_ID: u32 = 1;

fn detect_alphabet(content: &[u8]) -> SmsAlphabet {
    if content.iter().all(|&b| b < 128) {
        SmsAlphabet::ASCII
    } else {
        SmsAlphabet::UCS2
    }
}

/// 把文本按目标编码转为 wire 字节：UCS2 须为 UTF-16BE（每字符 2 字节大端），
/// 否则按 ASCII/原字节直接取。
/// **关键**：LongMessageSplitter 只按字节分段不转码，
/// 若直接传 content.as_bytes()（UTF-8）却标 msg_fmt=4(UCS2)，
/// 对端按 UTF-16BE 解 UTF-8 → 全乱码。
fn to_wire_bytes(content: &str, alphabet: SmsAlphabet) -> Vec<u8> {
    match alphabet {
        SmsAlphabet::UCS2 => content.encode_utf16().flat_map(|u| u.to_be_bytes()).collect(),
        _ => content.as_bytes().to_vec(),
    }
}

/// 按编码解码 wire 字节为可显示字符串：UCS2 按 UTF-16BE 解，否则按 UTF-8 宽松解。
fn decode_text(content: &[u8], encoding: Encoding) -> String {
    match encoding {
        Encoding::Ucs2 => {
            // UTF-16BE：每两字节一个 u16，不足则截断
            let words: Vec<u16> = content
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&words)
        }
        _ => String::from_utf8_lossy(content).into_owned(),
    }
}

/// 把统一 SmsAlphabet 翻译为统一模型 Encoding。
/// SGIP adapter 内部把 Ascii→msg_fmt 0、Ucs2→msg_fmt 4。
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
            line.trim()
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

/// 构造一条 SGIP 提交的统一消息。
/// SGIP 方言（charge_number/corp_id/service_type/fee_*/tppid/tpudhi/message_type 等）
/// 落在 ProtocolExtra::Sgip(SgipExtra)，由 SgipAdapter.encode 还原回裸 Submit 字段。
/// tpudhi=1 标记长短信分段（含 UDH），单条短信 tpudhi=0。
fn build_submit(phone: &str, content: &[u8], encoding: Encoding, concat: Option<Concat>) -> UnifiedMessage {
    UnifiedMessage::Submit(UnifiedSubmit {
        src: Address::plain(SP_NUMBER),
        dests: vec![Address::plain(phone)],
        content: content.to_vec(),
        encoding,
        want_report: true, // report_flag=1：请求状态报告
        // 传 concat（Some=长短信分段），adapter 据此重建 UDH 并置 tp_udhi=1。
        concat,
        extra: ProtocolExtra::Sgip(SgipExtra::default()),
        tlvs: vec![],
    })
}

// ============================================================================
// MessageSource：把待发短信编码为 SGIP 字节（经 SgipAdapter.encode）
//
// 框架的 run_outbound_fetcher 会循环调用 fetch(endpoint.id, 16)，
// 取出的消息通过 write_frame 直接发出（不走 window 机制）。
//
// 关键：fetch 的 account 参数就是 EndpointConfig.id，必须和 connect() 时的 endpoint id 一致。
// 序列号：SGIP 发起方用复合序列 Sequence::Sgip{node_id, timestamp, number}，number 自增。
// ============================================================================

struct ClientMessageSource {
    queue: Arc<Mutex<VecDeque<MessageItem>>>,
    authenticated: Arc<AtomicBool>,
}

impl ClientMessageSource {
    fn from_messages(messages: &[(String, String)], authenticated: Arc<AtomicBool>) -> Self {
        let mut queue = VecDeque::new();
        let mut splitter = LongMessageSplitter::new();
        let mut number = 1000u32; // 复合序列的 number 分量，自增
        let timestamp = sgip_timestamp();
        // 复合序列工厂：固定 node_id/timestamp，number 自增。
        let mut next_seq = || {
            let n = number;
            number += 1;
            Sequence::Sgip {
                node_id: NODE_ID,
                timestamp,
                number: n,
            }
        };

        for (phone, content) in messages {
            let content_bytes = content.as_bytes();
            let alphabet = detect_alphabet(content_bytes);
            let encoding = encoding_of(alphabet);
            // UCS2 每字符 2 字节，单段上限 70 字符 = 140 字节；ASCII 上限 160 字节
            let single_max = match alphabet {
                SmsAlphabet::GSM7 | SmsAlphabet::ASCII => 160,
                _ => 70,
            };
            // 转为 wire 字节：UCS2→UTF-16BE，ASCII→as_bytes
            let wire = to_wire_bytes(content, alphabet);

            if wire.len() > single_max {
                // 长短信：每段传 concat+纯载荷，adapter 重建 UDH 并置 tp_udhi=1，同组顺序发出
                let frames = splitter.split(&wire, alphabet);
                let total = frames.len();
                let items: Vec<Arc<dyn EncodedPdu>> = frames
                    .into_iter()
                    .map(|frame| {
                        let concat = if frame.has_udhi {
                            Some(Concat {
                                reference: frame.reference_id,
                                total: frame.total_segments,
                                sequence: frame.segment_number,
                            })
                        } else {
                            None
                        };
                        let payload = UdhParser::strip_udh(&frame.content);
                        let msg = build_submit(phone, &payload, encoding, concat);
                        let bytes = SgipAdapter
                            .encode(&msg, next_seq())
                            .expect("encode submit segment");
                        Arc::new(RawPdu::from(bytes)) as Arc<dyn EncodedPdu>
                    })
                    .collect();

                tracing::info!(
                    "长短信拆分: {} wire 字节 → {} 段 (phone={})",
                    wire.len(),
                    total,
                    phone
                );
                queue.push_back(MessageItem::Group { items });
            } else {
                // 单段短信：直接用 wire 字节，无 concat
                let msg = build_submit(phone, &wire, encoding, None);
                let bytes = SgipAdapter
                    .encode(&msg, next_seq())
                    .expect("encode submit");
                queue.push_back(MessageItem::Single(
                    Arc::new(RawPdu::from(bytes)) as Arc<dyn EncodedPdu>
                ));
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
//
// - BindResp：认证结果
// - SubmitResp：提交结果（看 status）
// - Report：独立状态报告（SGIP 特有，不通过 Deliver 承载）→ 必须回 ReportResp
// - Deliver：MO 上行短信（不承载报告，含长短信合包）→ 回 DeliverResp
// - UnbindResp：断连响应
// ============================================================================

struct SgipClientHandler {
    authenticated: Arc<AtomicBool>,
    submit_count: AtomicU32,
    report_count: AtomicU32,
    mo_merger: Mutex<LongMessageMerger>,
}

impl SgipClientHandler {
    fn new(authenticated: Arc<AtomicBool>) -> Self {
        Self {
            authenticated,
            submit_count: AtomicU32::new(0),
            report_count: AtomicU32::new(0),
            mo_merger: Mutex::new(LongMessageMerger::new()),
        }
    }

    /// 处理上行短信内容：有 concat 则合包，否则直接呈现。
    /// adapter 已把 UDH 剥成 concat、content 为纯载荷；据 concat 重建含 UDH 段交 merger。
    /// encoding 用于按正确编码解码显示（UCS2→UTF-16BE，其余→UTF-8 宽松）。
    fn handle_mo(&self, src: &str, concat: Option<&Concat>, content: Vec<u8>, encoding: Encoding) {
        if let Some(c) = concat {
            let mut seg = c.to_udh_prefix();
            seg.extend_from_slice(&content);
            let udh = UdhParser::extract_udh(&seg).map(|(h, _)| h);
            let frame = LongMessageFrame::new(c.reference, c.total, c.sequence, seg, true, udh);
            let mut merger = self.mo_merger.lock().unwrap();
            match merger.add_frame(src, frame) {
                Ok(Some(merged)) => tracing::info!(
                    "长短信 MO 合包完成: src={}, 内容={}",
                    src,
                    decode_text(&merged, encoding)
                ),
                Ok(None) => tracing::info!(
                    "长短信 MO 分段 {}/{} 等待更多分段",
                    c.sequence,
                    c.total
                ),
                Err(e) => tracing::warn!("长短信 MO 合包失败: {}", e),
            }
        } else {
            tracing::info!(
                "收到 Deliver（MO 上行短信）: src={}, content={}",
                src,
                decode_text(&content, encoding)
            );
        }
    }
}

#[async_trait]
impl ClientHandler for SgipClientHandler {
    fn name(&self) -> &'static str {
        "sgip-client"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let unified = match SgipAdapter.decode(frame) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("解码失败 cmd_id=0x{:08x}: {}", frame.command_id, e);
                return Ok(());
            }
        };

        match unified {
            UnifiedMessage::BindResp(resp) => {
                if resp.status == 0 {
                    tracing::info!("SGIP 认证成功");
                    self.authenticated.store(true, Ordering::Relaxed);
                } else {
                    tracing::error!("SGIP 认证失败: status={}", resp.status);
                }
            }
            UnifiedMessage::SubmitResp(resp) => {
                let count = self.submit_count.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::info!("[{}] SubmitResp: status={}", count, resp.status);
            }
            UnifiedMessage::Report(report) => {
                let count = self.report_count.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::info!(
                    "[{}] 收到 Report（状态报告）: dest={}, status={:?}",
                    count,
                    report.dest.number,
                    report.status
                );
                reply_report_resp(ctx, frame).await?;
            }
            UnifiedMessage::Deliver(deliver) => {
                reply_deliver_resp(ctx, frame).await?;
                // 传入 deliver.encoding，handle_mo 按编码正确解码显示内容
                self.handle_mo(&deliver.dest.number, deliver.concat.as_ref(), deliver.content, deliver.encoding);
            }
            UnifiedMessage::UnbindResp => tracing::debug!("收到 UnbindResp"),
            other => tracing::warn!("收到未处理统一消息: {:?}", other),
        }
        Ok(())
    }
}

/// 回 ReportResp（SGIP 独立 Report 命令的应答）。
/// sequence_of(frame) 自动解 12B 复合序列，回显请求序列（不手剥 data[8..20]）。
async fn reply_report_resp(ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
    let bytes = SgipAdapter.encode(&UnifiedMessage::ReportResp, SgipAdapter.sequence_of(frame))?;
    ctx.conn.write_frame(&bytes).await
}

/// 回 DeliverResp（MO-Deliver 的应答，经 adapter 编码，sequence_of 自动回显请求序列）。
async fn reply_deliver_resp(ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
    let bytes = SgipAdapter.encode(&UnifiedMessage::DeliverResp, SgipAdapter.sequence_of(frame))?;
    ctx.conn.write_frame(&bytes).await
}

/// 生成 SGIP 时间戳（MMDDHHMMSS 压缩格式，复合序列的 timestamp 分量）。
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
//   2. write_frame() 发送 Bind 认证（不能用 send_request，SGIP 复合序列偏移不同）
//   3. MessageSource.fetch() 被 outbound fetcher 循环调用，认证后自动发出 Submit
//   4. ClientHandler.on_inbound() 处理所有服务端响应
//   5. 定时等待 Report/MO，然后显式 Unbind 干净关闭
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

    let (host, port) = SERVER_ADDR
        .rsplit_once(':')
        .map(|(h, p)| (h.to_string(), p.parse().unwrap_or(16890)))
        .unwrap_or_else(|| ("127.0.0.1".to_string(), 16890));

    // 注意：endpoint id 用 SP_NUMBER（与 MessageSource.fetch 的 account 键一致）。
    let endpoint = Arc::new(
        EndpointConfig::new(SP_NUMBER, host, port, 100, 60)
            .with_protocol(Protocol::Sgip)
            .with_window_size(2048)
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!("正在连接 SGIP 服务端 {}...", SERVER_ADDR);

    let conn = ClientBuilder::new(endpoint, handler, SgipDecoder)
        .message_source(msg_source as Arc<dyn MessageSource>)
        .connect()
        .await?;

    tracing::info!("TCP 连接已建立 (conn_id={})", conn.id);

    // 认证：构造统一 Bind，经 adapter 编码。
    // SGIP 鉴权明文：authenticator 直接装口令字节（非 MD5）；version 字段承载 login_type=1。
    // 登录用 write_frame（非 send_request）：SGIP 复合序列在 data[8..19]，与 send_request 的 CMPP 偏移不符。
    let bind = UnifiedMessage::Bind(UnifiedBind {
        client_id: LOGIN_NAME.to_string(),
        authenticator: LOGIN_PASSWORD.as_bytes().to_vec(),
        version: 1, // SGIP login_type=1
        timestamp: 0,
        system_type: None,
        mode: BindMode::default(),
        login_mode: None,
    });
    let bind_bytes = SgipAdapter.encode(
        &bind,
        Sequence::Sgip {
            node_id: NODE_ID,
            timestamp: sgip_timestamp(),
            number: 1,
        },
    )?;
    conn.write_frame(&bind_bytes).await?;
    tracing::info!("已发送 Bind 认证请求（明文，login_type=1，统一模型）");

    tracing::info!("认证成功后 MessageSource 将自动发送短信，等待 Report/MO...");

    tracing::info!("已连接并开始收发，按 Ctrl+C 退出...");
    tokio::signal::ctrl_c().await?;
    tracing::info!("收到退出信号，发送 SGIP Unbind 优雅关闭...");
    let unbind_bytes = SgipAdapter.encode(
        &UnifiedMessage::Unbind,
        Sequence::Sgip {
            node_id: NODE_ID,
            timestamp: sgip_timestamp(),
            number: 2,
        },
    )?;
    conn.write_frame(&unbind_bytes).await?;

    Ok(())
}
