use async_trait::async_trait;
use rsms_connector::{
    serve, connect, SgipDecoder,
    AuthCredentials, AuthHandler, AuthResult,
    AccountConfigProvider,
    protocol::MessageSource,
};
use rsms_connector::client::{ClientContext, ClientConfig, ClientHandler};
use rsms_core::{ConnectionInfo, EncodedPdu, RawPdu, EndpointConfig, Frame, Result};
use rsms_codec_sgip::{
    decode_message, SgipMessage,
    CommandId, Submit, SubmitResp, Deliver, DeliverResp, Report, ReportResp,
    Bind, Encodable, SgipSequence,
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
        let bind = Bind {
            login_type: 1,
            login_name: self.account.account.clone(),
            login_password: self.account.password.clone(),
            reserve: [0u8; 8],
        };

        let body_bytes = {
            let mut buf = bytes::BytesMut::new();
            bind.encode(&mut buf).unwrap();
            buf.to_vec()
        };

        let total_len = (20 + body_bytes.len()) as u32;
        let seq_num = self.shared.next_seq();

        let mut pdu = Vec::new();
        pdu.extend_from_slice(&total_len.to_be_bytes());
        pdu.extend_from_slice(&(CommandId::Bind as u32).to_be_bytes());
        pdu.extend_from_slice(&SGIP_NODE_ID.to_be_bytes());
        pdu.extend_from_slice(&SGIP_TIMESTAMP.to_be_bytes());
        pdu.extend_from_slice(&seq_num.to_be_bytes());
        pdu.extend(body_bytes);

        pdu.into()
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
        let pdu = frame.data_as_slice();
        if pdu.len() < 20 {
            return Ok(());
        }

        let cmd_id = frame.command_id;

        if cmd_id == CommandId::BindResp as u32 {
            let result = pdu[20] as u32;
            *self.login_status.lock().unwrap() = Some(result);
            if result == 0 {
                self.connected.store(true, Ordering::Relaxed);
            }
        } else if cmd_id == CommandId::SubmitResp as u32 {
            let result = u32::from_be_bytes([pdu[20], pdu[21], pdu[22], pdu[23]]);
            self.stats.submit_resp_received.fetch_add(1, Ordering::Relaxed);
            if result != 0 {
                self.stats.submit_errors.fetch_add(1, Ordering::Relaxed);
            }
        } else if cmd_id == CommandId::Report as u32 {
            self.stats.report_received.fetch_add(1, Ordering::Relaxed);

            if let Ok(msg) = decode_message(pdu) {
                if let SgipMessage::Report(r) = msg {
                    let report_seq = r.submit_sequence.number;

                    let already_matched = self.shared.matched_seq_numbers.lock().unwrap().contains(&report_seq);
                    if already_matched {
                        return build_report_resp(ctx, frame).await;
                    }

                    let mut pending = self.shared.pending_seq_numbers.write().unwrap();
                    if let Some(pos) = pending.iter().position(|&s| s == report_seq) {
                        pending.remove(pos);
                        self.shared.matched_seq_numbers.lock().unwrap().insert(report_seq);
                        self.stats.report_matched.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }

            return build_report_resp(ctx, frame).await;
        } else if cmd_id == CommandId::Deliver as u32 {
            self.stats.mo_received.fetch_add(1, Ordering::Relaxed);
            return build_deliver_resp(ctx, frame).await;
        }

        Ok(())
    }
}

async fn build_report_resp(ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
    let resp = ReportResp { result: 0 };
    let body_bytes = {
        let mut buf = bytes::BytesMut::new();
        resp.encode(&mut buf).unwrap();
        buf.to_vec()
    };
    let total_len = (20 + body_bytes.len()) as u32;
    let pdu_bytes = frame.data_as_slice();
    let mut resp_pdu = Vec::new();
    resp_pdu.extend_from_slice(&total_len.to_be_bytes());
    resp_pdu.extend_from_slice(&(CommandId::ReportResp as u32).to_be_bytes());
    if pdu_bytes.len() >= 20 {
        resp_pdu.extend_from_slice(&pdu_bytes[8..20]);
    } else {
        resp_pdu.extend_from_slice(&SGIP_NODE_ID.to_be_bytes());
        resp_pdu.extend_from_slice(&SGIP_TIMESTAMP.to_be_bytes());
        resp_pdu.extend_from_slice(&frame.sequence_id.to_be_bytes());
    }
    resp_pdu.extend(body_bytes);
    ctx.conn.write_frame(resp_pdu.as_slice()).await
}

async fn build_deliver_resp(ctx: &ClientContext<'_>, frame: &Frame) -> Result<()> {
    let resp = DeliverResp { result: 0 };
    let body_bytes = {
        let mut buf = bytes::BytesMut::new();
        resp.encode(&mut buf).unwrap();
        buf.to_vec()
    };
    let total_len = (20 + body_bytes.len()) as u32;
    let pdu_bytes = frame.data_as_slice();
    let mut resp_pdu = Vec::new();
    resp_pdu.extend_from_slice(&total_len.to_be_bytes());
    resp_pdu.extend_from_slice(&(CommandId::DeliverResp as u32).to_be_bytes());
    if pdu_bytes.len() >= 20 {
        resp_pdu.extend_from_slice(&pdu_bytes[8..20]);
    } else {
        resp_pdu.extend_from_slice(&SGIP_NODE_ID.to_be_bytes());
        resp_pdu.extend_from_slice(&SGIP_TIMESTAMP.to_be_bytes());
        resp_pdu.extend_from_slice(&frame.sequence_id.to_be_bytes());
    }
    resp_pdu.extend(body_bytes);
    ctx.conn.write_frame(resp_pdu.as_slice()).await
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
        if let Ok(msg) = decode_message(frame.data_as_slice()) {
            match msg {
                SgipMessage::Submit(s) => {
                    let count = self.submit_count.fetch_add(1, Ordering::Relaxed);

                    let resp = SubmitResp { result: 0 };
                    let body_bytes = {
                        let mut buf = bytes::BytesMut::new();
                        resp.encode(&mut buf).unwrap();
                        buf.to_vec()
                    };
                    let total_len = (20 + body_bytes.len()) as u32;
                    let pdu_bytes = frame.data_as_slice();
                    let mut resp_pdu = Vec::new();
                    resp_pdu.extend_from_slice(&total_len.to_be_bytes());
                    resp_pdu.extend_from_slice(&(CommandId::SubmitResp as u32).to_be_bytes());
                    if pdu_bytes.len() >= 20 {
                        resp_pdu.extend_from_slice(&pdu_bytes[8..20]);
                    } else {
                        resp_pdu.extend_from_slice(&SGIP_NODE_ID.to_be_bytes());
                        resp_pdu.extend_from_slice(&SGIP_TIMESTAMP.to_be_bytes());
                        resp_pdu.extend_from_slice(&frame.sequence_id.to_be_bytes());
                    }
                    resp_pdu.extend(body_bytes);
                    ctx.conn.write_frame(resp_pdu.as_slice()).await?;

                    let submit_seq_number = if pdu_bytes.len() >= 20 {
                        seq_3_from_pdu(pdu_bytes)
                    } else {
                        count as u32
                    };

                    let dest_number = s.user_numbers.first().cloned().unwrap_or_default();
                    self.msg_source.push_item(&account, ReportItem {
                        submit_seq_number,
                        conn_id: ctx.conn.id(),
                        dest_number,
                    }.to_bytes()).await;
                }
                SgipMessage::ReportResp { .. } => {}
                SgipMessage::DeliverResp { .. } => {}
                _ => {}
            }
        }
        Ok(())
    }
}

fn seq_3_from_pdu(pdu: &[u8]) -> u32 {
    if pdu.len() >= 20 {
        u32::from_be_bytes([pdu[16], pdu[17], pdu[18], pdu[19]])
    } else {
        0
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
    ).with_protocol("sgip").with_log_level(tracing::Level::WARN));
    let mut auth = PasswordAuthHandler::new();
    for (account, password) in ACCOUNTS {
        auth = auth.add_account(account, password);
    }
    let server = serve(
        cfg,
        vec![biz_handler],
        Some(Arc::new(auth)),
        None,
        Some(Arc::new(MockAccountConfigProvider::with_limits(10000, 4096)) as Arc<dyn AccountConfigProvider>),
        None,
        None,
    )
    .await
    .expect("bind");
    let port = server.local_addr.port();
    let account_pool = server.account_pool();
    let handle = tokio::spawn(async move { let _ = server.run().await; });
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok((port, account_pool, handle))
}

fn build_sgip_submit_pdu(sp_number: &str, dest_number: &str, content: &str, seq_num: u32) -> Vec<u8> {
    let mut submit = Submit::new();
    submit.sp_number = sp_number.to_string();
    submit.charge_number = sp_number.to_string();
    submit.user_count = 1;
    submit.user_numbers = vec![dest_number.to_string()];
    submit.corp_id = "".to_string();
    submit.service_type = "SMS".to_string();
    submit.fee_type = 2;
    submit.fee_value = "000000".to_string();
    submit.given_value = "000000".to_string();
    submit.agent_flag = 0;
    submit.morelate_to_mt_flag = 0;
    submit.priority = 0;
    submit.expire_time = "".to_string();
    submit.schedule_time = "".to_string();
    submit.report_flag = 1;
    submit.tppid = 0;
    submit.tpudhi = 0;
    submit.msg_fmt = 15;
    submit.message_type = 0;
    submit.message_content = content.as_bytes().to_vec();
    submit.reserve = [0u8; 8];

    let body_bytes = {
        let mut buf = bytes::BytesMut::new();
        submit.encode(&mut buf).unwrap();
        buf.to_vec()
    };

    let total_len = (20 + body_bytes.len()) as u32;

    let mut pdu = Vec::new();
    pdu.extend_from_slice(&total_len.to_be_bytes());
    pdu.extend_from_slice(&(CommandId::Submit as u32).to_be_bytes());
    pdu.extend_from_slice(&SGIP_NODE_ID.to_be_bytes());
    pdu.extend_from_slice(&SGIP_TIMESTAMP.to_be_bytes());
    pdu.extend_from_slice(&seq_num.to_be_bytes());
    pdu.extend(body_bytes);

    pdu
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
                    let report = Report {
                        submit_sequence: SgipSequence::new(SGIP_NODE_ID, SGIP_TIMESTAMP, item.submit_seq_number),
                        report_type: 0,
                        user_number: item.dest_number,
                        state: 0,
                        error_code: 0,
                        reserve: [0u8; 8],
                    };

                    let body_bytes = {
                        let mut buf = bytes::BytesMut::new();
                        report.encode(&mut buf).unwrap();
                        buf.to_vec()
                    };

                    let total_len = (20 + body_bytes.len()) as u32;
                    let seq = rand_u32();
                    let mut pdu = Vec::new();
                    pdu.extend_from_slice(&total_len.to_be_bytes());
                    pdu.extend_from_slice(&(CommandId::Report as u32).to_be_bytes());
                    pdu.extend_from_slice(&SGIP_NODE_ID.to_be_bytes());
                    pdu.extend_from_slice(&SGIP_TIMESTAMP.to_be_bytes());
                    pdu.extend_from_slice(&seq.to_be_bytes());
                    pdu.extend(body_bytes);

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

                let mut deliver = Deliver::new();
                deliver.user_number = src.to_string();
                deliver.sp_number = account.clone();
                deliver.tppid = 0;
                deliver.tpudhi = 0;
                deliver.msg_fmt = 15;
                deliver.message_content = content.as_bytes().to_vec();
                deliver.reserve = [0u8; 8];

                let body_bytes = {
                    let mut buf = bytes::BytesMut::new();
                    deliver.encode(&mut buf).unwrap();
                    buf.to_vec()
                };

                let total_len = (20 + body_bytes.len()) as u32;
                let seq = rand_u32();
                let mut pdu = Vec::new();
                pdu.extend_from_slice(&total_len.to_be_bytes());
                pdu.extend_from_slice(&(CommandId::Deliver as u32).to_be_bytes());
                pdu.extend_from_slice(&SGIP_NODE_ID.to_be_bytes());
                pdu.extend_from_slice(&SGIP_TIMESTAMP.to_be_bytes());
                pdu.extend_from_slice(&seq.to_be_bytes());
                pdu.extend(body_bytes);

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
            ).with_window_size(WINDOW_SIZE as u16).with_protocol("sgip").with_log_level(tracing::Level::WARN));

            let mut conn = None;
            for retry in 0..50 {
                match connect(
                    endpoint.clone(),
                    client_state.clone(),
                    SgipDecoder,
                    Some(ClientConfig::new()),
                    Some(msg_source.clone() as Arc<dyn MessageSource>),
                    None,
                )
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
