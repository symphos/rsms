// ============================================================================
// SGIP 服务端完整参考实现
//
// 功能：明文认证 + 限流 + MessageSource 队列 + 长短信合包 + 独立 Report 命令
// 运行：cargo run
// 配置：accounts.conf（账号密码）、messages.conf（模拟 MO 消息）
//
// SGIP 与 CMPP 的关键差异：
//   1. 明文认证（Bind.login_name + Bind.login_password），无 MD5
//   2. 20 字节 Header（12 字节 SgipSequence: node_id + timestamp + number）
//   3. 独立 Report 命令（不通过 Deliver 承载）
//   4. to_pdu_bytes(node_id, timestamp, number) 三个参数
//   5. SubmitResp 只有 result 字段，没有 msg_id
//
// 核心流程：
//   1. 从 accounts.conf 读取账号配置
//   2. 客户端连接 → 框架自动完成 SGIP 协议握手（Bind/BindResp）
//   3. 客户端发送 Submit → BusinessHandler.on_inbound() 收到
//   4. 业务方解码 Submit、回 SubmitResp、处理业务（含长短信合包）
//   5. 通过 MessageSource 异步发送 Report（状态报告）和 Deliver（MO 上行）
// ============================================================================

use async_trait::async_trait;
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_codec_sgip::{
    decode_message, Deliver, Encodable, Report, SgipMessage, SgipSequence, SubmitResp,
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
// 从原始 PDU 字节中提取 SgipSequence（bytes 8-19）
// ============================================================================

fn extract_sgip_sequence(data: &[u8]) -> Option<SgipSequence> {
    if data.len() < 20 {
        return None;
    }
    let node_id = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let timestamp = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
    let number = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    Some(SgipSequence::new(node_id, timestamp, number))
}

// ============================================================================
// AuthHandler：SGIP 明文认证（无 MD5）
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

struct SgipBusinessHandler {
    msg_source: Arc<FileMessageSource>,
    merger: Arc<std::sync::Mutex<LongMessageMerger>>,
    report_seq: std::sync::atomic::AtomicU32,
}

#[async_trait]
impl BusinessHandler for SgipBusinessHandler {
    fn name(&self) -> &'static str {
        "sgip-business"
    }

    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        let data = frame.data_as_slice();
        let msg = match decode_message(data) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(conn_id = ctx.conn.id(), "消息解码失败: {}", e);
                return Ok(());
            }
        };

        match msg {
            SgipMessage::Submit(submit) => {
                self.handle_submit(ctx, data, &submit).await?;
            }
            SgipMessage::Report(report) => {
                tracing::info!(
                    conn_id = ctx.conn.id(),
                    state = report.state,
                    error_code = report.error_code,
                    "收到 Report"
                );

                let seq = match extract_sgip_sequence(data) {
                    Some(s) => s,
                    None => return Ok(()),
                };
                let resp = rsms_codec_sgip::ReportResp { result: 0 };
                ctx.conn
                    .write_frame(&resp.to_pdu_bytes(seq.node_id, seq.timestamp, seq.number))
                    .await?;
            }
            SgipMessage::Deliver(deliver) => {
                let content = String::from_utf8_lossy(&deliver.message_content);
                tracing::info!(
                    conn_id = ctx.conn.id(),
                    user_number = %deliver.user_number,
                    content = %content,
                    "收到 Deliver（MO 上行）"
                );

                let seq = match extract_sgip_sequence(data) {
                    Some(s) => s,
                    None => return Ok(()),
                };
                let resp = rsms_codec_sgip::DeliverResp { result: 0 };
                ctx.conn
                    .write_frame(&resp.to_pdu_bytes(seq.node_id, seq.timestamp, seq.number))
                    .await?;
            }
            SgipMessage::Unbind(_) => {
                tracing::debug!(conn_id = ctx.conn.id(), "收到 Unbind");
            }
            _ => {}
        }
        Ok(())
    }
}

impl SgipBusinessHandler {
    async fn handle_submit(
        &self,
        ctx: &InboundContext,
        data: &[u8],
        submit: &rsms_codec_sgip::Submit,
    ) -> Result<()> {
        let phone = submit
            .user_numbers
            .first()
            .map(|s| s.as_str())
            .unwrap_or("unknown");

        let seq = match extract_sgip_sequence(data) {
            Some(s) => s,
            None => return Ok(()),
        };

        let resp = SubmitResp { result: 0 };
        ctx.conn
            .write_frame(&resp.to_pdu_bytes(seq.node_id, seq.timestamp, seq.number))
            .await?;

        if submit.tpudhi == 1 {
            if let Some((udh, _)) = UdhParser::extract_udh(&submit.message_content) {
                let seg_num = udh.segment_number;
                let seg_total = udh.total_segments;
                let frame = LongMessageFrame::new(
                    udh.reference_id,
                    seg_total,
                    seg_num,
                    submit.message_content.clone(),
                    true,
                    Some(udh),
                );
                let mut merger = self.merger.lock().unwrap();
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
                            seg = seg_num,
                            total = seg_total,
                            "长短信分段接收"
                        );
                    }
                    Err(e) => tracing::warn!(conn_id = ctx.conn.id(), "长短信合包错误: {}", e),
                }
            }
        } else {
            let content = String::from_utf8_lossy(&submit.message_content);
            tracing::info!(
                conn_id = ctx.conn.id(),
                sp_number = %submit.sp_number,
                phone = phone,
                content = %content,
                "收到短信提交"
            );
        }

        if submit.report_flag == 1 {
            if let Some(account) = ctx.conn.authenticated_account().await {
                let report = build_report(&account, &seq, phone, &self.report_seq);
                self.msg_source.push(&account, report).await;
            }
        }

        Ok(())
    }
}

// ============================================================================
// 辅助函数：构建 Report / Deliver PDU
// ============================================================================

fn build_report(
    _account: &str,
    submit_seq: &SgipSequence,
    phone: &str,
    report_seq_counter: &std::sync::atomic::AtomicU32,
) -> RawPdu {
    let report = Report {
        submit_sequence: *submit_seq,
        report_type: 0,
        user_number: phone.to_string(),
        state: 0,
        error_code: 0,
        reserve: [0u8; 8],
    };

    let number = report_seq_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let now_ts = sgip_timestamp();
    report
        .to_pdu_bytes(submit_seq.node_id, now_ts, number)
        .to_vec()
        .into()
}

fn build_deliver_mo(account: &str, phone: &str, content: &str) -> RawPdu {
    let deliver = Deliver {
        user_number: phone.to_string(),
        sp_number: account.to_string(),
        tppid: 0,
        tpudhi: 0,
        msg_fmt: 15,
        message_content: content.as_bytes().to_vec(),
        reserve: [0u8; 8],
    };

    let now_ts = sgip_timestamp();
    deliver.to_pdu_bytes(1, now_ts, 0).to_vec().into()
}

fn build_deliver_mo_with_udh(account: &str, phone: &str, content: &[u8], has_udhi: bool) -> RawPdu {
    let deliver = Deliver {
        user_number: phone.to_string(),
        sp_number: account.to_string(),
        tppid: 0,
        tpudhi: if has_udhi { 1 } else { 0 },
        msg_fmt: 15,
        message_content: content.to_vec(),
        reserve: [0u8; 8],
    };

    let now_ts = sgip_timestamp();
    deliver.to_pdu_bytes(1, now_ts, 0).to_vec().into()
}

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
            .with_protocol("sgip")
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!(
        "SGIP 网关启动于 {}:{}",
        config.host,
        config.port
    );

    let server = serve(
        config,
        vec![Arc::new(SgipBusinessHandler {
            msg_source: msg_source.clone(),
            merger: Arc::new(std::sync::Mutex::new(LongMessageMerger::new())),
            report_seq: std::sync::atomic::AtomicU32::new(1),
        })],
        Some(Arc::new(SgipAuthHandler { accounts })),
        Some(msg_source as Arc<dyn MessageSource>),
        Some(Arc::new(SimpleAccountConfigProvider)),
        Some(Arc::new(SgipServerEventHandler)),
        Some(AccountPoolConfig::new()),
    )
    .await?;

    tracing::info!("监听地址: {}", server.local_addr);
    server.run().await
}
