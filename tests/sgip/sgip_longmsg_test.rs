use async_trait::async_trait;
use rsms_business::{MessageContext, MessageHandler};
// 窄腰统一模型：收发一律走 SgipAdapter + UnifiedMessage，不再手构裸 codec / 手剥头部字节。
use rsms_codec_sgip::adapter::SgipAdapter;
use rsms_model::{
    Address, Concat, Encoding, ProtocolAdapter, ProtocolExtra, Sequence, SgipExtra, UnifiedBind,
    UnifiedDeliver, UnifiedMessage, UnifiedSubmit, UnifiedSubmitResp,
};
use rsms_connector::client::ClientConfig;
use rsms_connector::{
    AccountConfig, AccountConfigProvider, AuthCredentials, AuthHandler, AuthResult,
    ClientBuilder, ServerBuilder, SgipDecoder,
};
use rsms_core::{ConnectionInfo, EncodedPdu, EndpointConfig, Protocol, RawPdu, Result};
use rsms_longmsg::split::SmsAlphabet;
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::time::Duration;

/// 把 splitter 的分段帧转为窄腰模型的 (concat, 纯载荷)：has_udhi 段剥掉 UDH、concat 承载分段信息。
fn frame_to_concat(f: &LongMessageFrame) -> (Option<Concat>, Vec<u8>) {
    if f.has_udhi {
        (
            Some(Concat {
                reference: f.reference_id,
                total: f.total_segments,
                sequence: f.segment_number,
            }),
            UdhParser::strip_udh(&f.content),
        )
    } else {
        (None, f.content.clone())
    }
}

/// 消费侧：把窄腰 (concat, 纯载荷) 重建为含 UDH 的分段字节，供既有 merge_segments/has_udhi 断言复用。
fn seg_with_udh(concat: &Option<Concat>, content: &[u8]) -> Vec<u8> {
    match concat {
        Some(c) => {
            let mut v = c.to_udh_prefix();
            v.extend_from_slice(content);
            v
        }
        None => content.to_vec(),
    }
}

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
impl MessageHandler for LongMsgBizHandler {
    fn name(&self) -> &'static str {
        "sgip-longmsg-biz"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        match msg {
            UnifiedMessage::Submit(s) => {
                // tpudhi 取自 SGIP 方言字段（adapter 据 concat 已置位）；
                // 窄腰：框架已解码，adapter 已把 UDH 剥成 s.concat、s.content 为纯载荷，
                // 这里据 concat 重建含 UDH 的分段字节，供后续 has_udhi/merge 断言复用。
                let tpudhi = match &s.extra {
                    ProtocolExtra::Sgip(e) => e.tpudhi,
                    _ => 0,
                };
                self.received_segments.lock().unwrap().push(SgipSegment {
                    tpudhi,
                    msg_content: seg_with_udh(&s.concat, &s.content),
                });
                // 回 SubmitResp：ctx.reply 内部自动回显 12B 复合序列（delta-2 已验证）。
                ctx.reply(UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                    msg_id: rsms_model::MessageId::Text(String::new()),
                    status: 0,
                })).await?;
            }
            _ => {}
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
        // 明文认证：authenticator 装口令字节；version 承载 login_type=1。
        let bind = UnifiedMessage::Bind(UnifiedBind {
            client_id: TEST_ACCOUNT.to_string(),
            authenticator: TEST_PASSWORD.as_bytes().to_vec(),
            timestamp: 0,
            version: 1,
            system_type: None,
            mode: rsms_model::BindMode::default(),
            login_mode: None,
        });
        let seq = Sequence::Sgip { node_id: SGIP_NODE_ID, timestamp: SGIP_TIMESTAMP, number: self.next_seq() };
        RawPdu::from(SgipAdapter.encode(&bind, seq).expect("encode bind"))
    }

    fn build_long_submit_pdus(&self, content: &[u8], msg_fmt: u8) -> Vec<RawPdu> {
        let alphabet = match msg_fmt {
            8 => SmsAlphabet::UCS2,
            _ => SmsAlphabet::ASCII,
        };
        // msg_fmt → 统一 Encoding（8=UCS2，否则 ASCII）。
        let encoding = match msg_fmt {
            8 => Encoding::Ucs2,
            _ => Encoding::Ascii,
        };
        let mut splitter = LongMessageSplitter::new();
        let frames = splitter.split(content, alphabet);
        // 保留原序列号生成节奏（首段取一个 number，再额外消耗一个 seq）。
        let start_number = self.next_seq();
        let _ = self.next_seq();
        let start_number = start_number;

        frames.iter().enumerate().map(|(i, frame)| {
            // 窄腰：传 concat + 纯载荷，由 adapter 重建 UDH 并置 tp_udhi=1。
            let (concat, payload) = frame_to_concat(frame);
            let submit = UnifiedMessage::Submit(UnifiedSubmit {
                src: Address::plain("106900"),
                dests: vec![Address::plain("13800138000")],
                content: payload,
                encoding,
                want_report: false, // report_flag=0
                concat,
                extra: ProtocolExtra::Sgip(SgipExtra::default()),
                tlvs: vec![],
            });
            let number = start_number + i as u32;
            let seq = Sequence::Sgip { node_id: SGIP_NODE_ID, timestamp: SGIP_TIMESTAMP, number };
            RawPdu::from(SgipAdapter.encode(&submit, seq).expect("encode submit segment"))
        }).collect()
    }
}

#[async_trait]
impl MessageHandler for LongMsgClientHandler {
    fn name(&self) -> &'static str {
        "sgip-longmsg-client"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        match msg {
            UnifiedMessage::BindResp(resp) => {
                if resp.status == 0 {
                    self.connected.store(true, Ordering::Relaxed);
                }
            }
            UnifiedMessage::SubmitResp(_) => {
                self.submit_resp_count.fetch_add(1, Ordering::Relaxed);
            }
            UnifiedMessage::Deliver(d) => {
                // 窄腰：框架已解码，adapter 已把 UDH 剥成 d.concat、d.content 为纯载荷；
                // 据 concat 重建含 UDH 段供合包断言复用。
                self.deliver_segments
                    .lock()
                    .unwrap()
                    .push(seg_with_udh(&d.concat, &d.content));
                // 回 DeliverResp：ctx.reply 内部自动回显 12B 复合序列（delta-2 已验证）。
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            _ => {}
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
    concat: Option<Concat>,
    payload: Vec<u8>,
) -> RawPdu {
    // msg_fmt → 统一 Encoding（8=UCS2，否则 ASCII）。
    let encoding = match msg_fmt {
        8 => Encoding::Ucs2,
        _ => Encoding::Ascii,
    };
    // 窄腰：传 concat + 纯载荷，由 adapter 重建 UDH 并置 tp_udhi=1。
    // decode 对称 src=sp_number, dest=user_number。
    let deliver = UnifiedMessage::Deliver(UnifiedDeliver {
        src: Address::plain(sp),
        dest: Address::plain(user),
        content: payload,
        encoding,
        concat,
        extra: ProtocolExtra::None,
        tlvs: vec![],
    });
    let seq = Sequence::Sgip { node_id, timestamp, number };
    RawPdu::from(SgipAdapter.encode(&deliver, seq).expect("encode deliver(MO)"))
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
        if let Ok(Some(merged)) = merger.add_frame("s", frame) {
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
    biz_handler: Arc<dyn MessageHandler>,
) -> Result<(u16, Arc<rsms_connector::ConnectionPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(
        EndpointConfig::new("sgip-longmsg-server", "127.0.0.1", 0, 8, 30)
            .with_protocol(Protocol::Sgip),
    );
    let server = ServerBuilder::new(cfg)
        .message_handlers(vec![biz_handler])
        .auth_handler(Arc::new(PasswordAuthHandler::new().add_account(TEST_ACCOUNT, TEST_PASSWORD)))
        .account_config_provider(Arc::new(MockAccountConfigProvider) as Arc<dyn AccountConfigProvider>)
        .serve()
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
            .with_protocol(Protocol::Sgip),
    );
    let handler = Arc::new(LongMsgClientHandler::new());
    let conn = ClientBuilder::new(endpoint, handler.clone(), SgipDecoder)
        .client_config(ClientConfig::new())
        .connect()
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
        let result = merger.add_frame("s", frame.clone()).expect("add_frame failed");
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
        let (concat, payload) = frame_to_concat(frame);
        let deliver_pdu = build_deliver_mo_pdu(
            SGIP_NODE_ID,
            SGIP_TIMESTAMP,
            (100 + i) as u32,
            "13800138000",
            "106900",
            8,
            concat,
            payload,
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
        merger.add_frame("s", frame.clone()).expect("add_frame failed");
    }
    let last_frame = frames.last().unwrap();
    let result = merger.add_frame("s", last_frame.clone()).expect("duplicate add_frame");
    assert!(result.is_none(), "重复分段应返回None");

    let mut merger2 = LongMessageMerger::new();
    let mut final_result = None;
    for frame in &frames {
        if let Ok(Some(merged)) = merger2.add_frame("s", frame.clone()) {
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
        let (concat, payload) = frame_to_concat(frame);
        let deliver_pdu = build_deliver_mo_pdu(
            SGIP_NODE_ID,
            SGIP_TIMESTAMP,
            (200 + i) as u32,
            "13800138000",
            "106900",
            8,
            concat,
            payload,
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
