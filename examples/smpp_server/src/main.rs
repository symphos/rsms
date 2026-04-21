// ============================================================================
// SMPP 服务端完整参考实现
//
// 功能：认证 + 限流 + MessageSource 队列 + 错误处理 + 长短信合包/拆分
// 运行：cargo run
// 配置：accounts.conf（账号密码）、messages.conf（模拟 MO 消息）
//
// 核心流程：
//   1. 从 accounts.conf 读取账号配置
//   2. 客户端连接 → 框架自动完成 SMPP 协议握手（BindTransmitter/Resp）
//   3. 客户端发送 SubmitSm → BusinessHandler.on_inbound() 收到
//   4. 业务方解码 SubmitSm、回 SubmitSmResp、处理业务
//   5. 通过 MessageSource 异步发送 DeliverSm（MO/Report）
//
// SMPP 与 CMPP 的关键差异：
//   - 认证是明文（无 MD5）：AuthCredentials::Smpp { system_id, password, interface_version }
//   - 16 字节 Header（多了 4 字节 command_status）
//   - Report 通过 DeliverSm(esm_class=0x04) 承载
//   - MsgId 是 String 类型（非 [u8; 8]）
//   - decode_message_with_version 需要版本参数
//   - 长短信用 esm_class bit 6 (0x40) 表示 TP-UDHI（无独立 tpudhi 字段）
// ============================================================================

use async_trait::async_trait;
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_codec_smpp::{
    decode_message_with_version, DeliverSm, DeliverSmResp, Pdu, SmppMessage, SmppVersion,
    SubmitSmResp, Tlv,
};
use rsms_connector::{
    serve, AccountConfig, AccountConfigProvider, AccountPoolConfig, AuthCredentials,
    AuthHandler, AuthResult, MessageItem, MessageSource, ProtocolConnection, ServerEventHandler,
};
use rsms_core::{ConnectionInfo, EncodedPdu, EndpointConfig, Frame, RawPdu, Result};
use rsms_longmsg::{
    LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser,
    split::SmsAlphabet,
};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

use rsms_codec_smpp::datatypes::tags;

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

// ============================================================================
// AuthHandler：SMPP 明文认证
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
//   - 超过单条限制的 MO 消息自动拆分为多个 DeliverSm
//   - 每个分段 esm_class |= 0x40（TP-UDHI）
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
            let frames = splitter.split(mo.content.as_bytes(), alphabet);

            if frames.len() == 1 && !frames[0].has_udhi {
                let pdu = build_deliver_sm_mo(&mo.account, &mo.phone, &mo.content, 0);
                source.push_sync(&mo.account, pdu);
            } else {
                let items: Vec<Arc<dyn EncodedPdu>> = frames
                    .into_iter()
                    .map(|frame| {
                        let pdu =
                            build_deliver_sm_mo_raw(&mo.account, &mo.phone, 0x40, &frame.content);
                        Arc::new(pdu) as Arc<dyn EncodedPdu>
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
// BusinessHandler：处理 SubmitSm，回 SubmitSmResp，推送 Report
//
// 长短信 MT 合包：
//   - 检查 esm_class & 0x40 判断是否有 UDH
//   - 有 UDH 的 SubmitSm 用 merger 合包
//   - 合包完成后处理完整消息
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
        let msg = match decode_message_with_version(frame.data_as_slice(), Some(SmppVersion::V34))
        {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(conn_id = ctx.conn.id(), "消息解码失败: {}", e);
                return Ok(());
            }
        };

        match msg {
            SmppMessage::SubmitSm(ref s) => {
                self.handle_submit(ctx, frame.sequence_id, s).await?;
            }
            SmppMessage::DeliverSm(ref _d) => {
                let resp = DeliverSmResp {
                    message_id: String::new(),
                };
                let resp_pdu: Pdu = resp.into();
                ctx.conn
                    .write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_bytes_ref())
                    .await?;
            }
            SmppMessage::EnquireLink { .. } => {
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
        sequence_id: u32,
        submit: &rsms_codec_smpp::SubmitSm,
    ) -> Result<()> {
        let phone = &submit.destination_addr;
        let source = &submit.source_addr;

        let msg_id = ctx.id_generator
            .as_ref()
            .map(|g| g.next_msg_id().to_string())
            .unwrap_or_else(|| format!("{:010}", sequence_id));
        let resp = SubmitSmResp {
            message_id: msg_id.clone(),
        };
        let resp_pdu: Pdu = resp.into();
        ctx.conn
            .write_frame(resp_pdu.to_pdu_bytes(sequence_id).as_bytes_ref())
            .await?;

        if submit.esm_class & 0x40 != 0 {
            if let Some((udh, _)) = UdhParser::extract_udh(&submit.short_message) {
                let seg_num = udh.segment_number;
                let total = udh.total_segments;
                let frame = LongMessageFrame::new(
                    udh.reference_id,
                    total,
                    seg_num,
                    submit.short_message.clone(),
                    true,
                    Some(udh),
                );
                let mut merger = self.merger.lock().unwrap();
                match merger.add_frame(frame) {
                    Ok(Some(merged)) => {
                        let content = String::from_utf8_lossy(&merged);
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
            }
        } else {
            let content = String::from_utf8_lossy(&submit.short_message);
            tracing::info!(
                conn_id = ctx.conn.id(),
                source = %source,
                phone = %phone,
                content = %content,
                "收到短信提交"
            );
        }

        if submit.registered_delivery == 1 {
            if let Some(account) = ctx.conn.authenticated_account().await {
                let report =
                    build_deliver_sm_report(&account, &msg_id, phone);
                self.msg_source.push(&account, report).await;
            }
        }

        Ok(())
    }
}

// ============================================================================
// 辅助函数：构建 DeliverSm PDU
// ============================================================================

fn build_deliver_sm_report(account: &str, msg_id: &str, phone: &str) -> RawPdu {
    let deliver = DeliverSm {
        service_type: String::new(),
        source_addr_ton: 0,
        source_addr_npi: 0,
        source_addr: phone.to_string(),
        dest_addr_ton: 0,
        dest_addr_npi: 0,
        destination_addr: account.to_string(),
        esm_class: 0x04,
        protocol_id: 0,
        priority_flag: 0,
        schedule_delivery_time: String::new(),
        validity_period: String::new(),
        registered_delivery: 0,
        replace_if_present_flag: 0,
        data_coding: 0,
        sm_default_msg_id: 0,
        short_message: format!(
            "id:{} sub:001 dlvrd:001 submit date:done date:stat:DELIVRD err:000",
            msg_id
        )
        .into_bytes(),
        tlvs: vec![
            Tlv::new(tags::RECEIPTED_MESSAGE_ID, msg_id.as_bytes().to_vec()),
            Tlv::new(tags::MESSAGE_STATE, vec![1]),
        ],
    };
    let pdu: Pdu = deliver.into();
    pdu.to_pdu_bytes(0)
}

fn build_deliver_sm_mo(account: &str, phone: &str, content: &str, esm_class: u8) -> RawPdu {
    let deliver = DeliverSm {
        service_type: String::new(),
        source_addr_ton: 0,
        source_addr_npi: 0,
        source_addr: phone.to_string(),
        dest_addr_ton: 0,
        dest_addr_npi: 0,
        destination_addr: account.to_string(),
        esm_class,
        protocol_id: 0,
        priority_flag: 0,
        schedule_delivery_time: String::new(),
        validity_period: String::new(),
        registered_delivery: 0,
        replace_if_present_flag: 0,
        data_coding: 0,
        sm_default_msg_id: 0,
        short_message: content.as_bytes().to_vec(),
        tlvs: vec![],
    };
    let pdu: Pdu = deliver.into();
    pdu.to_pdu_bytes(0)
}

fn build_deliver_sm_mo_raw(account: &str, phone: &str, esm_class: u8, short_message: &[u8]) -> RawPdu {
    let deliver = DeliverSm {
        service_type: String::new(),
        source_addr_ton: 0,
        source_addr_npi: 0,
        source_addr: phone.to_string(),
        dest_addr_ton: 0,
        dest_addr_npi: 0,
        destination_addr: account.to_string(),
        esm_class,
        protocol_id: 0,
        priority_flag: 0,
        schedule_delivery_time: String::new(),
        validity_period: String::new(),
        registered_delivery: 0,
        replace_if_present_flag: 0,
        data_coding: 0,
        sm_default_msg_id: 0,
        short_message: short_message.to_vec(),
        tlvs: vec![],
    };
    let pdu: Pdu = deliver.into();
    pdu.to_pdu_bytes(0)
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
            .with_protocol("smpp")
            .with_log_level(tracing::Level::INFO),
    );

    tracing::info!(
        "SMPP 网关启动于 {}:{}",
        config.host,
        config.port
    );

    let merger = Arc::new(std::sync::Mutex::new(LongMessageMerger::new()));

    let server = serve(
        config,
        vec![Arc::new(SmppBusinessHandler {
            msg_source: msg_source.clone(),
            merger: merger.clone(),
        })],
        Some(Arc::new(SmppAuthHandler { accounts })),
        Some(msg_source as Arc<dyn MessageSource>),
        Some(Arc::new(SimpleAccountConfigProvider)),
        Some(Arc::new(SmppServerEventHandler)),
        Some(AccountPoolConfig::new()),
    )
    .await?;

    tracing::info!("监听地址: {}", server.local_addr);
    server.run().await
}
