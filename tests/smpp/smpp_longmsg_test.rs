use async_trait::async_trait;
use rsms_business::{BusinessHandler, InboundContext};
use rsms_codec_smpp::{
    decode_message, BindTransmitter, CommandId, DeliverSm, DeliverSmResp, Encodable, Pdu,
    SmppMessage, SmppVersion, SubmitSm, SubmitSmResp,
};
use rsms_connector::client::{ClientConfig, ClientContext, ClientHandler, ClientConnection};
use rsms_connector::{
    connect, serve, AccountConfig, AccountConfigProvider, AuthCredentials, AuthHandler, AuthResult,
    SmppDecoder,
};
use rsms_core::{ConnectionInfo, EndpointConfig, Frame, RawPdu, Result};
use rsms_longmsg::split::SmsAlphabet;
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::time::Duration;

const TEST_SYSTEM_ID: &str = "smppcli";
const TEST_PASSWORD: &str = "pwd12345";

struct PasswordAuthHandler {
    accounts: HashMap<String, String>,
}

impl PasswordAuthHandler {
    fn new() -> Self {
        Self {
            accounts: HashMap::new(),
        }
    }

    fn add_account(mut self, system_id: &str, password: &str) -> Self {
        self.accounts
            .insert(system_id.to_string(), password.to_string());
        self
    }
}

#[async_trait]
impl AuthHandler for PasswordAuthHandler {
    fn name(&self) -> &'static str {
        "smpp-longmsg-auth"
    }

    async fn authenticate(
        &self,
        _client_id: &str,
        credentials: AuthCredentials,
        _conn_info: &ConnectionInfo,
    ) -> Result<AuthResult> {
        if let AuthCredentials::Smpp {
            system_id,
            password,
            interface_version: _,
        } = credentials
        {
            if let Some(expected) = self.accounts.get(&system_id) {
                if &password == expected {
                    return Ok(AuthResult::success(&system_id));
                }
            }
            Ok(AuthResult::failure(1, "认证失败"))
        } else {
            Ok(AuthResult::failure(1, "无效凭证"))
        }
    }
}

struct MockAccountConfigProvider;

#[async_trait]
impl AccountConfigProvider for MockAccountConfigProvider {
    async fn get_config(&self, _account: &str) -> Result<AccountConfig> {
        Ok(AccountConfig::new().with_max_qps(10000))
    }
}

struct LongMsgBizHandler {
    received_segments: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl LongMsgBizHandler {
    fn new() -> Self {
        Self {
            received_segments: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl BusinessHandler for LongMsgBizHandler {
    fn name(&self) -> &'static str {
        "smpp-longmsg-biz"
    }

    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        if let Ok(msg) = decode_message(frame.data_as_slice()) {
            match msg {
                SmppMessage::SubmitSm(s) => {
                    self.received_segments
                        .lock()
                        .unwrap()
                        .push(s.short_message.clone());

                    let resp = SubmitSmResp {
                        message_id: String::new(),
                    };
                    let mut buf = bytes::BytesMut::new();
                    resp.encode(&mut buf).unwrap();
                    let body_len = buf.len() as u32;
                    let total_len = 16 + body_len;
                    let mut resp_pdu = Vec::new();
                    resp_pdu.extend_from_slice(&total_len.to_be_bytes());
                    resp_pdu.extend_from_slice(
                        &(CommandId::SUBMIT_SM_RESP as u32).to_be_bytes(),
                    );
                    resp_pdu.extend_from_slice(&0u32.to_be_bytes());
                    resp_pdu.extend_from_slice(&frame.sequence_id.to_be_bytes());
                    resp_pdu.extend_from_slice(&buf);
                    ctx.conn.write_frame(resp_pdu.as_slice()).await?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

struct LongMsgClientHandler {
    smpp_version: u8,
    connected: Arc<AtomicBool>,
    submit_resp_count: Arc<AtomicUsize>,
    deliver_segments: Arc<Mutex<Vec<Vec<u8>>>>,
    seq: AtomicUsize,
}

impl LongMsgClientHandler {
    fn new(smpp_version: u8) -> Self {
        Self {
            smpp_version,
            connected: Arc::new(AtomicBool::new(false)),
            submit_resp_count: Arc::new(AtomicUsize::new(0)),
            deliver_segments: Arc::new(Mutex::new(Vec::new())),
            seq: AtomicUsize::new(1),
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u32
    }

    fn build_bind_pdu(&self) -> RawPdu {
        let bind = BindTransmitter::new(TEST_SYSTEM_ID, TEST_PASSWORD, "CMT", self.smpp_version);
        let pdu: Pdu = bind.into();
        pdu.to_pdu_bytes(self.next_seq())
    }

    fn build_long_submit_pdus(&self, content: &[u8], data_coding: u8) -> Vec<RawPdu> {
        let alphabet = match data_coding {
            8 => SmsAlphabet::UCS2,
            _ => SmsAlphabet::ASCII,
        };
        let mut splitter = LongMessageSplitter::new();
        let frames = splitter.split(content, alphabet);

        frames
            .iter()
            .map(|frame| {
                let mut submit = SubmitSm::new();
                submit.source_addr = "106900".to_string();
                submit.dest_addr_ton = 1;
                submit.dest_addr_npi = 1;
                submit.destination_addr = "13800138000".to_string();
                submit.data_coding = data_coding;
                submit.short_message = frame.content.clone();
                submit.esm_class = if frame.has_udhi { 0x40 } else { 0 };
                submit.registered_delivery = 0;
                let pdu: Pdu = submit.into();
                pdu.to_pdu_bytes(self.next_seq())
            })
            .collect()
    }
}

#[async_trait]
impl ClientHandler for LongMsgClientHandler {
    fn name(&self) -> &'static str {
        "smpp-longmsg-client"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        if pdu.len() < 16 {
            return Ok(());
        }

        let cmd_id = frame.command_id;
        let status = u32::from_be_bytes([pdu[8], pdu[9], pdu[10], pdu[11]]);

        if cmd_id == CommandId::BIND_TRANSMITTER_RESP as u32 {
            if status == 0 {
                self.connected.store(true, Ordering::Relaxed);
            }
        } else if cmd_id == CommandId::SUBMIT_SM_RESP as u32 {
            if status == 0 {
                self.submit_resp_count.fetch_add(1, Ordering::Relaxed);
            }
        } else if cmd_id == CommandId::DELIVER_SM as u32 {
            if let Ok(msg) = decode_message(pdu) {
                if let SmppMessage::DeliverSm(d) = msg {
                    self.deliver_segments
                        .lock()
                        .unwrap()
                        .push(d.short_message.clone());
                }
            }
            let seq_id = u32::from_be_bytes([pdu[12], pdu[13], pdu[14], pdu[15]]);
            let resp = DeliverSmResp {
                message_id: String::new(),
            };
            let mut buf = bytes::BytesMut::new();
            resp.encode(&mut buf).unwrap();
            let body_len = buf.len() as u32;
            let total_len = 16 + body_len;
            let mut resp_pdu = Vec::new();
            resp_pdu.extend_from_slice(&total_len.to_be_bytes());
            resp_pdu.extend_from_slice(&(CommandId::DELIVER_SM_RESP as u32).to_be_bytes());
            resp_pdu.extend_from_slice(&0u32.to_be_bytes());
            resp_pdu.extend_from_slice(&seq_id.to_be_bytes());
            resp_pdu.extend_from_slice(&buf);
            ctx.conn.write_frame(resp_pdu.as_slice()).await?;
        }
        Ok(())
    }
}

fn build_deliver_mo_pdu(
    seq: u32,
    src: &str,
    dest: &str,
    data_coding: u8,
    esm_class: u8,
    short_message: Vec<u8>,
) -> RawPdu {
    let deliver = DeliverSm {
        service_type: String::new(),
        source_addr_ton: 1,
        source_addr_npi: 1,
        source_addr: src.to_string(),
        dest_addr_ton: 0,
        dest_addr_npi: 0,
        destination_addr: dest.to_string(),
        esm_class,
        protocol_id: 0,
        priority_flag: 0,
        schedule_delivery_time: String::new(),
        validity_period: String::new(),
        registered_delivery: 0,
        replace_if_present_flag: 0,
        data_coding,
        sm_default_msg_id: 0,
        short_message,
        tlvs: Vec::new(),
    };
    let pdu: Pdu = deliver.into();
    pdu.to_pdu_bytes(seq)
}

fn merge_segments(segments: &[Vec<u8>]) -> Vec<u8> {
    let mut merger = LongMessageMerger::new();
    let mut result = None;
    for seg in segments {
        let has_udhi = UdhParser::has_udhi(seg);
        let (ref_id, total, number) = if has_udhi {
            UdhParser::extract_udh(seg)
                .map(|(h, _)| (h.reference_id, h.total_segments, h.segment_number))
                .unwrap_or((0, 1, 1))
        } else {
            (0, 1, 1)
        };
        let frame = LongMessageFrame::new(ref_id, total, number, seg.clone(), has_udhi, None);
        if let Ok(Some(merged)) = merger.add_frame(frame) {
            result = Some(merged);
        }
    }
    result.unwrap_or_default()
}

async fn start_server(
    biz_handler: Arc<dyn BusinessHandler>,
) -> Result<(
    u16,
    Arc<rsms_connector::ConnectionPool>,
    tokio::task::JoinHandle<()>,
)> {
    let cfg = Arc::new(
        EndpointConfig::new("smpp-longmsg-server", "127.0.0.1", 0, 8, 30)
            .with_protocol("smpp"),
    );
    let auth = Arc::new(PasswordAuthHandler::new().add_account(TEST_SYSTEM_ID, TEST_PASSWORD));
    let server = serve(
        cfg,
        vec![biz_handler],
        Some(auth),
        None,
        Some(Arc::new(MockAccountConfigProvider) as Arc<dyn AccountConfigProvider>),
        None,
        None,
    )
    .await
    .expect("bind");
    let port = server.local_addr.port();
    let pool = server.pool();
    let handle = tokio::spawn(async move {
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok((port, pool, handle))
}

async fn connect_client(
    port: u16,
    smpp_version: u8,
) -> Result<(Arc<LongMsgClientHandler>, Arc<ClientConnection>)> {
    let endpoint = Arc::new(
        EndpointConfig::new("smpp-longmsg-client", "127.0.0.1", port, 8, 30)
            .with_protocol("smpp"),
    );
    let handler = Arc::new(LongMsgClientHandler::new(smpp_version));
    let conn = connect(
        endpoint,
        handler.clone(),
        SmppDecoder,
        Some(ClientConfig::new()),
        None,
        None,
    )
    .await?;

    let bind_pdu = handler.build_bind_pdu();
    conn.write_frame(bind_pdu.as_slice()).await?;

    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if handler.connected.load(Ordering::Relaxed) {
            break;
        }
    }
    assert!(
        handler.connected.load(Ordering::Relaxed),
        "SMPP V{:02X} BindTransmitter 认证失败",
        smpp_version
    );
    Ok((handler, conn))
}

fn version_tag(v: u8) -> &'static str {
    if v == 0x50 { "V5.0" } else { "V3.4" }
}

// ==================== 纯逻辑测试 ====================

#[tokio::test]
async fn test_longmsg_mt_split_and_merge() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let original = "这是一条非常长的测试短信，用于验证SMPP协议下长短信拆分和合包功能是否正常工作。长短信需要被拆分成多个分段，每个分段带有UDH头部信息，接收方需要将这些分段重新合并为完整的原始消息。This is a very long test message to verify SMPP long SMS split and merge functionality."
        .as_bytes()
        .to_vec();

    let mut splitter = LongMessageSplitter::new();
    let frames = splitter.split(&original, SmsAlphabet::UCS2);

    assert!(frames.len() > 1, "长短信应该被拆分为多个分段");

    let mut merger = LongMessageMerger::new();
    for frame in &frames {
        let result = merger.add_frame(frame.clone()).expect("add_frame 失败");
        if frame.segment_number == frame.total_segments {
            assert!(result.is_some(), "最后一个分段后应该得到完整消息");
            assert_eq!(result.unwrap(), original, "合包后的消息应与原始消息一致");
        } else {
            assert!(result.is_none(), "非最后一个分段不应返回完整消息");
        }
    }
}

#[tokio::test]
async fn test_longmsg_single_segment_no_udh() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let content = "短消息".as_bytes().to_vec();
    let mut splitter = LongMessageSplitter::new();
    let frames = splitter.split(&content, SmsAlphabet::UCS2);

    assert_eq!(frames.len(), 1);
    assert!(!frames[0].has_udhi);
    assert_eq!(frames[0].content, content);
}

#[tokio::test]
async fn test_longmsg_ascii_split() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let content = "A".repeat(200);
    let mut splitter = LongMessageSplitter::new();
    let frames = splitter.split(content.as_bytes(), SmsAlphabet::ASCII);

    assert!(frames.len() > 1);

    let mut merger = LongMessageMerger::new();
    for frame in &frames {
        merger.add_frame(frame.clone()).expect("add_frame 失败");
    }
    let result = merger.add_frame(frames.last().unwrap().clone()).expect("重复 add_frame");
    assert!(result.is_none(), "重复分段应返回None");

    let mut merger2 = LongMessageMerger::new();
    let mut final_result = None;
    for frame in &frames {
        if let Ok(Some(merged)) = merger2.add_frame(frame.clone()) {
            final_result = Some(merged);
        }
    }
    assert_eq!(final_result.unwrap(), content.as_bytes());
}

// ==================== SMPP V3.4 测试 ====================

#[tokio::test]
async fn test_longmsg_v34_mt_submit_sm() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new());
    let (port, _pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port, 0x34).await.unwrap();

    let original = "这是一条SMPP V3.4长短信测试消息，内容足够长以触发拆分。包含中文字符和English characters混合内容，确保UCS2编码下拆分合包正确。验证SubmitSm的esm_class=0x40和UDH头部。"
        .as_bytes()
        .to_vec();

    let submit_pdus = client.build_long_submit_pdus(&original, 8);
    assert!(submit_pdus.len() > 1, "长短信应拆分为多个SubmitSm PDU");

    for pdu in &submit_pdus {
        conn.write_frame(pdu.as_slice()).await.expect("发送 SubmitSm");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let segments = biz.received_segments.lock().unwrap();
    assert_eq!(segments.len(), submit_pdus.len(), "服务端应收到的分段数不匹配");
    for seg in segments.iter() {
        assert!(UdhParser::has_udhi(seg), "长短信 short_message 应包含 UDH");
    }
    let merged = merge_segments(&segments);
    assert_eq!(merged, original, "V3.4 合包后的内容应与原始消息一致");

    handle.abort();
}

#[tokio::test]
async fn test_longmsg_v34_mo_deliver_sm() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new());
    let (port, pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, _conn) = connect_client(port, 0x34).await.unwrap();

    let original = "这是一条从手机终端上行的SMPP V3.4长短信测试消息，手机用户发送了一条超过70个Unicode字符的长消息，网关需要将其拆分为多个DeliverSm分段下发给SP。验证接收方能否正确合包。"
        .as_bytes()
        .to_vec();

    let mut splitter = LongMessageSplitter::new();
    let frames = splitter.split(&original, SmsAlphabet::UCS2);
    assert!(frames.len() > 1);

    let server_conn = pool.first().await.expect("应有一个服务端连接");
    for (i, frame) in frames.iter().enumerate() {
        let pdu = build_deliver_mo_pdu(
            (100 + i) as u32, "13800138000", "106900", 8,
            if frame.has_udhi { 0x40 } else { 0 }, frame.content.clone(),
        );
        server_conn.write_frame(pdu.as_slice()).await.expect("发送 DeliverSm");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let segs = client.deliver_segments.lock().unwrap();
    assert_eq!(segs.len(), frames.len());
    let merged = merge_segments(&segs);
    assert_eq!(merged, original, "V3.4 DeliverSm 合包内容应一致");

    handle.abort();
}

#[tokio::test]
async fn test_longmsg_v34_mt_and_mo_roundtrip() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new());
    let (port, pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port, 0x34).await.unwrap();

    let mt = "SMPP V3.4 MT长短信：从SP下发到手机用户的长短信，验证拆分和合包的完整流程。"
        .as_bytes().to_vec();

    let pdus = client.build_long_submit_pdus(&mt, 8);
    for p in &pdus {
        conn.write_frame(p.as_slice()).await.expect("发送 SubmitSm");
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    {
        let segs = biz.received_segments.lock().unwrap();
        assert_eq!(segs.len(), pdus.len());
        assert_eq!(merge_segments(&segs), mt, "V3.4 MT 合包一致");
    }

    let mo = "SMPP V3.4 MO长短信：从手机用户上行到SP的长短信，验证DeliverSm拆分和合包的完整流程。"
        .as_bytes().to_vec();
    let mut splitter = LongMessageSplitter::new();
    let dframes = splitter.split(&mo, SmsAlphabet::UCS2);

    let srv = pool.first().await.expect("服务端连接");
    for (i, f) in dframes.iter().enumerate() {
        let p = build_deliver_mo_pdu(
            (200 + i) as u32, "13800138000", "106900", 8,
            if f.has_udhi { 0x40 } else { 0 }, f.content.clone(),
        );
        srv.write_frame(p.as_slice()).await.expect("发送 DeliverSm");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let segs = client.deliver_segments.lock().unwrap();
    assert_eq!(segs.len(), dframes.len());
    assert_eq!(merge_segments(&segs), mo, "V3.4 MO 合包一致");

    handle.abort();
}

#[tokio::test]
async fn test_longmsg_v34_submit_resp_all_success() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new());
    let (port, _pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port, 0x34).await.unwrap();

    let original = "V3.4验证每个分段SubmitSm都返回成功的SubmitSmResp，command_status=0。这条消息足够长以产生多个分段。"
        .as_bytes().to_vec();

    let pdus = client.build_long_submit_pdus(&original, 8);
    let expected = pdus.len();
    assert!(expected > 1);

    for p in &pdus {
        conn.write_frame(p.as_slice()).await.expect("发送 SubmitSm");
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(client.submit_resp_count.load(Ordering::Relaxed), expected,
        "V3.4 SubmitSmResp 数量应等于 SubmitSm 数量");

    handle.abort();
}

// ==================== SMPP V5.0 测试 ====================

#[tokio::test]
async fn test_longmsg_v50_mt_submit_sm() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new());
    let (port, _pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port, 0x50).await.unwrap();

    let original = "这是一条SMPP V5.0长短信测试消息，内容足够长以触发拆分。包含中文字符和English characters混合内容，确保UCS2编码下拆分合包正确。验证V5.0的SubmitSm的esm_class=0x40和UDH头部。"
        .as_bytes()
        .to_vec();

    let submit_pdus = client.build_long_submit_pdus(&original, 8);
    assert!(submit_pdus.len() > 1, "长短信应拆分为多个SubmitSm PDU");

    for pdu in &submit_pdus {
        conn.write_frame(pdu.as_slice()).await.expect("发送 SubmitSm");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let segments = biz.received_segments.lock().unwrap();
    assert_eq!(segments.len(), submit_pdus.len(), "服务端应收到的分段数不匹配");
    for seg in segments.iter() {
        assert!(UdhParser::has_udhi(seg), "长短信 short_message 应包含 UDH");
    }
    let merged = merge_segments(&segments);
    assert_eq!(merged, original, "V5.0 合包后的内容应与原始消息一致");

    handle.abort();
}

#[tokio::test]
async fn test_longmsg_v50_mo_deliver_sm() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new());
    let (port, pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, _conn) = connect_client(port, 0x50).await.unwrap();

    let original = "这是一条从手机终端上行的SMPP V5.0长短信测试消息，手机用户发送了一条超过70个Unicode字符的长消息，网关需要将其拆分为多个DeliverSm分段下发给SP。验证V5.0接收方能否正确合包。"
        .as_bytes()
        .to_vec();

    let mut splitter = LongMessageSplitter::new();
    let frames = splitter.split(&original, SmsAlphabet::UCS2);
    assert!(frames.len() > 1);

    let server_conn = pool.first().await.expect("应有一个服务端连接");
    for (i, frame) in frames.iter().enumerate() {
        let pdu = build_deliver_mo_pdu(
            (300 + i) as u32, "13800138000", "106900", 8,
            if frame.has_udhi { 0x40 } else { 0 }, frame.content.clone(),
        );
        server_conn.write_frame(pdu.as_slice()).await.expect("发送 DeliverSm");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let segs = client.deliver_segments.lock().unwrap();
    assert_eq!(segs.len(), frames.len());
    let merged = merge_segments(&segs);
    assert_eq!(merged, original, "V5.0 DeliverSm 合包内容应一致");

    handle.abort();
}

#[tokio::test]
async fn test_longmsg_v50_mt_and_mo_roundtrip() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new());
    let (port, pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port, 0x50).await.unwrap();

    let mt = "SMPP V5.0 MT长短信：从SP下发到手机用户的长短信，验证V5.0协议下SubmitSm拆分和合包的完整流程。"
        .as_bytes().to_vec();

    let pdus = client.build_long_submit_pdus(&mt, 8);
    for p in &pdus {
        conn.write_frame(p.as_slice()).await.expect("发送 SubmitSm");
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    {
        let segs = biz.received_segments.lock().unwrap();
        assert_eq!(segs.len(), pdus.len());
        assert_eq!(merge_segments(&segs), mt, "V5.0 MT 合包一致");
    }

    let mo = "SMPP V5.0 MO长短信：从手机用户上行到SP的长短信，验证V5.0协议下DeliverSm拆分和合包的完整流程。"
        .as_bytes().to_vec();
    let mut splitter = LongMessageSplitter::new();
    let dframes = splitter.split(&mo, SmsAlphabet::UCS2);

    let srv = pool.first().await.expect("服务端连接");
    for (i, f) in dframes.iter().enumerate() {
        let p = build_deliver_mo_pdu(
            (400 + i) as u32, "13800138000", "106900", 8,
            if f.has_udhi { 0x40 } else { 0 }, f.content.clone(),
        );
        srv.write_frame(p.as_slice()).await.expect("发送 DeliverSm");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let segs = client.deliver_segments.lock().unwrap();
    assert_eq!(segs.len(), dframes.len());
    assert_eq!(merge_segments(&segs), mo, "V5.0 MO 合包一致");

    handle.abort();
}

#[tokio::test]
async fn test_longmsg_v50_submit_resp_all_success() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new());
    let (port, _pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port, 0x50).await.unwrap();

    let original = "V5.0验证每个分段SubmitSm都返回成功的SubmitSmResp，command_status=0。这条消息足够长以产生多个分段，确保V5.0协议下全部分段都成功提交。"
        .as_bytes().to_vec();

    let pdus = client.build_long_submit_pdus(&original, 8);
    let expected = pdus.len();
    assert!(expected > 1);

    for p in &pdus {
        conn.write_frame(p.as_slice()).await.expect("发送 SubmitSm");
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(client.submit_resp_count.load(Ordering::Relaxed), expected,
        "V5.0 SubmitSmResp 数量应等于 SubmitSm 数量");

    handle.abort();
}
