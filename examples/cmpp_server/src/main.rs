// ============================================================================
// CMPP 服务端完整参考实现
//
// 功能：认证 + 限流 + MessageSource 队列 + 长短信合包 + 错误处理
// 运行：cargo run
// 配置：accounts.conf（账号密码）、messages.conf（模拟 MO 消息）
//
// 核心流程：
//   1. 从 accounts.conf 读取账号配置
//   2. 客户端连接 → 框架自动完成 CMPP 协议握手（Connect/ConnectResp）
//   3. 客户端发送 Submit → BusinessHandler.on_inbound() 收到
//   4. 业务方解码 Submit、回 SubmitResp、处理业务（含长短信合包）
//   5. 通过 MessageSource 异步发送 Deliver（MO/Report）
// ============================================================================

use async_trait::async_trait;
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_codec_cmpp::{
    compute_connect_auth, decode_message,
    CmppMessage, Deliver, Pdu, SubmitResp,
};
use rsms_connector::{
    serve, AccountConfig, AccountConfigProvider, AccountPoolConfig, AuthCredentials,
    AuthHandler, AuthResult, MessageItem, MessageSource, ProtocolConnection, ServerEventHandler,
};
use rsms_core::{ConnectionInfo, EncodedPdu, EndpointConfig, Frame, RawPdu, Result};
use rsms_longmsg::split::SmsAlphabet;
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser};
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

// ============================================================================
// AuthHandler：MD5 认证
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
            let content_bytes = mo.content.as_bytes();
            let alphabet = detect_alphabet(content_bytes);
            let single_max = match alphabet {
                SmsAlphabet::GSM7 | SmsAlphabet::ASCII => 160,
                _ => 70,
            };

            if content_bytes.len() > single_max {
                let frames = splitter.split(content_bytes, alphabet);
                let mut items = Vec::new();
                for frame in frames {
                    let pdu = build_deliver_mo_with_udh(&mo.account, &mo.phone, &frame.content, frame.has_udhi);
                    items.push(Arc::new(pdu) as Arc<dyn EncodedPdu>);
                }
                source.push_group_sync(&mo.account, MessageItem::Group { items });
            } else {
                let pdu = build_deliver_mo(&mo.account, &mo.phone, &mo.content);
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
        let msg = match decode_message(frame.data_as_slice()) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(conn_id = ctx.conn.id(), "消息解码失败: {}", e);
                return Ok(());
            }
        };

        match msg {
            CmppMessage::SubmitV20 { sequence_id, submit } => {
                self.handle_submit(ctx, sequence_id, &submit.msg_id, &submit.dest_terminal_ids, &submit.msg_content, submit.registered_delivery, submit.tpudhi).await?;
            }
            CmppMessage::SubmitV30 { sequence_id, submit } => {
                self.handle_submit(ctx, sequence_id, &submit.msg_id, &submit.dest_terminal_ids, &submit.msg_content, submit.registered_delivery, submit.tpudhi).await?;
            }
            CmppMessage::ActiveTest { .. } => {
                tracing::debug!(conn_id = ctx.conn.id(), "收到 ActiveTest");
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
        sequence_id: u32,
        msg_id: &[u8; 8],
        dest_terminal_ids: &[String],
        msg_content: &[u8],
        registered_delivery: u8,
        tpudhi: u8,
    ) -> Result<()> {
        let phone = dest_terminal_ids
            .first()
            .map(|s| s.as_str())
            .unwrap_or("unknown");

        // 回 SubmitResp（框架不自动回，业务方自己回）
        let resp = SubmitResp {
            msg_id: *msg_id,
            result: 0,
        };
        let resp_pdu: Pdu = resp.into();
        ctx.conn
            .write_frame(resp_pdu.to_pdu_bytes(sequence_id).as_bytes_ref())
            .await?;

        // 处理长短信合包
        if tpudhi == 1 {
            if let Some((udh, _)) = UdhParser::extract_udh(msg_content) {
                let frame = LongMessageFrame::new(
                    udh.reference_id,
                    udh.total_segments,
                    udh.segment_number,
                    msg_content.to_vec(),
                    true,
                    Some(udh),
                );
                let mut merger = self.merger.lock().unwrap();
                let seg = frame.segment_number;
                let total = frame.total_segments;
                match merger.add_frame(frame) {
                    Ok(Some(complete)) => {
                        let content = String::from_utf8_lossy(&complete);
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
                            phone = phone,
                            seg = seg,
                            total = total,
                            "长短信分段接收"
                        );
                    }
                    Err(e) => tracing::warn!(conn_id = ctx.conn.id(), "长短信合包错误: {}", e),
                }
            }
        } else {
            let content = String::from_utf8_lossy(msg_content);
            tracing::info!(
                conn_id = ctx.conn.id(),
                phone = phone,
                content = %content,
                "收到短信提交"
            );
        }

        // 需要状态报告 → 通过 MessageSource 异步发送
        if registered_delivery == 1 {
            if let Some(account) = ctx.conn.authenticated_account().await {
                let report = build_deliver_report(&account, msg_id, phone);
                self.msg_source.push(&account, report).await;
            }
        }

        Ok(())
    }
}

// ============================================================================
// 辅助函数：构建 Deliver PDU
// ============================================================================

fn build_deliver_report(account: &str, msg_id: &[u8; 8], phone: &str) -> RawPdu {
    let msg_id_hex: String = msg_id.iter().map(|b| format!("{:02x}", b)).collect();
    let now = chrono_now_str();

    let report_content = format!(
        "MsgId:{} Stat:DELIVRD SubmitTime:{} DoneTime:{} DestTerminalId:{} SMSCSequence:0",
        msg_id_hex, now, now, phone
    );

    let deliver = Deliver {
        msg_id: msg_id.clone(),
        dest_id: account.to_string(),
        service_id: "SMS".to_string(),
        tppid: 0,
        tpudhi: 0,
        msg_fmt: 15,
        src_terminal_id: phone.to_string(),
        src_terminal_type: 0,
        registered_delivery: 1,
        msg_content: report_content.into_bytes(),
        link_id: String::new(),
    };

    let pdu: Pdu = deliver.into();
    pdu.to_pdu_bytes(0)
}

fn build_deliver_mo(account: &str, phone: &str, content: &str) -> RawPdu {
    let deliver = Deliver {
        msg_id: 0u64.to_be_bytes(),
        dest_id: account.to_string(),
        service_id: "SMS".to_string(),
        tppid: 0,
        tpudhi: 0,
        msg_fmt: 15,
        src_terminal_id: phone.to_string(),
        src_terminal_type: 0,
        registered_delivery: 0,
        msg_content: content.as_bytes().to_vec(),
        link_id: String::new(),
    };

    let pdu: Pdu = deliver.into();
    pdu.to_pdu_bytes(0)
}

fn build_deliver_mo_with_udh(account: &str, phone: &str, content: &[u8], has_udhi: bool) -> RawPdu {
    let deliver = Deliver {
        msg_id: 0u64.to_be_bytes(),
        dest_id: account.to_string(),
        service_id: "SMS".to_string(),
        tppid: 0,
        tpudhi: if has_udhi { 1 } else { 0 },
        msg_fmt: 15,
        src_terminal_id: phone.to_string(),
        src_terminal_type: 0,
        registered_delivery: 0,
        msg_content: content.to_vec(),
        link_id: String::new(),
    };

    let pdu: Pdu = deliver.into();
    pdu.to_pdu_bytes(0)
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
    let msg_source = Arc::new(FileMessageSource::load_from_file(&messages_path.to_string_lossy()));

    let config = Arc::new(
        EndpointConfig::new("cmpp-gateway", "0.0.0.0", 7890, 500, 60)
            .with_protocol("cmpp")
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!(
        "CMPP 网关启动于 {}:{}",
        config.host,
        config.port
    );

    let server = serve(
        config,
        vec![Arc::new(CmppBusinessHandler {
            msg_source: msg_source.clone(),
            merger: Arc::new(std::sync::Mutex::new(LongMessageMerger::new())),
        })],
        Some(Arc::new(CmppAuthHandler { accounts })),
        Some(msg_source as Arc<dyn MessageSource>),
        Some(Arc::new(SimpleAccountConfigProvider)),
        Some(Arc::new(CmppServerEventHandler)),
        Some(AccountPoolConfig::new()),
    )
    .await?;

    tracing::info!("监听地址: {}", server.local_addr);
    server.run().await
}
