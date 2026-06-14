use async_trait::async_trait;
use rsms_connector::{
    ServerBuilder, ClientBuilder, SgipDecoder,
    AuthCredentials, AuthHandler, AuthResult,
    AccountConfigProvider,
    protocol::MessageSource,
};
use rsms_connector::client::{ClientContext, ClientConfig, ClientHandler};
use rsms_core::{ConnectionInfo, EncodedPdu, RawPdu, EndpointConfig, Protocol, Frame, Result};
// 窄腰统一模型：收发一律走 SgipAdapter + UnifiedMessage，不再手构裸 codec / 手剥头部字节。
use rsms_codec_sgip::adapter::SgipAdapter;
use rsms_codec_sgip::CommandId;
use rsms_model::{
    Address, DeliveryStatus, MessageId, ProtocolAdapter, ProtocolExtra, Sequence, SgipExtra,
    UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit, UnifiedSubmitResp,
};
use rsms_test_common::{
    TestStats, StressMockMessageSource, MockAccountConfigProvider, rand_u32,
    print_stress_results, StressTestResults,
    drain_wait_submit_resp, drain_wait_queue_and_reports_multi, drain_wait_final_multi,
    spawn_stats_monitor,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::time::{Duration, Instant};

const NUM_ACCOUNTS: usize = 5;
const CONNECTIONS_PER_ACCOUNT: usize = 5;
const MT_RATE_PER_ACCOUNT: f64 = 2500.0;
const REPORT_RATE_PER_ACCOUNT: f64 = 2500.0;
const MO_RATE_PER_ACCOUNT: f64 = 1250.0;
const STRESS_TEST_DURATION_SECS: u64 = 300;
const WINDOW_SIZE: usize = 2048;
const SGIP_NODE_ID: u32 = 1;
const SGIP_TIMESTAMP: u32 = 0x04051200;

const ACCOUNTS: &[(&str, &str)] = &[
    ("106900", "password123"),
    ("106901", "password123"),
    ("106902", "password123"),
    ("106903", "password123"),
    ("106904", "password123"),
];

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

#[derive(Clone)]
struct AccountCredential {
    account: String,
    password: String,
}

struct ClientState {
    connected: AtomicBool,
    login_status: Mutex<Option<u32>>,
    shared: Arc<SharedSeqState>,
    stats: Arc<TestStats>,
    account: AccountCredential,
}

impl ClientState {
    fn new(stats: Arc<TestStats>, shared: Arc<SharedSeqState>, account: AccountCredential) -> Self {
        Self {
            connected: AtomicBool::new(false),
            login_status: Mutex::new(None),
            shared,
            stats,
            account,
        }
    }

    pub fn build_bind_pdu(&self) -> RawPdu {
        // 明文认证：authenticator 装口令字节；version 承载 login_type=1。复合序列 number 走 next_seq。
        let bind = UnifiedMessage::Bind(UnifiedBind {
            client_id: self.account.account.clone(),
            authenticator: self.account.password.as_bytes().to_vec(),
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
impl ClientHandler for ClientState {
    fn name(&self) -> &'static str {
        "multi-account-stress-client"
    }

    async fn on_inbound(&self, ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
        let unified = match SgipAdapter.decode(frame) {
            Ok(m) => m,
            Err(_) => return Ok(()),
        };

        match unified {
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

                // 被报告 Submit 的复合序列 number 分量承载在 msg_id(12B Binary)。
                let report_seq = report_number_of(&report.msg_id);

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

                return build_report_resp(ctx, frame).await;
            }
            UnifiedMessage::Deliver(_) => {
                self.stats.mo_received.fetch_add(1, Ordering::Relaxed);
                return build_deliver_resp(ctx, frame).await;
            }
            _ => {}
        }

        Ok(())
    }
}

/// 从 UnifiedReport.msg_id(12B Binary: node_id+timestamp+number) 解出 number 分量。
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

// 回 ReportResp：用 Unknown{command_id=ReportResp} 经 adapter 还原；序列用 sequence_of 回显请求复合序列。
async fn build_report_resp(ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
    let resp = UnifiedMessage::Unknown {
        command_id: CommandId::ReportResp as u32,
        raw: vec![],
    };
    let bytes = SgipAdapter.encode(&resp, SgipAdapter.sequence_of(frame))?;
    ctx.conn.write_frame(bytes.as_slice()).await
}

// 回 DeliverResp：序列用 sequence_of 回显请求复合序列。
async fn build_deliver_resp(ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
    let bytes = SgipAdapter.encode(&UnifiedMessage::DeliverResp, SgipAdapter.sequence_of(frame))?;
    ctx.conn.write_frame(bytes.as_slice()).await
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
impl rsms_business::BusinessHandler for ServerHandler {
    fn name(&self) -> &'static str {
        "multi-account-stress-server"
    }

    async fn on_inbound(&self, ctx: &rsms_business::InboundContext, frame: &Frame) -> Result<()> {
        let account = ctx.conn.authenticated_account().await.unwrap_or_else(|| "unknown".to_string());
        if let Ok(unified) = SgipAdapter.decode(frame) {
            match unified {
                UnifiedMessage::Submit(s) => {
                    let _count = self.submit_count.fetch_add(1, Ordering::Relaxed);

                    // 回 SubmitResp：序列用 sequence_of 回显请求复合序列。
                    let resp = UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                        msg_id: MessageId::Text(String::new()),
                        status: 0,
                    });
                    let resp_bytes = SgipAdapter.encode(&resp, SgipAdapter.sequence_of(frame))?;
                    ctx.conn.write_frame(resp_bytes.as_slice()).await?;

                    // 被报告 Submit 的复合序列 number 分量：直接取自 sequence_of。
                    let submit_seq_number = match SgipAdapter.sequence_of(frame) {
                        Sequence::Sgip { number, .. } => number,
                        Sequence::Plain(n) => n,
                    };

                    let dest_number = s.dests.first().map(|a| a.number.clone()).unwrap_or_default();
                    self.msg_source.push_item(&account, ReportItem {
                        submit_seq_number,
                        conn_id: ctx.conn.id(),
                        dest_number,
                    }.to_bytes()).await;
                }
                // ReportResp 退化为 Unknown{command_id=ReportResp}；DeliverResp 为独立变体。
                UnifiedMessage::Unknown { command_id, .. }
                    if command_id == CommandId::ReportResp as u32 => {}
                UnifiedMessage::DeliverResp => {}
                _ => {}
            }
        }
        Ok(())
    }
}

struct PasswordAuthHandler {
    accounts: HashMap<String, String>,
}

impl PasswordAuthHandler {
    fn new() -> Self {
        Self { accounts: HashMap::new() }
    }

    fn add_account(mut self, login_name: &str, password: &str) -> Self {
        self.accounts.insert(login_name.to_string(), password.to_string());
        self
    }
}

#[async_trait]
impl AuthHandler for PasswordAuthHandler {
    fn name(&self) -> &'static str { "sgip-password-auth" }

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
    biz_handler: Arc<dyn rsms_business::BusinessHandler>,
) -> Result<(u16, Arc<rsms_connector::AccountPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "sgip-multi-account-stress-server",
        "127.0.0.1",
        0,
        500,
        60,
    ).with_protocol(Protocol::Sgip).with_log_level(tracing::Level::WARN));
    let mut auth = PasswordAuthHandler::new();
    for (account, password) in ACCOUNTS {
        auth = auth.add_account(account, password);
    }
    let server = ServerBuilder::new(cfg)
        .handlers(vec![biz_handler])
        .auth_handler(Arc::new(auth))
        .account_config_provider(Arc::new(MockAccountConfigProvider::with_limits(10000, 4096)) as Arc<dyn AccountConfigProvider>)
        .serve()
        .await
        .expect("bind");
    let port = server.local_addr.port();
    let account_pool = server.account_pool();
    let handle = tokio::spawn(async move { let _ = server.run().await; });
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok((port, account_pool, handle))
}

fn build_sgip_submit_pdu(sp_number: &str, dest_number: &str, content: &str, seq_num: u32) -> Vec<u8> {
    // 统一模型构造 Submit；SGIP 方言字段经 SgipExtra 传递；复合序列 number=seq_num。
    let submit = UnifiedMessage::Submit(UnifiedSubmit {
        src: Address::plain(sp_number),
        dests: vec![Address::plain(dest_number)],
        content: content.as_bytes().to_vec(),
        encoding: rsms_model::Encoding::Gbk, // msg_fmt=15(GBK)
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
    account: String,
    msg_source: Arc<StressMockMessageSource>,
    stats: Arc<TestStats>,
    shared: Arc<SharedSeqState>,
    target_rate: f64,
) {
    let fetch_key = format!("stress-client-{}", account);
    let inter_msg_interval = Duration::from_secs_f64(1.0 / target_rate);
    let mut interval = tokio::time::interval(inter_msg_interval);
    let src_numbers = ["13800138000", "13800138001", "13800138002", "13800138003", "13800138004"];
    let mut msg_count: u64 = 0;

    loop {
        interval.tick().await;
        let src = src_numbers[msg_count as usize % src_numbers.len()];
        let content = format!("MT Test #{}", msg_count);
        let seq_num = shared.next_seq();
        let pdu_bytes = build_sgip_submit_pdu(src, &account, &content, seq_num);

        if msg_source.push(&fetch_key, pdu_bytes).await.is_ok() {
            stats.submit_sent.fetch_add(1, Ordering::Relaxed);
            shared.pending_seq_numbers.write().unwrap().push_back(seq_num);
        }

        msg_count += 1;
    }
}

async fn report_generator_task(
    account: String,
    msg_source: Arc<StressMockMessageSource>,
    account_pool: Arc<rsms_connector::AccountPool>,
    report_sent: Arc<AtomicU64>,
    target_rate: f64,
) {
    let inter_msg_interval = Duration::from_secs_f64(1.0 / target_rate);
    let mut interval = tokio::time::interval(inter_msg_interval);

    loop {
        interval.tick().await;

        let raw_items = msg_source.fetch_bytes(&account, 100).await;
        let items: Vec<ReportItem> = raw_items.into_iter()
            .filter_map(|b| ReportItem::from_bytes(&b))
            .collect();

        for item in items {
            if let Some(acc) = account_pool.get(&account).await {
                if let Some(conn) = acc.get_connection_by_id(item.conn_id).await {
                    // 独立 Report 命令（统一模型）：被报告 Submit 复合序列打 12B Binary 进 msg_id；
                    // raw=[report_type, state, error_code]，state 由 status 反映射（Delivered→0）。
                    let report = UnifiedMessage::Report(UnifiedReport {
                        msg_id: seq_to_msg_id(SGIP_NODE_ID, SGIP_TIMESTAMP, item.submit_seq_number),
                        status: DeliveryStatus::Delivered,
                        src: Address::plain(String::new()),
                        dest: Address::plain(item.dest_number),
                        raw: vec![0, 0, 0],
                    });
                    let seq = Sequence::Sgip { node_id: SGIP_NODE_ID, timestamp: SGIP_TIMESTAMP, number: rand_u32() };
                    let pdu = SgipAdapter.encode(&report, seq).expect("encode report");

                    if conn.write_frame(pdu.as_slice()).await.is_ok() {
                        report_sent.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }
}

async fn mo_generator_task(
    account: String,
    account_pool: Arc<rsms_connector::AccountPool>,
    mo_sent: Arc<AtomicU64>,
    target_rate: f64,
) {
    let inter_msg_interval = Duration::from_secs_f64(1.0 / target_rate);
    let mut interval = tokio::time::interval(inter_msg_interval);
    let src_numbers = ["13800138000", "13800138001", "13800138002", "13800138003", "13800138004"];

    loop {
        interval.tick().await;
        if let Some(acc) = account_pool.get(&account).await {
            if let Some(conn) = acc.first_connection().await {
                let src = src_numbers[rand_u32() as usize % src_numbers.len()];
                let content = format!("MO Test #{}", mo_sent.load(Ordering::Relaxed) + 1);

                // MO 上行 Deliver（统一模型）：decode 对称 src=sp_number, dest=user_number。
                let deliver = UnifiedMessage::Deliver(UnifiedDeliver {
                    src: Address::plain(account.clone()),
                    dest: Address::plain(src),
                    content: content.as_bytes().to_vec(),
                    encoding: rsms_model::Encoding::Gbk,
                    concat: None,
                    extra: ProtocolExtra::Sgip(SgipExtra::default()),
                    tlvs: vec![],
                });
                let seq = Sequence::Sgip { node_id: SGIP_NODE_ID, timestamp: SGIP_TIMESTAMP, number: rand_u32() };
                let pdu = SgipAdapter.encode(&deliver, seq).expect("encode deliver(MO)");

                if conn.write_frame(pdu.as_slice()).await.is_ok() {
                    mo_sent.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

#[tokio::test]
async fn stress_test_sgip_5accounts_5connections() {
    let total_start = Instant::now();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let stats = Arc::new(TestStats::new());
    let msg_source = Arc::new(StressMockMessageSource::new());
    let server_handler = Arc::new(ServerHandler::new(msg_source.clone()));
    let (port, account_pool, server_handle) = start_test_server(server_handler.clone()).await.unwrap();

    println!("\n");
    println!("==========================================================");
    println!("SGIP Multi-Account Stress Test");
    println!("==========================================================");
    println!("Accounts: {}", NUM_ACCOUNTS);
    println!("Connections per account: {}", CONNECTIONS_PER_ACCOUNT);
    println!("Total connections: {}", NUM_ACCOUNTS * CONNECTIONS_PER_ACCOUNT);
    println!("MT rate per account: {} msg/s", MT_RATE_PER_ACCOUNT as u64);
    println!("Total MT rate: {} msg/s", (MT_RATE_PER_ACCOUNT * NUM_ACCOUNTS as f64) as u64);
    println!("Report rate per account: {} msg/s", REPORT_RATE_PER_ACCOUNT as u64);
    println!("MO rate per account: {} msg/s", MO_RATE_PER_ACCOUNT as u64);
    println!("Duration: {} seconds", STRESS_TEST_DURATION_SECS);
    println!("==========================================================\n");

    let mut report_gen_handles = Vec::new();
    let mut mo_gen_handles = Vec::new();
    let mut producer_handles = Vec::new();

    for (idx, (account, password)) in ACCOUNTS.iter().enumerate() {
        let account = account.to_string();
        let password = password.to_string();

        let report_sent = Arc::new(AtomicU64::new(0));
        let mo_sent = Arc::new(AtomicU64::new(0));

        report_gen_handles.push((
            account.clone(),
            Arc::clone(&report_sent),
            tokio::spawn(report_generator_task(
                account.clone(),
                msg_source.clone(),
                account_pool.clone(),
                report_sent.clone(),
                REPORT_RATE_PER_ACCOUNT,
            )),
        ));

        mo_gen_handles.push((
            account.clone(),
            Arc::clone(&mo_sent),
            tokio::spawn(mo_generator_task(
                account.clone(),
                account_pool.clone(),
                mo_sent.clone(),
                MO_RATE_PER_ACCOUNT,
            )),
        ));

        let shared_seq = Arc::new(SharedSeqState::new());

        for conn_idx in 0..CONNECTIONS_PER_ACCOUNT {
            let client_state = Arc::new(ClientState::new(
                stats.clone(),
                shared_seq.clone(),
                AccountCredential {
                    account: account.clone(),
                    password: password.clone(),
                },
            ));
            let endpoint = Arc::new(EndpointConfig::new(
                &format!("stress-client-{}", account),
                "127.0.0.1",
                port,
                2048,
                30,
            ).with_window_size(WINDOW_SIZE as u16).with_protocol(Protocol::Sgip).with_log_level(tracing::Level::WARN));

            let mut conn = None;
            for retry in 0..50 {
                match ClientBuilder::new(endpoint.clone(), client_state.clone(), SgipDecoder)
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
                        tracing::warn!("Account {} conn {} attempt {} failed: {:?}", account, conn_idx, retry, e);
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            }

            let conn = conn.expect("Failed to establish connection after retries");

            let bind_pdu = client_state.build_bind_pdu();
            conn.write_frame(bind_pdu.as_bytes()).await.expect("send bind");

            tokio::time::sleep(Duration::from_millis(100)).await;

            let mut retries = 0;
            while !client_state.is_connected() && retries < 30 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                retries += 1;
            }
            assert!(client_state.is_connected(), "Account {} conn {} failed", account, conn_idx);

            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        producer_handles.push(tokio::spawn(mt_producer_task(
            account.clone(),
            msg_source.clone(),
            stats.clone(),
            shared_seq.clone(),
            MT_RATE_PER_ACCOUNT,
        )));

        tracing::info!("Account {} ready ({} connections)", account, CONNECTIONS_PER_ACCOUNT);
        let _ = idx;
    }

    let warmup_secs = total_start.elapsed().as_secs_f64();
    stats.start();

    println!("All {} accounts x {} connections ready, stress test started", NUM_ACCOUNTS, CONNECTIONS_PER_ACCOUNT);

    let monitor_handle = spawn_stats_monitor(
        stats.clone(),
        msg_source.clone(),
        "SGIP",
        STRESS_TEST_DURATION_SECS,
        5,
        None,
    );

    tokio::time::sleep(Duration::from_secs(STRESS_TEST_DURATION_SECS)).await;
    stats.end();

    for handle in &producer_handles {
        handle.abort();
    }

    drain_wait_submit_resp(&stats, Duration::from_secs(15)).await;

    let total_report_sent: u64 = report_gen_handles.iter().map(|(_, c, _)| c.load(Ordering::Relaxed)).sum();
    drain_wait_queue_and_reports_multi(&stats, &msg_source, total_report_sent, Duration::from_secs(30)).await;

    for (_, _, handle) in &report_gen_handles {
        handle.abort();
    }
    for (_, _, handle) in &mo_gen_handles {
        handle.abort();
    }

    let total_report_sent: u64 = report_gen_handles.iter().map(|(_, c, _)| c.load(Ordering::Relaxed)).sum();
    let total_mo_sent: u64 = mo_gen_handles.iter().map(|(_, c, _)| c.load(Ordering::Relaxed)).sum();
    drain_wait_final_multi(&stats, total_report_sent, total_mo_sent, Duration::from_secs(10)).await;

    monitor_handle.abort();

    let total_secs = total_start.elapsed().as_secs_f64();

    let total_report_sent: u64 = report_gen_handles.iter().map(|(_, c, _)| c.load(Ordering::Relaxed)).sum();
    let total_mo_sent: u64 = mo_gen_handles.iter().map(|(_, c, _)| c.load(Ordering::Relaxed)).sum();
    let results = StressTestResults::from_stats(
        &stats,
        total_report_sent,
        total_mo_sent,
        warmup_secs,
        total_secs,
    );
    print_stress_results(&results, "SGIP", "Multi-Account Stress Test");
    println!("[配置]");
    println!("  账号数: {}", NUM_ACCOUNTS);
    println!("  每账号连接数: {}", CONNECTIONS_PER_ACCOUNT);
    println!("  总连接数: {}", NUM_ACCOUNTS * CONNECTIONS_PER_ACCOUNT);
    println!("");
    println!("[Per-Account TPS - 压测时间]");
    println!("  MT Submit:  {:.1}", results.submit_sent as f64 / results.stress_secs / NUM_ACCOUNTS as f64);
    println!("  Report:     {:.1}", results.report_received as f64 / results.stress_secs / NUM_ACCOUNTS as f64);
    println!("  MO:         {:.1}", results.mo_received as f64 / results.stress_secs / NUM_ACCOUNTS as f64);
    println!("==========================================================\n");

    server_handle.abort();

    let stress_secs = results.stress_secs;
    let submit_sent = results.submit_sent;
    let submit_resp = results.submit_resp;
    let report_matched = results.report_matched;

    let actual_mt_qps = submit_sent as f64 / stress_secs;
    let expected_min = (MT_RATE_PER_ACCOUNT * NUM_ACCOUNTS as f64 * STRESS_TEST_DURATION_SECS as f64 * 0.4) as u64;
    assert!(submit_sent >= expected_min, "Expected at least {} MT, got {} ({:.1} QPS)", expected_min, submit_sent, actual_mt_qps);

    let match_ratio = if submit_resp > 0 { report_matched as f64 / submit_resp as f64 } else { 0.0 };
    assert!(
        report_matched >= submit_resp.saturating_sub(100),
        "Report match too low: {}/{} ({:.1}%)", report_matched, submit_resp, match_ratio * 100.0
    );
}
