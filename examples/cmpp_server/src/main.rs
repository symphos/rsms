// ============================================================================
// CMPP 服务端完整参考实现（统一模型 / 窄腰架构版）
//
// 与旧版的根本区别：业务代码不再直接接触 CMPP 裸 codec 类型（Submit/Deliver/
// SubmitResp/Pdu…），全程只用协议无关的 `rsms_model::UnifiedMessage` + `CmppAdapter`
// （实现 ProtocolAdapter）：
//   - 收包：CmppAdapter.decode(frame) -> UnifiedMessage，业务按统一枚举分支处理
//   - 回包/发包：构造 UnifiedMessage -> CmppAdapter.encode(msg, Sequence) -> 字节
//   - 回 SubmitResp 用 CmppAdapter.sequence_of(frame) 回显请求序列（不再手剥 data[8..]）
// 切换协议只需换 adapter（SmgpAdapter/SmppAdapter/SgipAdapter）与 Decoder，业务逻辑零改。
//
// 唯一仍触碰 codec 的地方是 AuthHandler 的 MD5 比对（compute_connect_auth）——这是
// 框架握手范畴的加密工具，不构造消息 PDU，故保持原样。
//
// 功能：认证 + 限流 + MessageSource 队列 + 长短信合包 + 错误处理
// 运行：cargo run
// 配置：accounts.conf（账号密码）、messages.conf（模拟 MO 消息）
//
// 核心流程：
//   1. 从 accounts.conf 读取账号配置
//   2. 客户端连接 → 框架自动完成 CMPP 协议握手（Connect/ConnectResp）
//   3. 客户端发送 Submit → BusinessHandler.on_inbound() 收到
//   4. 业务方解码为 UnifiedMessage::Submit、回 SubmitResp、处理业务（含长短信合包）
//   5. 通过 MessageSource 异步发送 Deliver(MO) / Report（状态报告）
// ============================================================================

use async_trait::async_trait;
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
// compute_connect_auth 是 MD5 加密工具（握手鉴权用），非消息 PDU 构造，保留。
use rsms_codec_cmpp::adapter::CmppAdapter;
use rsms_codec_cmpp::compute_connect_auth;
use rsms_connector::{
    AccountConfig, AccountConfigProvider, AccountPoolConfig, AuthCredentials, AuthHandler,
    AuthResult, BoundServer, MessageItem, MessageSource, ProtocolConnection, ServerBuilder,
    ServerEventHandler, SimpleIdGenerator,
};
use rsms_core::{
    ConnectionInfo, EncodedPdu, EndpointConfig, Frame, IdGenerator, Protocol, RawPdu, Result,
};
use rsms_longmsg::split::SmsAlphabet;
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser};
use rsms_model::{
    Address, CmppExtra, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra,
    Sequence, UnifiedDeliver, UnifiedMessage, UnifiedReport,
};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

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

/// 把统一 SmsAlphabet 翻译为统一模型 Encoding（与 CMPP msg_fmt 语义对齐）。
fn encoding_of(alphabet: SmsAlphabet) -> Encoding {
    match alphabet {
        // 旧实现 MO/Report 统一用 msg_fmt=15（GBK）承载文本，这里沿用以保持业务语义。
        SmsAlphabet::ASCII | SmsAlphabet::GSM7 => Encoding::Gbk,
        _ => Encoding::Ucs2,
    }
}

/// 把文本按目标编码转为线路字节：UCS2 须为 UTF-16BE（每字符 2 字节大端），
/// 否则按原始字节。发送 MO 时必须先转换，否则对端按 UTF-16BE 解 UTF-8 会乱码。
fn to_wire_bytes(content: &str, alphabet: SmsAlphabet) -> Vec<u8> {
    match alphabet {
        SmsAlphabet::UCS2 => content.encode_utf16().flat_map(|u| u.to_be_bytes()).collect(),
        _ => content.as_bytes().to_vec(),
    }
}

/// 按 encoding 把线路字节解码为可显示字符串：UCS2 按 UTF-16BE 解，否则按 UTF-8 解。
fn decode_text(bytes: &[u8], encoding: Encoding) -> std::borrow::Cow<'_, str> {
    match encoding {
        Encoding::Ucs2 => {
            // UTF-16BE：每两个字节构成一个 u16，再转 String
            let u16s: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            std::borrow::Cow::Owned(String::from_utf16_lossy(&u16s))
        }
        _ => String::from_utf8_lossy(bytes),
    }
}

// ============================================================================
// AuthHandler：MD5 认证（保持原样——仅校验账密 + MD5 比对，不构造消息 PDU）
// ============================================================================

struct CmppAuthHandler {
    accounts: HashMap<String, String>,
}

#[async_trait]
impl AuthHandler for CmppAuthHandler {
    fn name(&self) -> &'static str {
        "cmpp-auth"
    }

    async fn authenticate(
        &self,
        _client_id: &str,
        credentials: AuthCredentials,
        _conn_info: &ConnectionInfo,
    ) -> Result<AuthResult> {
        let (source_addr, authenticator_source, timestamp) = match credentials {
            AuthCredentials::Cmpp {
                source_addr,
                authenticator_source,
                version: _,
                timestamp,
            } => (source_addr, authenticator_source, timestamp),
            _ => {
                return Ok(AuthResult::failure(2, "非CMPP认证凭证"));
            }
        };

        let password = match self.accounts.get(&source_addr) {
            Some(p) => p,
            None => {
                tracing::warn!(source_addr = %source_addr, "账号不存在");
                return Ok(AuthResult::failure(2, "账号不存在"));
            }
        };

        let expected = compute_connect_auth(&source_addr, password, timestamp);
        if authenticator_source == expected {
            tracing::info!(source_addr = %source_addr, "认证成功");
            Ok(AuthResult::success(source_addr))
        } else {
            tracing::warn!(source_addr = %source_addr, "密码验证失败");
            Ok(AuthResult::failure(3, "密码错误"))
        }
    }
}

// ============================================================================
// MessageSource：内存队列（按账号隔离）
// ============================================================================

struct FileMessageSource {
    queues: Mutex<HashMap<String, VecDeque<MessageItem>>>,
    id_generator: Arc<dyn IdGenerator>,
}

impl FileMessageSource {
    fn new(id_generator: Arc<dyn IdGenerator>) -> Self {
        Self {
            queues: Mutex::new(HashMap::new()),
            id_generator,
        }
    }

    fn load_from_file(path: &str, id_generator: Arc<dyn IdGenerator>) -> Self {
        let source = Self::new(id_generator);
        let messages = load_mo_messages(path);
        let mut splitter = LongMessageSplitter::new();
        for mo in messages {
            // 先按目标编码把文本转为线路字节（UCS2 → UTF-16BE），再按字节数决定是否拆段。
            // 若直接用 as_bytes()（UTF-8）却标 UCS2，对端按 UTF-16BE 解会全乱码。
            let alphabet = if mo.content.is_ascii() {
                SmsAlphabet::ASCII
            } else {
                SmsAlphabet::UCS2
            };
            let wire = to_wire_bytes(&mo.content, alphabet);
            let single_max = match alphabet {
                SmsAlphabet::GSM7 | SmsAlphabet::ASCII => 160,
                _ => 70,
            };

            if wire.len() > single_max {
                let frames = splitter.split(&wire, alphabet);
                let mut items = Vec::new();
                for frame in frames {
                    let msg_id = source.id_generator.next_msg_id().to_be_bytes();
                    let pdu = build_deliver_mo_with_udh(
                        &mo.account,
                        &mo.phone,
                        &frame.content,
                        frame.has_udhi,
                        &msg_id,
                        alphabet,
                    );
                    items.push(Arc::new(pdu) as Arc<dyn EncodedPdu>);
                }
                source.push_group_sync(&mo.account, MessageItem::Group { items });
            } else {
                let msg_id = source.id_generator.next_msg_id().to_be_bytes();
                // 单条短信：wire 字节已是正确编码（UCS2→UTF-16BE），直接构造 MO。
                let pdu = build_deliver_mo_with_udh(
                    &mo.account,
                    &mo.phone,
                    &wire,
                    false,
                    &msg_id,
                    alphabet,
                );
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
// BusinessHandler：处理 Submit，回 SubmitResp，推送 Report，长短信合包
// ============================================================================

struct CmppBusinessHandler {
    msg_source: Arc<FileMessageSource>,
    merger: Arc<std::sync::Mutex<LongMessageMerger>>,
}

#[async_trait]
impl BusinessHandler for CmppBusinessHandler {
    fn name(&self) -> &'static str {
        "cmpp-business"
    }

    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        let unified = match CmppAdapter.decode(frame) {
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
            UnifiedMessage::Ping => {
                tracing::debug!(conn_id = ctx.conn.id(), "收到 ActiveTest（心跳）");
            }
            _ => {}
        }
        Ok(())
    }
}

impl CmppBusinessHandler {
    async fn handle_submit(
        &self,
        ctx: &InboundContext,
        frame: &Frame,
        submit: rsms_model::UnifiedSubmit,
    ) -> Result<()> {
        let phone = submit
            .dests
            .first()
            .map(|a| a.number.as_str())
            .unwrap_or("unknown")
            .to_string();

        // CMPP 方言字段（msg_id / tpudhi）在 ProtocolExtra::Cmpp 里。
        let (msg_id, tpudhi) = match &submit.extra {
            ProtocolExtra::Cmpp(e) => (e.msg_id, e.tpudhi),
            _ => ([0u8; 8], 0),
        };

        // 回 SubmitResp（框架不自动回，业务方自己回）。
        // 用 CmppAdapter.sequence_of(frame) 回显请求序列号，跨协议统一写法。
        let resp = UnifiedMessage::SubmitResp(rsms_model::UnifiedSubmitResp {
            msg_id: MessageId::Binary(msg_id.to_vec()),
            status: 0,
        });
        let resp_bytes = CmppAdapter.encode(&resp, CmppAdapter.sequence_of(frame))?;
        ctx.conn.write_frame(&resp_bytes).await?;

        // 处理长短信合包（对 UnifiedSubmit.content 跑 UdhParser/merger）。
        if tpudhi == 1 {
            if let Some((udh, _)) = UdhParser::extract_udh(&submit.content) {
                let frame_lm = LongMessageFrame::new(
                    udh.reference_id,
                    udh.total_segments,
                    udh.segment_number,
                    submit.content.clone(),
                    true,
                    Some(udh),
                );
                let mut merger = self.merger.lock().unwrap();
                let seg = frame_lm.segment_number;
                let total = frame_lm.total_segments;
                match merger.add_frame(frame_lm) {
                    Ok(Some(complete)) => {
                        // 长短信合包完成后按 encoding 正确解码（UCS2→UTF-16BE，否则 UTF-8）
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
            }
        } else {
            // 按 encoding 解码显示（UCS2→UTF-16BE，否则 UTF-8）
            let content = decode_text(&submit.content, submit.encoding);
            tracing::info!(
                conn_id = ctx.conn.id(),
                phone = %phone,
                content = %content,
                "收到短信提交"
            );
        }

        // 需要状态报告 → 通过 MessageSource 异步发送。
        if submit.want_report {
            if let Some(account) = ctx.conn.authenticated_account().await {
                let report = build_deliver_report(&account, &msg_id, &phone);
                self.msg_source.push(&account, report).await;
            }
        }

        Ok(())
    }
}

// ============================================================================
// 辅助函数：构建 Deliver(MO) / Report 字节（经 CmppAdapter.encode 产出）
//
// 旧版直接 new 出 codec 的 Deliver + Pdu::to_pdu_bytes；现统一改为构造
// UnifiedMessage::{Report,Deliver} → CmppAdapter.encode(.., Sequence::Plain(0)) → 字节。
// 注意：旧版下行 Deliver 序列号一律传 0（框架 run_outbound_fetcher 不改写已编码字节的序列），
// 此处保持 Sequence::Plain(0) 以维持原行为。
// ============================================================================

fn build_deliver_report(account: &str, msg_id: &[u8; 8], phone: &str) -> RawPdu {
    let msg_id_hex: String = msg_id.iter().map(|b| format!("{:02x}", b)).collect();
    let now = chrono_now_str();

    let report_content = format!(
        "MsgId:{} Stat:DELIVRD SubmitTime:{} DoneTime:{} DestTerminalId:{} SMSCSequence:0",
        msg_id_hex, now, now, phone
    );

    // 状态报告：UnifiedReport（src=终端号 phone，dest=账号），由 adapter 编码为
    // Deliver(registered_delivery=1) 字节。status 取 Delivered（DELIVRD）。
    let report = UnifiedMessage::Report(UnifiedReport {
        msg_id: MessageId::Binary(msg_id.to_vec()),
        status: DeliveryStatus::Delivered,
        src: Address::plain(phone),
        dest: Address::plain(account),
        raw: report_content.into_bytes(),
    });
    let bytes = CmppAdapter
        .encode(&report, Sequence::Plain(0))
        .expect("encode CMPP report");
    RawPdu::from(bytes)
}

fn build_deliver_mo(account: &str, phone: &str, content: &str, msg_id: &[u8; 8]) -> RawPdu {
    // 先转线路字节（UCS2→UTF-16BE），再传给底层构造函数，避免对端按 UTF-16BE 解 UTF-8 乱码。
    let alphabet = if content.is_ascii() {
        SmsAlphabet::ASCII
    } else {
        SmsAlphabet::UCS2
    };
    let wire = to_wire_bytes(content, alphabet);
    build_deliver_mo_with_udh(account, phone, &wire, false, msg_id, alphabet)
}

fn build_deliver_mo_with_udh(
    account: &str,
    phone: &str,
    content: &[u8],
    has_udhi: bool,
    _msg_id: &[u8; 8],
    alphabet: SmsAlphabet,
) -> RawPdu {
    // MO 上行：UnifiedDeliver（src=终端号 phone，dest=账号），由 adapter 编码为
    // Deliver(registered_delivery=0) 字节。长短信 UDH 标志通过 ProtocolExtra::Cmpp.tpudhi 传递。
    let extra = ProtocolExtra::Cmpp(CmppExtra {
        tpudhi: if has_udhi { 1 } else { 0 },
        ..Default::default()
    });
    let deliver = UnifiedMessage::Deliver(UnifiedDeliver {
        src: Address::plain(phone),
        dest: Address::plain(account),
        content: content.to_vec(),
        encoding: encoding_of(alphabet),
        concat: None,
        extra,
        tlvs: vec![],
    });
    let bytes = CmppAdapter
        .encode(&deliver, Sequence::Plain(0))
        .expect("encode CMPP deliver(MO)");
    RawPdu::from(bytes)
}

fn chrono_now_str() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let h = ((secs / 3600) % 24 + 8) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    let month = ((secs / 86400 / 30) % 12 + 1) as u8;
    let day = ((secs / 86400) % 30 + 1) as u8;
    format!("{:02}{:02}{:02}{:02}{:02}", month, day, h, m, s)
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

struct CmppServerEventHandler;

#[async_trait]
impl ServerEventHandler for CmppServerEventHandler {
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
    let msg_source = Arc::new(FileMessageSource::load_from_file(
        &messages_path.to_string_lossy(),
        Arc::new(SimpleIdGenerator::new()),
    ));

    let config = Arc::new(
        EndpointConfig::new("cmpp-gateway", "0.0.0.0", 7890, 500, 60)
            .with_protocol(Protocol::Cmpp)
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!("CMPP 网关启动于 {}:{}", config.host, config.port);

    let server: BoundServer = ServerBuilder::new(config)
        .handlers(vec![Arc::new(CmppBusinessHandler {
            msg_source: msg_source.clone(),
            merger: Arc::new(std::sync::Mutex::new(LongMessageMerger::new())),
        })])
        .auth_handler(Arc::new(CmppAuthHandler { accounts }))
        .message_source(msg_source as Arc<dyn MessageSource>)
        .account_config_provider(Arc::new(SimpleAccountConfigProvider))
        .event_handler(Arc::new(CmppServerEventHandler))
        .account_pool_config(AccountPoolConfig::new())
        .serve()
        .await?;

    tracing::info!("监听地址: {}", server.local_addr);
    server.run().await
}
