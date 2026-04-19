use async_trait::async_trait;
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_codec_smgp::{
    decode_message,
    datatypes::{tlv_tags, OptionalParameters, SmgpReport, Tlv},
    Deliver, Pdu, SmgpMessage, SmgpMsgId, SubmitResp,
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
            let single_max = match alphabet {
                SmsAlphabet::ASCII | SmsAlphabet::GSM7 => 160,
                SmsAlphabet::UCS2 => 70,
                SmsAlphabet::Binary => 140,
            };

            if mo.content.len() > single_max {
                let frames = splitter.split(mo.content.as_bytes(), alphabet);
                let mut items = Vec::new();
                for frame in frames {
                    let pdu =
                        build_deliver_mo_with_udh(&mo.account, &mo.phone, &frame.content, frame.has_udhi, frame.total_segments, frame.segment_number);
                    items.push(Arc::new(pdu) as Arc<dyn EncodedPdu>);
                }
                source.push_sync(&mo.account, MessageItem::Group { items });
            } else {
                let pdu = build_deliver_mo(&mo.account, &mo.phone, &mo.content);
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
        let msg = match decode_message(frame.data_as_slice()) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(conn_id = ctx.conn.id(), "消息解码失败: {}", e);
                return Ok(());
            }
        };

        match msg {
            SmgpMessage::Submit {
                sequence_id,
                submit,
            } => {
                self.handle_submit(ctx, sequence_id, &submit).await?;
            }
            SmgpMessage::ActiveTest { .. } => {
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
        sequence_id: u32,
        submit: &rsms_codec_smgp::Submit,
    ) -> Result<()> {
        let phone = submit
            .dest_term_ids
            .first()
            .map(|s| s.as_str())
            .unwrap_or("unknown");

        let msg_id = SmgpMsgId::from_u64(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
        );

        let resp = SubmitResp {
            msg_id,
            status: 0,
        };
        let resp_pdu: Pdu = resp.into();
        ctx.conn
            .write_frame(resp_pdu.to_pdu_bytes(sequence_id).as_bytes_ref())
            .await?;

        if let Some((udh, _)) = UdhParser::extract_udh(&submit.msg_content) {
            let ref_id = udh.reference_id;
            let total = udh.total_segments;
            let seg = udh.segment_number;
            let frame = LongMessageFrame::new(
                udh.reference_id,
                udh.total_segments,
                udh.segment_number,
                submit.msg_content.clone(),
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
            let content = String::from_utf8_lossy(&submit.msg_content);
            tracing::info!(
                conn_id = ctx.conn.id(),
                phone = phone,
                content = %content,
                "收到短信提交"
            );
        }

        if submit.need_report == 1 {
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

fn build_deliver_report(account: &str, msg_id: &SmgpMsgId, phone: &str) -> RawPdu {
    let msg_id_str = format!("{}", msg_id);
    let now = chrono_now_str();

    let report = SmgpReport {
        msg_id: msg_id_str,
        sub: "001".to_string(),
        dlvrd: "001".to_string(),
        submit_time: now.clone(),
        done_time: now,
        stat: "DELIVRD".to_string(),
        err: "000".to_string(),
        txt: String::new(),
    };

    let deliver = Deliver {
        msg_id: msg_id.clone(),
        is_report: 1,
        msg_fmt: 0,
        recv_time: chrono_now_str_short(),
        src_term_id: phone.to_string(),
        dest_term_id: account.to_string(),
        msg_content: report.to_bytes(),
        reserve: [0u8; 8],
        optional_params: OptionalParameters::new(),
    };

    let pdu: Pdu = deliver.into();
    pdu.to_pdu_bytes(0)
}

fn build_deliver_mo(account: &str, phone: &str, content: &str) -> RawPdu {
    let deliver = Deliver {
        msg_id: SmgpMsgId::default(),
        is_report: 0,
        msg_fmt: 15,
        recv_time: chrono_now_str_short(),
        src_term_id: phone.to_string(),
        dest_term_id: account.to_string(),
        msg_content: content.as_bytes().to_vec(),
        reserve: [0u8; 8],
        optional_params: OptionalParameters::new(),
    };

    let pdu: Pdu = deliver.into();
    pdu.to_pdu_bytes(0)
}

fn build_deliver_mo_with_udh(account: &str, phone: &str, content_with_udh: &[u8], has_udhi: bool, pk_total: u8, pk_number: u8) -> RawPdu {
    let mut optional_params = OptionalParameters::new();
    if has_udhi {
        optional_params.add(Tlv::Byte { tag: tlv_tags::TP_UDHI, value: 1 });
        optional_params.add(Tlv::Byte { tag: tlv_tags::PK_TOTAL, value: pk_total });
        optional_params.add(Tlv::Byte { tag: tlv_tags::PK_NUMBER, value: pk_number });
    }
    let deliver = Deliver {
        msg_id: SmgpMsgId::default(),
        is_report: 0,
        msg_fmt: 15,
        recv_time: chrono_now_str_short(),
        src_term_id: phone.to_string(),
        dest_term_id: account.to_string(),
        msg_content: content_with_udh.to_vec(),
        reserve: [0u8; 8],
        optional_params,
    };

    let pdu: Pdu = deliver.into();
    pdu.to_pdu_bytes(0)
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

fn chrono_now_str_short() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let y = 1970 + (secs / 31536000);
    let month = ((secs / 86400 / 30) % 12 + 1) as u8;
    let day = ((secs / 86400) % 30 + 1) as u8;
    let h = ((secs / 3600) % 24 + 8) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{:04}{:02}{:02}{:02}{:02}{:02}", y, month, day, h, m, s)
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
            .with_protocol("smgp")
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!(
        "SMGP 网关启动于 {}:{}",
        config.host,
        config.port
    );

    let server = serve(
        config,
        vec![Arc::new(SmgpBusinessHandler {
            msg_source: msg_source.clone(),
            merger: merger.clone(),
        })],
        Some(Arc::new(SmgpAuthHandler { accounts })),
        Some(msg_source as Arc<dyn MessageSource>),
        Some(Arc::new(SimpleAccountConfigProvider)),
        Some(Arc::new(SmgpServerEventHandler)),
        Some(AccountPoolConfig::new()),
    )
    .await?;

    tracing::info!("监听地址: {}", server.local_addr);
    server.run().await
}
