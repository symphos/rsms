use async_trait::async_trait;
use rsms_business::{BusinessHandler, InboundContext};
// 窄腰统一模型：编解码统一走 SmgpAdapter + UnifiedMessage。
// 长短信级联信息经 rsms_model::Concat 承载：构造侧传 concat + 纯载荷，由 adapter 自动建 UDH +
// 置 SMGP 的 TP_UDHI 可选参数 TLV(0x0002,[1])；消费侧据 concat 用 seg_with_udh 重建含 UDH 段供合包。
use rsms_codec_smgp::adapter::SmgpAdapter;
use rsms_codec_smgp::auth::compute_login_auth;
use rsms_model::{
    Address, BindMode, Concat, Encoding, MessageId, ProtocolAdapter, ProtocolExtra, Sequence,
    UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedSubmit, UnifiedSubmitResp,
};
use rsms_connector::{
    ClientBuilder, ServerBuilder, AccountConfig, AccountConfigProvider, AuthCredentials, AuthHandler,
    AuthResult, SmgpDecoder,
};
use rsms_connector::client::{ClientConfig, ClientConnection, ClientContext, ClientHandler};
use rsms_core::{ConnectionInfo, Frame, RawPdu, EndpointConfig, Protocol, Result};
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, UdhParser};
use rsms_longmsg::split::SmsAlphabet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::time::Duration;

const TEST_ACCOUNT: &str = "106900";
const TEST_PASSWORD: &str = "password123";

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

/// 消费侧：把窄腰 (concat, 纯载荷) 重建为含 UDH 的分段字节，供既有 merge_*/has_udhi 断言复用。
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

struct PasswordAuthHandler;

#[async_trait]
impl AuthHandler for PasswordAuthHandler {
    fn name(&self) -> &'static str {
        "auth"
    }

    async fn authenticate(&self, _client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        if let AuthCredentials::Smgp { client_id, authenticator, .. } = credentials {
            let expected = compute_login_auth(TEST_ACCOUNT, TEST_PASSWORD, 0);
            if client_id == TEST_ACCOUNT && expected == authenticator {
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
        "longmsg-biz"
    }

    async fn on_inbound(&self, ctx: &InboundContext, frame: &Frame) -> Result<()> {
        if let Ok(msg) = SmgpAdapter.decode(frame) {
            match msg {
                UnifiedMessage::Submit(s) => {
                    // 窄腰：adapter 已把 UDH 剥成 s.concat、s.content 为纯载荷。
                    // 据 concat 重建含 UDH 的分段字节，供后续 merge_submit_segments 合包断言复用。
                    self.received_segments
                        .lock()
                        .unwrap()
                        .push(seg_with_udh(&s.concat, &s.content));
                    let resp = UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                        msg_id: MessageId::Binary(vec![0u8; 10]),
                        status: 0,
                    });
                    let resp_bytes = SmgpAdapter.encode(&resp, SmgpAdapter.sequence_of(frame))?;
                    ctx.conn.write_frame(&resp_bytes).await?;
                }
                _ => {}
            }
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

    fn build_login_pdu(&self) -> RawPdu {
        let timestamp = 0u32;
        // compute_login_auth 保留：鉴权 MD5 非 codec 范畴。
        let auth = compute_login_auth(TEST_ACCOUNT, TEST_PASSWORD, timestamp).to_vec();
        let bind = UnifiedMessage::Bind(UnifiedBind {
            client_id: TEST_ACCOUNT.to_string(),
            authenticator: auth,
            timestamp,
            version: 0x30,
            system_type: None,
            mode: BindMode::default(),
            login_mode: Some(0),
        });
        let bytes = SmgpAdapter.encode(&bind, Sequence::Plain(self.next_seq())).expect("encode login");
        RawPdu::from(bytes)
    }

    fn build_long_submit_pdus(&self, content: &[u8], msg_fmt: u8) -> Vec<RawPdu> {
        let alphabet = match msg_fmt {
            8 => SmsAlphabet::UCS2,
            _ => SmsAlphabet::ASCII,
        };
        let encoding = match msg_fmt {
            8 => Encoding::Ucs2,
            _ => Encoding::Ascii,
        };
        let mut splitter = LongMessageSplitter::new();
        let frames = splitter.split(content, alphabet);

        frames.iter().map(|frame| {
            // 窄腰：传 concat + 纯载荷，由 adapter 重建 UDH 并置 SMGP TP_UDHI 可选参数 TLV。
            let (concat, payload) = frame_to_concat(frame);
            let submit = UnifiedMessage::Submit(UnifiedSubmit {
                src: Address::plain("106900"),
                dests: vec![Address::plain("13800138000")],
                content: payload,
                encoding,
                want_report: false,
                concat,
                extra: ProtocolExtra::None,
                tlvs: vec![],
            });
            let bytes = SmgpAdapter.encode(&submit, Sequence::Plain(self.next_seq())).expect("encode submit");
            RawPdu::from(bytes)
        }).collect()
    }
}

#[async_trait]
impl ClientHandler for LongMsgClientHandler {
    fn name(&self) -> &'static str {
        "longmsg-client"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let unified = match SmgpAdapter.decode(frame) {
            Ok(m) => m,
            Err(_) => return Ok(()),
        };

        match unified {
            UnifiedMessage::BindResp(resp) => {
                if resp.status == 0 {
                    self.connected.store(true, Ordering::Relaxed);
                }
            }
            UnifiedMessage::SubmitResp(_) => {
                self.submit_resp_count.fetch_add(1, Ordering::Relaxed);
            }
            // is_report=0 的 MO 分段：据 concat 重建含 UDH 段供合包断言复用，并回 DeliverResp。
            UnifiedMessage::Deliver(d) => {
                self.deliver_segments
                    .lock()
                    .unwrap()
                    .push(seg_with_udh(&d.concat, &d.content));
                let resp_bytes = SmgpAdapter.encode(&UnifiedMessage::DeliverResp, SmgpAdapter.sequence_of(frame))?;
                ctx.conn.write_frame(&resp_bytes).await?;
            }
            _ => {}
        }
        Ok(())
    }
}

async fn start_server(
    biz_handler: Arc<dyn BusinessHandler>,
) -> Result<(u16, Arc<rsms_connector::ConnectionPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new("test-server", "127.0.0.1", 0, 8, 30).with_protocol(Protocol::Smgp));
    let server = ServerBuilder::new(cfg)
        .handlers(vec![biz_handler])
        .auth_handler(Arc::new(PasswordAuthHandler))
        .account_config_provider(Arc::new(MockAccountConfigProvider) as Arc<dyn AccountConfigProvider>)
        .serve()
        .await
        .expect("bind");
    let port = server.local_addr.port();
    let pool = server.pool();
    let handle = tokio::spawn(async move { let _ = server.run().await; });
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok((port, pool, handle))
}

async fn connect_client(port: u16) -> Result<(Arc<LongMsgClientHandler>, Arc<ClientConnection>)> {
    let endpoint = Arc::new(EndpointConfig::new("test-client", "127.0.0.1", port, 8, 30));
    let handler = Arc::new(LongMsgClientHandler::new());
    let conn = ClientBuilder::new(endpoint, handler.clone(), SmgpDecoder)
        .client_config(ClientConfig::new())
        .connect()
        .await?;
    let login_pdu = handler.build_login_pdu();
    conn.send_request(login_pdu).await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(handler.connected.load(Ordering::Relaxed), "连接失败");
    Ok((handler, conn))
}

fn build_deliver_mo_pdu(seq_id: u32, src: &str, dest: &str, msg_fmt: u8, concat: Option<Concat>, payload: Vec<u8>) -> RawPdu {
    let encoding = match msg_fmt {
        8 => Encoding::Ucs2,
        0 => Encoding::Ascii,
        15 => Encoding::Gbk,
        other => Encoding::Other(other),
    };
    // 窄腰：传 concat + 纯载荷，由 adapter 重建 UDH 并置 SMGP TP_UDHI 可选参数 TLV。
    let unified = UnifiedMessage::Deliver(UnifiedDeliver {
        src: Address::plain(src),
        dest: Address::plain(dest),
        content: payload,
        encoding,
        concat,
        extra: ProtocolExtra::None,
        tlvs: vec![],
    });
    let bytes = SmgpAdapter.encode(&unified, Sequence::Plain(seq_id)).expect("encode MO");
    RawPdu::from(bytes)
}

fn merge_deliver_segments(segments: &[Vec<u8>]) -> Vec<u8> {
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

fn merge_submit_segments(segments: &[Vec<u8>]) -> Vec<u8> {
    let mut merger = LongMessageMerger::new();
    let mut result = None;
    for content in segments {
        let has_udhi = UdhParser::has_udhi(content);
        let (reference_id, total_segments, segment_number) = if has_udhi {
            if let Some((udh, _)) = UdhParser::extract_udh(content) {
                (udh.reference_id, udh.total_segments, udh.segment_number)
            } else {
                (0, 1, 1)
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

// ==================== 测试用例 ====================

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
async fn test_longmsg_mt_submit() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let biz = Arc::new(LongMsgBizHandler::new());
    let (port, _pool, handle) = start_server(biz.clone()).await.unwrap();

    let (client, conn) = connect_client(port).await.unwrap();

    let original = "这是一条SMGP长短信测试消息，内容足够长以触发拆分。包含中文字符和English characters混合内容，确保UCS2编码下拆分合包正确。"
        .as_bytes()
        .to_vec();

    let submit_pdus = client.build_long_submit_pdus(&original, 8);
    assert!(submit_pdus.len() > 1, "长短信应拆分为多个Submit PDU");

    for pdu in &submit_pdus {
        conn.send_request(pdu.clone()).await.expect("send submit");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let segments = biz.received_segments.lock().unwrap();
    assert_eq!(segments.len(), submit_pdus.len(), "服务端应收到的分段数不匹配");

    for seg in segments.iter() {
        // adapter 据 concat 重建的段字节应含 UDH（seg_with_udh 已在 biz handler 重建）。
        assert!(UdhParser::has_udhi(seg), "重建的分段字节应包含 UDH");
    }

    let merged = merge_submit_segments(&segments);
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

    let original = "这是一条从手机终端上行的SMGP长短信测试消息，手机用户发送了一条超过70个Unicode字符的长消息，网关需要将其拆分为多个Deliver分段下发给SP。验证接收方能否正确合包。"
        .as_bytes()
        .to_vec();

    let mut splitter = LongMessageSplitter::new();
    let frames = splitter.split(&original, SmsAlphabet::UCS2);
    assert!(frames.len() > 1, "长短信应拆分为多个分段");

    let server_conn = pool.first().await.expect("应有一个服务端连接");

    for (i, frame) in frames.iter().enumerate() {
        let (concat, payload) = frame_to_concat(frame);
        let deliver_pdu = build_deliver_mo_pdu(
            (100 + i) as u32,
            "13800138000",
            "106900",
            8,
            concat,
            payload,
        );
        server_conn.write_frame(deliver_pdu.as_slice()).await.expect("send deliver segment");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let deliver_segments = client.deliver_segments.lock().unwrap();
    assert_eq!(deliver_segments.len(), frames.len(), "客户端应收到的Deliver分段数不匹配");

    let merged = merge_deliver_segments(&deliver_segments);
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

    let mt_content = "MT长短信测试：从SP下发到手机用户的长短信，验证SMGP协议下Submit拆分和合包的完整流程。"
        .as_bytes()
        .to_vec();

    let submit_pdus = client.build_long_submit_pdus(&mt_content, 8);
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

    let mo_content = "MO长短信测试：从手机用户上行到SP的长短信，验证SMGP协议下Deliver拆分和合包的完整流程。手机用户发送长消息。"
        .as_bytes()
        .to_vec();

    let mut splitter = LongMessageSplitter::new();
    let deliver_frames = splitter.split(&mo_content, SmsAlphabet::UCS2);

    let server_conn = pool.first().await.expect("应有一个服务端连接");
    for (i, frame) in deliver_frames.iter().enumerate() {
        let (concat, payload) = frame_to_concat(frame);
        let deliver_pdu = build_deliver_mo_pdu(
            (200 + i) as u32,
            "13800138000",
            "106900",
            8,
            concat,
            payload,
        );
        server_conn.write_frame(deliver_pdu.as_slice()).await.expect("send deliver");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let deliver_segments = client.deliver_segments.lock().unwrap();
    assert_eq!(deliver_segments.len(), deliver_frames.len());
    let merged = merge_deliver_segments(&deliver_segments);
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

    let original = "验证每个分段Submit都返回成功的SubmitResp，status=0。这条消息足够长以产生多个分段，确保全部分段都成功提交。"
        .as_bytes()
        .to_vec();

    let submit_pdus = client.build_long_submit_pdus(&original, 8);
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
