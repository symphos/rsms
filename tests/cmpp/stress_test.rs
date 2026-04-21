use async_trait::async_trait;
use rsms_connector::{
    AuthCredentials, AuthHandler, AuthResult,
    AccountConfigProvider, CmppDecoder, connect,
    protocol::MessageSource,
};
use rsms_connector::client::{ClientContext, ClientConfig, ClientHandler};
use rsms_core::{ConnectionInfo, RawPdu, EndpointConfig, Frame, Result};
use rsms_codec_cmpp::{
    decode_message, CmppMessage, Pdu, Connect, Deliver, DeliverResp,
    CommandId, Submit, SubmitResp, SubmitV20,
};
use rsms_codec_cmpp::auth::compute_connect_auth;
use rsms_test_common::{
    TestStats, StressMockMessageSource, MockAccountConfigProvider,
    rand_u32, format_timestamp, print_stress_results, StressTestResults,
    drain_wait_submit_resp, drain_wait_queue_and_reports_single, drain_wait_final_single,
    spawn_stats_monitor,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::time::{Duration, Instant};

const STRESS_TEST_ACCOUNT: &str = "900001";
const STRESS_TEST_PASSWORD: &str = "password123";
const STRESS_TEST_CONNECTIONS: usize = 1;
const STRESS_TEST_RATE: f64 = 2500.0;
const STRESS_TEST_DURATION_SECS: u64 = 30;

const CMPP_VERSION_2_0: u8 = 0x20;
const CMPP_VERSION_3_0: u8 = 0x30;

#[derive(Clone)]
struct ReportItem {
    msg_id: [u8; 8],
    conn_id: u64,
    dest_id: String,
}

impl ReportItem {
    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8 + 8 + self.dest_id.len());
        buf.extend_from_slice(&self.msg_id);
        buf.extend_from_slice(&self.conn_id.to_be_bytes());
        buf.extend_from_slice(self.dest_id.as_bytes());
        buf
    }

    fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 16 {
            return None;
        }
        let msg_id: [u8; 8] = data[0..8].try_into().ok()?;
        let conn_id = u64::from_be_bytes(data[8..16].try_into().ok()?);
        let dest_id = String::from_utf8(data[16..].to_vec()).ok()?;
        Some(Self { msg_id, conn_id, dest_id })
    }
}

#[allow(dead_code)]
struct ClientState {
    connected: AtomicBool,
    connect_status: Mutex<Option<u32>>,
    seq: AtomicU64,
    msg_ids: Arc<RwLock<VecDeque<[u8; 8]>>>,
    matched_msg_ids: Arc<Mutex<HashSet<[u8; 8]>>>,
    stats: Arc<TestStats>,
    version: u8,
}

impl ClientState {
    fn new(stats: Arc<TestStats>, version: u8) -> Self {
        Self {
            connected: AtomicBool::new(false),
            connect_status: Mutex::new(None),
            seq: AtomicU64::new(1),
            msg_ids: Arc::new(RwLock::new(VecDeque::new())),
            matched_msg_ids: Arc::new(Mutex::new(HashSet::new())),
            stats,
            version,
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u32
    }

    pub fn build_connect_pdu(&self) -> RawPdu {
        let timestamp = 0u32;
        let auth = compute_connect_auth(STRESS_TEST_ACCOUNT, STRESS_TEST_PASSWORD, timestamp);
        let connect = Connect {
            source_addr: STRESS_TEST_ACCOUNT.to_string(),
            authenticator_source: auth,
            version: self.version,
            timestamp,
        };
        let pdu: Pdu = connect.into();
        pdu.to_pdu_bytes(self.next_seq())
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

        if cmd_id == CommandId::ConnectResp as u32 && pdu.len() >= 25 {
            let status = u32::from_be_bytes([pdu[12], pdu[13], pdu[14], pdu[15]]);
            *self.connect_status.lock().unwrap() = Some(status);
            if status == 0 {
                self.connected.store(true, Ordering::Relaxed);
            }
        } else if cmd_id == CommandId::SubmitResp as u32 && pdu.len() >= 24 {
            let result = u32::from_be_bytes([pdu[20], pdu[21], pdu[22], pdu[23]]);
            self.stats.submit_resp_received.fetch_add(1, Ordering::Relaxed);
            if result == 0 {
                let msg_id: [u8; 8] = [pdu[12], pdu[13], pdu[14], pdu[15], pdu[16], pdu[17], pdu[18], pdu[19]];
                self.msg_ids.write().unwrap().push_back(msg_id);
            } else {
                self.stats.submit_errors.fetch_add(1, Ordering::Relaxed);
            }
        } else if cmd_id == CommandId::Deliver as u32 {
            if let Ok(msg) = decode_message(pdu) {
                match msg {
                    CmppMessage::DeliverV20 { deliver: d, .. } => {
                        self.handle_deliver(&d.registered_delivery, &d.msg_content, &d.src_terminal_id);
                    }
                    CmppMessage::DeliverV30 { deliver: d, .. } => {
                        self.handle_deliver(&d.registered_delivery, &d.msg_content, &d.src_terminal_id);
                    }
                    _ => {}
                }
            }
            let resp = DeliverResp { msg_id: [0u8; 8], result: 0 };
            let resp_pdu: Pdu = resp.into();
            ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;
        }

        Ok(())
    }
}

impl ClientState {
    fn handle_deliver(&self, registered_delivery: &u8, msg_content: &[u8], src_terminal_id: &str) {
        if *registered_delivery == 1 {
            let content = String::from_utf8_lossy(msg_content).to_string();
            if let Some(report_msg_id) = Self::parse_msg_id_from_report(&content) {
                self.stats.report_received.fetch_add(1, Ordering::Relaxed);
                let matched = self.matched_msg_ids.lock().unwrap();
                if matched.contains(&report_msg_id) {
                    return;
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
            tracing::trace!("[Client] Received MO from {}: {:02x?}", src_terminal_id, msg_content);
        }
    }

    fn parse_msg_id_from_report(content: &str) -> Option<[u8; 8]> {
        let parts: Vec<&str> = content.split_whitespace().collect();
        if parts.len() >= 2 && parts[0].starts_with("id:") {
            let hex_str = parts[0].trim_start_matches("id:");
            if hex_str.len() == 16 {
                let mut msg_id = [0u8; 8];
                for i in 0..8 {
                    if let Ok(byte) = u8::from_str_radix(&hex_str[i*2..i*2+2], 16) {
                        msg_id[i] = byte;
                    } else {
                        return None;
                    }
                }
                return Some(msg_id);
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

    async fn handle_submit(
        &self,
        dest_id: String,
        ctx: &rsms_business::InboundContext,
        frame: &Frame,
    ) -> Result<()> {
        let count = self.submit_count.fetch_add(1, Ordering::Relaxed);
        let mut msg_id = [0u8; 8];
        msg_id[4..8].copy_from_slice(&(count as u32).to_be_bytes());

        let resp = SubmitResp { msg_id, result: 0 };
        let resp_pdu: Pdu = resp.into();
        ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;

        let item = ReportItem { msg_id, conn_id: ctx.conn.id(), dest_id };
        self.msg_source.push_item(STRESS_TEST_ACCOUNT, item.to_bytes()).await;
        Ok(())
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
                CmppMessage::SubmitV20 { submit, .. } => {
                    self.handle_submit(submit.dest_terminal_ids.first().cloned().unwrap_or_default(), ctx, frame).await?;
                }
                CmppMessage::SubmitV30 { submit, .. } => {
                    self.handle_submit(submit.dest_terminal_ids.first().cloned().unwrap_or_default(), ctx, frame).await?;
                }
                CmppMessage::DeliverResp { .. } => {}
                _ => {}
            }
        }
        Ok(())
    }
}

pub struct PasswordAuthHandler {
    accounts: HashMap<String, String>,
}

impl PasswordAuthHandler {
    pub fn new() -> Self {
        Self { accounts: HashMap::new() }
    }

    pub fn add_account(mut self, account: &str, password: &str) -> Self {
        self.accounts.insert(account.to_string(), password.to_string());
        self
    }
}

#[async_trait]
impl AuthHandler for PasswordAuthHandler {
    fn name(&self) -> &'static str { "password-auth" }

    async fn authenticate(&self, _client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        if let AuthCredentials::Cmpp { source_addr, authenticator_source, timestamp, .. } = credentials {
            if let Some(password) = self.accounts.get(&source_addr) {
                let expected = compute_connect_auth(&source_addr, password, timestamp);
                if expected == authenticator_source {
                    return Ok(AuthResult::success(&source_addr));
                }
            }
            Ok(AuthResult::failure(1, "Invalid credentials"))
        } else {
            Ok(AuthResult::failure(1, "Invalid credentials"))
        }
    }
}

use rsms_connector::serve;

async fn start_test_server(
    biz_handler: Arc<dyn rsms_business::BusinessHandler>,
) -> Result<(u16, Arc<rsms_connector::AccountPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "stress-test-server", "127.0.0.1", 0, 500, 60,
    ).with_log_level(tracing::Level::WARN));
    let auth = Arc::new(PasswordAuthHandler::new().add_account(STRESS_TEST_ACCOUNT, STRESS_TEST_PASSWORD));
    let server = serve(
        cfg,
        vec![biz_handler],
        Some(auth),
        None,
        Some(Arc::new(MockAccountConfigProvider::with_limits(5000, 2048)) as Arc<dyn AccountConfigProvider>),
        None,
        None,
    ).await.expect("bind");
    let port = server.local_addr.port();
    let account_pool = server.account_pool();
    let handle = tokio::spawn(async move { let _ = server.run().await; });
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

        let raw_items = msg_source.fetch_bytes(STRESS_TEST_ACCOUNT, 100).await;
        let items: Vec<ReportItem> = raw_items.into_iter()
            .filter_map(|b| ReportItem::from_bytes(&b))
            .collect();

        for item in items {
            if let Some(acc) = account_pool.get(STRESS_TEST_ACCOUNT).await {
                if let Some(conn) = acc.get_connection_by_id(item.conn_id).await {
                    let now = format_timestamp(false);
                    let report_content = format!(
                        "id:{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x} sub:001 dlvrd:001 submit date:{} done date:{} stat:DELIVRD err:000 text:Hello",
                        item.msg_id[0], item.msg_id[1], item.msg_id[2], item.msg_id[3],
                        item.msg_id[4], item.msg_id[5], item.msg_id[6], item.msg_id[7],
                        &now, &now
                    );

                    let deliver = Deliver {
                        msg_id: [0u8; 8],
                        dest_id: item.dest_id,
                        service_id: "SMS".to_string(),
                        tppid: 0,
                        tpudhi: 0,
                        msg_fmt: 15,
                        src_terminal_id: "13800138000".to_string(),
                        src_terminal_type: 0,
                        registered_delivery: 1,
                        msg_content: report_content.as_bytes().to_vec(),
                        link_id: "".to_string(),
                    };

                    let pdu: Pdu = deliver.into();
                    let seq = rand_u32();
                    if conn.write_frame(pdu.to_pdu_bytes(seq).as_slice()).await.is_ok() {
                        report_sent.fetch_add(1, Ordering::Relaxed);
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
                let deliver = Deliver {
                    msg_id: [0u8; 8],
                    dest_id: STRESS_TEST_ACCOUNT.to_string(),
                    service_id: "SMS".to_string(),
                    tppid: 0,
                    tpudhi: 0,
                    msg_fmt: 15,
                    src_terminal_id: src.to_string(),
                    src_terminal_type: 0,
                    registered_delivery: 0,
                    msg_content: content.as_bytes().to_vec(),
                    link_id: "".to_string(),
                };
                let pdu: Pdu = deliver.into();
                let seq = rand_u32();
                if conn.write_frame(pdu.to_pdu_bytes(seq).as_slice()).await.is_ok() {
                    mo_sent.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

fn build_cmpp_submit_pdu(src: &str, dst: &str, content: &str, seq: u32, version: u8) -> Vec<u8> {
    if version == CMPP_VERSION_2_0 {
        let mut submit = SubmitV20::new();
        submit.pk_total = 1;
        submit.pk_number = 1;
        submit.registered_delivery = 1;
        submit.msg_level = 1;
        submit.service_id = "SMS".to_string();
        submit.fee_user_type = 0;
        submit.fee_terminal_id = dst.to_string();
        submit.tppid = 0;
        submit.tpudhi = 0;
        submit.msg_fmt = 15;
        submit.msg_src = "src".to_string();
        submit.fee_type = "01".to_string();
        submit.fee_code = "000000".to_string();
        submit.valid_time = String::new();
        submit.at_time = String::new();
        submit.src_id = src.to_string();
        submit.dest_usr_tl = 1;
        submit.dest_terminal_ids = vec![dst.to_string()];
        submit.msg_content = content.as_bytes().to_vec();
        let pdu: Pdu = submit.into();
        pdu.to_pdu_bytes(seq).as_slice().to_vec()
    } else {
        let mut submit = Submit::new();
        submit.pk_total = 1;
        submit.pk_number = 1;
        submit.registered_delivery = 1;
        submit.msg_level = 1;
        submit.service_id = "SMS".to_string();
        submit.fee_user_type = 0;
        submit.fee_terminal_id = dst.to_string();
        submit.tppid = 0;
        submit.tpudhi = 0;
        submit.msg_fmt = 15;
        submit.msg_src = "src".to_string();
        submit.fee_type = "01".to_string();
        submit.fee_code = "000000".to_string();
        submit.valid_time = String::new();
        submit.at_time = String::new();
        submit.src_id = src.to_string();
        submit.dest_usr_tl = 1;
        submit.dest_terminal_ids = vec![dst.to_string()];
        submit.msg_content = content.as_bytes().to_vec();
        let pdu: Pdu = submit.into();
        pdu.to_pdu_bytes(seq).as_slice().to_vec()
    }
}

async fn mt_producer_task(
    msg_source: Arc<StressMockMessageSource>,
    stats: Arc<TestStats>,
    mt_rate: f64,
    account: String,
    version: u8,
) {
    let inter_msg_interval = Duration::from_secs_f64(1.0 / mt_rate);
    let mut interval = tokio::time::interval(inter_msg_interval);
    let src_numbers = ["13800138000", "13800138001", "13800138002", "13800138003", "13800138004"];
    let mut msg_count: u64 = 0;
    let mut seq: u32 = 1;

    loop {
        interval.tick().await;
        let src = src_numbers[msg_count as usize % src_numbers.len()];
        let content = format!("MT Test #{}", msg_count);
        let pdu_bytes = build_cmpp_submit_pdu(src, STRESS_TEST_ACCOUNT, &content, seq, version);
        seq = seq.wrapping_add(1);

        match msg_source.push(&account, pdu_bytes).await {
            Ok(_) => { stats.submit_sent.fetch_add(1, Ordering::Relaxed); }
            Err(_) => { stats.submit_errors.fetch_add(1, Ordering::Relaxed); }
        }
        msg_count += 1;
    }
}

async fn run_stress_test(version: u8) {
    let total_start = Instant::now();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let version_name = if version == CMPP_VERSION_3_0 { "3.0" } else { "2.0" };

    println!("\n==========================================");
    println!("CMPP {} Stress Test", version_name);
    println!("==========================================");
    println!("Connections: {}", STRESS_TEST_CONNECTIONS);
    println!("Rate: {} msg/s, Duration: {}s", STRESS_TEST_RATE, STRESS_TEST_DURATION_SECS);
    println!("==========================================\n");

    let stats = Arc::new(TestStats::new());
    let msg_source = Arc::new(StressMockMessageSource::new());
    let server_handler = Arc::new(ServerHandler::new(msg_source.clone()));
    let (port, account_pool, server_handle) = start_test_server(server_handler.clone()).await.unwrap();

    let mt_rate = 2500.0;
    let report_rate = 2500.0;
    let mo_rate = 1250.0;

    let report_sent = Arc::new(AtomicU64::new(0));
    let mo_sent = Arc::new(AtomicU64::new(0));

    let report_gen_handle = tokio::spawn(report_generator_task(
        msg_source.clone(), account_pool.clone(), report_sent.clone(), report_rate,
    ));
    let mo_gen_handle = tokio::spawn(mo_generator_task(
        account_pool.clone(), mo_sent.clone(), mo_rate,
    ));

    tokio::time::sleep(Duration::from_secs(1)).await;

    let mut client_conns = Vec::new();
    for i in 0..STRESS_TEST_CONNECTIONS {
        let client_state = Arc::new(ClientState::new(stats.clone(), version));
        let endpoint = Arc::new(EndpointConfig::new(
            "stress-client", "127.0.0.1", port, 1024, 30,
        ).with_window_size(2048).with_log_level(tracing::Level::WARN));

        let conn = connect(
            endpoint, client_state.clone(), CmppDecoder,
            Some(ClientConfig::new()),
            Some(msg_source.clone() as Arc<dyn MessageSource>),
            None,
        ).await.expect("connect");

        let connect_pdu = client_state.build_connect_pdu();
        conn.send_request(connect_pdu).await.expect("send connect");

        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut retries = 0;
        while !client_state.is_connected() && retries < 10 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            retries += 1;
        }
        assert!(client_state.is_connected(), "Connection {} failed", i);
        client_conns.push(conn.clone());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let producer_handle = tokio::spawn(mt_producer_task(
        msg_source.clone(), stats.clone(), mt_rate, "stress-client".to_string(), version,
    ));

    let warmup_secs = total_start.elapsed().as_secs_f64();
    stats.start();

    let monitor_handle = spawn_stats_monitor(
        stats.clone(), msg_source.clone(), "CMPP",
        STRESS_TEST_DURATION_SECS, 1, Some(STRESS_TEST_ACCOUNT.to_string()),
    );

    tokio::time::sleep(Duration::from_secs(STRESS_TEST_DURATION_SECS)).await;
    stats.end();

    producer_handle.abort();
    drain_wait_submit_resp(&stats, Duration::from_secs(10)).await;

    let report_sent_val = report_sent.load(Ordering::Relaxed);
    drain_wait_queue_and_reports_single(
        &stats, &msg_source, STRESS_TEST_ACCOUNT, report_sent_val, Duration::from_secs(15),
    ).await;

    report_gen_handle.abort();
    mo_gen_handle.abort();

    let report_sent_val = report_sent.load(Ordering::Relaxed);
    let mo_sent_val = mo_sent.load(Ordering::Relaxed);
    drain_wait_final_single(&stats, report_sent_val, mo_sent_val, Duration::from_secs(5)).await;

    monitor_handle.abort();

    let total_secs = total_start.elapsed().as_secs_f64();
    let results = StressTestResults::from_stats(&stats, report_sent_val, mo_sent_val, warmup_secs, total_secs);
    print_stress_results(&results, &format!("CMPP {}", version_name), "Stress Test");

    server_handle.abort();

    let submit_sent = results.submit_sent;
    let submit_resp = results.submit_resp;
    let report_matched = results.report_matched;
    let mo_recv = results.mo_received;
    let stress_secs = results.stress_secs;

    let actual_mt_qps = submit_sent as f64 / stress_secs;
    let actual_mo_qps = mo_recv as f64 / stress_secs;
    let expected_min_mt = (mt_rate * STRESS_TEST_DURATION_SECS as f64 * 0.4) as u64;
    let expected_min_mo = (mo_rate * STRESS_TEST_DURATION_SECS as f64 * 0.3) as u64;

    assert!(submit_sent >= expected_min_mt,
        "Expected at least {} MT, got {} ({:.1} QPS)", expected_min_mt, submit_sent, actual_mt_qps);

    let match_ratio = if submit_resp > 0 { report_matched as f64 / submit_resp as f64 } else { 0.0 };
    assert!(report_matched >= submit_resp.saturating_sub(100),
        "Report match too low: {}/{} ({:.1}%)", report_matched, submit_resp, match_ratio * 100.0);

    assert!(mo_recv >= expected_min_mo,
        "Expected at least {} MO, got {} ({:.1} QPS)", expected_min_mo, mo_recv, actual_mo_qps);
}

async fn run_stress_test_with_connections(version: u8, num_connections: usize) {
    let total_start = Instant::now();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let version_name = if version == CMPP_VERSION_3_0 { "3.0" } else { "2.0" };

    println!("\n==========================================");
    println!("CMPP {} Stress Test - {} Connections", version_name, num_connections);
    println!("==========================================");
    println!("Rate: {} msg/s, Duration: {}s", STRESS_TEST_RATE, STRESS_TEST_DURATION_SECS);
    println!("==========================================\n");

    let stats = Arc::new(TestStats::new());
    let msg_source = Arc::new(StressMockMessageSource::new());
    let server_handler = Arc::new(ServerHandler::new(msg_source.clone()));
    let (port, account_pool, server_handle) = start_test_server(server_handler.clone()).await.unwrap();

    let mt_rate = 2500.0;
    let report_rate = 2500.0;
    let mo_rate = 1250.0;

    let report_sent = Arc::new(AtomicU64::new(0));
    let mo_sent = Arc::new(AtomicU64::new(0));

    let report_gen_handle = tokio::spawn(report_generator_task(
        msg_source.clone(), account_pool.clone(), report_sent.clone(), report_rate,
    ));
    let mo_gen_handle = tokio::spawn(mo_generator_task(
        account_pool.clone(), mo_sent.clone(), mo_rate,
    ));

    tokio::time::sleep(Duration::from_secs(1)).await;

    let mut client_conns = Vec::new();
    for i in 0..num_connections {
        let client_state = Arc::new(ClientState::new(stats.clone(), version));
        let endpoint = Arc::new(EndpointConfig::new(
            "stress-client", "127.0.0.1", port, 2048, 30,
        ).with_window_size(2048).with_log_level(tracing::Level::WARN));

        let mut conn = None;
        for retry in 0..50 {
            match connect(
                endpoint.clone(), client_state.clone(), CmppDecoder,
                Some(ClientConfig::new()),
                Some(msg_source.clone() as Arc<dyn MessageSource>),
                None,
            ).await {
                Ok(c) => { conn = Some(c); break; }
                Err(_) => { tokio::time::sleep(Duration::from_millis(500)).await; }
            }
        }
        let conn = conn.expect("Failed to connect after retries");

        let connect_pdu = client_state.build_connect_pdu();
        conn.send_request(connect_pdu).await.expect("send connect");

        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut retries = 0;
        while !client_state.is_connected() && retries < 30 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            retries += 1;
        }
        assert!(client_state.is_connected(), "Connection {} failed", i);
        client_conns.push(conn.clone());
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let producer_handle = tokio::spawn(mt_producer_task(
        msg_source.clone(), stats.clone(), mt_rate, "stress-client".to_string(), version,
    ));

    let warmup_secs = total_start.elapsed().as_secs_f64();
    stats.start();

    let monitor_handle = spawn_stats_monitor(
        stats.clone(), msg_source.clone(), "CMPP",
        STRESS_TEST_DURATION_SECS, 1, Some(STRESS_TEST_ACCOUNT.to_string()),
    );

    tokio::time::sleep(Duration::from_secs(STRESS_TEST_DURATION_SECS)).await;
    stats.end();

    producer_handle.abort();
    drain_wait_submit_resp(&stats, Duration::from_secs(10)).await;

    let report_sent_val = report_sent.load(Ordering::Relaxed);
    drain_wait_queue_and_reports_single(
        &stats, &msg_source, STRESS_TEST_ACCOUNT, report_sent_val, Duration::from_secs(15),
    ).await;

    report_gen_handle.abort();
    mo_gen_handle.abort();

    let report_sent_val = report_sent.load(Ordering::Relaxed);
    let mo_sent_val = mo_sent.load(Ordering::Relaxed);
    drain_wait_final_single(&stats, report_sent_val, mo_sent_val, Duration::from_secs(5)).await;

    monitor_handle.abort();

    let total_secs = total_start.elapsed().as_secs_f64();
    let results = StressTestResults::from_stats(&stats, report_sent_val, mo_sent_val, warmup_secs, total_secs);
    print_stress_results(&results, &format!("CMPP {}", version_name), &format!("Stress Test ({} connections)", num_connections));

    server_handle.abort();

    let submit_sent = results.submit_sent;
    let submit_resp = results.submit_resp;
    let report_matched = results.report_matched;
    let mo_recv = results.mo_received;
    let stress_secs = results.stress_secs;

    let actual_mt_qps = submit_sent as f64 / stress_secs;
    let actual_mo_qps = mo_recv as f64 / stress_secs;
    let expected_min_mt = (mt_rate * STRESS_TEST_DURATION_SECS as f64 * 0.4) as u64;
    let expected_min_mo = (mo_rate * STRESS_TEST_DURATION_SECS as f64 * 0.3) as u64;

    assert!(submit_sent >= expected_min_mt,
        "Expected at least {} MT, got {} ({:.1} QPS)", expected_min_mt, submit_sent, actual_mt_qps);

    let match_ratio = if submit_resp > 0 { report_matched as f64 / submit_resp as f64 } else { 0.0 };
    assert!(report_matched >= submit_resp.saturating_sub(100),
        "Report match too low: {}/{} ({:.1}%)", report_matched, submit_resp, match_ratio * 100.0);

    assert!(mo_recv >= expected_min_mo,
        "Expected at least {} MO, got {} ({:.1} QPS)", expected_min_mo, mo_recv, actual_mo_qps);
}

#[tokio::test]
async fn stress_test_cmpp30_mt_mo() {
    run_stress_test(CMPP_VERSION_3_0).await;
}

#[tokio::test]
async fn stress_test_cmpp20_mt_mo() {
    run_stress_test(CMPP_VERSION_2_0).await;
}

#[tokio::test]
async fn stress_test_cmpp30_5connections() {
    run_stress_test_with_connections(CMPP_VERSION_3_0, 5).await;
}
