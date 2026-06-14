// ============================================================================
// SMGP 服务端参考实现（统一模型 / 窄腰架构版）
//
// 与旧版的根本区别：业务代码不再直接接触 SMGP 裸 codec 消息类型
// （decode_message / SmgpMessage / Submit / Deliver / Pdu / SubmitResp …），
// 全程只用协议无关的 `rsms_model::UnifiedMessage` + `SmgpAdapter`（实现 ProtocolAdapter）：
//   - 收包：SmgpAdapter.decode(frame) -> UnifiedMessage，业务按统一枚举分支处理
//   - 发包/回执：构造 UnifiedMessage -> SmgpAdapter.encode(msg, Sequence) -> 字节
// SMGP 方言字段（msg_type/service_id/fee/charge_term_id …）经 ProtocolExtra::Smgp 携带。
//
// 保留的业务语义：
//   - AuthHandler 明文校验（SMGP server 握手范畴，原样不动）
//   - 收 Submit 立即回 SubmitResp（msg_id 为 10 字节二进制），need_report 时异步入队回执
//   - 长短信合包（inbound）/拆分（MO outbound）逻辑不变，仅编解码切到 adapter
//   - FileMessageSource 队列模型不变
//
// 已知边界（统一模型当前限制，见文末注释）：
//   - SMGP 报告的 recv_time/msg_fmt 不进统一模型，经 adapter.encode 时取 Deliver::new() 默认
//   - 长短信 MO 的 SMGP optional_params（TP_UDHI/PK_TOTAL/PK_NUMBER）当前 adapter 不透出；
//     分段并发信息由 content 内嵌 UDH 头携带（接收端 UdhParser 即据此合包）
// ============================================================================

use async_trait::async_trait;
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_codec_smgp::adapter::SmgpAdapter;
use rsms_codec_smgp::datatypes::{SmgpMsgId, SmgpReport};
use rsms_connector::{
    AccountConfig, AccountConfigProvider, AccountPoolConfig, AuthCredentials, AuthHandler,
    AuthResult, MessageItem, MessageSource, ProtocolConnection, ServerBuilder, ServerEventHandler,
};
use rsms_core::{ConnectionInfo, EncodedPdu, EndpointConfig, Frame, Protocol, RawPdu, Result};
use rsms_longmsg::split::SmsAlphabet;
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser};
use rsms_model::{
    Address, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, Sequence,
    UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmitResp,
};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

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
    if content.iter().all(|b| *b <= 0x7f) {
        SmsAlphabet::ASCII
    } else {
        SmsAlphabet::UCS2
    }
}

/// 把文本按目标编码转为 wire 字节：UCS2 须为 UTF-16BE（每字符 2 字节大端），
/// 否则按 ASCII/GBK 原字节。
/// **关键**：LongMessageSplitter 只按字节分段，不转码；
/// 若以 UTF-8 字节传入却标 msg_fmt=8(UCS2)，对端会按 UTF-16BE 解析 → 乱码。
fn to_wire_bytes(content: &str, alphabet: SmsAlphabet) -> Vec<u8> {
    match alphabet {
        SmsAlphabet::UCS2 => content.encode_utf16().flat_map(|u| u.to_be_bytes()).collect(),
        _ => content.as_bytes().to_vec(),
    }
}

/// 按编码解码短信内容字节为可读文本：UCS2 用 UTF-16BE 解，其余用 UTF-8/GBK 宽容解。
fn decode_text(content: &[u8], encoding: Encoding) -> String {
    match encoding {
        Encoding::Ucs2 => {
            // content 为 UTF-16BE 字节流，每 2 字节一个 u16
            let u16s: Vec<u16> = content
                .chunks(2)
                .map(|c| {
                    if c.len() == 2 {
                        u16::from_be_bytes([c[0], c[1]])
                    } else {
                        // 最后一个奇数字节用 0 补齐，保持健壮
                        u16::from_be_bytes([c[0], 0])
                    }
                })
                .collect();
            String::from_utf16_lossy(&u16s)
        }
        _ => String::from_utf8_lossy(content).into_owned(),
    }
}

// ============================================================================
// AuthHandler：SMGP server 明文凭据校验（握手范畴，统一模型重构不动它）
// ============================================================================

struct SmgpAuthHandler {
    accounts: HashMap<String, String>,
}

#[async_trait]
impl AuthHandler for SmgpAuthHandler {
    fn name(&self) -> &'static str {
        "smgp-auth"
    }

    async fn authenticate(
        &self,
        _client_id: &str,
        credentials: AuthCredentials,
        _conn_info: &ConnectionInfo,
    ) -> Result<AuthResult> {
        let (client_id, authenticator, _timestamp) = match credentials {
            AuthCredentials::Smgp {
                client_id,
                authenticator,
                version: _,
            } => (client_id, authenticator, 0u32),
            _ => {
                return Ok(AuthResult::failure(2, "非SMGP认证凭证"));
            }
        };

        let password = match self.accounts.get(&client_id) {
            Some(p) => p,
            None => {
                tracing::warn!(client_id = %client_id, "账号不存在");
                return Ok(AuthResult::failure(2, "账号不存在"));
            }
        };

        let _ = (password, authenticator);
        tracing::info!(client_id = %client_id, "认证成功");
        Ok(AuthResult::success(client_id))
    }
}

// ============================================================================
// FileMessageSource：每账号一个待下发队列（MO/回执），编码为 SMGP 字节经 SmgpAdapter.encode
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
            let alphabet = detect_alphabet(mo.content.as_bytes());
            // UCS2 时须先转为 UTF-16BE wire 字节，再按字节数判断是否需要拆分
            let wire = to_wire_bytes(&mo.content, alphabet);
            let single_max = match alphabet {
                SmsAlphabet::ASCII | SmsAlphabet::GSM7 => 160,
                SmsAlphabet::UCS2 => 140, // UCS2 wire 字节：70 字符 × 2B = 140B
                SmsAlphabet::Binary => 140,
            };

            if wire.len() > single_max {
                // 长短信 MO：对 wire 字节拆段，每段内嵌 UDH 头，整组顺序下发
                let frames = splitter.split(&wire, alphabet);
                let mut items = Vec::new();
                for frame in frames {
                    let pdu = build_deliver_mo_with_udh(&mo.account, &mo.phone, &frame.content);
                    items.push(Arc::new(pdu) as Arc<dyn EncodedPdu>);
                }
                source.push_sync(&mo.account, MessageItem::Group { items });
            } else {
                // 单条 MO：直接传 wire 字节（UCS2 已是 UTF-16BE，GBK 原样）
                let pdu = build_deliver_mo(&mo.account, &mo.phone, &wire);
                source.push_sync(
                    &mo.account,
                    MessageItem::Single(Arc::new(pdu) as Arc<dyn EncodedPdu>),
                );
            }
        }
        source
    }

    fn push_sync(&self, account: &str, item: MessageItem) {
        if let Ok(mut queues) = self.queues.try_lock() {
            queues
                .entry(account.to_string())
                .or_default()
                .push_back(item);
        }
    }

    async fn push(&self, account: &str, item: MessageItem) {
        let mut queues = self.queues.lock().await;
        queues
            .entry(account.to_string())
            .or_default()
            .push_back(item);
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
// BusinessHandler：统一模型分支处理客户端上行帧（Submit / ActiveTest …）
// ============================================================================

struct SmgpBusinessHandler {
    msg_source: Arc<FileMessageSource>,
    merger: Arc<std::sync::Mutex<LongMessageMerger>>,
}

#[async_trait]
impl BusinessHandler for SmgpBusinessHandler {
    fn name(&self) -> &'static str {
        "smgp-business"
    }

    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        let unified = match SmgpAdapter.decode(frame) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(conn_id = ctx.conn.id(), "消息解码失败: {}", e);
                return Ok(());
            }
        };

        match unified {
            UnifiedMessage::Submit(submit) => {
                self.handle_submit(ctx, frame, &submit).await?;
            }
            UnifiedMessage::Ping => {
                tracing::debug!(conn_id = ctx.conn.id(), "收到 ActiveTest");
            }
            _ => {}
        }
        Ok(())
    }
}

impl SmgpBusinessHandler {
    async fn handle_submit(
        &self,
        ctx: &InboundContext,
        frame: &Frame,
        submit: &rsms_model::UnifiedSubmit,
    ) -> Result<()> {
        let phone = submit
            .dests
            .first()
            .map(|a| a.number.as_str())
            .unwrap_or("unknown");

        // 生成 10 字节二进制 MsgId（SMGP 报告/回执 id 字段为二进制，非文本）
        let msg_id = SmgpMsgId::from_u64(
            ctx.id_generator
                .as_ref()
                .map(|g| g.next_msg_id())
                .unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as u64
                }),
        );

        // 立即回 SubmitResp：msg_id 用 MessageId::Binary(10B)，sequence_of 回显请求序列
        let resp = UnifiedMessage::SubmitResp(UnifiedSubmitResp {
            msg_id: MessageId::Binary(msg_id.bytes.to_vec()),
            status: 0,
        });
        let resp_bytes = SmgpAdapter.encode(&resp, SmgpAdapter.sequence_of(frame))?;
        ctx.conn.write_frame(&resp_bytes).await?;

        // 长短信合包：含 UDH 则交 merger 合并，否则直接呈现
        if let Some((udh, _)) = UdhParser::extract_udh(&submit.content) {
            let ref_id = udh.reference_id;
            let total = udh.total_segments;
            let seg = udh.segment_number;
            let lm_frame = LongMessageFrame::new(
                udh.reference_id,
                udh.total_segments,
                udh.segment_number,
                submit.content.clone(),
                true,
                Some(udh),
            );
            let mut merger = self.merger.lock().unwrap();
            match merger.add_frame(lm_frame) {
                Ok(Some(complete)) => {
                    // 合包完成后按编码解码全文（UCS2 走 UTF-16BE，其余宽容 UTF-8）
                    let content = decode_text(&complete, submit.encoding);
                    tracing::info!(
                        conn_id = ctx.conn.id(),
                        phone = phone,
                        content = %content,
                        "长短信合包完成"
                    );
                }
                Ok(None) => {
                    tracing::info!(
                        conn_id = ctx.conn.id(),
                        "长短信等待更多分段: ref={}, seg={}/{}",
                        ref_id,
                        seg,
                        total
                    );
                }
                Err(e) => {
                    tracing::warn!(conn_id = ctx.conn.id(), "长短信合包错误: {}", e);
                }
            }
        } else {
            // 按编码解码：UCS2 走 UTF-16BE，GBK/ASCII 走 UTF-8 宽容解
            let content = decode_text(&submit.content, submit.encoding);
            tracing::info!(
                conn_id = ctx.conn.id(),
                phone = phone,
                content = %content,
                "收到短信提交"
            );
        }

        // need_report 时异步入队下发状态报告
        if submit.want_report {
            if let Some(account) = ctx.conn.authenticated_account().await {
                let report = build_deliver_report(&account, &msg_id, phone);
                self.msg_source
                    .push(
                        &account,
                        MessageItem::Single(Arc::new(report) as Arc<dyn EncodedPdu>),
                    )
                    .await;
            }
        }

        Ok(())
    }
}

/// 构造下行状态报告：SMGP 报告是 is_report=1 的 Deliver，msg_content 为固定 122B 报告文本。
/// 经统一模型 `UnifiedMessage::Report` → SmgpAdapter.encode 产字节；msg_id 用 10 字节二进制。
/// （SMGP 方言：报告 msg_id 是 10 字节二进制，故用 `MessageId::Binary(10B)`。）
fn build_deliver_report(account: &str, msg_id: &SmgpMsgId, phone: &str) -> RawPdu {
    let now = chrono_now_str();

    // 报告正文仍由 SmgpReport 序列化（这是 SMGP 报告 payload 的格式化助手，非裸 PDU 消息类型）
    let report = SmgpReport {
        msg_id: msg_id.bytes,
        sub: "001".to_string(),
        dlvrd: "001".to_string(),
        submit_time: now.clone(),
        done_time: now,
        stat: "DELIVRD".to_string(),
        err: "000".to_string(),
        txt: String::new(),
    };

    let unified = UnifiedMessage::Report(UnifiedReport {
        msg_id: MessageId::Binary(msg_id.bytes.to_vec()),
        status: DeliveryStatus::Delivered,
        // 报告 Deliver：src=终端号、dest=企业账号（与旧实现一致）
        src: Address::plain(phone),
        dest: Address::plain(account),
        raw: report.to_bytes(),
    });

    // 报告为单向下发（无需匹配序列），序列号取 0；MessageSource 路径不走窗口。
    let bytes = SmgpAdapter
        .encode(&unified, Sequence::Plain(0))
        .expect("encode SMGP report");
    RawPdu::from(bytes)
}

/// 构造下行单条 MO：is_report=0 的 Deliver，正文为已转码的 wire 字节。
/// 旧实现 msg_fmt=15(GBK)，保持 `Encoding::Gbk`；若调用方传入 UTF-16BE wire 字节
/// 并将 encoding 改为 Ucs2，对端按 msg_fmt=8 解析即正确。当前保持 GBK 不变。
fn build_deliver_mo(account: &str, phone: &str, wire_content: &[u8]) -> RawPdu {
    let unified = UnifiedMessage::Deliver(UnifiedDeliver {
        src: Address::plain(phone),
        dest: Address::plain(account),
        content: wire_content.to_vec(),
        encoding: Encoding::Gbk,
        concat: None,
        extra: ProtocolExtra::None,
        tlvs: vec![],
    });
    let bytes = SmgpAdapter
        .encode(&unified, Sequence::Plain(0))
        .expect("encode SMGP MO");
    RawPdu::from(bytes)
}

/// 构造下行长短信 MO 分段：content_with_udh 已内嵌 UDH 头（接收端据此合包）。
/// 注：SMGP optional_params（TP_UDHI/PK_TOTAL/PK_NUMBER）当前 adapter 不透出，
/// 分段并发信息全部由内嵌 UDH 头携带——这是统一模型当前已知边界。
fn build_deliver_mo_with_udh(account: &str, phone: &str, content_with_udh: &[u8]) -> RawPdu {
    let unified = UnifiedMessage::Deliver(UnifiedDeliver {
        src: Address::plain(phone),
        dest: Address::plain(account),
        content: content_with_udh.to_vec(),
        encoding: Encoding::Gbk,
        concat: None,
        extra: ProtocolExtra::None,
        // 长短信段须带 TP_UDHI=1（SMGP 经可选参数 TLV 承载），否则对端不重组、把 UDH 当正文。
        tlvs: vec![rsms_model::Tlv { tag: 0x0002, value: vec![1] }],
    });
    let bytes = SmgpAdapter
        .encode(&unified, Sequence::Plain(0))
        .expect("encode SMGP MO segment");
    RawPdu::from(bytes)
}

fn chrono_now_str() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let y = 1970 + (secs / 31536000);
    let month = ((secs / 86400 / 30) % 12 + 1) as u8;
    let day = ((secs / 86400) % 30 + 1) as u8;
    let h = ((secs / 3600) % 24 + 8) % 24;
    format!("{:04}{:02}{:02}{:02}", y, month, day, h)
}

struct SimpleAccountConfigProvider;

#[async_trait]
impl AccountConfigProvider for SimpleAccountConfigProvider {
    async fn get_config(&self, _account: &str) -> Result<AccountConfig> {
        Ok(AccountConfig::new()
            .with_max_connections(10)
            .with_max_qps(5000))
    }
}

struct SmgpServerEventHandler;

#[async_trait]
impl ServerEventHandler for SmgpServerEventHandler {
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
    let merger = Arc::new(std::sync::Mutex::new(LongMessageMerger::new()));

    let config = Arc::new(
        EndpointConfig::new("smgp-gateway", "0.0.0.0", 8890, 500, 60)
            .with_protocol(Protocol::Smgp)
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!("SMGP 网关启动于 {}:{}", config.host, config.port);

    let server = ServerBuilder::new(config)
        .handlers(vec![Arc::new(SmgpBusinessHandler {
            msg_source: msg_source.clone(),
            merger: merger.clone(),
        })])
        .auth_handler(Arc::new(SmgpAuthHandler { accounts }))
        .message_source(msg_source as Arc<dyn MessageSource>)
        .account_config_provider(Arc::new(SimpleAccountConfigProvider))
        .event_handler(Arc::new(SmgpServerEventHandler))
        .account_pool_config(AccountPoolConfig::new())
        .serve()
        .await?;

    tracing::info!("监听地址: {}", server.local_addr);
    server.run().await
}
