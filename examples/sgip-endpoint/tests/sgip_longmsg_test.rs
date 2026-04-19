use async_trait::async_trait;
use rsms_business::{BusinessHandler, InboundContext};
use rsms_codec_sgip::{
    decode_message, Bind, CommandId, Deliver, DeliverResp, Encodable, SgipMessage,
    Submit, SubmitResp,
};
use rsms_connector::client::{ClientConfig, ClientContext, ClientHandler};
use rsms_connector::{
    connect, serve, AccountConfig, AccountConfigProvider, AuthCredentials, AuthHandler, AuthResult,
    SgipDecoder,
};
use rsms_core::{ConnectionInfo, EncodedPdu, EndpointConfig, Frame, RawPdu, Result};
use rsms_longmsg::split::SmsAlphabet;
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::time::Duration;

const TEST_ACCOUNT: &str = "106900";
const TEST_PASSWORD: &str = "password123";
const SGIP_NODE_ID: u32 = 1;
const SGIP_TIMESTAMP: u32 = 0x04051200;

struct PasswordAuthHandler {
    accounts: HashMap<String, String>,
}

impl PasswordAuthHandler {
    fn new() -> Self {
        Self {
            accounts: HashMap::new(),
        }
    }

    fn add_account(mut self, name: &str, password: &str) -> Self {
        self.accounts.insert(name.to_string(), password.to_string());
        self
    }
}

#[async_trait]
impl AuthHandler for PasswordAuthHandler {
    fn name(&self) -> &'static str {
        "sgip-longmsg-auth"
    }

    async fn authenticate(&self, _client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        if let AuthCredentials::Sgip { login_name, login_password } = credentials {
            if let Some(expected) = self.accounts.get(&login_name) {
                if &login_password == expected {
                    return Ok(AuthResult::success(&login_name));
                }
            }
        }
        Ok(AuthResult::failure(1, "Invalid credentials"))
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
struct SgipSegment {
    tpudhi: u8,
    msg_content: Vec<u8>,
}

struct LongMsgBizHandler {
    received_segments: Arc<Mutex<Vec<SgipSegment>>>,
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
        "sgip-longmsg-biz"
    }

    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        if pdu.len() < 20 {
            return Ok(());
        }

        let cmd_id = u32::from_be_bytes([pdu[4], pdu[5], pdu[6], pdu[7]]);
        if cmd_id == CommandId::Submit as u32 {
            if let Ok(msg) = decode_message(pdu) {
                if let SgipMessage::Submit(s) = msg {
                    self.received_segments.lock().unwrap().push(SgipSegment {
                        tpudhi: s.tpudhi,
                        msg_content: s.message_content.clone(),
                    });
                }
            }

            let node_id = u32::from_be_bytes([pdu[8], pdu[9], pdu[10], pdu[11]]);
            let timestamp = u32::from_be_bytes([pdu[12], pdu[13], pdu[14], pdu[15]]);
            let number = u32::from_be_bytes([pdu[16], pdu[17], pdu[18], pdu[19]]);

            let resp = SubmitResp { result: 0 };
            let resp_bytes = resp.to_pdu_bytes(node_id, timestamp, number);
            ctx.conn.write_frame(resp_bytes.as_ref()).await?;
        }
        Ok(())
    }
}

struct LongMsgClientHandler {
    connected: Arc<AtomicBool>,
    submit_resp_count: Arc<AtomicUsize>,
    deliver_segments: Arc<Mutex<Vec<Vec<u8>>>>,
    seq: AtomicUsize,
}

impl LongMsgClientHandler {
    fn new() -> Self {
        Self {
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
        let bind = Bind {
            login_type: 1,
            login_name: TEST_ACCOUNT.to_string(),
            login_password: TEST_PASSWORD.to_string(),
            reserve: [0u8; 8],
        };
        let seq_num = self.next_seq();
        bind.to_pdu_bytes(SGIP_NODE_ID, SGIP_TIMESTAMP, seq_num).into()
    }

    fn build_long_submit_pdus(&self, content: &[u8], msg_fmt: u8) -> Vec<RawPdu> {
        let alphabet = match msg_fmt {
            8 => SmsAlphabet::UCS2,
            _ => SmsAlphabet::ASCII,
        };
        let mut splitter = LongMessageSplitter::new();
        let frames = splitter.split(content, alphabet);
        let start_number = self.next_seq();
        let _ = self.next_seq();
        let start_number = start_number;

        frames.iter().enumerate().map(|(i, frame)| {
            let mut submit = Submit::new();
            submit.sp_number = "106900".to_string();
            submit.charge_number = "106900".to_string();
            submit.user_count = 1;
            submit.user_numbers = vec!["13800138000".to_string()];
            submit.msg_fmt = msg_fmt;
            submit.message_content = frame.content.clone();
            submit.tpudhi = if frame.has_udhi { 1 } else { 0 };
            submit.report_flag = 0;
            let number = start_number + i as u32;
            RawPdu::from(submit.to_pdu_bytes(SGIP_NODE_ID, SGIP_TIMESTAMP, number))
        }).collect()
    }
}

#[async_trait]
impl ClientHandler for LongMsgClientHandler {
    fn name(&self) -> &'static str {
        "sgip-longmsg-client"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        if pdu.len() < 20 {
            return Ok(());
        }

        let cmd_id = frame.command_id;

        if cmd_id == CommandId::BindResp as u32 {
            let result = pdu[20] as u32;
            if result == 0 {
                self.connected.store(true, Ordering::Relaxed);
            }
        } else if cmd_id == CommandId::SubmitResp as u32 {
            self.submit_resp_count.fetch_add(1, Ordering::Relaxed);
        } else if cmd_id == CommandId::Deliver as u32 {
            if let Ok(msg) = decode_message(pdu) {
                if let SgipMessage::Deliver(d) = msg {
                    self.deliver_segments.lock().unwrap().push(d.message_content.clone());
                }
            }

            let node_id = u32::from_be_bytes([pdu[8], pdu[9], pdu[10], pdu[11]]);
            let timestamp = u32::from_be_bytes([pdu[12], pdu[13], pdu[14], pdu[15]]);
            let number = u32::from_be_bytes([pdu[16], pdu[17], pdu[18], pdu[19]]);

            let resp = DeliverResp { result: 0 };
            let resp_bytes = resp.to_pdu_bytes(node_id, timestamp, number);
            ctx.conn.write_frame(resp_bytes.as_ref()).await?;
        }
        Ok(())
    }
}

fn build_deliver_mo_pdu(
    node_id: u32,
    timestamp: u32,
    number: u32,
    user: &str,
    sp: &str,
    msg_fmt: u8,
    tpudhi: u8,
    msg_content: Vec<u8>,
) -> RawPdu {
    let mut deliver = Deliver::new();
    deliver.user_number = user.to_string();
    deliver.sp_number = sp.to_string();
    deliver.msg_fmt = msg_fmt;
    deliver.tpudhi = tpudhi;
    deliver.message_content = msg_content;
    deliver.to_pdu_bytes(node_id, timestamp, number).into()
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

fn merge_sgip_segments(segments: &[SgipSegment]) -> Vec<u8> {
    let contents: Vec<Vec<u8>> = segments.iter().map(|s| s.msg_content.clone()).collect();
    merge_segments(&contents)
}

async fn start_server(
    biz_handler: Arc<dyn BusinessHandler>,
) -> Result<(u16, Arc<rsms_connector::ConnectionPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(
        EndpointConfig::new("sgip-longmsg-server", "127.0.0.1", 0, 8, 30)
            .with_protocol("sgip"),
    );
    let server = serve(
        cfg,
        vec![biz_handler],
        Some(Arc::new(PasswordAuthHandler::new().add_account(TEST_ACCOUNT, TEST_PASSWORD))),
        None,
        Some(Arc::new(MockAccountConfigProvider) as Arc<dyn AccountConfigProvider>),
        None,
        None,
    )
    .await
    .expect("bind");
    let port = server.local_addr.port();
    let pool = server.pool();
    let pool_clone = pool.clone();
    let handle = tokio::spawn(async move {
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok((port, pool_clone, handle))
}

async fn connect_client(
    port: u16,
) -> Result<(Arc<LongMsgClientHandler>, Arc<rsms_connector::client::ClientConnection>)> {
    let endpoint = Arc::new(
        EndpointConfig::new("sgip-longmsg-client", "127.0.0.1", port, 8, 30)
            .with_protocol("sgip"),
    );
    let handler = Arc::new(LongMsgClientHandler::new());
    let conn = connect(
        endpoint,
        handler.clone(),
        SgipDecoder,
        Some(ClientConfig::new()),
        None,
        None,
    )
    .await?;
    let bind_pdu = handler.build_bind_pdu();
    conn.write_frame(bind_pdu.as_bytes()).await?;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if handler.connected.load(Ordering::Relaxed) {
            break;
        }
    }
    assert!(handler.connected.load(Ordering::Relaxed), "SGIP连接认证失败");
    Ok((handler, conn))
}

#[tokio::test]
async fn test_longmsg_mt_split_and_merge() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let original = "这是一条非常长的测试短信，用于验证SGIP长短信拆分和合包功能是否正常工作。长短信需要被拆分成多个分段，每个分段带有UDH头部信息，接收方需要将这些分段重新合并为完整的原始消息。This is a very long test message to verify long SMS split and merge functionality."
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
async fn test_longmsg_mt_submit() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new());
    let (port, _pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port).await.unwrap();

    let original = "这是一条SGIP长短信测试消息，内容足够长以触发拆分。包含中文字符和English characters混合内容，确保UCS2编码下拆分合包正确。SGIP协议通过UDH头部实现长短信拆分。"
        .as_bytes()
        .to_vec();

    let submit_pdus = client.build_long_submit_pdus(&original, 8);
    assert!(submit_pdus.len() > 1, "长短信应拆分为多个Submit PDU");

    for pdu in &submit_pdus {
        conn.write_frame(pdu.as_bytes()).await.expect("send submit segment");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let segments = biz.received_segments.lock().unwrap();
    assert_eq!(segments.len(), submit_pdus.len(), "服务端应收到的分段数不匹配");

    for seg in segments.iter() {
        assert!(seg.tpudhi == 1, "长短信 tpudhi 应为 1");
        assert!(UdhParser::has_udhi(&seg.msg_content), "msg_content 应包含 UDH");
    }

    let merged = merge_sgip_segments(&segments);
    assert_eq!(merged, original, "合包后的内容应与原始消息一致");

    handle.abort();
}

#[tokio::test]
async fn test_longmsg_mo_deliver() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new());
    let (port, pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, _conn) = connect_client(port).await.unwrap();

    let original = "这是一条从手机终端上行的SGIP长短信测试消息，手机用户发送了一条超过70个Unicode字符的长消息，网关需要将其拆分为多个Deliver分段下发给SP。验证接收方能否正确合包。"
        .as_bytes()
        .to_vec();

    let mut splitter = LongMessageSplitter::new();
    let frames = splitter.split(&original, SmsAlphabet::UCS2);
    assert!(frames.len() > 1, "长短信应拆分为多个分段");

    let server_conn = pool.first().await.expect("应有一个服务端连接");

    for (i, frame) in frames.iter().enumerate() {
        let deliver_pdu = build_deliver_mo_pdu(
            SGIP_NODE_ID,
            SGIP_TIMESTAMP,
            (100 + i) as u32,
            "13800138000",
            "106900",
            8,
            if frame.has_udhi { 1 } else { 0 },
            frame.content.clone(),
        );
        server_conn.write_frame(deliver_pdu.as_bytes()).await.expect("send deliver segment");
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

#[tokio::test]
async fn test_longmsg_mt_and_mo_roundtrip() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new());
    let (port, pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port).await.unwrap();

    let mt_content = "MT长短信测试：从SP下发到手机用户的SGIP长短信，验证Submit拆分和合包的完整流程。SGIP协议通过tpudhi和UDH实现长短信。"
        .as_bytes()
        .to_vec();

    let submit_pdus = client.build_long_submit_pdus(&mt_content, 8);
    for pdu in &submit_pdus {
        conn.write_frame(pdu.as_bytes()).await.expect("send submit");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    {
        let segments = biz.received_segments.lock().unwrap();
        assert_eq!(segments.len(), submit_pdus.len());
        let merged = merge_sgip_segments(&segments);
        assert_eq!(merged, mt_content, "MT合包内容应一致");
    }

    let mo_content = "MO长短信测试：从手机用户上行到SP的SGIP长短信，验证Deliver拆分和合包的完整流程。手机用户发送长消息，网关拆分为多个Deliver。"
        .as_bytes()
        .to_vec();

    let mut splitter = LongMessageSplitter::new();
    let deliver_frames = splitter.split(&mo_content, SmsAlphabet::UCS2);

    let server_conn = pool.first().await.expect("应有一个服务端连接");
    for (i, frame) in deliver_frames.iter().enumerate() {
        let deliver_pdu = build_deliver_mo_pdu(
            SGIP_NODE_ID,
            SGIP_TIMESTAMP,
            (200 + i) as u32,
            "13800138000",
            "106900",
            8,
            if frame.has_udhi { 1 } else { 0 },
            frame.content.clone(),
        );
        server_conn.write_frame(deliver_pdu.as_bytes()).await.expect("send deliver");
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
async fn test_longmsg_submit_resp_all_success() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new());
    let (port, _pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port).await.unwrap();

    let original = "验证每个分段Submit都返回成功的SubmitResp，result=0。这条消息足够长以产生多个分段，确保全部分段都成功提交。SGIP协议长短信拆分合包功能验证。"
        .as_bytes()
        .to_vec();

    let submit_pdus = client.build_long_submit_pdus(&original, 8);
    let expected_count = submit_pdus.len();
    assert!(expected_count > 1);

    for pdu in &submit_pdus {
        conn.write_frame(pdu.as_bytes()).await.expect("send submit");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        client.submit_resp_count.load(Ordering::Relaxed),
        expected_count,
        "应收到的SubmitResp数量 = 发送的Submit数量"
    );

    handle.abort();
}
