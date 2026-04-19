use async_trait::async_trait;
use rsms_connector::{
    AuthCredentials, AuthHandler, AuthResult,
    AccountConfig, AccountConfigProvider, SmgpDecoder, connect,
    serve,
    protocol::{MessageSource, MessageItem},
};
use rsms_connector::client::{ClientContext, ClientConfig, ClientHandler};
use rsms_core::{ConnectionInfo, EncodedPdu, RawPdu, EndpointConfig, Frame, Result};
use rsms_codec_smgp::{
    decode_message, SmgpMessage, Pdu,
    CommandId, Login, SmgpMsgId, Submit, SubmitResp, Deliver, DeliverResp,
    compute_login_auth,
};
use rsms_codec_smgp::datatypes::tlv::OptionalParameters;
use dashmap::DashMap;
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

struct AccountCredential {
    client_id: &'static str,
    password: &'static str,
}

const ACCOUNTS: [AccountCredential; NUM_ACCOUNTS] = [
    AccountCredential { client_id: "106900", password: "password123" },
    AccountCredential { client_id: "106901", password: "password123" },
    AccountCredential { client_id: "106902", password: "password123" },
    AccountCredential { client_id: "106903", password: "password123" },
    AccountCredential { client_id: "106904", password: "password123" },
];

struct TestStats {
    submit_sent: AtomicU64,
    submit_resp_received: AtomicU64,
    submit_errors: AtomicU64,
    report_received: AtomicU64,
    report_matched: AtomicU64,
    mo_received: AtomicU64,
    start_time: RwLock<Option<Instant>>,
    end_time: RwLock<Option<Instant>>,
}

impl TestStats {
    fn new() -> Self {
        Self {
            submit_sent: AtomicU64::new(0),
            submit_resp_received: AtomicU64::new(0),
            submit_errors: AtomicU64::new(0),
            report_received: AtomicU64::new(0),
            report_matched: AtomicU64::new(0),
            mo_received: AtomicU64::new(0),
            start_time: RwLock::new(None),
            end_time: RwLock::new(None),
        }
    }

    fn start(&self) {
        *self.start_time.write().unwrap() = Some(Instant::now());
    }

    fn end(&self) {
        *self.end_time.write().unwrap() = Some(Instant::now());
    }

    fn elapsed_secs(&self) -> f64 {
        let start = self.start_time.read().unwrap();
        if let Some(start) = *start {
            if let Some(end) = *self.end_time.read().unwrap() {
                return (end - start).as_secs_f64();
            }
            return (Instant::now() - start).as_secs_f64();
        }
        0.0
    }
}

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

struct MockMessageSource {
    queues: Arc<DashMap<String, VecDeque<Vec<u8>>>>,
}

impl MockMessageSource {
    fn new() -> Self {
        Self {
            queues: Arc::new(DashMap::new()),
        }
    }

    async fn push_item(&self, account: &str, item: ReportItem) {
        let queues = self.queues.clone();
        let mut queue = queues.entry(account.to_string()).or_insert_with(VecDeque::new);
        queue.push_back(item.to_bytes());
    }

    async fn fetch_items(&self, account: &str, batch_size: usize) -> Vec<ReportItem> {
        if let Some(mut queue) = self.queues.get_mut(account) {
            let mut batch = Vec::with_capacity(batch_size);
            for _ in 0..batch_size {
                if let Some(data) = queue.pop_front() {
                    if let Some(item) = ReportItem::from_bytes(&data) {
                        batch.push(item);
                    }
                } else {
                    break;
                }
            }
            batch
        } else {
            Vec::new()
        }
    }

    async fn total_queue_len(&self) -> usize {
        self.queues.iter().map(|r| r.value().len()).sum()
    }

    async fn push(&self, account: &str, item: Vec<u8>) -> Result<()> {
        let mut queue = self.queues.entry(account.to_string()).or_insert_with(VecDeque::new);
        queue.push_back(item);
        Ok(())
    }
}

#[async_trait]
impl MessageSource for MockMessageSource {
    async fn fetch(&self, account: &str, batch_size: usize) -> Result<Vec<MessageItem>> {
        if let Some(mut queue) = self.queues.get_mut(account) {
            let mut batch = Vec::with_capacity(batch_size);
            for _ in 0..batch_size {
                if let Some(data) = queue.pop_front() {
                    batch.push(MessageItem::Single(Arc::new(RawPdu::from_vec(data)) as Arc<dyn EncodedPdu>));
                } else {
                    break;
                }
            }
            Ok(batch)
        } else {
            Ok(Vec::new())
        }
    }
}

#[derive(Clone)]
struct ClientAccount {
    client_id: String,
    password: String,
}

struct ClientState {
    connected: AtomicBool,
    login_status: Mutex<Option<u32>>,
    seq: AtomicU64,
    msg_ids: Arc<RwLock<VecDeque<SmgpMsgId>>>,
    matched_msg_ids: Arc<Mutex<HashSet<SmgpMsgId>>>,
    stats: Arc<TestStats>,
    account: ClientAccount,
}

impl ClientState {
    fn new(stats: Arc<TestStats>, account: ClientAccount) -> Self {
        Self {
            connected: AtomicBool::new(false),
            login_status: Mutex::new(None),
            seq: AtomicU64::new(1),
            msg_ids: Arc::new(RwLock::new(VecDeque::new())),
            matched_msg_ids: Arc::new(Mutex::new(HashSet::new())),
            stats,
            account,
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u32
    }

    pub fn build_login_pdu(&self) -> RawPdu {
        let timestamp = 0u32;
        let authenticator = compute_login_auth(&self.account.client_id, &self.account.password, timestamp);
        let login = Login {
            client_id: self.account.client_id.clone(),
            authenticator,
            login_mode: 0,
            timestamp,
            version: 0x30,
        };
        let pdu: Pdu = login.into();
        pdu.to_pdu_bytes(self.next_seq())
    }

    fn is_connected(&self) -> bool {
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
            } else {
                self.stats.submit_errors.fetch_add(1, Ordering::Relaxed);
            }
        } else if cmd_id == CommandId::Deliver as u32 {
            if let Ok(msg) = decode_message(pdu) {
                if let SmgpMessage::Deliver { deliver: d, .. } = msg {
                    if d.is_report == 1 {
                        let content = String::from_utf8_lossy(&d.msg_content).to_string();
                        if let Some(report_msg_id) = Self::parse_msg_id_from_report(&content) {
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
                    }
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

struct ServerHandler {
    submit_count: AtomicU64,
    msg_source: Arc<MockMessageSource>,
}

impl ServerHandler {
    fn new(msg_source: Arc<MockMessageSource>) -> Self {
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
                SmgpMessage::Submit { submit: s, .. } => {
                    let count = self.submit_count.fetch_add(1, Ordering::Relaxed);
                    let msg_id = SmgpMsgId::from_u64(count);

                    let resp = SubmitResp { msg_id, status: 0 };
                    let resp_pdu: Pdu = resp.into();
                    ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;

                    let dest_id = s.dest_term_ids.first().cloned().unwrap_or_default();
                    self.msg_source.push_item(&account, ReportItem {
                        msg_id: msg_id.to_bytes(),
                        conn_id: ctx.conn.id(),
                        dest_id,
                    }).await;
                }
                SmgpMessage::DeliverResp { .. } => {}
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

    fn add_account(mut self, client_id: &str, password: &str) -> Self {
        self.accounts.insert(client_id.to_string(), password.to_string());
        self
    }
}

#[async_trait]
impl AuthHandler for PasswordAuthHandler {
    fn name(&self) -> &'static str { "smgp-password-auth" }

    async fn authenticate(&self, _client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        if let AuthCredentials::Smgp { client_id, authenticator, .. } = credentials {
            if let Some(password) = self.accounts.get(&client_id) {
                let expected = compute_login_auth(&client_id, password, 0);
                if expected == authenticator {
                    return Ok(AuthResult::success(&client_id));
                }
            }
            Ok(AuthResult::failure(1, "Invalid credentials"))
        } else {
            Ok(AuthResult::failure(1, "Invalid credentials"))
        }
    }
}

struct MockAccountConfigProvider;

#[async_trait]
impl AccountConfigProvider for MockAccountConfigProvider {
    async fn get_config(&self, _account: &str) -> Result<AccountConfig> {
        Ok(AccountConfig::new()
            .with_max_qps(10000)
            .with_window_size(4096))
    }
}

async fn start_test_server(
    biz_handler: Arc<dyn rsms_business::BusinessHandler>,
) -> Result<(u16, Arc<rsms_connector::AccountPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "multi-account-stress-server",
        "127.0.0.1",
        0,
        500,
        60,
    ).with_protocol("smgp").with_log_level(tracing::Level::WARN));
    let mut auth = PasswordAuthHandler::new();
    for cred in &ACCOUNTS {
        auth = auth.add_account(cred.client_id, cred.password);
    }
    let server = serve(
        cfg,
        vec![biz_handler],
        Some(Arc::new(auth)),
        None,
        Some(Arc::new(MockAccountConfigProvider) as Arc<dyn AccountConfigProvider>),
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

fn rand_u32() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos();
    nanos.wrapping_mul(0x5851F42D4C957F2D_u64 as u32)
}

fn smgp_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let s = now % (24 * 60 * 60);
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    let now_days = now / (24 * 60 * 60);
    let days_since_epoch = now_days % (365 * 100);
    let y = days_since_epoch / 365 + 2000;
    let yday = days_since_epoch % 365;
    let mth = yday / 30;
    let d = yday % 30 + 1;
    format!("{:04}{:02}{:02}{:02}{:02}{:02}", y, mth + 1, d, h, m, sec)
}

async fn report_generator_task(
    account: String,
    msg_source: Arc<MockMessageSource>,
    account_pool: Arc<rsms_connector::AccountPool>,
    report_sent: Arc<AtomicU64>,
    target_rate: f64,
) {
    let inter_msg_interval = Duration::from_secs_f64(1.0 / target_rate);
    let mut interval = tokio::time::interval(inter_msg_interval);

    loop {
        interval.tick().await;
        let items = msg_source.fetch_items(&account, 100).await;
        for item in items {
            if let Some(acc) = account_pool.get(&account).await {
                if let Some(conn) = acc.get_connection_by_id(item.conn_id).await {
                    let now = smgp_timestamp();
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
                    if conn.write_frame(pdu.to_pdu_bytes(seq).as_slice()).await.is_ok() {
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
                let now = smgp_timestamp();
                let mo_count = mo_sent.load(Ordering::Relaxed);

                let deliver = Deliver {
                    msg_id: SmgpMsgId::from_u64(mo_count + 1000000),
                    is_report: 0,
                    msg_fmt: 15,
                    recv_time: now,
                    src_term_id: src.to_string(),
                    dest_term_id: account.clone(),
                    msg_content: content.as_bytes().to_vec(),
                    reserve: [0u8; 8],
                    optional_params: OptionalParameters::new(),
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

fn build_smgp_submit_pdu(src: &str, dst: &str, content: &str, seq: u32) -> Vec<u8> {
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
    pdu.to_pdu_bytes(seq).as_slice().to_vec()
}

async fn mt_producer_task(
    account: String,
    msg_source: Arc<MockMessageSource>,
    stats: Arc<TestStats>,
    target_rate: f64,
) {
    let fetch_key = format!("stress-client-{}", account);
    let inter_msg_interval = Duration::from_secs_f64(1.0 / target_rate);
    let mut interval = tokio::time::interval(inter_msg_interval);
    let seq = AtomicU64::new(1);
    let mut msg_count: u64 = 0;
    let src = "13800138000";
    loop {
        interval.tick().await;
        let content = format!("MT Test #{}", msg_count);
        let pdu_bytes = build_smgp_submit_pdu(src, &account, &content, seq.fetch_add(1, Ordering::Relaxed) as u32);
        match msg_source.push(&fetch_key, pdu_bytes).await {
            Ok(_) => { stats.submit_sent.fetch_add(1, Ordering::Relaxed); }
            Err(_) => { stats.submit_errors.fetch_add(1, Ordering::Relaxed); }
        }
        msg_count += 1;
    }
}

#[tokio::test]
async fn stress_test_smgp_5accounts_5connections() {
    let total_start = Instant::now();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let stats = Arc::new(TestStats::new());
    let msg_source = Arc::new(MockMessageSource::new());
    let server_handler = Arc::new(ServerHandler::new(msg_source.clone()));
    let (port, account_pool, server_handle) = start_test_server(server_handler.clone()).await.unwrap();

    println!("\n");
    println!("==========================================================");
    println!("SMGP Multi-Account Stress Test");
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

    for cred in ACCOUNTS.iter() {
        let account = cred.client_id.to_string();
        let password = cred.password.to_string();

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

        for conn_idx in 0..CONNECTIONS_PER_ACCOUNT {
            let client_state = Arc::new(ClientState::new(
                stats.clone(),
                ClientAccount {
                    client_id: account.clone(),
                    password: password.clone(),
                },
            ));
            let endpoint = Arc::new(EndpointConfig::new(
                &format!("stress-client-{}", account),
                "127.0.0.1",
                port,
                1024,
                30,
            ).with_window_size(WINDOW_SIZE as u16).with_log_level(tracing::Level::WARN));

            let conn = connect(
                endpoint,
                client_state.clone(),
                SmgpDecoder,
                Some(ClientConfig::new()),
                Some(msg_source.clone() as Arc<dyn MessageSource>),
                None,
            )
            .await
            .expect("connect");

            let login_pdu = client_state.build_login_pdu();
            conn.send_request(login_pdu).await.expect("send login");

            tokio::time::sleep(Duration::from_millis(50)).await;

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
            MT_RATE_PER_ACCOUNT,
        )));

        tracing::info!("Account {} ready ({} connections)", account, CONNECTIONS_PER_ACCOUNT);
    }

    let warmup_secs = total_start.elapsed().as_secs_f64();
    stats.start();

    println!("All {} accounts x {} connections ready, stress test started", NUM_ACCOUNTS, CONNECTIONS_PER_ACCOUNT);

    let monitor_handle = tokio::spawn({
        let stats = stats.clone();
        let msg_source = msg_source.clone();
        async move {
            let mut last_submit = 0u64;
            let mut last_submit_resp = 0u64;
            let mut last_report = 0u64;
            let mut last_mo = 0u64;

            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let submit_sent = stats.submit_sent.load(Ordering::Relaxed);
                let submit_resp = stats.submit_resp_received.load(Ordering::Relaxed);
                let reports = stats.report_received.load(Ordering::Relaxed);
                let mo_recv = stats.mo_received.load(Ordering::Relaxed);
                let queue_len = msg_source.total_queue_len().await;

                let elapsed = stats.elapsed_secs();
                println!("\n--- Stats at {:.0}s ---", elapsed);
                println!("  MT Submit: {} ({:.0} msg/s)", submit_sent, (submit_sent - last_submit) as f64 / 5.0);
                println!("  SubmitResp: {} ({:.0} msg/s)", submit_resp, (submit_resp - last_submit_resp) as f64 / 5.0);
                println!("  Report RX: {} ({:.0} msg/s)", reports, (reports - last_report) as f64 / 5.0);
                println!("  MO RX: {} ({:.0} msg/s)", mo_recv, (mo_recv - last_mo) as f64 / 5.0);
                println!("  Queue Pending: {}", queue_len);

                last_submit = submit_sent;
                last_submit_resp = submit_resp;
                last_report = reports;
                last_mo = mo_recv;

                if elapsed >= STRESS_TEST_DURATION_SECS as f64 {
                    break;
                }
            }
        }
    });

    tokio::time::sleep(Duration::from_secs(STRESS_TEST_DURATION_SECS)).await;
    stats.end();

    for handle in &producer_handles {
        handle.abort();
    }

    let drain_start = Instant::now();
    loop {
        let sent = stats.submit_sent.load(Ordering::Relaxed);
        let resp = stats.submit_resp_received.load(Ordering::Relaxed);
        if resp >= sent || drain_start.elapsed() > Duration::from_secs(15) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let drain_start2 = Instant::now();
    loop {
        let queue_len = msg_source.total_queue_len().await;
        let sent = stats.submit_sent.load(Ordering::Relaxed);
        let rs: u64 = report_gen_handles.iter().map(|(_, c, _)| c.load(Ordering::Relaxed)).sum();
        if (queue_len == 0 && rs >= sent) || drain_start2.elapsed() > Duration::from_secs(30) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    for (_, _, handle) in &report_gen_handles {
        handle.abort();
    }
    for (_, _, handle) in &mo_gen_handles {
        handle.abort();
    }

    let drain_start3 = Instant::now();
    loop {
        let total_rs: u64 = report_gen_handles.iter().map(|(_, c, _)| c.load(Ordering::Relaxed)).sum();
        let total_ms: u64 = mo_gen_handles.iter().map(|(_, c, _)| c.load(Ordering::Relaxed)).sum();
        let rr = stats.report_received.load(Ordering::Relaxed);
        let mr = stats.mo_received.load(Ordering::Relaxed);
        if (rr >= total_rs && mr >= total_ms) || drain_start3.elapsed() > Duration::from_secs(10) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    monitor_handle.abort();

    let total_secs = total_start.elapsed().as_secs_f64();
    let stress_secs = stats.elapsed_secs();

    let submit_sent = stats.submit_sent.load(Ordering::Relaxed);
    let submit_resp = stats.submit_resp_received.load(Ordering::Relaxed);
    let submit_errors = stats.submit_errors.load(Ordering::Relaxed);
    let total_report_sent: u64 = report_gen_handles.iter().map(|(_, c, _)| c.load(Ordering::Relaxed)).sum();
    let total_mo_sent: u64 = mo_gen_handles.iter().map(|(_, c, _)| c.load(Ordering::Relaxed)).sum();
    let report_recv = stats.report_received.load(Ordering::Relaxed);
    let report_matched = stats.report_matched.load(Ordering::Relaxed);
    let mo_recv = stats.mo_received.load(Ordering::Relaxed);

    println!("\n");
    println!("==========================================================");
    println!("SMGP Multi-Account Stress Test Results");
    println!("==========================================================");
    println!("[配置]");
    println!("  账号数: {}", NUM_ACCOUNTS);
    println!("  每账号连接数: {}", CONNECTIONS_PER_ACCOUNT);
    println!("  总连接数: {}", NUM_ACCOUNTS * CONNECTIONS_PER_ACCOUNT);
    println!("");
    println!("[时间]");
    println!("  预热耗时:   {:.2}s (建连+认证)", warmup_secs);
    println!("  压测时间:   {:.2}s", stress_secs);
    println!("  收尾耗时:   {:.2}s", total_secs - warmup_secs - stress_secs);
    println!("  程序总时间: {:.2}s", total_secs);
    println!("");
    println!("[Client -> Server: MT Submit]");
    println!("  Sent: {}", submit_sent);
    println!("  Resp: {}", submit_resp);
    println!("  Errors: {}", submit_errors);
    println!("");
    println!("[Server -> Client: Report]");
    println!("  Sent: {}", total_report_sent);
    println!("  Received: {}", report_recv);
    println!("  MsgId Matched: {}", report_matched);
    println!("  Pending (unmatched): {}", submit_resp.saturating_sub(report_matched));
    println!("");
    println!("[Server -> Client: MO]");
    println!("  Sent: {}", total_mo_sent);
    println!("  Received: {}", mo_recv);
    println!("");
    println!("[TPS - 压测时间({:.1}s)]", stress_secs);
    println!("  MT Submit:  {:.1}", submit_sent as f64 / stress_secs);
    println!("  SubmitResp: {:.1}", submit_resp as f64 / stress_secs);
    println!("  Report:     {:.1}", report_recv as f64 / stress_secs);
    println!("  MO:         {:.1}", mo_recv as f64 / stress_secs);
    println!("");
    println!("[TPS - 程序总时间({:.1}s)]", total_secs);
    println!("  MT Submit:  {:.1}", submit_sent as f64 / total_secs);
    println!("  SubmitResp: {:.1}", submit_resp as f64 / total_secs);
    println!("  Report:     {:.1}", report_recv as f64 / total_secs);
    println!("  MO:         {:.1}", mo_recv as f64 / total_secs);
    println!("");
    println!("[Per-Account TPS - 压测时间]");
    println!("  MT Submit:  {:.1}", submit_sent as f64 / stress_secs / NUM_ACCOUNTS as f64);
    println!("  Report:     {:.1}", report_recv as f64 / stress_secs / NUM_ACCOUNTS as f64);
    println!("  MO:         {:.1}", mo_recv as f64 / stress_secs / NUM_ACCOUNTS as f64);
    println!("==========================================================\n");

    server_handle.abort();

    let actual_mt_qps = submit_sent as f64 / stress_secs;
    let expected_min = (MT_RATE_PER_ACCOUNT * NUM_ACCOUNTS as f64 * STRESS_TEST_DURATION_SECS as f64 * 0.4) as u64;
    assert!(submit_sent >= expected_min, "Expected at least {} MT, got {} ({:.1} QPS)", expected_min, submit_sent, actual_mt_qps);

    let match_ratio = if submit_resp > 0 { report_matched as f64 / submit_resp as f64 } else { 0.0 };
    assert!(
        report_matched >= submit_resp.saturating_sub(100),
        "Report match too low: {}/{} ({:.1}%)", report_matched, submit_resp, match_ratio * 100.0
    );
}
