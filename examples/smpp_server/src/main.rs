// ============================================================================
// SMPP 服务端参考实现（统一模型 / 窄腰架构版）
//
// 与旧版的根本区别：业务代码不再直接接触 SMPP 裸 codec 类型
// （SubmitSm/DeliverSm/SubmitSmResp/Pdu/decode_message_with_version…），
// 全程只用协议无关的 `rsms_model::UnifiedMessage` + `SmppAdapter`（实现 ProtocolAdapter）：
//   - 收包：SmppAdapter.decode(frame) -> UnifiedMessage，业务按统一枚举分支处理
//   - 回包/下发：构造 UnifiedMessage -> SmppAdapter.encode(msg, Sequence) -> 字节
// 切换协议只需换 adapter（CmppAdapter/SmgpAdapter/SgipAdapter）与 EndpointConfig.protocol，业务逻辑零改。
//
// 功能：认证 + 限流 + MessageSource 队列 + 错误处理 + 长短信合包/拆分
// 运行：cargo run
// 配置：accounts.conf（账号密码）、messages.conf（模拟 MO 消息）
//
// 核心流程：
//   1. 从 accounts.conf 读取账号配置
//   2. 客户端连接 → 框架自动完成 SMPP 协议握手（Bind/BindResp）
//   3. 客户端发送 Submit → BusinessHandler.on_inbound() 收到，统一模型解码
//   4. 业务方回 SubmitResp、处理业务（含长短信合包）
//   5. 通过 MessageSource 异步发送 Deliver（MO）/ Report（回执）
//
// SMPP 协议要点（统一模型已封装在 SmppAdapter 内，业务无需关心）：
//   - 认证明文（无 MD5）：AuthHandler 走框架握手范畴，保持原样
//   - Report 经 DeliverSm(esm_class=0x04) 承载——SmppAdapter.encode(Report) 自动置位
//   - MO 长短信用 esm_class bit6 (0x40) 表示 TP-UDHI（无独立 tpudhi 字段）
//   - adapter.decode 不透传 esm_class，故收包侧靠 UdhParser 直接判 UDH 决定是否合包
//   - SMPP 版本差异(V3.4/V5.0)仅字段长度限制，统一模型无需显式处理；如需限定版本
//     可在 EndpointConfig 侧设置（本例使用默认）
// ============================================================================

use async_trait::async_trait;
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_codec_smpp::adapter::SmppAdapter;
use rsms_connector::{
    ServerBuilder, AccountConfig, AccountConfigProvider, AccountPoolConfig, AuthCredentials,
    AuthHandler, AuthResult, MessageItem, MessageSource, ProtocolConnection, ServerEventHandler,
};
use rsms_core::{ConnectionInfo, EncodedPdu, EndpointConfig, Protocol, Frame, RawPdu, Result};
use rsms_longmsg::{
    LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser,
    split::SmsAlphabet,
};
use rsms_model::{
    Address, Concat, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, Sequence,
    SmppExtra, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmitResp,
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

/// 把文本按目标编码转为 wire 字节：UCS2 须为 UTF-16BE（每字符 2 字节大端），
/// 否则按 ASCII/原字节。
/// **关键**：LongMessageSplitter 只按字节分段、不转码，若直接传 content.as_bytes()
/// （UTF-8）却标 data_coding=8(UCS2)，对端按 UTF-16BE 解 UTF-8 → 全乱码。
fn to_wire_bytes(content: &str, alphabet: SmsAlphabet) -> Vec<u8> {
    match alphabet {
        SmsAlphabet::UCS2 => content.encode_utf16().flat_map(|u| u.to_be_bytes()).collect(),
        _ => content.as_bytes().to_vec(),
    }
}

/// 将 wire 字节按编码解码为可显示字符串：UCS2 按 UTF-16BE 解，否则按 UTF-8 解。
fn decode_text(bytes: &[u8], encoding: Encoding) -> String {
    match encoding {
        Encoding::Ucs2 => {
            // UTF-16BE：每 2 字节一个 u16，按大端序组装后解码
            let u16s: Vec<u16> = bytes
                .chunks(2)
                .map(|chunk| {
                    if chunk.len() == 2 {
                        u16::from_be_bytes([chunk[0], chunk[1]])
                    } else {
                        // 字节数为奇数时最后一字节补 0
                        u16::from_be_bytes([chunk[0], 0])
                    }
                })
                .collect();
            String::from_utf16_lossy(&u16s)
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

// ============================================================================
// AuthHandler：SMPP 明文认证
//
// 注：AuthHandler 属框架握手范畴（仅校验账密，不构造业务 PDU），统一模型重构保持原样。
// ============================================================================

struct SmppAuthHandler {
    accounts: HashMap<String, String>,
}

#[async_trait]
impl AuthHandler for SmppAuthHandler {
    fn name(&self) -> &'static str {
        "smpp-auth"
    }

    async fn authenticate(
        &self,
        _client_id: &str,
        credentials: AuthCredentials,
        _conn_info: &ConnectionInfo,
    ) -> Result<AuthResult> {
        let (system_id, password) = match credentials {
            AuthCredentials::Smpp {
                system_id,
                password,
                interface_version: _,
            } => (system_id, password),
            _ => {
                return Ok(AuthResult::failure(15, "非SMPP认证凭证"));
            }
        };

        let expected = match self.accounts.get(&system_id) {
            Some(p) => p,
            None => {
                tracing::warn!(system_id = %system_id, "账号不存在");
                return Ok(AuthResult::failure(15, "账号不存在"));
            }
        };

        if &password == expected {
            tracing::info!(system_id = %system_id, "认证成功");
            Ok(AuthResult::success(&system_id))
        } else {
            tracing::warn!(system_id = %system_id, "密码验证失败");
            Ok(AuthResult::failure(15, "密码错误"))
        }
    }
}

// ============================================================================
// MessageSource：内存队列（按账号隔离）
//
// 长短信 MO 支持：
//   - 超过单条限制的 MO 消息自动拆分为多个 Deliver（统一模型 UnifiedDeliver）
//   - 每个分段经 SmppExtra.esm_class |= 0x40（TP-UDHI）标记，内容携带 UDH 头
//   - 使用 MessageItem::Group 保证同组帧走同一连接顺序发出
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

        for mo in &messages {
            let alphabet = detect_alphabet(&mo.content);
            let encoding = encoding_of(alphabet);
            // UCS2 须先转 UTF-16BE 再分段，否则 splitter 按字节分段后对端解 UTF-16BE 得乱码
            let wire = to_wire_bytes(&mo.content, alphabet);
            let frames = splitter.split(&wire, alphabet);

            if frames.len() == 1 && !frames[0].has_udhi {
                // 普通 MO：单条 Deliver，无 concat
                let bytes = encode_deliver_mo(&mo.account, &mo.phone, &frames[0].content, encoding, None);
                source.push_sync(&mo.account, RawPdu::from(bytes));
            } else {
                // 长短信 MO：每段传 concat+纯载荷，adapter 重建 UDH 并置 TP-UDHI，同组顺序发出
                let items: Vec<Arc<dyn EncodedPdu>> = frames
                    .into_iter()
                    .map(|frame| {
                        let concat = Some(Concat {
                            reference: frame.reference_id,
                            total: frame.total_segments,
                            sequence: frame.segment_number,
                        });
                        let payload = UdhParser::strip_udh(&frame.content);
                        let bytes =
                            encode_deliver_mo(&mo.account, &mo.phone, &payload, encoding, concat);
                        Arc::new(RawPdu::from(bytes)) as Arc<dyn EncodedPdu>
                    })
                    .collect();
                source.push_group_sync(&mo.account, items);
            }
        }
        source
    }

    fn push_sync(&self, account: &str, pdu: RawPdu) {
        if let Ok(mut queues) = self.queues.try_lock() {
            queues
                .entry(account.to_string())
                .or_default()
                .push_back(MessageItem::Single(
                    Arc::new(pdu) as Arc<dyn EncodedPdu>,
                ));
        }
    }

    fn push_group_sync(&self, account: &str, items: Vec<Arc<dyn EncodedPdu>>) {
        if let Ok(mut queues) = self.queues.try_lock() {
            queues
                .entry(account.to_string())
                .or_default()
                .push_back(MessageItem::Group { items });
        }
    }

    async fn push(&self, account: &str, pdu: RawPdu) {
        let mut queues = self.queues.lock().await;
        queues
            .entry(account.to_string())
            .or_default()
            .push_back(MessageItem::Single(
                Arc::new(pdu) as Arc<dyn EncodedPdu>,
            ));
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
//
// 长短信 MT 合包：
//   - adapter.decode 不透传 esm_class，故对 UnifiedSubmit.content 直接跑 UdhParser
//   - 有 UDH 则 merger 合包，合包完成后处理完整消息
// ============================================================================

struct SmppBusinessHandler {
    msg_source: Arc<FileMessageSource>,
    merger: Arc<std::sync::Mutex<LongMessageMerger>>,
}

#[async_trait]
impl BusinessHandler for SmppBusinessHandler {
    fn name(&self) -> &'static str {
        "smpp-business"
    }

    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        let unified = match SmppAdapter.decode(frame) {
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
            UnifiedMessage::Deliver(_) | UnifiedMessage::Report(_) => {
                // 客户端上行 DeliverSm（极少见），回 DeliverResp（sequence_of 自动回显请求序列）
                let bytes =
                    SmppAdapter.encode(&UnifiedMessage::DeliverResp, SmppAdapter.sequence_of(frame))?;
                ctx.conn.write_frame(&bytes).await?;
            }
            UnifiedMessage::Ping => {
                tracing::debug!(conn_id = ctx.conn.id(), "收到 EnquireLink");
            }
            _ => {}
        }
        Ok(())
    }
}

impl SmppBusinessHandler {
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
        let source = submit.src.number.clone();

        // 生成 msg_id：经 IdGenerator（无则回退用序列号），文本化承载到 SubmitResp
        let msg_id = ctx
            .id_generator
            .as_ref()
            .map(|g| g.next_msg_id().to_string())
            .unwrap_or_else(|| format!("{:010}", frame.sequence_id));

        // 回 SubmitResp（框架不自动回，业务方自己回）。sequence_of 自动回显请求序列。
        let resp = UnifiedMessage::SubmitResp(UnifiedSubmitResp {
            msg_id: MessageId::Text(msg_id.clone()),
            status: 0,
        });
        let resp_bytes = SmppAdapter.encode(&resp, SmppAdapter.sequence_of(frame))?;
        ctx.conn.write_frame(&resp_bytes).await?;

        // 长短信合包：adapter 已把 UDH 剥成 submit.concat、content 为纯载荷；
        // 据 concat 重建含 UDH 段交给 merger（其内部再 strip_udh 取 payload 合并）。
        if let Some(concat) = &submit.concat {
            let seg_num = concat.sequence;
            let total = concat.total;
            let mut seg_bytes = concat.to_udh_prefix();
            seg_bytes.extend_from_slice(&submit.content);
            let udh = UdhParser::extract_udh(&seg_bytes).map(|(h, _)| h);
            let lm_frame = LongMessageFrame::new(
                concat.reference,
                total,
                seg_num,
                seg_bytes,
                true,
                udh,
            );
            let mut merger = self.merger.lock().unwrap();
            match merger.add_frame(lm_frame) {
                Ok(Some(merged)) => {
                    // 合包完成后按实际编码解码（UCS2→UTF-16BE，否则 UTF-8）
                    let content = decode_text(&merged, submit.encoding);
                    tracing::info!(
                        conn_id = ctx.conn.id(),
                        source = %source,
                        phone = %phone,
                        content = %content,
                        "长短信 MT 合包完成"
                    );
                }
                Ok(None) => {
                    tracing::debug!(
                        conn_id = ctx.conn.id(),
                        segment = seg_num,
                        total = total,
                        "长短信 MT 分段等待更多"
                    );
                }
                Err(e) => {
                    tracing::warn!(conn_id = ctx.conn.id(), "长短信合包失败: {}", e);
                }
            }
        } else {
            // 按实际编码解码（UCS2→UTF-16BE，否则 UTF-8）
            let content = decode_text(&submit.content, submit.encoding);
            tracing::info!(
                conn_id = ctx.conn.id(),
                source = %source,
                phone = %phone,
                content = %content,
                "收到短信提交"
            );
        }

        // 需要状态报告 → 通过 MessageSource 异步发送（registered_delivery bit0 即 want_report）
        if submit.want_report {
            if let Some(account) = ctx.conn.authenticated_account().await {
                let report = encode_deliver_report(&account, &msg_id, &phone);
                self.msg_source.push(&account, RawPdu::from(report)).await;
            }
        }

        Ok(())
    }
}

// ============================================================================
// 辅助函数：构建下行帧字节（经 SmppAdapter.encode，零裸 codec）
// ============================================================================

/// 构建投递状态报告字节。
/// 统一模型 UnifiedMessage::Report → SmppAdapter 自动产出 DeliverSm(esm_class=0x04)
/// 并写入 receipted_message_id TLV，raw 作为回执正文 short_message。
fn encode_deliver_report(account: &str, msg_id: &str, phone: &str) -> Vec<u8> {
    let raw = format!(
        "id:{} sub:001 dlvrd:001 submit date:done date:stat:DELIVRD err:000",
        msg_id
    )
    .into_bytes();
    let report = UnifiedMessage::Report(UnifiedReport {
        msg_id: MessageId::Text(msg_id.to_string()),
        status: DeliveryStatus::Delivered,
        // 报告源是手机号，目的是接入账号（与下行 MO 方向一致）
        src: Address::plain(phone),
        dest: Address::plain(account),
        raw,
    });
    SmppAdapter
        .encode(&report, Sequence::Plain(0))
        .expect("encode report")
}

/// 构建 MO 上行字节。
/// 统一模型 UnifiedMessage::Deliver（非回执位）→ SmppAdapter 产出 DeliverSm。
/// 传 concat（Some=长短信分段）则由 adapter 重建 UDH 并置 esm_class TP-UDHI(0x40)。
fn encode_deliver_mo(
    account: &str,
    phone: &str,
    content: &[u8],
    encoding: Encoding,
    concat: Option<Concat>,
) -> Vec<u8> {
    let deliver = UnifiedMessage::Deliver(UnifiedDeliver {
        src: Address::plain(phone),
        dest: Address::plain(account),
        content: content.to_vec(),
        encoding,
        concat,
        extra: ProtocolExtra::None,
        tlvs: vec![],
    });
    SmppAdapter
        .encode(&deliver, Sequence::Plain(0))
        .expect("encode deliver mo")
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

struct SmppServerEventHandler;

#[async_trait]
impl ServerEventHandler for SmppServerEventHandler {
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
        EndpointConfig::new("smpp-gateway", "0.0.0.0", 7893, 500, 60)
            .with_protocol(Protocol::Smpp)
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!(
        "SMPP 网关启动于 {}:{}",
        config.host,
        config.port
    );

    let merger = Arc::new(std::sync::Mutex::new(LongMessageMerger::new()));

    let server = ServerBuilder::new(config)
        .handlers(vec![Arc::new(SmppBusinessHandler {
            msg_source: msg_source.clone(),
            merger: merger.clone(),
        })])
        .auth_handler(Arc::new(SmppAuthHandler { accounts }))
        .message_source(msg_source as Arc<dyn MessageSource>)
        .account_config_provider(Arc::new(SimpleAccountConfigProvider))
        .event_handler(Arc::new(SmppServerEventHandler))
        .account_pool_config(AccountPoolConfig::new())
        .serve()
        .await?;

    tracing::info!("监听地址: {}", server.local_addr);
    server.run().await
}
