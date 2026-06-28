use async_trait::async_trait;
use rsms_connector::{
    ServerBuilder, ClientBuilder, SgipDecoder, NoopClientHandler,
    AuthCredentials, AuthHandler, AuthResult,
    AccountConfigProvider,
    protocol::MessageSource,
};
use rsms_connector::client::ClientConfig;
use rsms_business::{MessageContext, MessageHandler};
use rsms_core::{ConnectionInfo, EncodedPdu, RawPdu, EndpointConfig, Protocol, Result};
// 窄腰统一模型：收发一律走 SgipAdapter + UnifiedMessage，不再手构裸 codec / 手剥头部字节。
use rsms_codec_sgip::adapter::SgipAdapter;
use rsms_model::{
    Address, DeliveryStatus, MessageId, ProtocolAdapter, ProtocolExtra, Sequence, SgipExtra,
    UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit, UnifiedSubmitResp,
};
use rsms_test_common::{
    TestStats, StressMockMessageSource, MockAccountConfigProvider, rand_u32,
    print_stress_results, StressTestResults,
    drain_wait_submit_resp, drain_wait_queue_and_reports_single, drain_wait_final_single,
    spawn_stats_monitor,
};
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::time::{Duration, Instant};

const STRESS_TEST_ACCOUNT: &str = "106900";
const STRESS_TEST_PASSWORD: &str = "password123";
const STRESS_TEST_DURATION_SECS: u64 = 30;
const STRESS_TEST_RATE: f64 = 2500.0;
const SGIP_NODE_ID: u32 = 1;
const SGIP_TIMESTAMP: u32 = 0x04051200;

#[derive(Clone)]
struct ReportItem {
    submit_seq_number: u32,
    conn_id: u64,
    dest_number: String,
}

impl ReportItem {
    fn to_bytes(&self) -> Vec<u8> {
        let dest_bytes = self.dest_number.as_bytes();
        let mut buf = Vec::with_capacity(4 + 8 + 4 + dest_bytes.len());
        buf.extend_from_slice(&self.submit_seq_number.to_be_bytes());
        buf.extend_from_slice(&self.conn_id.to_be_bytes());
        buf.extend_from_slice(&(dest_bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(dest_bytes);
        buf
    }

    fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 16 {
            return None;
        }
        let submit_seq_number = u32::from_be_bytes(data[0..4].try_into().ok()?);
        let conn_id = u64::from_be_bytes(data[4..12].try_into().ok()?);
        let dest_len = u32::from_be_bytes(data[12..16].try_into().ok()?) as usize;
        if data.len() < 16 + dest_len {
            return None;
        }
        let dest_number = String::from_utf8(data[16..16 + dest_len].to_vec()).ok()?;
        Some(Self { submit_seq_number, conn_id, dest_number })
    }
}

struct SharedSeqState {
    seq: AtomicU64,
    pending_seq_numbers: RwLock<VecDeque<u32>>,
    matched_seq_numbers: Mutex<HashSet<u32>>,
}

impl SharedSeqState {
    fn new() -> Self {
        Self {
            seq: AtomicU64::new(1),
            pending_seq_numbers: RwLock::new(VecDeque::new()),
            matched_seq_numbers: Mutex::new(HashSet::new()),
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u32
    }
}

#[allow(dead_code)]
struct ClientState {
    connected: AtomicBool,
    login_status: Mutex<Option<u32>>,
    shared: Arc<SharedSeqState>,
    stats: Arc<TestStats>,
}

impl ClientState {
    fn new(stats: Arc<TestStats>, shared: Arc<SharedSeqState>) -> Self {
        Self {
            connected: AtomicBool::new(false),
            login_status: Mutex::new(None),
            shared,
            stats,
        }
    }

    pub fn build_bind_pdu(&self, login_name: &str, login_password: &str) -> RawPdu {
        // 明文认证：authenticator 装口令字节；version 承载 login_type=1。复合序列 number 走 next_seq。
        let bind = UnifiedMessage::Bind(UnifiedBind {
            client_id: login_name.to_string(),
            authenticator: login_password.as_bytes().to_vec(),
            timestamp: 0,
            version: 1,
            system_type: None,
            mode: rsms_model::BindMode::default(),
            login_mode: None,
        });
        let seq = Sequence::Sgip {
            node_id: SGIP_NODE_ID,
            timestamp: SGIP_TIMESTAMP,
            number: self.shared.next_seq(),
        };
        RawPdu::from(SgipAdapter.encode(&bind, seq).expect("encode bind"))
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl MessageHandler for ClientState {
    fn name(&self) -> &'static str {
        "stress_client"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        match msg {
            UnifiedMessage::BindResp(resp) => {
                *self.login_status.lock().unwrap() = Some(resp.status);
                if resp.status == 0 {
                    self.connected.store(true, Ordering::Relaxed);
                }
            }
            UnifiedMessage::SubmitResp(resp) => {
                self.stats.submit_resp_received.fetch_add(1, Ordering::Relaxed);
                if resp.status != 0 {
                    self.stats.submit_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
            UnifiedMessage::Report(report) => {
                self.stats.report_received.fetch_add(1, Ordering::Relaxed);

                // 被报告 Submit 的复合序列承载在 msg_id(12B Binary)，取其 number 分量比对。
                let report_seq = report_number_of(&report.msg_id);
                tracing::trace!("[Client] Report received for seq_number: {}", report_seq);

                // 所有 std 锁操作收敛进同步块：guard 在 await 前全部释放，
                // 避免 RwLock/Mutex guard 进入 async 协程 witness 导致 future 非 Send。
                {
                    let already_matched =
                        self.shared.matched_seq_numbers.lock().unwrap().contains(&report_seq);
                    if !already_matched {
                        let mut pending = self.shared.pending_seq_numbers.write().unwrap();
                        if let Some(pos) = pending.iter().position(|&s| s == report_seq) {
                            pending.remove(pos);
                            self.shared.matched_seq_numbers.lock().unwrap().insert(report_seq);
                            self.stats.report_matched.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }

                return ctx.reply(UnifiedMessage::ReportResp).await;
            }
            UnifiedMessage::Deliver(_) => {
                self.stats.mo_received.fetch_add(1, Ordering::Relaxed);
                tracing::trace!("[Client] Received Deliver/MO");
                return ctx.reply(UnifiedMessage::DeliverResp).await;
            }
            _ => {}
        }

        Ok(())
    }
}

/// 从 UnifiedReport.msg_id(12B Binary: node_id+timestamp+number) 解出 number 分量；
/// 非 12B 时回退 0（与 adapter 的 seq_to_msg_id 反向对称）。
fn report_number_of(msg_id: &MessageId) -> u32 {
    match msg_id {
        MessageId::Binary(b) if b.len() == 12 => u32::from_be_bytes([b[8], b[9], b[10], b[11]]),
        _ => 0,
    }
}

/// 把 SGIP 复合序列三分量打 12B 大端进 MessageId::Binary（作 Report 的 submit_sequence 承载）。
fn seq_to_msg_id(node_id: u32, timestamp: u32, number: u32) -> MessageId {
    let mut b = Vec::with_capacity(12);
    b.extend_from_slice(&node_id.to_be_bytes());
    b.extend_from_slice(&timestamp.to_be_bytes());
    b.extend_from_slice(&number.to_be_bytes());
    MessageId::Binary(b)
}

struct ServerHandler {
    submit_count: AtomicU64,
    msg_source: Arc<StressMockMessageSource>,
}

impl ServerHandler {
    fn new(msg_source: Arc<StressMockMessageSource>) -> Self {
        Self {
            submit_count: AtomicU64::new(0),
            msg_source,
        }
    }
}

#[async_trait]
impl MessageHandler for ServerHandler {
    fn name(&self) -> &'static str {
        "stress-server-handler"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        match msg {
            UnifiedMessage::Submit(s) => {
                let count = self.submit_count.fetch_add(1, Ordering::Relaxed);
                tracing::trace!("[Server] Received Submit #{}", count + 1);

                // 回 SubmitResp：ctx.reply 自动回显请求帧序列（含 SGIP 复合序列）。
                let resp = UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                    msg_id: MessageId::Text(String::new()),
                    status: 0,
                });
                ctx.reply(resp).await?;

                // 被报告 Submit 的复合序列 number 分量：由框架注入的 frame_sequence 取出。
                let submit_seq_number = match ctx.frame_sequence() {
                    Sequence::Sgip { number, .. } => number,
                    Sequence::Plain(n) => n,
                };

                let dest_number = s.dests.first().map(|a| a.number.clone()).unwrap_or_default();
                self.msg_source.push_item(STRESS_TEST_ACCOUNT, ReportItem {
                    submit_seq_number,
                    conn_id: ctx.conn.id(),
                    dest_number,
                }.to_bytes()).await;
            }
            // ReportResp 统一模型独立变体；DeliverResp 同。
            UnifiedMessage::ReportResp => {
                tracing::trace!("[Server] Received ReportResp");
            }
            UnifiedMessage::DeliverResp => {
                tracing::trace!("[Server] Received DeliverResp");
            }
            _ => {
                tracing::debug!("[Server] Received other message");
            }
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

    pub fn add_account(mut self, login_name: &str, password: &str) -> Self {
        self.accounts.insert(login_name.to_string(), password.to_string());
        self
    }
}

#[async_trait]
impl AuthHandler for PasswordAuthHandler {
    fn name(&self) -> &'static str {
        "sgip-password-auth"
    }

    async fn authenticate(&self, _client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        if let AuthCredentials::Sgip { login_name, login_password } = credentials {
            if let Some(expected_password) = self.accounts.get(&login_name) {
                if *expected_password == login_password {
                    return Ok(AuthResult::success(&login_name));
                }
            }
            Ok(AuthResult::failure(1, "Invalid password"))
        } else {
            Ok(AuthResult::failure(1, "Invalid credentials"))
        }
    }
}

async fn start_test_server(
    biz_handler: Arc<dyn MessageHandler>,
) -> Result<(u16, Arc<rsms_connector::AccountPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "sgip-stress-server",
        "127.0.0.1",
        0,
        500,
        60,
    ).with_protocol(Protocol::Sgip).with_log_level(tracing::Level::WARN));
    let auth = Arc::new(PasswordAuthHandler::new().add_account(STRESS_TEST_ACCOUNT, STRESS_TEST_PASSWORD));
    let server = ServerBuilder::new(cfg)
        .message_handlers(vec![biz_handler])
        .auth_handler(auth)
        .account_config_provider(Arc::new(MockAccountConfigProvider::with_limits(10000, 2048)) as Arc<dyn AccountConfigProvider>)
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

fn build_sgip_submit_pdu(sp_number: &str, dest_number: &str, content: &str, seq_num: u32) -> Vec<u8> {
    // 统一模型构造 Submit；SGIP 方言字段经 SgipExtra 传递；复合序列 number=seq_num。
    let submit = UnifiedMessage::Submit(UnifiedSubmit {
        src: Address::plain(sp_number),
        dests: vec![Address::plain(dest_number)],
        content: content.as_bytes().to_vec(),
        encoding: rsms_model::Encoding::Gbk, // msg_fmt=15(GBK)，与原测试一致
        want_report: true,                   // report_flag=1
        concat: None,
        extra: ProtocolExtra::Sgip(SgipExtra {
            charge_number: sp_number.to_string(),
            service_type: "SMS".to_string(),
            fee_type: 2,
            fee_value: "000000".to_string(),
            given_value: "000000".to_string(),
            ..Default::default()
        }),
        tlvs: vec![],
    });
    let seq = Sequence::Sgip { node_id: SGIP_NODE_ID, timestamp: SGIP_TIMESTAMP, number: seq_num };
    SgipAdapter.encode(&submit, seq).expect("encode submit")
}

async fn mt_producer_task(
    msg_source: Arc<StressMockMessageSource>,
    stats: Arc<TestStats>,
    shared: Arc<SharedSeqState>,
    target_rate: f64,
) {
    let inter_msg_interval = Duration::from_secs_f64(1.0 / target_rate);
    let mut interval = tokio::time::interval(inter_msg_interval);

    let src_numbers = ["13800138000", "13800138001", "13800138002", "13800138003", "13800138004"];
    let mut msg_count: u64 = 0;

    loop {
        interval.tick().await;

        let src = src_numbers[msg_count as usize % src_numbers.len()];
        let content = format!("MT Test #{}", msg_count);
        let seq_num = shared.next_seq();
        let pdu_bytes = build_sgip_submit_pdu(src, STRESS_TEST_ACCOUNT, &content, seq_num);

        if msg_source.push("sgip-stress-client", pdu_bytes).await.is_ok() {
            stats.submit_sent.fetch_add(1, Ordering::Relaxed);
            shared.pending_seq_numbers.write().unwrap().push_back(seq_num);
        }

        msg_count += 1;
    }
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

        let raw_items = msg_source.fetch_bytes(STRESS_TEST_ACCOUNT, 100).await;
        let items: Vec<ReportItem> = raw_items.into_iter()
            .filter_map(|b| ReportItem::from_bytes(&b))
            .collect();

        for item in items {
            if let Some(acc) = account_pool.get(STRESS_TEST_ACCOUNT).await {
                if let Some(conn) = acc.first_connection().await {
                    // 独立 Report 命令（统一模型）：被报告 Submit 的复合序列打 12B Binary 进 msg_id；
                    // raw=[report_type, state, error_code]，state 由 status 反映射（Delivered→0）。
                    let report = UnifiedMessage::Report(UnifiedReport {
                        msg_id: seq_to_msg_id(SGIP_NODE_ID, SGIP_TIMESTAMP, item.submit_seq_number),
                        status: DeliveryStatus::Delivered,
                        src: Address::plain(String::new()),
                        dest: Address::plain(item.dest_number),
                        raw: vec![0, 0, 0],
                    });
                    // Report 帧自身的头部复合序列：number 用随机值。
                    let seq = Sequence::Sgip {
                        node_id: SGIP_NODE_ID,
                        timestamp: SGIP_TIMESTAMP,
                        number: rand_u32(),
                    };
                    let pdu = SgipAdapter.encode(&report, seq).expect("encode report");

                    match conn.write_frame(pdu.as_slice()).await {
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

        if let Some(acc) = account_pool.get(STRESS_TEST_ACCOUNT).await {
            if let Some(conn) = acc.first_connection().await {
                let src = src_numbers[rand_u32() as usize % src_numbers.len()];
                let content = format!("MO Test #{}", mo_sent.load(Ordering::Relaxed) + 1);

                // MO 上行 Deliver（统一模型）：decode 对称 src=sp_number, dest=user_number。
                let deliver = UnifiedMessage::Deliver(UnifiedDeliver {
                    src: Address::plain(STRESS_TEST_ACCOUNT),
                    dest: Address::plain(src),
                    content: content.as_bytes().to_vec(),
                    encoding: rsms_model::Encoding::Gbk,
                    concat: None,
                    extra: ProtocolExtra::Sgip(SgipExtra::default()),
                    tlvs: vec![],
                });
                let seq = Sequence::Sgip { node_id: SGIP_NODE_ID, timestamp: SGIP_TIMESTAMP, number: rand_u32() };
                let pdu = SgipAdapter.encode(&deliver, seq).expect("encode deliver(MO)");

                match conn.write_frame(pdu.as_slice()).await {
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

#[tokio::test]
async fn stress_test_sgip_1connection() {
    run_stress_test(1).await;
}

#[tokio::test]
async fn stress_test_sgip_5connections() {
    run_stress_test(5).await;
}

async fn run_stress_test(num_connections: usize) {
    let total_start = Instant::now();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    println!("\n");
    println!("==========================================");
    println!("SGIP Stress Test - {} Connection(s)", num_connections);
    println!("==========================================");
    println!("Account: {}", STRESS_TEST_ACCOUNT);
    println!("Connections: {}", num_connections);
    println!("Target Rate: {} msg/s", STRESS_TEST_RATE);
    println!("Duration: {} seconds", STRESS_TEST_DURATION_SECS);
    println!("==========================================\n");

    let stats = Arc::new(TestStats::new());
    let msg_source = Arc::new(StressMockMessageSource::new());
    let shared_seq = Arc::new(SharedSeqState::new());
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
    let mut client_states = Vec::new();

    for i in 0..num_connections {
        let client_state = Arc::new(ClientState::new(stats.clone(), shared_seq.clone()));
        let endpoint = Arc::new(EndpointConfig::new(
            "sgip-stress-client",
            "127.0.0.1",
            port,
            if num_connections == 1 { 1024 } else { 2048 },
            30,
        ).with_window_size(2048).with_protocol(Protocol::Sgip).with_log_level(tracing::Level::WARN));

        let mut conn = None;
        for retry in 0..50 {
            match ClientBuilder::new(endpoint.clone(), Arc::new(NoopClientHandler), SgipDecoder)
                .with_message_handler(client_state.clone())
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

        let bind_pdu = client_state.build_bind_pdu(STRESS_TEST_ACCOUNT, STRESS_TEST_PASSWORD);
        conn.write_frame(bind_pdu.as_bytes()).await.expect("send bind");

        tokio::time::sleep(Duration::from_millis(100)).await;

        let mut retries = 0;
        while !client_state.is_connected() && retries < 30 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            retries += 1;
        }

        assert!(client_state.is_connected(), "Connection {} failed after {} retries", i, retries);
        tracing::warn!("Client {} connected", i);

        client_conns.push(conn.clone());
        client_states.push(client_state.clone());

        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let producer_handle = tokio::spawn(mt_producer_task(
        msg_source.clone(),
        stats.clone(),
        shared_seq.clone(),
        mt_rate,
    ));

    let warmup_secs = total_start.elapsed().as_secs_f64();
    stats.start();

    let monitor_handle = spawn_stats_monitor(
        stats.clone(),
        msg_source.clone(),
        "SGIP",
        STRESS_TEST_DURATION_SECS,
        1,
        Some(STRESS_TEST_ACCOUNT.to_string()),
    );

    tokio::time::sleep(Duration::from_secs(STRESS_TEST_DURATION_SECS)).await;

    stats.end();

    producer_handle.abort();

    drain_wait_submit_resp(&stats, Duration::from_secs(10)).await;

    drain_wait_queue_and_reports_single(
        &stats, &msg_source, STRESS_TEST_ACCOUNT,
        report_sent.load(Ordering::Relaxed),
        Duration::from_secs(15),
    ).await;

    report_gen_handle.abort();
    mo_gen_handle.abort();

    drain_wait_final_single(
        &stats,
        report_sent.load(Ordering::Relaxed),
        mo_sent.load(Ordering::Relaxed),
        Duration::from_secs(5),
    ).await;

    monitor_handle.abort();

    let total_secs = total_start.elapsed().as_secs_f64();

    let results = StressTestResults::from_stats(
        &stats,
        report_sent.load(Ordering::Relaxed),
        mo_sent.load(Ordering::Relaxed),
        warmup_secs,
        total_secs,
    );
    print_stress_results(&results, "SGIP", &format!("Stress Test ({} connections)", num_connections));

    server_handle.abort();

    let stress_secs = results.stress_secs;
    let submit_sent = results.submit_sent;
    let submit_resp = results.submit_resp;
    let report_matched = results.report_matched;
    let mo_recv = results.mo_received;

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
        "Report SeqNumber should match SubmitResp (1:1), got {}/{} ({:.1}% match)",
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
