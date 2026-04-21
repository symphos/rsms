use async_trait::async_trait;
use rsms_connector::{
    serve, connect, SmgpDecoder,
    AuthCredentials, AuthHandler, AuthResult,
    AccountConfigProvider,
};
use rsms_connector::client::{ClientContext, ClientConfig, ClientHandler};
use rsms_core::{ConnectionInfo, RawPdu, EndpointConfig, Frame, Result};
use rsms_codec_smgp::{
    decode_message, SmgpMessage, Pdu,
    CommandId, Login, SmgpMsgId, Submit, SubmitResp, Deliver, DeliverResp,
    compute_login_auth,
};
use rsms_codec_smgp::datatypes::tlv::OptionalParameters;
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

const STRESS_TEST_CLIENT_ID: &str = "106900";
const STRESS_TEST_PASSWORD: &str = "password123";
const STRESS_TEST_DURATION_SECS: u64 = 30;
const STRESS_TEST_RATE: f64 = 2500.0;

#[derive(Clone)]
struct ReportItem {
    msg_id: [u8; 10],
    conn_id: u64,
    dest_id: String,
}

impl ReportItem {
    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(10 + 8 + self.dest_id.len());
        buf.extend_from_slice(&self.msg_id);
        buf.extend_from_slice(&self.conn_id.to_be_bytes());
        buf.extend_from_slice(self.dest_id.as_bytes());
        buf
    }

    fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 18 {
            return None;
        }
        let msg_id: [u8; 10] = data[0..10].try_into().ok()?;
        let conn_id = u64::from_be_bytes(data[10..18].try_into().ok()?);
        let dest_id = String::from_utf8(data[18..].to_vec()).ok()?;
        Some(Self { msg_id, conn_id, dest_id })
    }
}

#[allow(dead_code)]
struct ClientState {
    connected: AtomicBool,
    login_status: Mutex<Option<u32>>,
    seq: AtomicU64,
    pending_seqs: Arc<RwLock<VecDeque<u32>>>,
    msg_ids: Arc<RwLock<VecDeque<SmgpMsgId>>>,
    matched_msg_ids: Arc<Mutex<HashSet<SmgpMsgId>>>,
    stats: Arc<TestStats>,
}

impl ClientState {
    fn new(stats: Arc<TestStats>) -> Self {
        Self {
            connected: AtomicBool::new(false),
            login_status: Mutex::new(None),
            seq: AtomicU64::new(1),
            pending_seqs: Arc::new(RwLock::new(VecDeque::new())),
            msg_ids: Arc::new(RwLock::new(VecDeque::new())),
            matched_msg_ids: Arc::new(Mutex::new(HashSet::new())),
            stats,
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u32
    }

    pub fn build_login_pdu(&self) -> RawPdu {
        let timestamp = 0u32;
        let authenticator = compute_login_auth(STRESS_TEST_CLIENT_ID, STRESS_TEST_PASSWORD, timestamp);

        let login = Login {
            client_id: STRESS_TEST_CLIENT_ID.to_string(),
            authenticator,
            login_mode: 0,
            timestamp,
            version: 0x30,
        };

        let pdu: Pdu = login.into();
        pdu.to_pdu_bytes(self.next_seq())
    }

    pub fn build_submit_pdu(&self, src: &str, dst: &str, content: &str) -> (RawPdu, u32) {
        let mut submit = Submit::new();
        submit.need_report = 1;
        submit.service_id = "SMS".to_string();
        submit.fee_type = "02".to_string();
        submit.fee_code = "000000".to_string();
        submit.fixed_fee = "000000".to_string();
        submit.msg_fmt = 15;
        submit.src_term_id = src.to_string();
        submit.charge_term_id = dst.to_string();
        submit.dest_term_id_count = 1;
        submit.dest_term_ids = vec![dst.to_string()];
        submit.msg_content = content.as_bytes().to_vec();

        let pdu: Pdu = submit.into();
        let seq = self.next_seq();
        (pdu.to_pdu_bytes(seq), seq)
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
        if pdu.len() < 12 {
            return Ok(());
        }

        let cmd_id = u32::from_be_bytes([pdu[4], pdu[5], pdu[6], pdu[7]]);

        if cmd_id == CommandId::LoginResp as u32 && pdu.len() >= 33 {
            let status = u32::from_be_bytes([pdu[12], pdu[13], pdu[14], pdu[15]]);
            *self.login_status.lock().unwrap() = Some(status);
            if status == 0 {
                self.connected.store(true, Ordering::Relaxed);
            }
        } else if cmd_id == CommandId::SubmitResp as u32 && pdu.len() >= 26 {
            let status = u32::from_be_bytes([pdu[22], pdu[23], pdu[24], pdu[25]]);
            self.stats.submit_resp_received.fetch_add(1, Ordering::Relaxed);
            if status == 0 {
                let msg_id_bytes: [u8; 10] = [
                    pdu[12], pdu[13], pdu[14], pdu[15], pdu[16],
                    pdu[17], pdu[18], pdu[19], pdu[20], pdu[21],
                ];
                let msg_id = SmgpMsgId::new(msg_id_bytes);
                self.msg_ids.write().unwrap().push_back(msg_id);
                tracing::trace!("[Client] SubmitResp received, stored msg_id: {:?}", msg_id);
            } else {
                self.stats.submit_errors.fetch_add(1, Ordering::Relaxed);
            }
        } else if cmd_id == CommandId::Deliver as u32 {
            if let Ok(msg) = decode_message(pdu) {
                match msg {
                    SmgpMessage::Deliver { deliver: d, .. } => {
                        if d.is_report == 1 {
                            let content = String::from_utf8_lossy(&d.msg_content).to_string();
                            if let Some(report_msg_id) = Self::parse_msg_id_from_report(&content) {
                                tracing::trace!("[Client] Received report: {:?}", report_msg_id);
                                self.stats.report_received.fetch_add(1, Ordering::Relaxed);
                                let matched = self.matched_msg_ids.lock().unwrap();
                                if matched.contains(&report_msg_id) {
                                    return Ok(());
                                }
                                drop(matched);
                                let mut pending = self.msg_ids.write().unwrap();
                                if let Some(pos) = pending.iter().position(|&id| id == report_msg_id) {
                                    pending.remove(pos);
                                    self.matched_msg_ids.lock().unwrap().insert(report_msg_id);
                                    self.stats.report_matched.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        } else {
                            self.stats.mo_received.fetch_add(1, Ordering::Relaxed);
                            tracing::trace!("[Client] Received MO from {}: {:?}",
                                d.src_term_id, &d.msg_content);
                        }
                    }
                    _ => {}
                }
            }
            let resp = DeliverResp { status: 0 };
            let resp_pdu: Pdu = resp.into();
            ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;
        }

        Ok(())
    }
}

impl ClientState {
    fn parse_msg_id_from_report(content: &str) -> Option<SmgpMsgId> {
        let parts: Vec<&str> = content.split_whitespace().collect();
        if parts.len() >= 1 && parts[0].starts_with("id:") {
            let dec_str = parts[0].trim_start_matches("id:");
            if let Ok(value) = dec_str.parse::<u64>() {
                return Some(SmgpMsgId::from_u64(value));
            }
        }
        None
    }
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
        if let Ok(msg) = decode_message(frame.data_as_slice()) {
            match msg {
                SmgpMessage::Submit { submit: s, .. } => {
                    let count = self.submit_count.fetch_add(1, Ordering::Relaxed);
                    let msg_id = SmgpMsgId::from_u64(count);

                    tracing::trace!("[Server] Received Submit #{}", count + 1);

                    let resp = SubmitResp {
                        msg_id,
                        status: 0,
                    };
                    let resp_pdu: Pdu = resp.into();
                    ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;

                    let dest_id = s.dest_term_ids.first().cloned().unwrap_or_default();
                    let item = ReportItem {
                        msg_id: msg_id.to_bytes(),
                        conn_id: ctx.conn.id(),
                        dest_id,
                    };
                    self.msg_source.push_item(STRESS_TEST_CLIENT_ID, item.to_bytes()).await;
                }
                SmgpMessage::DeliverResp { .. } => {
                    tracing::trace!("[Server] Received DeliverResp");
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

    pub fn add_account(mut self, client_id: &str, password: &str) -> Self {
        self.accounts.insert(client_id.to_string(), password.to_string());
        self
    }
}

#[async_trait]
impl AuthHandler for PasswordAuthHandler {
    fn name(&self) -> &'static str {
        "smgp-password-auth"
    }

    async fn authenticate(&self, _client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        if let AuthCredentials::Smgp {
            client_id,
            authenticator,
            ..
        } = credentials
        {
            if let Some(password) = self.accounts.get(&client_id) {
                let expected = compute_login_auth(&client_id, password, 0);
                if expected == authenticator {
                    return Ok(AuthResult::success(&client_id));
                } else {
                    return Ok(AuthResult::failure(1, "Invalid password"));
                }
            }
            Ok(AuthResult::failure(1, "Unknown account"))
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
    ).with_protocol("smgp").with_log_level(tracing::Level::WARN));
    let auth = Arc::new(PasswordAuthHandler::new().add_account(STRESS_TEST_CLIENT_ID, STRESS_TEST_PASSWORD));
    let server = serve(
        cfg,
        vec![biz_handler],
        Some(auth),
        None,
        Some(Arc::new(MockAccountConfigProvider::with_limits(5000, 2048)) as Arc<dyn AccountConfigProvider>),
        None,
        None,
    )
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

        let raw_items = msg_source.fetch_bytes(STRESS_TEST_CLIENT_ID, 100).await;
        let items: Vec<ReportItem> = raw_items.into_iter()
            .filter_map(|b| ReportItem::from_bytes(&b))
            .collect();

        for item in items {
            if let Some(acc) = account_pool.get(STRESS_TEST_CLIENT_ID).await {
                if let Some(conn) = acc.get_connection_by_id(item.conn_id).await {
                    let now = format_timestamp(true);
                    let msg_id_value = SmgpMsgId::new(item.msg_id).to_u64();
                    let date_part = &now[..10];
                    let report_content = format!(
                        "id:{} sub:001 dlvrd:001 submit date:{} done date:{} stat:DELIVRD err:000 text:Hello",
                        msg_id_value, date_part, date_part
                    );

                    let deliver = Deliver {
                        msg_id: SmgpMsgId::default(),
                        is_report: 1,
                        msg_fmt: 15,
                        recv_time: now,
                        src_term_id: "13800138000".to_string(),
                        dest_term_id: item.dest_id,
                        msg_content: report_content.as_bytes().to_vec(),
                        reserve: [0u8; 8],
                        optional_params: OptionalParameters::new(),
                    };

                    let pdu: Pdu = deliver.into();
                    let seq = rand_u32();
                    match conn.write_frame(pdu.to_pdu_bytes(seq).as_slice()).await {
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

        if let Some(acc) = account_pool.get(STRESS_TEST_CLIENT_ID).await {
            if let Some(conn) = acc.first_connection().await {
                let src = src_numbers[rand_u32() as usize % src_numbers.len()];
                let content = format!("MO Test #{}", mo_sent.load(Ordering::Relaxed) + 1);
                let now = format_timestamp(true);
                let mo_count = mo_sent.load(Ordering::Relaxed);

                let deliver = Deliver {
                    msg_id: SmgpMsgId::from_u64(mo_count + 1000000),
                    is_report: 0,
                    msg_fmt: 15,
                    recv_time: now,
                    src_term_id: src.to_string(),
                    dest_term_id: STRESS_TEST_CLIENT_ID.to_string(),
                    msg_content: content.as_bytes().to_vec(),
                    reserve: [0u8; 8],
                    optional_params: OptionalParameters::new(),
                };

                let pdu: Pdu = deliver.into();
                let seq = rand_u32();
                match conn.write_frame(pdu.to_pdu_bytes(seq).as_slice()).await {
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

async fn sender_task(
    conn: Arc<rsms_connector::ClientConnection>,
    state: Arc<ClientState>,
    target_rate: f64,
    conn_index: usize,
) {
    let inter_msg_interval = Duration::from_secs_f64(1.0 / target_rate);
    let mut interval = tokio::time::interval(inter_msg_interval);

    let src_numbers = ["13800138000", "13800138001", "13800138002", "13800138003", "13800138004"];
    let src = src_numbers[conn_index % src_numbers.len()];

    let mut msg_count: u64 = 0;

    loop {
        interval.tick().await;

        let content = format!("MT Test #{}", msg_count);
        let (pdu, _seq) = state.build_submit_pdu(src, STRESS_TEST_CLIENT_ID, &content);

        match conn.send_request(pdu).await {
            Ok(_) => {
                state.stats.submit_sent.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                state.stats.submit_errors.fetch_add(1, Ordering::Relaxed);
            }
        }

        msg_count += 1;
    }
}

#[tokio::test]
async fn stress_test_smgp_1connection() {
    run_stress_test(1).await;
}

#[tokio::test]
async fn stress_test_smgp_5connections() {
    run_stress_test(5).await;
}

async fn run_stress_test(num_connections: usize) {
    let total_start = Instant::now();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    println!("\n");
    println!("==========================================");
    println!("SMGP Stress Test - {} Connection(s)", num_connections);
    println!("==========================================");
    println!("Account: {}", STRESS_TEST_CLIENT_ID);
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

    let per_conn_rate = mt_rate / num_connections as f64;

    let mut client_conns = Vec::new();
    let mut sender_handles = Vec::new();

    for i in 0..num_connections {
        let client_state = Arc::new(ClientState::new(stats.clone()));
        let endpoint = Arc::new(EndpointConfig::new(
            "stress-client",
            "127.0.0.1",
            port,
            if num_connections == 1 { 1024 } else { 2048 },
            30,
        ).with_window_size(2048).with_log_level(tracing::Level::WARN));

        let mut conn = None;
        for retry in 0..50 {
            match connect(
                endpoint.clone(),
                client_state.clone(),
                SmgpDecoder,
                Some(ClientConfig::new()),
                None,
                None,
            )
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

        let login_pdu = client_state.build_login_pdu();
        conn.send_request(login_pdu).await.expect("send login");

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

        let sender_conn = conn.clone();
        let sender_state = client_state.clone();
        sender_handles.push(tokio::spawn(sender_task(
            sender_conn,
            sender_state,
            per_conn_rate,
            i,
        )));
    }

    let warmup_secs = total_start.elapsed().as_secs_f64();
    stats.start();

    let monitor_handle = spawn_stats_monitor(
        stats.clone(), msg_source.clone(), "SMGP",
        STRESS_TEST_DURATION_SECS, 1, Some(STRESS_TEST_CLIENT_ID.to_string()),
    );

    tokio::time::sleep(Duration::from_secs(STRESS_TEST_DURATION_SECS)).await;

    stats.end();

    for handle in sender_handles {
        handle.abort();
    }

    drain_wait_submit_resp(&stats, Duration::from_secs(10)).await;

    let report_sent_val = report_sent.load(Ordering::Relaxed);
    drain_wait_queue_and_reports_single(
        &stats, &msg_source, STRESS_TEST_CLIENT_ID, report_sent_val, Duration::from_secs(15),
    ).await;

    report_gen_handle.abort();
    mo_gen_handle.abort();

    let report_sent_val = report_sent.load(Ordering::Relaxed);
    let mo_sent_val = mo_sent.load(Ordering::Relaxed);
    drain_wait_final_single(&stats, report_sent_val, mo_sent_val, Duration::from_secs(5)).await;

    monitor_handle.abort();

    let total_secs = total_start.elapsed().as_secs_f64();
    let results = StressTestResults::from_stats(&stats, report_sent_val, mo_sent_val, warmup_secs, total_secs);
    print_stress_results(&results, "SMGP", &format!("Stress Test ({} connections)", num_connections));

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
