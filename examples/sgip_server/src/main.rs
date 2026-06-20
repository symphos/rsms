// ============================================================================
// SGIP 服务端完整参考实现（统一模型 / 窄腰架构版）
//
// 与旧版的根本区别：业务代码不再直接接触 SGIP 裸 codec 类型（Submit/Deliver/Report…），
// 也不再手剥字节（data[8..20] 解 SgipSequence）。全程只用协议无关的
// `rsms_model::UnifiedMessage` + `SgipAdapter`（实现 ProtocolAdapter）：
//   - 收包：SgipAdapter.decode(frame) -> UnifiedMessage，业务按统一枚举分支处理
//   - 回执/回包：构造 UnifiedMessage -> SgipAdapter.encode(msg, Sequence) -> 字节 -> write_frame
//   - 序列：回复一律 SgipAdapter.sequence_of(frame)（自动解 12B 复合序列，回显请求序列）；
//           server 主动发起的帧（Report/Deliver MO）用 Sequence::Sgip{node_id, timestamp, number}
//
// 功能：明文认证 + 限流 + MessageSource 队列 + 长短信合包 + 独立 Report 命令
// 运行：cargo run
// 配置：accounts.conf（账号密码）、messages.conf（模拟 MO 消息）
//
// SGIP 与 CMPP 的关键差异（统一模型已吸收，业务方无需关心）：
//   1. 明文认证（无 MD5）——AuthHandler 保持原样，只校验账密
//   2. 20 字节 Header（12 字节复合 SgipSequence: node_id + timestamp + number）
//   3. 独立 Report 命令（不通过 Deliver 承载）——统一模型映射为 UnifiedMessage::Report
//   4. SubmitResp 只有 result 字段，没有 msg_id——统一模型 msg_id 置空 Text("")
// ============================================================================

use async_trait::async_trait;
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_codec_sgip::adapter::SgipAdapter;
use rsms_connector::{
    AccountConfig, AccountConfigProvider, AccountPoolConfig, AuthCredentials, AuthHandler,
    AuthResult, MessageItem, MessageSource, ProtocolConnection, ServerBuilder, ServerEventHandler,
};
use rsms_core::{ConnectionInfo, EncodedPdu, EndpointConfig, Frame, Protocol, RawPdu, Result};
use rsms_longmsg::split::SmsAlphabet;
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser};
use rsms_model::{
    Address, Concat, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, Sequence,
    SgipExtra, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmitResp,
};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

// SGIP 主动发起帧（Report/Deliver MO）使用的本机 SGIP 节点号（保留原 example 约定）。
const NODE_ID: u32 = 1;

// ============================================================================
// 配置读取
// ============================================================================

fn load_accounts(path: &str) -> HashMap<String, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("读取账号配置失败 {}: {}, 使用空配置", path, e);
            return HashMap::new();
        }
    };

    let mut accounts = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
        if parts.len() == 2 {
            accounts.insert(parts[0].to_string(), parts[1].to_string());
        }
    }
    tracing::info!("加载 {} 个账号配置", accounts.len());
    accounts
}

struct MoMessage {
    account: String,
    phone: String,
    content: String,
}

fn load_mo_messages(path: &str) -> Vec<MoMessage> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("读取消息配置失败 {}: {}", path, e);
            return Vec::new();
        }
    };

    let mut messages = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, char::is_whitespace).collect();
        if parts.len() >= 3 {
            messages.push(MoMessage {
                account: parts[0].to_string(),
                phone: parts[1].to_string(),
                content: parts[2].to_string(),
            });
        }
    }
    tracing::info!("加载 {} 条预定义 MO 消息", messages.len());
    messages
}

fn detect_alphabet(content: &[u8]) -> SmsAlphabet {
    if content.iter().all(|&b| b < 128) {
        SmsAlphabet::ASCII
    } else {
        SmsAlphabet::UCS2
    }
}

/// 检测字符串内容所需编码（含非 ASCII 字符则 UCS2，否则 ASCII）。
fn detect_alphabet_str(content: &str) -> SmsAlphabet {
    if content.is_ascii() {
        SmsAlphabet::ASCII
    } else {
        SmsAlphabet::UCS2
    }
}

/// 把文本按目标编码转为 wire 字节：UCS2 须为 UTF-16BE（每字符 2 字节大端），
/// 否则按原 UTF-8/ASCII 字节。
/// LongMessageSplitter 只按字节分段、不转码，
/// 若直接传 content.as_bytes()（UTF-8）却标 Encoding::Ucs2，
/// 对端按 UTF-16BE 解 UTF-8 会全乱码。
fn to_wire_bytes(content: &str, alphabet: SmsAlphabet) -> Vec<u8> {
    match alphabet {
        SmsAlphabet::UCS2 => content.encode_utf16().flat_map(|u| u.to_be_bytes()).collect(),
        _ => content.as_bytes().to_vec(),
    }
}

/// 按编码把 wire 字节解为可显示字符串：
/// UCS2 按 UTF-16BE 解（`from_utf16_lossy`），否则按 UTF-8 宽容解。
fn decode_text(content: &[u8], encoding: Encoding) -> String {
    match encoding {
        Encoding::Ucs2 => {
            // UTF-16BE：每 2 字节一个 u16，不足对齐则截断
            let u16s: Vec<u16> = content
                .chunks(2)
                .filter(|c| c.len() == 2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&u16s)
        }
        _ => String::from_utf8_lossy(content).into_owned(),
    }
}

/// 把 SmsAlphabet 翻译为统一模型 Encoding。
fn encoding_of(alphabet: SmsAlphabet) -> Encoding {
    match alphabet {
        SmsAlphabet::GSM7 | SmsAlphabet::ASCII => Encoding::Ascii,
        _ => Encoding::Ucs2,
    }
}

/// SGIP 时间戳（MMDDHHMMSS 紧凑格式），保留原 example 实现。
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

/// 把复合 SGIP 序列（node_id + timestamp + number）打包为 12 字节大端，装进 MessageId::Binary。
/// 这是 Report.submit_sequence 在统一模型中的承载形式（adapter encode 时会反解还原）。
fn seq_to_msg_id(node_id: u32, timestamp: u32, number: u32) -> MessageId {
    let mut b = Vec::with_capacity(12);
    b.extend_from_slice(&node_id.to_be_bytes());
    b.extend_from_slice(&timestamp.to_be_bytes());
    b.extend_from_slice(&number.to_be_bytes());
    MessageId::Binary(b)
}

// ============================================================================
// AuthHandler：SGIP 明文认证（无 MD5）——保持原样，只校验账密，不构造业务 PDU
// ============================================================================

struct SgipAuthHandler {
    accounts: HashMap<String, String>,
}

#[async_trait]
impl AuthHandler for SgipAuthHandler {
    fn name(&self) -> &'static str {
        "sgip-auth"
    }

    async fn authenticate(
        &self,
        _client_id: &str,
        credentials: AuthCredentials,
        _conn_info: &ConnectionInfo,
    ) -> Result<AuthResult> {
        let (login_name, login_password) = match credentials {
            AuthCredentials::Sgip {
                login_name,
                login_password,
            } => (login_name, login_password),
            _ => {
                return Ok(AuthResult::failure(2, "非SGIP认证凭证"));
            }
        };

        let password = match self.accounts.get(&login_name) {
            Some(p) => p,
            None => {
                tracing::warn!(login_name = %login_name, "账号不存在");
                return Ok(AuthResult::failure(2, "账号不存在"));
            }
        };

        if login_password == *password {
            tracing::info!(login_name = %login_name, "认证成功");
            Ok(AuthResult::success(login_name))
        } else {
            tracing::warn!(login_name = %login_name, "密码验证失败");
            Ok(AuthResult::failure(3, "密码错误"))
        }
    }
}

// ============================================================================
// MessageSource：内存队列（按账号隔离）
// ============================================================================

struct FileMessageSource {
    queues: Mutex<HashMap<String, VecDeque<MessageItem>>>,
}

impl FileMessageSource {
    fn new() -> Self {
        Self {
            queues: Mutex::new(HashMap::new()),
        }
    }

    fn load_from_file(path: &str) -> Self {
        let source = Self::new();
        let messages = load_mo_messages(path);
        let mut splitter = LongMessageSplitter::new();
        for mo in messages {
            // 检测编码并转为 wire 字节（UCS2 → UTF-16BE，ASCII → 原字节）。
            // 必须先转 wire 再送 splitter，否则 splitter 按 UTF-8 字节分段
            // 却标 UCS2，对端按 UTF-16BE 解会全乱码。
            let alphabet = detect_alphabet_str(&mo.content);
            let wire = to_wire_bytes(&mo.content, alphabet);
            let encoding = encoding_of(alphabet);
            let single_max = match alphabet {
                SmsAlphabet::GSM7 | SmsAlphabet::ASCII => 160,
                _ => 70,
            };

            if wire.len() > single_max {
                let frames = splitter.split(&wire, alphabet);
                let mut items = Vec::new();
                for frame in frames {
                    // 窄腰：把分段帧转为 (concat, 纯载荷)，由 adapter 重建 UDH 并置 tp_udhi。
                    let (concat, payload) = frame_to_concat(&frame);
                    let pdu = build_deliver_mo(&mo.account, &mo.phone, &payload, encoding, concat);
                    items.push(Arc::new(pdu) as Arc<dyn EncodedPdu>);
                }
                source.push_group_sync(&mo.account, MessageItem::Group { items });
            } else {
                // 单条：直接用 wire 字节构造 Deliver（无 concat），不再用原始 UTF-8。
                let pdu = build_deliver_mo(&mo.account, &mo.phone, &wire, encoding, None);
                source.push_sync(&mo.account, pdu);
            }
        }
        source
    }

    fn push_sync(&self, account: &str, pdu: RawPdu) {
        if let Ok(mut queues) = self.queues.try_lock() {
            queues
                .entry(account.to_string())
                .or_default()
                .push_back(MessageItem::Single(Arc::new(pdu) as Arc<dyn EncodedPdu>));
        }
    }

    fn push_group_sync(&self, account: &str, item: MessageItem) {
        if let Ok(mut queues) = self.queues.try_lock() {
            queues
                .entry(account.to_string())
                .or_default()
                .push_back(item);
        }
    }

    async fn push(&self, account: &str, pdu: RawPdu) {
        let mut queues = self.queues.lock().await;
        queues
            .entry(account.to_string())
            .or_default()
            .push_back(MessageItem::Single(Arc::new(pdu) as Arc<dyn EncodedPdu>));
    }
}

#[async_trait]
impl MessageSource for FileMessageSource {
    async fn fetch(&self, account: &str, batch_size: usize) -> Result<Vec<MessageItem>> {
        let mut queues = self.queues.lock().await;
        let queue = match queues.get_mut(account) {
            Some(q) => q,
            None => return Ok(Vec::new()),
        };

        let mut items = Vec::with_capacity(batch_size.min(queue.len()));
        while items.len() < batch_size {
            match queue.pop_front() {
                Some(item) => items.push(item),
                None => break,
            }
        }
        Ok(items)
    }
}

// ============================================================================
// BusinessHandler：统一模型分支处理客户端上行的所有帧
// ============================================================================

struct SgipBusinessHandler {
    msg_source: Arc<FileMessageSource>,
    merger: Arc<std::sync::Mutex<LongMessageMerger>>,
}

#[async_trait]
impl BusinessHandler for SgipBusinessHandler {
    fn name(&self) -> &'static str {
        "sgip-business"
    }

    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        let unified = match SgipAdapter.decode(frame) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(conn_id = ctx.conn.id(), "消息解码失败: {}", e);
                return Ok(());
            }
        };

        match unified {
            UnifiedMessage::Submit(submit) => {
                self.handle_submit(ctx, frame, submit).await?;
            }
            UnifiedMessage::Report(report) => {
                tracing::info!(
                    conn_id = ctx.conn.id(),
                    status = ?report.status,
                    dest = %report.dest.number,
                    "收到 Report"
                );
                // SGIP 独立 Report 须回 ReportResp；统一模型无 ReportResp 变体，
                // 用 Unknown{command_id=ReportResp} 让 adapter 还原回 ReportResp 编码。
                let resp = UnifiedMessage::Unknown {
                    command_id: rsms_codec_sgip::CommandId::ReportResp as u32,
                    raw: vec![],
                };
                let bytes = SgipAdapter.encode(&resp, SgipAdapter.sequence_of(frame))?;
                ctx.conn.write_frame(&bytes).await?;
            }
            UnifiedMessage::Deliver(deliver) => {
                // 按编码正确解码内容（UCS2 → UTF-16BE，否则 UTF-8 宽容解）。
                let content = decode_text(&deliver.content, deliver.encoding);
                tracing::info!(
                    conn_id = ctx.conn.id(),
                    user_number = %deliver.dest.number,
                    content = %content,
                    "收到 Deliver（MO 上行）"
                );
                // MO-Deliver 的响应是 DeliverResp（语义不同于 ReportResp）。
                let bytes =
                    SgipAdapter.encode(&UnifiedMessage::DeliverResp, SgipAdapter.sequence_of(frame))?;
                ctx.conn.write_frame(&bytes).await?;
            }
            UnifiedMessage::Unbind => {
                tracing::debug!(conn_id = ctx.conn.id(), "收到 Unbind");
            }
            other => {
                tracing::debug!(conn_id = ctx.conn.id(), "收到未处理统一消息: {:?}", other);
            }
        }
        Ok(())
    }
}

impl SgipBusinessHandler {
    async fn handle_submit(
        &self,
        ctx: &InboundContext,
        frame: &Frame,
        submit: rsms_model::UnifiedSubmit,
    ) -> Result<()> {
        let phone = submit
            .dests
            .first()
            .map(|a| a.number.clone())
            .unwrap_or_else(|| "unknown".to_string());

        // 回 SubmitResp（SGIP 无 msg_id，msg_id 给空 Text）；序列回显请求序列。
        let resp = UnifiedMessage::SubmitResp(UnifiedSubmitResp {
            msg_id: MessageId::Text(String::new()),
            status: 0,
        });
        let resp_bytes = SgipAdapter.encode(&resp, SgipAdapter.sequence_of(frame))?;
        ctx.conn.write_frame(&resp_bytes).await?;

        // 处理长短信合包（窄腰）：adapter 已把级联 UDH 剥进 submit.concat、content 为纯载荷，
        // 业务不再读 SGIP tp_udhi / 手剥 UDH（与 cmpp/smgp/smpp server example 同范式）。
        // 据 concat 重建含 UDH 段喂 merger（merger 内部对多段 strip_udh 取纯载荷拼接）。
        if let Some(c) = submit.concat {
            let mut seg_bytes = c.to_udh_prefix();
            seg_bytes.extend_from_slice(&submit.content);
            let frame_lm = LongMessageFrame::new(c.reference, c.total, c.sequence, seg_bytes, true, None);
            let mut merger = self.merger.lock().unwrap();
            let seg = frame_lm.segment_number;
            let total = frame_lm.total_segments;
            match merger.add_frame(&phone, frame_lm) {
                Ok(Some(complete)) => {
                    // 长短信合包后按 submit.encoding 正确解码（UCS2 → UTF-16BE）。
                    let content = decode_text(&complete, submit.encoding);
                    tracing::info!(
                        conn_id = ctx.conn.id(),
                        phone = %phone,
                        content = %content,
                        "长短信合包完成"
                    );
                }
                Ok(None) => {
                    tracing::info!(
                        conn_id = ctx.conn.id(),
                        phone = %phone,
                        seg = seg,
                        total = total,
                        "长短信分段接收"
                    );
                }
                Err(e) => tracing::warn!(conn_id = ctx.conn.id(), "长短信合包错误: {}", e),
            }
        } else {
            // 按 submit.encoding 正确解码内容（UCS2 → UTF-16BE，否则 UTF-8 宽容解）。
            let content = decode_text(&submit.content, submit.encoding);
            tracing::info!(
                conn_id = ctx.conn.id(),
                sp_number = %submit.src.number,
                phone = %phone,
                content = %content,
                "收到短信提交"
            );
        }

        // 需要状态报告：取被报告 Submit 的复合序列（即请求序列）作为 Report.submit_sequence，
        // 并用一个新的复合序列作为 Report 帧自身的头部序列。
        if submit.want_report {
            if let Some(account) = ctx.conn.authenticated_account().await {
                let submit_seq = SgipAdapter.sequence_of(frame);
                let report_number = ctx
                    .id_generator
                    .as_ref()
                    .map(|g| g.next_sequence_id())
                    .unwrap_or(1);
                let report = build_report(&submit_seq, &phone, report_number);
                self.msg_source.push(&account, report).await;
            }
        }

        Ok(())
    }
}

// ============================================================================
// 辅助函数：构建 Report / Deliver（统一模型 -> SgipAdapter.encode -> 字节）
// ============================================================================

/// 构建投递状态报告（SGIP 独立 Report 命令）。
/// - msg_id：被报告 Submit 的复合序列（12B 大端打包），adapter encode 时反解为 Report.submit_sequence
/// - status：Delivered（state=0）
/// - dest：用户号；src：SGIP Report 无源地址字段，置空
/// - raw：[report_type, state, error_code]（与 adapter decode 对称，state 由 status 反映射）
/// - 头部序列：Sequence::Sgip{NODE_ID, sgip_timestamp(), report_number}（本机主动发起）
fn build_report(submit_seq: &Sequence, phone: &str, report_number: u32) -> RawPdu {
    // 把被报告 Submit 的复合序列打包进 msg_id。
    let msg_id = match submit_seq {
        Sequence::Sgip {
            node_id,
            timestamp,
            number,
        } => seq_to_msg_id(*node_id, *timestamp, *number),
        Sequence::Plain(n) => seq_to_msg_id(0, 0, *n),
    };

    let report = UnifiedMessage::Report(UnifiedReport {
        msg_id,
        status: DeliveryStatus::Delivered,
        src: Address::plain(String::new()),
        dest: Address::plain(phone),
        // [report_type=0, state=0(成功), error_code=0]
        raw: vec![0, 0, 0],
    });

    let seq = Sequence::Sgip {
        node_id: NODE_ID,
        timestamp: sgip_timestamp(),
        number: report_number,
    };
    let bytes = SgipAdapter
        .encode(&report, seq)
        .expect("encode SGIP report");
    RawPdu::from(bytes)
}

/// 把 splitter 的分段帧转为窄腰模型的 (concat, 纯载荷)：has_udhi 段剥掉 UDH、concat 承载分段信息。
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

/// 构建 MO 上行 Deliver（窄腰：传 concat + 纯载荷，由 adapter 重建 UDH 并置 tp_udhi）。
/// - 调用方须先用 `to_wire_bytes` 把文本转为正确的线路字节（UCS2 → UTF-16BE），
///   再（必要时）经 splitter 拆分、`frame_to_concat` 转为纯载荷传入。
/// - src=sp_number(account)，dest=user_number(phone)（与 adapter decode 对称）
/// - 单条短信传 concat=None；长短信分段传 Some(concat)；头部序列 Sequence::Sgip{NODE_ID, sgip_timestamp(), 0}
fn build_deliver_mo(
    account: &str,
    phone: &str,
    payload: &[u8],
    encoding: Encoding,
    concat: Option<Concat>,
) -> RawPdu {
    let deliver = UnifiedMessage::Deliver(UnifiedDeliver {
        src: Address::plain(account),
        dest: Address::plain(phone),
        content: payload.to_vec(),
        encoding,
        concat,
        extra: ProtocolExtra::None,
        tlvs: vec![],
    });

    let seq = Sequence::Sgip {
        node_id: NODE_ID,
        timestamp: sgip_timestamp(),
        number: 0,
    };
    let bytes = SgipAdapter
        .encode(&deliver, seq)
        .expect("encode SGIP deliver(MO)");
    RawPdu::from(bytes)
}

// ============================================================================
// AccountConfigProvider
// ============================================================================

struct SimpleAccountConfigProvider;

#[async_trait]
impl AccountConfigProvider for SimpleAccountConfigProvider {
    async fn get_config(&self, _account: &str) -> Result<AccountConfig> {
        Ok(AccountConfig::new()
            .with_max_connections(10)
            .with_max_qps(5000))
    }
}

// ============================================================================
// ServerEventHandler
// ============================================================================

struct SgipServerEventHandler;

#[async_trait]
impl ServerEventHandler for SgipServerEventHandler {
    async fn on_connected(&self, conn: &Arc<dyn ProtocolConnection>) {
        tracing::info!(conn_id = conn.id(), "客户端连接");
    }

    async fn on_disconnected(&self, conn_id: u64, account: Option<&str>) {
        tracing::info!(conn_id = conn_id, account = ?account, "客户端断开");
    }

    async fn on_authenticated(&self, conn: &Arc<dyn ProtocolConnection>, account: &str) {
        tracing::info!(conn_id = conn.id(), account = %account, "认证成功");
    }
}

// ============================================================================
// main
// ============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let accounts_path = Path::new(manifest_dir).join("accounts.conf");
    let messages_path = Path::new(manifest_dir).join("messages.conf");

    let accounts = load_accounts(&accounts_path.to_string_lossy());
    let msg_source = Arc::new(FileMessageSource::load_from_file(&messages_path.to_string_lossy()));

    let config = Arc::new(
        EndpointConfig::new("sgip-gateway", "0.0.0.0", 7891, 500, 60)
            .with_protocol(Protocol::Sgip)
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!("SGIP 网关启动于 {}:{}", config.host, config.port);

    let server = ServerBuilder::new(config)
        .handlers(vec![Arc::new(SgipBusinessHandler {
            msg_source: msg_source.clone(),
            merger: Arc::new(std::sync::Mutex::new(LongMessageMerger::new())),
        })])
        .auth_handler(Arc::new(SgipAuthHandler { accounts }))
        .message_source(msg_source as Arc<dyn MessageSource>)
        .account_config_provider(Arc::new(SimpleAccountConfigProvider))
        .event_handler(Arc::new(SgipServerEventHandler))
        .account_pool_config(AccountPoolConfig::new())
        .serve()
        .await?;

    tracing::info!("监听地址: {}", server.local_addr);
    server.run().await
}
