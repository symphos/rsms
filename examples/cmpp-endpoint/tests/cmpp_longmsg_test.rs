use async_trait::async_trait;
use rsms_business::{BusinessHandler, InboundContext};
use rsms_codec_cmpp::{
    decode_message, decode_message_with_version, auth::compute_connect_auth,
    CmppMessage, CommandId, Connect, Decodable, Deliver, DeliverV20, DeliverResp, Pdu, Submit,
    SubmitResp, SubmitV20,
    codec::PduHeader,
};
use rsms_connector::{
    connect, serve, AccountConfig, AccountConfigProvider, AuthCredentials, AuthHandler,
    AuthResult, ClientHandler, CmppDecoder,
};
use rsms_connector::client::{ClientConfig, ClientContext, ClientConnection};
use rsms_core::{ConnectionInfo, Frame, RawPdu, EndpointConfig, Result};
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser};
use rsms_longmsg::split::SmsAlphabet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::time::Duration;

const TEST_ACCOUNT: &str = "106900";
const TEST_PASSWORD: &str = "password123";

struct PasswordAuthHandler;

#[async_trait]
impl AuthHandler for PasswordAuthHandler {
    fn name(&self) -> &'static str {
        "auth"
    }

    async fn authenticate(&self, _client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        if let AuthCredentials::Cmpp { source_addr, authenticator_source, timestamp, .. } = credentials {
            let expected = compute_connect_auth(TEST_ACCOUNT, TEST_PASSWORD, timestamp);
            if source_addr == TEST_ACCOUNT && expected == authenticator_source {
                Ok(AuthResult::success(TEST_ACCOUNT))
            } else {
                Ok(AuthResult::failure(1, "Invalid password"))
            }
        } else {
            Ok(AuthResult::failure(1, "Invalid credentials"))
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

#[derive(Clone)]
struct LongMsgSegment {
    pk_total: u8,
    pk_number: u8,
    tpudhi: u8,
    msg_content: Vec<u8>,
}

struct LongMsgBizHandler {
    version: u8,
    received_segments: Arc<Mutex<Vec<LongMsgSegment>>>,
}

impl LongMsgBizHandler {
    fn new(version: u8) -> Self {
        Self {
            version,
            received_segments: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl BusinessHandler for LongMsgBizHandler {
    fn name(&self) -> &'static str {
        "longmsg-biz"
    }

    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        let msg = if self.version == 0x20 {
            decode_message_with_version(frame.data_as_slice(), Some(0x20))
        } else {
            decode_message(frame.data_as_slice())
        };
        if let Ok(msg) = msg {
            match msg {
                CmppMessage::SubmitV20 { submit: s, .. } => {
                    self.received_segments.lock().unwrap().push(LongMsgSegment {
                        pk_total: s.pk_total,
                        pk_number: s.pk_number,
                        tpudhi: s.tpudhi,
                        msg_content: s.msg_content.clone(),
                    });
                    let resp = SubmitResp { msg_id: [0u8; 8], result: 0 };
                    let resp_pdu: Pdu = resp.into();
                    ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;
                }
                CmppMessage::SubmitV30 { submit: s, .. } => {
                    self.received_segments.lock().unwrap().push(LongMsgSegment {
                        pk_total: s.pk_total,
                        pk_number: s.pk_number,
                        tpudhi: s.tpudhi,
                        msg_content: s.msg_content.clone(),
                    });
                    let resp = SubmitResp { msg_id: [0u8; 8], result: 0 };
                    let resp_pdu: Pdu = resp.into();
                    ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

struct LongMsgClientHandler {
    version: u8,
    connected: Arc<AtomicBool>,
    submit_resp_count: Arc<AtomicUsize>,
    deliver_segments: Arc<Mutex<Vec<Vec<u8>>>>,
    seq: AtomicUsize,
}

impl LongMsgClientHandler {
    fn new(version: u8) -> Self {
        Self {
            version,
            connected: Arc::new(AtomicBool::new(false)),
            submit_resp_count: Arc::new(AtomicUsize::new(0)),
            deliver_segments: Arc::new(Mutex::new(Vec::new())),
            seq: AtomicUsize::new(1),
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u32
    }

    fn build_connect_pdu(&self) -> RawPdu {
        let timestamp = 0u32;
        let auth = compute_connect_auth(TEST_ACCOUNT, TEST_PASSWORD, timestamp);
        let connect = Connect {
            source_addr: TEST_ACCOUNT.to_string(),
            authenticator_source: auth,
            version: self.version,
            timestamp,
        };
        let pdu: Pdu = connect.into();
        pdu.to_pdu_bytes(self.next_seq())
    }

    fn build_long_submit_pdus_v20(&self, content: &[u8], msg_fmt: u8) -> Vec<RawPdu> {
        let alphabet = match msg_fmt {
            8 => SmsAlphabet::UCS2,
            _ => SmsAlphabet::ASCII,
        };
        let mut splitter = LongMessageSplitter::new();
        let frames = splitter.split(content, alphabet);

        frames.iter().map(|frame| {
            let mut submit = SubmitV20::new();
            submit.src_id = "106900".to_string();
            submit.dest_usr_tl = 1;
            submit.dest_terminal_ids = vec!["13800138000".to_string()];
            submit.msg_fmt = msg_fmt;
            submit.msg_content = frame.content.clone();
            submit.pk_total = frame.total_segments;
            submit.pk_number = frame.segment_number;
            submit.tpudhi = if frame.has_udhi { 1 } else { 0 };
            submit.registered_delivery = 0;
            let pdu: Pdu = submit.into();
            pdu.to_pdu_bytes(self.next_seq())
        }).collect()
    }

    fn build_long_submit_pdus_v30(&self, content: &[u8], msg_fmt: u8) -> Vec<RawPdu> {
        let alphabet = match msg_fmt {
            8 => SmsAlphabet::UCS2,
            _ => SmsAlphabet::ASCII,
        };
        let mut splitter = LongMessageSplitter::new();
        let frames = splitter.split(content, alphabet);

        frames.iter().map(|frame| {
            let mut submit = Submit::new();
            submit.src_id = "106900".to_string();
            submit.dest_usr_tl = 1;
            submit.dest_terminal_ids = vec!["13800138000".to_string()];
            submit.msg_fmt = msg_fmt;
            submit.msg_content = frame.content.clone();
            submit.pk_total = frame.total_segments;
            submit.pk_number = frame.segment_number;
            submit.tpudhi = if frame.has_udhi { 1 } else { 0 };
            submit.registered_delivery = 0;
            let pdu: Pdu = submit.into();
            pdu.to_pdu_bytes(self.next_seq())
        }).collect()
    }
}

#[async_trait]
impl ClientHandler for LongMsgClientHandler {
    fn name(&self) -> &'static str {
        "longmsg-client"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        if pdu.len() < 12 {
            return Ok(());
        }
        let cmd_id = u32::from_be_bytes([pdu[4], pdu[5], pdu[6], pdu[7]]);

        if cmd_id == CommandId::ConnectResp as u32 && pdu.len() >= 25 {
            let status = u32::from_be_bytes([pdu[12], pdu[13], pdu[14], pdu[15]]);
            if status == 0 {
                self.connected.store(true, Ordering::Relaxed);
            }
        } else if cmd_id == CommandId::SubmitResp as u32 {
            self.submit_resp_count.fetch_add(1, Ordering::Relaxed);
        } else if cmd_id == CommandId::Deliver as u32 {
            let decode_ver = if self.version == 0x20 { Some(0x20) } else { None };
            let msg = if let Some(v) = decode_ver {
                decode_message_with_version(pdu, Some(v))
            } else {
                decode_message(pdu)
            };
            if let Ok(msg) = msg {
                match msg {
                    CmppMessage::DeliverV20 { deliver: d, .. } => {
                        if d.registered_delivery == 0 {
                            self.deliver_segments.lock().unwrap().push(d.msg_content.clone());
                        }
                        let resp = DeliverResp { msg_id: [0u8; 8], result: 0 };
                        let resp_pdu: Pdu = resp.into();
                        ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;
                    }
                    CmppMessage::DeliverV30 { deliver: d, .. } => {
                        if d.registered_delivery == 0 {
                            self.deliver_segments.lock().unwrap().push(d.msg_content.clone());
                        }
                        let resp = DeliverResp { msg_id: [0u8; 8], result: 0 };
                        let resp_pdu: Pdu = resp.into();
                        ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

async fn start_server(
    biz_handler: Arc<dyn BusinessHandler>,
) -> Result<(u16, Arc<rsms_connector::ConnectionPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new("test-server", "127.0.0.1", 0, 8, 30));
    let server = serve(
        cfg,
        vec![biz_handler],
        Some(Arc::new(PasswordAuthHandler)),
        None,
        Some(Arc::new(MockAccountConfigProvider) as Arc<dyn AccountConfigProvider>),
        None,
        None,
    )
    .await
    .expect("bind");
    let port = server.local_addr.port();
    let pool = server.pool();
    let handle = tokio::spawn(async move { let _ = server.run().await; });
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok((port, pool, handle))
}

async fn connect_client(port: u16, version: u8) -> Result<(Arc<LongMsgClientHandler>, Arc<ClientConnection>)> {
    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let handler = Arc::new(LongMsgClientHandler::new(version));
    let conn = connect(
        endpoint,
        handler.clone(),
        CmppDecoder,
        Some(ClientConfig::new()),
        None,
        None,
    )
    .await?;
    let connect_pdu = handler.build_connect_pdu();
    conn.send_request(connect_pdu).await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(handler.connected.load(Ordering::Relaxed), "连接失败");
    Ok((handler, conn))
}

fn build_deliver_mo_pdu_v20(seq_id: u32, src: &str, dest: &str, msg_fmt: u8, tpudhi: u8, msg_content: Vec<u8>) -> RawPdu {
    let pdu: Pdu = DeliverV20 {
        msg_id: [0u8; 8],
        dest_id: dest.to_string(),
        service_id: "SMS".to_string(),
        tppid: 0,
        tpudhi,
        msg_fmt,
        src_terminal_id: src.to_string(),
        registered_delivery: 0,
        msg_content,
    }.into();
    pdu.to_pdu_bytes(seq_id)
}

fn build_deliver_mo_pdu_v30(seq_id: u32, src: &str, dest: &str, msg_fmt: u8, tpudhi: u8, msg_content: Vec<u8>) -> RawPdu {
    let pdu: Pdu = Deliver {
        msg_id: [0u8; 8],
        dest_id: dest.to_string(),
        service_id: "SMS".to_string(),
        tppid: 0,
        tpudhi,
        msg_fmt,
        src_terminal_id: src.to_string(),
        src_terminal_type: 0,
        registered_delivery: 0,
        msg_content,
        link_id: String::new(),
    }.into();
    pdu.to_pdu_bytes(seq_id)
}

fn merge_segments(segments: &[Vec<u8>]) -> Vec<u8> {
    let mut merger = LongMessageMerger::new();
    let mut result = None;
    for (i, seg) in segments.iter().enumerate() {
        let has_udhi = UdhParser::has_udhi(seg);
        let (reference_id, total_segments, segment_number) = if has_udhi {
            if let Some((udh, _)) = UdhParser::extract_udh(seg) {
                (udh.reference_id, udh.total_segments, udh.segment_number)
            } else {
                (0, 1, (i + 1) as u8)
            }
        } else {
            (0, 1, 1)
        };
        let frame = LongMessageFrame::new(reference_id, total_segments, segment_number, seg.clone(), has_udhi, None);
        if let Ok(Some(merged)) = merger.add_frame(frame) {
            result = Some(merged);
        }
    }
    result.unwrap_or_default()
}

fn merge_submit_segments(segments: &[LongMsgSegment]) -> Vec<u8> {
    let mut merger = LongMessageMerger::new();
    let mut result = None;
    for seg in segments {
        let content = &seg.msg_content;
        let has_udhi = UdhParser::has_udhi(content);
        let (reference_id, total_segments, segment_number) = if has_udhi {
            if let Some((udh, _)) = UdhParser::extract_udh(content) {
                (udh.reference_id, udh.total_segments, udh.segment_number)
            } else {
                (0, seg.pk_total, seg.pk_number)
            }
        } else {
            (0, 1, 1)
        };
        let frame = LongMessageFrame::new(reference_id, total_segments, segment_number, content.clone(), has_udhi, None);
        if let Ok(Some(merged)) = merger.add_frame(frame) {
            result = Some(merged);
        }
    }
    result.unwrap_or_default()
}

// ==================== 纯逻辑测试 ====================

#[tokio::test]
async fn test_longmsg_mt_split_and_merge() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let original = "这是一条非常长的测试短信，用于验证长短信拆分和合包功能是否正常工作。长短信需要被拆分成多个分段，每个分段带有UDH头部信息，接收方需要将这些分段重新合并为完整的原始消息。This is a very long test message to verify long SMS split and merge functionality."
        .as_bytes()
        .to_vec();

    let mut splitter = LongMessageSplitter::new();
    let frames = splitter.split(&original, SmsAlphabet::UCS2);

    assert!(frames.len() > 1, "长短信应该被拆分为多个分段，实际 {} 个", frames.len());

    for (i, frame) in frames.iter().enumerate() {
        assert!(frame.has_udhi, "分段 {} 应该有 UDH 头", i + 1);
        assert_eq!(frame.total_segments, frames.len() as u8, "total_segments 不匹配");
        assert_eq!(frame.segment_number, (i + 1) as u8, "segment_number 应为 {}", i + 1);
        assert!(frame.content.len() <= 140, "分段 {} 内容超过 140 字节: {}", i + 1, frame.content.len());
    }

    let mut merger = LongMessageMerger::new();
    for frame in &frames {
        let result = merger.add_frame(frame.clone()).expect("add_frame failed");
        if frame.segment_number == frame.total_segments {
            assert!(result.is_some(), "最后一个分段后应该得到完整消息");
            let merged = result.unwrap();
            assert_eq!(merged, original, "合包后的消息应与原始消息一致");
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

    assert_eq!(frames.len(), 1, "短消息应该只有1个分段");
    assert!(!frames[0].has_udhi, "短消息不应该有 UDH");
    assert_eq!(frames[0].total_segments, 1);
    assert_eq!(frames[0].segment_number, 1);
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

    assert!(frames.len() > 1, "200字符ASCII应拆分为多个分段");

    let mut merger = LongMessageMerger::new();
    for frame in &frames {
        merger.add_frame(frame.clone()).expect("add_frame failed");
    }
    let last_frame = frames.last().unwrap();
    let result = merger.add_frame(last_frame.clone()).expect("duplicate add_frame");
    assert!(result.is_none(), "重复分段应返回None");

    let mut merger2 = LongMessageMerger::new();
    let mut final_result = None;
    for frame in &frames {
        if let Ok(Some(merged)) = merger2.add_frame(frame.clone()) {
            final_result = Some(merged);
        }
    }
    assert_eq!(final_result.unwrap(), content.as_bytes(), "ASCII合包内容应一致");
}

// ==================== CMPP 2.0 测试 ====================

#[tokio::test]
async fn test_longmsg_v20_mt_submit() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new(0x20));
    let (port, _pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port, 0x20).await.unwrap();

    let original = "这是一条CMPP20长短信测试消息，内容足够长以触发拆分。包含中文字符和English characters混合内容，确保UCS2编码下拆分合包正确。"
        .as_bytes()
        .to_vec();

    let submit_pdus = client.build_long_submit_pdus_v20(&original, 8);
    assert!(submit_pdus.len() > 1, "长短信应拆分为多个Submit PDU");

    for pdu in &submit_pdus {
        conn.send_request(pdu.clone()).await.expect("send submit");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let segments = biz.received_segments.lock().unwrap();
    assert_eq!(segments.len(), submit_pdus.len(), "服务端应收到的分段数不匹配");

    for seg in segments.iter() {
        assert!(seg.tpudhi == 1, "长短信 tpudhi 应为 1");
        assert!(UdhParser::has_udhi(&seg.msg_content), "msg_content 应包含 UDH");
    }

    let merged = merge_submit_segments(&segments);
    assert_eq!(merged, original, "合包后的内容应与原始消息一致");

    handle.abort();
}

#[tokio::test]
async fn test_longmsg_v20_mo_deliver() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new(0x20));
    let (port, pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, _conn) = connect_client(port, 0x20).await.unwrap();

    let original = "这是一条从手机终端上行的CMPP20长短信测试消息，手机用户发送了一条超过70个Unicode字符的长消息，网关需要将其拆分为多个Deliver分段下发给SP。验证接收方能否正确合包。"
        .as_bytes()
        .to_vec();

    let mut splitter = LongMessageSplitter::new();
    let frames = splitter.split(&original, SmsAlphabet::UCS2);
    assert!(frames.len() > 1, "长短信应拆分为多个分段");

    let server_conn = pool.first().await.expect("应有一个服务端连接");

    for (i, frame) in frames.iter().enumerate() {
        let deliver_pdu = build_deliver_mo_pdu_v20(
            (100 + i) as u32,
            "13800138000",
            "106900",
            8,
            if frame.has_udhi { 1 } else { 0 },
            frame.content.clone(),
        );
        server_conn.write_frame(deliver_pdu.as_slice()).await.expect("send deliver segment");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let deliver_segments = client.deliver_segments.lock().unwrap();
    assert_eq!(deliver_segments.len(), frames.len(), "客户端应收到的Deliver分段数不匹配");

    let merged = merge_segments(&deliver_segments);
    assert_eq!(merged, original, "合包后的Deliver内容应与原始消息一致");

    handle.abort();
}

#[tokio::test]
async fn test_longmsg_v20_mt_and_mo_roundtrip() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new(0x20));
    let (port, pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port, 0x20).await.unwrap();

    let mt_content = "MT长短信测试：从SP下发到手机用户的长短信，验证CMPP2.0协议下Submit拆分和合包的完整流程。"
        .as_bytes()
        .to_vec();

    let submit_pdus = client.build_long_submit_pdus_v20(&mt_content, 8);
    for pdu in &submit_pdus {
        conn.send_request(pdu.clone()).await.expect("send submit");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    {
        let segments = biz.received_segments.lock().unwrap();
        assert_eq!(segments.len(), submit_pdus.len());
        let merged = merge_submit_segments(&segments);
        assert_eq!(merged, mt_content, "MT合包内容应一致");
    }

    let mo_content = "MO长短信测试：从手机用户上行到SP的长短信，验证CMPP2.0协议下Deliver拆分和合包的完整流程。手机用户发送长消息。"
        .as_bytes()
        .to_vec();

    let mut splitter = LongMessageSplitter::new();
    let deliver_frames = splitter.split(&mo_content, SmsAlphabet::UCS2);

    let server_conn = pool.first().await.expect("应有一个服务端连接");
    for (i, frame) in deliver_frames.iter().enumerate() {
        let deliver_pdu = build_deliver_mo_pdu_v20(
            (200 + i) as u32,
            "13800138000",
            "106900",
            8,
            if frame.has_udhi { 1 } else { 0 },
            frame.content.clone(),
        );
        server_conn.write_frame(deliver_pdu.as_slice()).await.expect("send deliver");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let deliver_segments = client.deliver_segments.lock().unwrap();
    assert_eq!(deliver_segments.len(), deliver_frames.len());
    let merged = merge_segments(&deliver_segments);
    assert_eq!(merged, mo_content, "MO合包内容应一致");

    handle.abort();
}

#[tokio::test]
async fn test_longmsg_v20_submit_resp_all_success() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new(0x20));
    let (port, _pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port, 0x20).await.unwrap();

    let original = "验证CMPP20每个分段Submit都返回成功的SubmitResp，result=0。这条消息足够长以产生多个分段，确保全部分段都成功提交。"
        .as_bytes()
        .to_vec();

    let submit_pdus = client.build_long_submit_pdus_v20(&original, 8);
    let expected_count = submit_pdus.len();
    assert!(expected_count > 1);

    for pdu in &submit_pdus {
        conn.send_request(pdu.clone()).await.expect("send submit");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        client.submit_resp_count.load(Ordering::Relaxed),
        expected_count,
        "应收到的SubmitResp数量 = 发送的Submit数量"
    );

    handle.abort();
}

// ==================== CMPP 3.0 测试 ====================

#[tokio::test]
async fn test_longmsg_v30_mt_submit() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new(0x30));
    let (port, _pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port, 0x30).await.unwrap();

    let original = "这是一条CMPP30长短信测试消息，内容足够长以触发拆分。包含中文字符和English characters混合内容，确保UCS2编码下拆分合包正确。验证V30协议的Submit结构。"
        .as_bytes()
        .to_vec();

    let submit_pdus = client.build_long_submit_pdus_v30(&original, 8);
    assert!(submit_pdus.len() > 1, "长短信应拆分为多个Submit PDU");

    for pdu in &submit_pdus {
        conn.send_request(pdu.clone()).await.expect("send submit");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let segments = biz.received_segments.lock().unwrap();
    assert_eq!(segments.len(), submit_pdus.len(), "服务端应收到的分段数不匹配");

    for seg in segments.iter() {
        assert!(seg.tpudhi == 1, "长短信 tpudhi 应为 1");
        assert!(UdhParser::has_udhi(&seg.msg_content), "msg_content 应包含 UDH");
    }

    let merged = merge_submit_segments(&segments);
    assert_eq!(merged, original, "合包后的内容应与原始消息一致");

    handle.abort();
}

#[tokio::test]
async fn test_longmsg_v30_mo_deliver() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new(0x30));
    let (port, pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, _conn) = connect_client(port, 0x30).await.unwrap();

    let original = "这是一条从手机终端上行的CMPP30长短信测试消息，手机用户发送了一条超过70个Unicode字符的长消息，网关需要将其拆分为多个DeliverV30分段下发给SP。验证接收方能否正确合包。"
        .as_bytes()
        .to_vec();

    let mut splitter = LongMessageSplitter::new();
    let frames = splitter.split(&original, SmsAlphabet::UCS2);
    assert!(frames.len() > 1, "长短信应拆分为多个分段");

    let server_conn = pool.first().await.expect("应有一个服务端连接");

    for (i, frame) in frames.iter().enumerate() {
        let deliver_pdu = build_deliver_mo_pdu_v30(
            (300 + i) as u32,
            "13800138000",
            "106900",
            8,
            if frame.has_udhi { 1 } else { 0 },
            frame.content.clone(),
        );
        server_conn.write_frame(deliver_pdu.as_slice()).await.expect("send deliver segment");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let deliver_segments = client.deliver_segments.lock().unwrap();
    assert_eq!(deliver_segments.len(), frames.len(), "客户端应收到的Deliver分段数不匹配");

    let merged = merge_segments(&deliver_segments);
    assert_eq!(merged, original, "合包后的Deliver内容应与原始消息一致");

    handle.abort();
}

#[tokio::test]
async fn test_longmsg_v30_mt_and_mo_roundtrip() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new(0x30));
    let (port, pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port, 0x30).await.unwrap();

    let mt_content = "CMPP30 MT长短信测试：从SP下发到手机用户的长短信，验证CMPP3.0协议下Submit拆分和合包的完整流程。包含额外的link_id、dest_terminal_type等V30特有字段。"
        .as_bytes()
        .to_vec();

    let submit_pdus = client.build_long_submit_pdus_v30(&mt_content, 8);
    for pdu in &submit_pdus {
        conn.send_request(pdu.clone()).await.expect("send submit");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    {
        let segments = biz.received_segments.lock().unwrap();
        assert_eq!(segments.len(), submit_pdus.len());
        let merged = merge_submit_segments(&segments);
        assert_eq!(merged, mt_content, "MT合包内容应一致");
    }

    let mo_content = "CMPP30 MO长短信测试：从手机用户上行到SP的长短信，验证CMPP3.0协议下Deliver拆分和合包的完整流程。手机用户发送长消息，网关拆分为DeliverV30。"
        .as_bytes()
        .to_vec();

    let mut splitter = LongMessageSplitter::new();
    let deliver_frames = splitter.split(&mo_content, SmsAlphabet::UCS2);

    let server_conn = pool.first().await.expect("应有一个服务端连接");
    for (i, frame) in deliver_frames.iter().enumerate() {
        let deliver_pdu = build_deliver_mo_pdu_v30(
            (400 + i) as u32,
            "13800138000",
            "106900",
            8,
            if frame.has_udhi { 1 } else { 0 },
            frame.content.clone(),
        );
        server_conn.write_frame(deliver_pdu.as_slice()).await.expect("send deliver");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let deliver_segments = client.deliver_segments.lock().unwrap();
    assert_eq!(deliver_segments.len(), deliver_frames.len());
    let merged = merge_segments(&deliver_segments);
    assert_eq!(merged, mo_content, "MO合包内容应一致");

    handle.abort();
}

#[tokio::test]
async fn test_longmsg_v30_submit_resp_all_success() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new(0x30));
    let (port, _pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port, 0x30).await.unwrap();

    let original = "验证CMPP30每个分段Submit都返回成功的SubmitResp，result=0。这条消息足够长以产生多个分段，确保V30协议下全部分段都成功提交。"
        .as_bytes()
        .to_vec();

    let submit_pdus = client.build_long_submit_pdus_v30(&original, 8);
    let expected_count = submit_pdus.len();
    assert!(expected_count > 1);

    for pdu in &submit_pdus {
        conn.send_request(pdu.clone()).await.expect("send submit");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        client.submit_resp_count.load(Ordering::Relaxed),
        expected_count,
        "应收到的SubmitResp数量 = 发送的Submit数量"
    );

    handle.abort();
}
