use async_trait::async_trait;
use rsms_connector::{
    ServerBuilder, ClientBuilder, SmppDecoder, NoopClientHandler,
    AuthCredentials, AuthHandler, AuthResult,
    AccountConfigProvider,
    protocol::MessageSource,
};
use rsms_connector::client::ClientConfig;
use rsms_business::{MessageContext, MessageHandler};
use rsms_core::{ConnectionInfo, RawPdu, EndpointConfig, Protocol, Result};
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
    drain_wait_submit_resp, drain_wait_queue_and_reports_multi, drain_wait_final_multi,
    spawn_stats_monitor,
};
use std::collections::{HashSet, VecDeque};
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
    system_id: &'static str,
    password: &'static str,
}

const ACCOUNTS: [AccountCredential; NUM_ACCOUNTS] = [
    AccountCredential { system_id: "smppcli0", password: "pwd12345" },
    AccountCredential { system_id: "smppcli1", password: "pwd12345" },
    AccountCredential { system_id: "smppcli2", password: "pwd12345" },
    AccountCredential { system_id: "smppcli3", password: "pwd12345" },
    AccountCredential { system_id: "smppcli4", password: "pwd12345" },
];

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

#[derive(Clone)]
struct ClientAccount {
    system_id: String,
    password: String,
}

struct ClientState {
    connected: AtomicBool,
    login_status: Mutex<Option<u32>>,
    seq: AtomicU64,
    msg_ids: Arc<RwLock<VecDeque<String>>>,
    matched_msg_ids: Arc<Mutex<HashSet<String>>>,
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

    /// BindTransmitter（统一模型）：SMPP 明文鉴权，口令明文字节进 authenticator。
    pub fn build_bind_transmitter_pdu(&self) -> RawPdu {
        let bind = UnifiedMessage::Bind(UnifiedBind {
            client_id: self.account.system_id.clone(),
            authenticator: self.account.password.as_bytes().to_vec(),
            timestamp: 0,
            version: 0x34,
            system_type: Some("CMT".to_string()),
            mode: BindMode::Transmitter,
            login_mode: None,
        });
        RawPdu::from(SmppAdapter.encode(&bind, Sequence::Plain(self.next_seq())).expect("encode bind"))
    }

    /// SubmitSm（统一模型）：registered_delivery=1 落 SmppExtra；data_coding=0x03→Encoding::Other(0x03)。
    pub fn build_submit_sm_pdu(&self, src: &str, dst: &str, content: &str) -> (RawPdu, u32) {
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
        let seq = self.next_seq();
        (RawPdu::from(SmppAdapter.encode(&submit, Sequence::Plain(seq)).expect("encode submit")), seq)
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl MessageHandler for ClientState {
    fn name(&self) -> &'static str {
        "multi-account-stress-client"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        match msg {
            UnifiedMessage::BindResp(r) => {
                *self.login_status.lock().unwrap() = Some(r.status);
                if r.status == 0 {
                    self.connected.store(true, Ordering::Relaxed);
                }
            }
            UnifiedMessage::SubmitResp(resp) => {
                self.stats.submit_resp_received.fetch_add(1, Ordering::Relaxed);
                if resp.status == 0 {
                    // 框架已解码，adapter 给出 MessageId::Text，直接存入待匹配队列。
                    let msg_id = match &resp.msg_id {
                        MessageId::Text(t) => t.clone(),
                        MessageId::Binary(b) => String::from_utf8_lossy(b).into_owned(),
                    };
                    if !msg_id.is_empty() {
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
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            // 普通 MO → 统一模型 Deliver。
            UnifiedMessage::Deliver(_) => {
                self.stats.mo_received.fetch_add(1, Ordering::Relaxed);
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            _ => {}
        }

        Ok(())
    }
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
        "multi-account-stress-server"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        let account = ctx.conn.authenticated_account().await.unwrap_or_else(|| "unknown".to_string());
        match msg {
            UnifiedMessage::Submit(s) => {
                let count = self.submit_count.fetch_add(1, Ordering::Relaxed);
                let msg_id = format!("{}", count);

                // 回 SubmitResp：msg_id 用 MessageId::Text，ctx.reply 自动回显 sequence。
                let resp = UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                    msg_id: MessageId::Text(msg_id.clone()),
                    status: 0,
                });
                ctx.reply(resp).await?;

                let dest_id = s.dests.first().map(|a| a.number.clone()).unwrap_or_default();
                let item = ReportItem {
                    msg_id,
                    conn_id: ctx.conn.id(),
                    dest_id,
                };
                self.msg_source.push_item(&account, item.to_bytes()).await;
            }
            UnifiedMessage::DeliverResp => {}
            _ => {}
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
    biz_handler: Arc<dyn MessageHandler>,
) -> Result<(u16, Arc<rsms_connector::AccountPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "multi-account-stress-server",
        "127.0.0.1",
        0,
        500,
        60,
    ).with_protocol(Protocol::Smpp).with_log_level(tracing::Level::WARN));
    let mut auth = PasswordAuthHandler::new();
    for cred in &ACCOUNTS {
        auth = auth.add_account(cred.system_id, cred.password);
    }
    let server = ServerBuilder::new(cfg)
        .message_handlers(vec![biz_handler])
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
                    if conn.write_frame(&pdu).await.is_ok() {
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

                // 普通 MO（esm_class=0）→ 统一模型 Deliver；data_coding=0x03→Encoding::Other(0x03)。
                let deliver = UnifiedMessage::Deliver(UnifiedDeliver {
                    src: Address { number: src.to_string(), ton: Some(1), npi: Some(1) },
                    dest: Address { number: account.clone(), ton: Some(0), npi: Some(0) },
                    content: content.into_bytes(),
                    encoding: Encoding::Other(0x03),
                    concat: None,
                    extra: ProtocolExtra::Smpp(SmppExtra { esm_class: 0, ..Default::default() }),
                    tlvs: vec![],
                });
                let pdu = SmppAdapter.encode(&deliver, Sequence::Plain(rand_u32())).expect("encode mo");
                if conn.write_frame(&pdu).await.is_ok() {
                    mo_sent.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

async fn mt_producer_task(
    account: String,
    msg_source: Arc<StressMockMessageSource>,
    stats: Arc<TestStats>,
    target_rate: f64,
) {
    let fetch_key = format!("stress-client-{}", account);
    let inter_msg_interval = Duration::from_secs_f64(1.0 / target_rate);
    let mut interval = tokio::time::interval(inter_msg_interval);
    let seq = AtomicU64::new(1);
    let mut msg_count: u64 = 0;

    loop {
        interval.tick().await;
        let content = format!("MT Test #{}", msg_count);
        // MT SubmitSm（统一模型）：registered_delivery=1 落 SmppExtra；data_coding=0x03→Encoding::Other(0x03)。
        let submit = UnifiedMessage::Submit(UnifiedSubmit {
            src: Address::plain("13800138000"),
            dests: vec![Address { number: account.clone(), ton: Some(1), npi: Some(1) }],
            content: content.into_bytes(),
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

        let seq_val = seq.fetch_add(1, Ordering::Relaxed) as u32;
        let pdu_bytes = SmppAdapter.encode(&submit, Sequence::Plain(seq_val)).expect("encode submit");
        if msg_source.push(&fetch_key, pdu_bytes).await.is_ok() {
            stats.submit_sent.fetch_add(1, Ordering::Relaxed);
        }
        msg_count += 1;
    }
}

#[tokio::test]
async fn stress_test_smpp_5accounts_5connections() {
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
    println!("SMPP Multi-Account Stress Test");
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
        let account = cred.system_id.to_string();
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
                    system_id: account.clone(),
                    password: password.clone(),
                },
            ));
            let endpoint = Arc::new(EndpointConfig::new(
                &format!("stress-client-{}", account),
                "127.0.0.1",
                port,
                1024,
                30,
            ).with_window_size(WINDOW_SIZE as u16).with_protocol(Protocol::Smpp).with_log_level(tracing::Level::WARN));

            let conn = ClientBuilder::new(endpoint, Arc::new(NoopClientHandler), SmppDecoder)
                .with_message_handler(client_state.clone())
                .client_config(ClientConfig::new())
                .message_source(msg_source.clone() as Arc<dyn MessageSource>)
                .connect()
                .await
                .expect("connect");

            let bind_pdu = client_state.build_bind_transmitter_pdu();
            conn.send_request(bind_pdu).await.expect("send bind");

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

    let monitor_handle = spawn_stats_monitor(
        stats.clone(),
        msg_source.clone(),
        "SMPP",
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
    drain_wait_queue_and_reports_multi(
        &stats, &msg_source, total_report_sent, Duration::from_secs(30),
    ).await;

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
    let results = StressTestResults::from_stats(&stats, total_report_sent, total_mo_sent, warmup_secs, total_secs);
    print_stress_results(&results, "SMPP", "Multi-Account Stress Test");

    println!("[配置]");
    println!("  账号数: {}", NUM_ACCOUNTS);
    println!("  每账号连接数: {}", CONNECTIONS_PER_ACCOUNT);
    println!("  总连接数: {}", NUM_ACCOUNTS * CONNECTIONS_PER_ACCOUNT);
    println!();
    println!("[Per-Account TPS - 压测时间]");
    println!("  MT Submit:  {:.1}", results.submit_sent as f64 / results.stress_secs / NUM_ACCOUNTS as f64);
    println!("  Report:     {:.1}", results.report_received as f64 / results.stress_secs / NUM_ACCOUNTS as f64);
    println!("  MO:         {:.1}", results.mo_received as f64 / results.stress_secs / NUM_ACCOUNTS as f64);

    server_handle.abort();

    let submit_sent = results.submit_sent;
    let submit_resp = results.submit_resp;
    let report_matched = results.report_matched;
    let stress_secs = results.stress_secs;

    let actual_mt_qps = submit_sent as f64 / stress_secs;
    let expected_min = (MT_RATE_PER_ACCOUNT * NUM_ACCOUNTS as f64 * STRESS_TEST_DURATION_SECS as f64 * 0.4) as u64;
    assert!(submit_sent >= expected_min, "Expected at least {} MT, got {} ({:.1} QPS)", expected_min, submit_sent, actual_mt_qps);

    let match_ratio = if submit_resp > 0 { report_matched as f64 / submit_resp as f64 } else { 0.0 };
    assert!(
        report_matched >= submit_resp.saturating_sub(100),
        "Report match too low: {}/{} ({:.1}%)", report_matched, submit_resp, match_ratio * 100.0
    );
}
