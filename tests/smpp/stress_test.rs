use async_trait::async_trait;
use rsms_connector::{
    ServerBuilder, ClientBuilder, SmppDecoder,
    AuthCredentials, AuthHandler, AuthResult,
    AccountConfigProvider,
    protocol::MessageSource,
};
use rsms_connector::client::{ClientContext, ClientConfig, ClientHandler};
use rsms_core::{ConnectionInfo, RawPdu, EndpointConfig, Protocol, Frame, Result};
// 窄腰统一模型：构造/解码全程走 SmppAdapter，不再裸 codec。
use rsms_codec_smpp::adapter::SmppAdapter;
use rsms_model::{
    Address, BindMode, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra,
    Sequence, SmppExtra, UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit,
    UnifiedSubmitResp,
};
use rsms_test_common::{
    TestStats, StressMockMessageSource, MockAccountConfigProvider,
    rand_u32, format_timestamp, print_stress_results, StressTestResults,
    drain_wait_submit_resp, drain_wait_queue_and_reports_single, drain_wait_final_single,
    spawn_stats_monitor,
};
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::time::{Duration, Instant};

const STRESS_TEST_SYSTEM_ID: &str = "smppcli";
const STRESS_TEST_PASSWORD: &str = "pwd12345";
const STRESS_TEST_DURATION_SECS: u64 = 30;
const STRESS_TEST_RATE: f64 = 2500.0;
const SMPP_V34: u8 = 0x34;
const SMPP_V50: u8 = 0x50;

#[derive(Clone)]
struct ReportItem {
    msg_id: String,
    conn_id: u64,
    dest_id: String,
}

impl ReportItem {
    fn to_bytes(&self) -> Vec<u8> {
        let msg_bytes = self.msg_id.as_bytes();
        let mut buf = Vec::with_capacity(4 + msg_bytes.len() + 8 + self.dest_id.len());
        buf.extend_from_slice(&(msg_bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(msg_bytes);
        buf.extend_from_slice(&self.conn_id.to_be_bytes());
        buf.extend_from_slice(self.dest_id.as_bytes());
        buf
    }

    fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 12 {
            return None;
        }
        let msg_id_len = u32::from_be_bytes(data[0..4].try_into().ok()?) as usize;
        if data.len() < 4 + msg_id_len + 8 {
            return None;
        }
        let msg_id = String::from_utf8(data[4..4 + msg_id_len].to_vec()).ok()?;
        let conn_id = u64::from_be_bytes(data[4 + msg_id_len..12 + msg_id_len].try_into().ok()?);
        let dest_id = String::from_utf8(data[12 + msg_id_len..].to_vec()).ok()?;
        Some(Self { msg_id, conn_id, dest_id })
    }
}

#[allow(dead_code)]
struct ClientState {
    connected: AtomicBool,
    login_status: Mutex<Option<u32>>,
    seq: AtomicU64,
    msg_ids: Arc<RwLock<VecDeque<String>>>,
    matched_msg_ids: Arc<Mutex<HashSet<String>>>,
    stats: Arc<TestStats>,
}

impl ClientState {
    fn new(stats: Arc<TestStats>) -> Self {
        Self {
            connected: AtomicBool::new(false),
            login_status: Mutex::new(None),
            seq: AtomicU64::new(1),
            msg_ids: Arc::new(RwLock::new(VecDeque::new())),
            matched_msg_ids: Arc::new(Mutex::new(HashSet::new())),
            stats,
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u32
    }

    /// BindTransmitter（统一模型）：SMPP 明文鉴权，口令明文字节进 authenticator。version 透传。
    pub fn build_bind_transmitter_pdu(&self, version: u8) -> RawPdu {
        let bind = UnifiedMessage::Bind(UnifiedBind {
            client_id: STRESS_TEST_SYSTEM_ID.to_string(),
            authenticator: STRESS_TEST_PASSWORD.as_bytes().to_vec(),
            timestamp: 0,
            version,
            system_type: Some("CMT".to_string()),
            mode: BindMode::Transmitter,
            login_mode: None,
        });
        RawPdu::from(SmppAdapter.encode(&bind, Sequence::Plain(self.next_seq())).expect("encode bind"))
    }

    /// SubmitSm（统一模型）：registered_delivery=1 落 SmppExtra；data_coding=0x03→Encoding::Other(0x03)。
    pub fn build_submit_sm_pdu(&self, src: &str, dst: &str, content: &str) -> RawPdu {
        let submit = UnifiedMessage::Submit(UnifiedSubmit {
            src: Address::plain(src),
            dests: vec![Address { number: dst.to_string(), ton: Some(1), npi: Some(1) }],
            content: content.as_bytes().to_vec(),
            encoding: Encoding::Other(0x03),
            want_report: true,
            concat: None,
            extra: ProtocolExtra::Smpp(SmppExtra {
                esm_class: 0,
                registered_delivery: 1,
                ..Default::default()
            }),
            tlvs: vec![],
        });
        RawPdu::from(SmppAdapter.encode(&submit, Sequence::Plain(self.next_seq())).expect("encode submit"))
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl ClientHandler for ClientState {
    fn name(&self) -> &'static str {
        "stress_client"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let pdu = frame.data_as_slice();
        if pdu.len() < 16 {
            return Ok(());
        }

        // 消息类型经 SmppAdapter 识别；BindResp/SubmitResp 的 command_status 不透传，裸读 bytes 8..12（规则3）。
        let status = u32::from_be_bytes([pdu[8], pdu[9], pdu[10], pdu[11]]);
        let unified = match SmppAdapter.decode(frame) {
            Ok(m) => m,
            Err(_) => return Ok(()),
        };

        match unified {
            UnifiedMessage::BindResp(_) => {
                *self.login_status.lock().unwrap() = Some(status);
                if status == 0 {
                    self.connected.store(true, Ordering::Relaxed);
                }
            }
            UnifiedMessage::SubmitResp(resp) => {
                self.stats.submit_resp_received.fetch_add(1, Ordering::Relaxed);
                if status == 0 {
                    // adapter 已把 message_id 落入 SubmitResp.msg_id（SMPP 用 MessageId::Text）。
                    let msg_id = match resp.msg_id {
                        MessageId::Text(t) => t,
                        MessageId::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                    };
                    if !msg_id.is_empty() {
                        tracing::trace!("[Client] SubmitResp received, stored msg_id: {}", msg_id);
                        self.msg_ids.write().unwrap().push_back(msg_id);
                    }
                } else {
                    self.stats.submit_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
            // esm_class=0x04 的 DeliverSm → 统一模型 Report（report 文本在 raw）。
            UnifiedMessage::Report(report) => {
                let content = String::from_utf8_lossy(&report.raw).to_string();
                if let Some(report_msg_id) = parse_msg_id_from_report(&content) {
                    tracing::trace!("[Client] Received report: {}", report_msg_id);
                    self.stats.report_received.fetch_add(1, Ordering::Relaxed);
                    let already_matched = self.matched_msg_ids.lock().unwrap().contains(&report_msg_id);
                    if !already_matched {
                        let mut pending = self.msg_ids.write().unwrap();
                        if let Some(pos) = pending.iter().position(|id| *id == report_msg_id) {
                            pending.remove(pos);
                            self.matched_msg_ids.lock().unwrap().insert(report_msg_id);
                            self.stats.report_matched.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                return build_deliver_sm_resp(ctx, frame).await;
            }
            // 普通 MO → 统一模型 Deliver。
            UnifiedMessage::Deliver(d) => {
                self.stats.mo_received.fetch_add(1, Ordering::Relaxed);
                tracing::trace!("[Client] Received MO from {}: {:?}", d.src.number, &d.content);
                return build_deliver_sm_resp(ctx, frame).await;
            }
            _ => {}
        }

        Ok(())
    }
}

/// 回 DeliverSmResp（统一模型 DeliverResp，经 adapter 编码，sequence_of 回显请求序列）。
async fn build_deliver_sm_resp(ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
    let resp_bytes = SmppAdapter.encode(&UnifiedMessage::DeliverResp, SmppAdapter.sequence_of(frame))?;
    ctx.conn.write_frame(&resp_bytes).await
}

fn parse_msg_id_from_report(content: &str) -> Option<String> {
    let parts: Vec<&str> = content.split_whitespace().collect();
    if parts.len() >= 1 && parts[0].starts_with("id:") {
        let id_str = parts[0].trim_start_matches("id:");
        if !id_str.is_empty() {
            return Some(id_str.to_string());
        }
    }
    None
}

#[allow(dead_code)]
struct ServerHandler {
    submit_count: AtomicU64,
    report_sent: AtomicU64,
    mo_sent: AtomicU64,
    msg_source: Arc<StressMockMessageSource>,
}

impl ServerHandler {
    #[allow(dead_code)]
    fn new(msg_source: Arc<StressMockMessageSource>) -> Self {
        Self {
            submit_count: AtomicU64::new(0),
            report_sent: AtomicU64::new(0),
            mo_sent: AtomicU64::new(0),
            msg_source,
        }
    }

    #[allow(dead_code)]
    fn increment_report_sent(&self) {
        self.report_sent.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    fn increment_mo_sent(&self) {
        self.mo_sent.fetch_add(1, Ordering::Relaxed);
    }
}

#[async_trait]
impl rsms_business::BusinessHandler for ServerHandler {
    fn name(&self) -> &'static str {
        "stress-server-handler"
    }

    async fn on_inbound(&self, ctx: &rsms_business::InboundContext, frame: &Frame) -> Result<()> {
        if let Ok(msg) = SmppAdapter.decode(frame) {
            match msg {
                UnifiedMessage::Submit(s) => {
                    let count = self.submit_count.fetch_add(1, Ordering::Relaxed);
                    let msg_id = format!("{}", count);

                    tracing::trace!("[Server] Received Submit #{}", count + 1);

                    // 回 SubmitResp：msg_id 用 MessageId::Text，sequence_of 回显请求序列。
                    let resp = UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                        msg_id: MessageId::Text(msg_id.clone()),
                        status: 0,
                    });
                    let resp_bytes = SmppAdapter.encode(&resp, SmppAdapter.sequence_of(frame))?;
                    ctx.conn.write_frame(&resp_bytes).await?;

                    let dest_id = s.dests.first().map(|a| a.number.clone()).unwrap_or_default();
                    let item = ReportItem {
                        msg_id,
                        conn_id: ctx.conn.id(),
                        dest_id,
                    };
                    self.msg_source.push_item(STRESS_TEST_SYSTEM_ID, item.to_bytes()).await;
                }
                UnifiedMessage::DeliverResp => {
                    tracing::trace!("[Server] Received DeliverSmResp");
                }
                other => {
                    tracing::debug!("[Server] Received other message: {:?}", std::mem::discriminant(&other));
                }
            }
        } else {
            tracing::warn!("[Server] Failed to decode message");
        }
        Ok(())
    }
}

pub struct PasswordAuthHandler {
    accounts: std::collections::HashMap<String, String>,
}

impl PasswordAuthHandler {
    pub fn new() -> Self {
        Self {
            accounts: std::collections::HashMap::new(),
        }
    }

    pub fn add_account(mut self, system_id: &str, password: &str) -> Self {
        self.accounts.insert(system_id.to_string(), password.to_string());
        self
    }
}

#[async_trait]
impl AuthHandler for PasswordAuthHandler {
    fn name(&self) -> &'static str {
        "smpp-password-auth"
    }

    async fn authenticate(&self, _client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        if let AuthCredentials::Smpp { system_id, password, .. } = credentials {
            if let Some(expected_password) = self.accounts.get(&system_id) {
                if *expected_password == password {
                    return Ok(AuthResult::success(&system_id));
                }
            }
            Ok(AuthResult::failure(1, "Invalid password"))
        } else {
            Ok(AuthResult::failure(1, "Invalid credentials"))
        }
    }
}

async fn start_test_server(
    biz_handler: Arc<dyn rsms_business::BusinessHandler>,
) -> Result<(u16, Arc<rsms_connector::AccountPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "stress-test-server",
        "127.0.0.1",
        0,
        500,
        60,
    ).with_protocol(Protocol::Smpp).with_log_level(tracing::Level::WARN));
    let auth = Arc::new(PasswordAuthHandler::new().add_account(STRESS_TEST_SYSTEM_ID, STRESS_TEST_PASSWORD));
    let server = ServerBuilder::new(cfg)
        .handlers(vec![biz_handler])
        .auth_handler(auth)
        .account_config_provider(Arc::new(MockAccountConfigProvider::with_limits(5000, 2048)) as Arc<dyn AccountConfigProvider>)
        .serve()
        .await
        .expect("bind");
    let port = server.local_addr.port();
    let account_pool = server.account_pool();
    let handle = tokio::spawn(async move {
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok((port, account_pool, handle))
}

async fn report_generator_task(
    msg_source: Arc<StressMockMessageSource>,
    account_pool: Arc<rsms_connector::AccountPool>,
    report_sent: Arc<AtomicU64>,
    target_rate: f64,
) {
    let inter_msg_interval = Duration::from_secs_f64(1.0 / target_rate);
    let mut interval = tokio::time::interval(inter_msg_interval);

    loop {
        interval.tick().await;

        let raw_items = msg_source.fetch_bytes(STRESS_TEST_SYSTEM_ID, 100).await;
        let items: Vec<ReportItem> = raw_items.into_iter()
            .filter_map(|b| ReportItem::from_bytes(&b))
            .collect();

        for item in items {
            if let Some(acc) = account_pool.get(STRESS_TEST_SYSTEM_ID).await {
                if let Some(conn) = acc.get_connection_by_id(item.conn_id).await {
                    let now = format_timestamp(true);
                    let report_content = format!(
                        "id:{} sub:001 dlvrd:001 submit date:{} done date:{} stat:DELIVRD err:000 text:Hello",
                        item.msg_id, now, now
                    );

                    // 投递回执 → 统一模型 Report，adapter 自动产出 esm_class=0x04 的 DeliverSm；
                    // report 文本作为 raw 落入 short_message（client 从 raw 解析 id:）。
                    let report = UnifiedMessage::Report(UnifiedReport {
                        msg_id: MessageId::Text(item.msg_id.clone()),
                        status: DeliveryStatus::Delivered,
                        src: Address { number: "13800138000".to_string(), ton: Some(1), npi: Some(1) },
                        dest: Address { number: item.dest_id, ton: Some(0), npi: Some(0) },
                        raw: report_content.into_bytes(),
                    });
                    let pdu = SmppAdapter.encode(&report, Sequence::Plain(rand_u32())).expect("encode report");
                    match conn.write_frame(&pdu).await {
                        Ok(()) => {
                            report_sent.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            tracing::debug!("Failed to send Report: {:?}", e);
                        }
                    }
                }
            }
        }
    }
}

async fn mo_generator_task(
    account_pool: Arc<rsms_connector::AccountPool>,
    mo_sent: Arc<AtomicU64>,
    target_rate: f64,
) {
    let inter_msg_interval = Duration::from_secs_f64(1.0 / target_rate);
    let mut interval = tokio::time::interval(inter_msg_interval);

    let src_numbers = ["13800138000", "13800138001", "13800138002", "13800138003", "13800138004"];

    loop {
        interval.tick().await;

        if let Some(acc) = account_pool.get(STRESS_TEST_SYSTEM_ID).await {
            if let Some(conn) = acc.first_connection().await {
                let src = src_numbers[rand_u32() as usize % src_numbers.len()];
                let content = format!("MO Test #{}", mo_sent.load(Ordering::Relaxed) + 1);

                // 普通 MO（esm_class=0）→ 统一模型 Deliver；data_coding=0x03→Encoding::Other(0x03)。
                let deliver = UnifiedMessage::Deliver(UnifiedDeliver {
                    src: Address { number: src.to_string(), ton: Some(1), npi: Some(1) },
                    dest: Address { number: STRESS_TEST_SYSTEM_ID.to_string(), ton: Some(0), npi: Some(0) },
                    content: content.into_bytes(),
                    encoding: Encoding::Other(0x03),
                    concat: None,
                    extra: ProtocolExtra::Smpp(SmppExtra { esm_class: 0, ..Default::default() }),
                    tlvs: vec![],
                });
                let pdu = SmppAdapter.encode(&deliver, Sequence::Plain(rand_u32())).expect("encode mo");
                match conn.write_frame(&pdu).await {
                    Ok(()) => {
                        mo_sent.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        tracing::debug!("Failed to send MO: {:?}", e);
                    }
                }
            }
        }
    }
}

/// 构造 MT SubmitSm 字节（统一模型）→ push 给 MessageSource 队列异步发出。
/// registered_delivery=1 落 SmppExtra；data_coding=0x03→Encoding::Other(0x03)。
fn build_smpp_submit_pdu(src: &str, dst: &str, content: &str, seq: u32) -> Vec<u8> {
    let submit = UnifiedMessage::Submit(UnifiedSubmit {
        src: Address::plain(src),
        dests: vec![Address { number: dst.to_string(), ton: Some(1), npi: Some(1) }],
        content: content.as_bytes().to_vec(),
        encoding: Encoding::Other(0x03),
        want_report: true,
        concat: None,
        extra: ProtocolExtra::Smpp(SmppExtra {
            esm_class: 0,
            registered_delivery: 1,
            ..Default::default()
        }),
        tlvs: vec![],
    });
    SmppAdapter.encode(&submit, Sequence::Plain(seq)).expect("encode submit")
}

async fn mt_producer_task(
    msg_source: Arc<StressMockMessageSource>,
    stats: Arc<TestStats>,
    target_rate: f64,
) {
    let inter_msg_interval = Duration::from_secs_f64(1.0 / target_rate);
    let mut interval = tokio::time::interval(inter_msg_interval);

    let src_numbers = ["13800138000", "13800138001", "13800138002", "13800138003", "13800138004"];
    let mut seq: u64 = 1;
    let mut msg_count: u64 = 0;

    loop {
        interval.tick().await;

        let src = src_numbers[msg_count as usize % src_numbers.len()];
        let content = format!("MT Test #{}", msg_count);
        let pdu_bytes = build_smpp_submit_pdu(src, STRESS_TEST_SYSTEM_ID, &content, seq as u32);

        if msg_source.push("stress-client", pdu_bytes).await.is_ok() {
            stats.submit_sent.fetch_add(1, Ordering::Relaxed);
        }

        seq += 1;
        msg_count += 1;
    }
}

#[tokio::test]
async fn stress_test_smpp_v34_1connection() {
    run_stress_test(SMPP_V34, 1).await;
}

#[tokio::test]
async fn stress_test_smpp_v34_5connections() {
    run_stress_test(SMPP_V34, 5).await;
}

#[tokio::test]
async fn stress_test_smpp_v50_1connection() {
    run_stress_test(SMPP_V50, 1).await;
}

#[tokio::test]
async fn stress_test_smpp_v50_5connections() {
    run_stress_test(SMPP_V50, 5).await;
}

async fn run_stress_test(version: u8, num_connections: usize) {
    let total_start = Instant::now();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    println!("\n");
    println!("==========================================");
    println!("SMPP Stress Test - V{:02X} {} Connection(s)", version, num_connections);
    println!("==========================================");
    println!("Account: {}", STRESS_TEST_SYSTEM_ID);
    println!("Connections: {}", num_connections);
    println!("Target Rate: {} msg/s", STRESS_TEST_RATE);
    println!("  - MT (Submit): ~{} msg/s", STRESS_TEST_RATE * 0.6);
    println!("  - Report: ~{} msg/s", STRESS_TEST_RATE * 0.2);
    println!("  - MO: ~{} msg/s", STRESS_TEST_RATE * 0.2);
    println!("Duration: {} seconds", STRESS_TEST_DURATION_SECS);
    println!("==========================================\n");

    let stats = Arc::new(TestStats::new());
    let msg_source = Arc::new(StressMockMessageSource::new());
    let server_handler = Arc::new(ServerHandler::new(msg_source.clone()));
    let (port, account_pool, server_handle) = start_test_server(server_handler.clone()).await.unwrap();

    tracing::warn!("Server started on port {}", port);

    let mt_rate = 2500.0;
    let report_rate = 2500.0;
    let mo_rate = 1250.0;

    let report_sent = Arc::new(AtomicU64::new(0));
    let mo_sent = Arc::new(AtomicU64::new(0));

    let report_gen_handle = tokio::spawn(report_generator_task(
        msg_source.clone(),
        account_pool.clone(),
        report_sent.clone(),
        report_rate,
    ));

    let mo_gen_handle = tokio::spawn(mo_generator_task(
        account_pool.clone(),
        mo_sent.clone(),
        mo_rate,
    ));

    tokio::time::sleep(Duration::from_secs(1)).await;

    let mut client_conns = Vec::new();

    for i in 0..num_connections {
        let client_state = Arc::new(ClientState::new(stats.clone()));
        let endpoint = Arc::new(EndpointConfig::new(
            "stress-client",
            "127.0.0.1",
            port,
            if num_connections == 1 { 1024 } else { 2048 },
            30,
        ).with_window_size(2048).with_protocol(Protocol::Smpp).with_log_level(tracing::Level::WARN));

        let mut conn = None;
        for retry in 0..50 {
            match ClientBuilder::new(endpoint.clone(), client_state.clone(), SmppDecoder)
                .client_config(ClientConfig::new())
                .message_source(msg_source.clone() as Arc<dyn MessageSource>)
                .connect()
                .await
            {
                Ok(c) => {
                    conn = Some(c);
                    break;
                }
                Err(e) => {
                    tracing::warn!("Connection {} attempt {} failed: {:?}", i, retry, e);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }

        let conn = conn.expect("Failed to establish connection after retries");

        let bind_pdu = client_state.build_bind_transmitter_pdu(version);
        conn.write_frame(bind_pdu.as_slice()).await.expect("send bind");

        tokio::time::sleep(Duration::from_millis(100)).await;

        let mut retries = 0;
        while !client_state.is_connected() && retries < 30 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            retries += 1;
        }

        assert!(client_state.is_connected(), "Connection {} failed after {} retries", i, retries);
        tracing::warn!("Client {} connected", i);

        client_conns.push(conn.clone());

        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let producer_handle = tokio::spawn(mt_producer_task(
        msg_source.clone(),
        stats.clone(),
        mt_rate,
    ));

    let warmup_secs = total_start.elapsed().as_secs_f64();
    stats.start();

    let monitor_handle = spawn_stats_monitor(
        stats.clone(),
        msg_source.clone(),
        &format!("SMPP V{:02X}", version),
        STRESS_TEST_DURATION_SECS,
        1,
        Some(STRESS_TEST_SYSTEM_ID.to_string()),
    );

    tokio::time::sleep(Duration::from_secs(STRESS_TEST_DURATION_SECS)).await;

    stats.end();

    producer_handle.abort();

    drain_wait_submit_resp(&stats, Duration::from_secs(10)).await;

    let report_sent_val = report_sent.load(Ordering::Relaxed);
    drain_wait_queue_and_reports_single(
        &stats, &msg_source, STRESS_TEST_SYSTEM_ID, report_sent_val, Duration::from_secs(15),
    ).await;

    report_gen_handle.abort();
    mo_gen_handle.abort();

    let report_sent_val = report_sent.load(Ordering::Relaxed);
    let mo_sent_val = mo_sent.load(Ordering::Relaxed);
    drain_wait_final_single(&stats, report_sent_val, mo_sent_val, Duration::from_secs(5)).await;

    monitor_handle.abort();

    let total_secs = total_start.elapsed().as_secs_f64();
    let results = StressTestResults::from_stats(&stats, report_sent_val, mo_sent_val, warmup_secs, total_secs);
    print_stress_results(&results, &format!("SMPP V{:02X}", version), &format!("Stress Test ({} connections)", num_connections));

    server_handle.abort();

    let submit_sent = results.submit_sent;
    let submit_resp = results.submit_resp;
    let report_matched = results.report_matched;
    let mo_recv = results.mo_received;
    let stress_secs = results.stress_secs;

    let actual_mt_qps = submit_sent as f64 / stress_secs;
    let actual_mo_qps = mo_recv as f64 / stress_secs;

    let expected_min_mt = (mt_rate * (STRESS_TEST_DURATION_SECS as f64) * 0.4) as u64;
    let expected_min_mo = (mo_rate * (STRESS_TEST_DURATION_SECS as f64) * 0.3) as u64;

    assert!(
        submit_sent >= expected_min_mt,
        "Expected at least {} MT messages, got {} ({:.1} QPS)",
        expected_min_mt,
        submit_sent,
        actual_mt_qps
    );

    let match_ratio = if submit_resp > 0 {
        report_matched as f64 / submit_resp as f64
    } else {
        0.0
    };

    assert!(
        report_matched >= submit_resp.saturating_sub(100),
        "Report MsgId should match SubmitResp MsgId (1:1), got {}/{} ({:.1}% match)",
        report_matched,
        submit_resp,
        match_ratio * 100.0
    );

    assert!(
        mo_recv >= expected_min_mo,
        "Expected at least {} MO messages, got {} ({:.1} QPS)",
        expected_min_mo,
        mo_recv,
        actual_mo_qps
    );
}
