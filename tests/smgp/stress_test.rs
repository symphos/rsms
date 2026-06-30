use async_trait::async_trait;
use rsms_connector::{
    ServerBuilder, ClientBuilder, SmgpDecoder,
    AuthCredentials, AuthHandler, AuthResult,
    AccountConfigProvider,
};
use rsms_connector::client::ClientConfig;
use rsms_business::{MessageContext, MessageHandler};
use rsms_core::{ConnectionInfo, RawPdu, EndpointConfig, Protocol, Result};
// 窄腰统一模型：编解码统一走 SmgpAdapter + UnifiedMessage。
// SmgpMsgId 为 payload 格式化助手（非裸 PDU 消息类型），按规则保留用于报告 id 匹配。
use rsms_codec_smgp::adapter::SmgpAdapter;
use rsms_codec_smgp::{SmgpMsgId, compute_login_auth};
use rsms_model::{
    Address, BindMode, DeliveryStatus, Encoding, MessageId, ProtocolAdapter, ProtocolExtra,
    Sequence, UnifiedBind, UnifiedDeliver, UnifiedMessage, UnifiedReport, UnifiedSubmit,
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

/// 把统一模型 MessageId 转回 SMGP 10 字节 MsgId 助手（adapter decode 出 Binary，回填定长数组）。
fn msg_id_to_smgp(id: &MessageId) -> SmgpMsgId {
    let mut arr = [0u8; 10];
    match id {
        MessageId::Binary(b) => {
            let n = b.len().min(10);
            arr[..n].copy_from_slice(&b[..n]);
        }
        MessageId::Text(t) => {
            let tb = t.as_bytes();
            let n = tb.len().min(10);
            arr[..n].copy_from_slice(&tb[..n]);
        }
    }
    SmgpMsgId::new(arr)
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
        // compute_login_auth 保留：鉴权 MD5 非 codec 范畴。
        let authenticator = compute_login_auth(STRESS_TEST_CLIENT_ID, STRESS_TEST_PASSWORD, timestamp).to_vec();

        let bind = UnifiedMessage::Bind(UnifiedBind {
            client_id: STRESS_TEST_CLIENT_ID.to_string(),
            authenticator,
            timestamp,
            version: 0x30,
            system_type: None,
            mode: BindMode::default(),
            login_mode: Some(0),
        });
        let bytes = SmgpAdapter.encode(&bind, Sequence::Plain(self.next_seq())).expect("encode login");
        RawPdu::from(bytes)
    }

    pub fn build_submit_pdu(&self, src: &str, dst: &str, content: &str) -> (RawPdu, u32) {
        // 原 msg_fmt=15(GBK)→Encoding::Gbk；service_id/fee 等方言字段走 ProtocolExtra::Smgp。
        let submit = UnifiedMessage::Submit(UnifiedSubmit {
            src: Address::plain(src),
            dests: vec![Address::plain(dst)],
            content: content.as_bytes().to_vec(),
            encoding: Encoding::Gbk,
            want_report: true,
            concat: None,
            extra: ProtocolExtra::Smgp(rsms_model::SmgpExtra {
                service_id: "SMS".to_string(),
                fee_type: "02".to_string(),
                fee_code: "000000".to_string(),
                fixed_fee: "000000".to_string(),
                charge_term_id: dst.to_string(),
                ..Default::default()
            }),
            tlvs: vec![],
        });
        let seq = self.next_seq();
        let bytes = SmgpAdapter.encode(&submit, Sequence::Plain(seq)).expect("encode submit");
        (RawPdu::from(bytes), seq)
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
                if resp.status == 0 {
                    // 框架已解码，adapter 给出 MessageId::Binary(10B)，转回 SmgpMsgId 存入待匹配队列。
                    let msg_id = msg_id_to_smgp(&resp.msg_id);
                    self.msg_ids.write().unwrap().push_back(msg_id);
                    tracing::trace!("[Client] SubmitResp received, stored msg_id: {:?}", msg_id);
                } else {
                    self.stats.submit_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
            // is_report=1→Report：报告匹配靠正文 id:N。
            UnifiedMessage::Report(r) => {
                let content = String::from_utf8_lossy(&r.raw).to_string();
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
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            // is_report=0→Deliver（MO）。
            UnifiedMessage::Deliver(d) => {
                self.stats.mo_received.fetch_add(1, Ordering::Relaxed);
                tracing::trace!("[Client] Received MO from {}: {:?}", d.src.number, &d.content);
                ctx.reply(UnifiedMessage::DeliverResp).await?;
            }
            _ => {}
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
impl MessageHandler for ServerHandler {
    fn name(&self) -> &'static str {
        "stress-server-handler"
    }

    async fn on_message(&self, ctx: &MessageContext, msg: &UnifiedMessage) -> Result<()> {
        match msg {
            UnifiedMessage::Submit(s) => {
                let count = self.submit_count.fetch_add(1, Ordering::Relaxed);
                let msg_id = SmgpMsgId::from_u64(count);

                tracing::trace!("[Server] Received Submit #{}", count + 1);

                let resp = UnifiedMessage::SubmitResp(UnifiedSubmitResp {
                    msg_id: MessageId::Binary(msg_id.bytes.to_vec()),
                    status: 0,
                });
                ctx.reply(resp).await?;

                let dest_id = s.dests.first().map(|a| a.number.clone()).unwrap_or_default();
                let item = ReportItem {
                    msg_id: msg_id.to_bytes(),
                    conn_id: ctx.conn.id(),
                    dest_id,
                };
                self.msg_source.push_item(STRESS_TEST_CLIENT_ID, item.to_bytes()).await;
            }
            UnifiedMessage::DeliverResp => {
                tracing::trace!("[Server] Received DeliverResp");
            }
            other => {
                tracing::debug!("[Server] Received other message: {:?}", std::mem::discriminant(other));
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
    biz_handler: Arc<dyn MessageHandler>,
) -> Result<(u16, Arc<rsms_connector::AccountPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "stress-test-server",
        "127.0.0.1",
        0,
        500,
        60,
    ).with_protocol(Protocol::Smgp).with_log_level(tracing::Level::WARN));
    let auth = Arc::new(PasswordAuthHandler::new().add_account(STRESS_TEST_CLIENT_ID, STRESS_TEST_PASSWORD));
    let server = ServerBuilder::new(cfg)
        .message_handlers(vec![biz_handler])
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
                    // 报告正文须保持 `id:N ...` 文本（客户端按它匹配 SubmitResp 的 msg_id）。
                    let report_content = format!(
                        "id:{} sub:001 dlvrd:001 submit date:{} done date:{} stat:DELIVRD err:000 text:Hello",
                        msg_id_value, date_part, date_part
                    );

                    // is_report=1 经 UnifiedMessage::Report → adapter；msg_id 头取默认全零（与原 SmgpMsgId::default() 等价）。
                    let unified = UnifiedMessage::Report(UnifiedReport {
                        msg_id: MessageId::Binary(vec![0u8; 10]),
                        status: DeliveryStatus::Delivered,
                        src: Address::plain("13800138000"),
                        dest: Address::plain(item.dest_id),
                        raw: report_content.into_bytes(),
                    });

                    let seq = rand_u32();
                    let bytes = match SmgpAdapter.encode(&unified, Sequence::Plain(seq)) {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::debug!("Failed to encode Report: {:?}", e);
                            continue;
                        }
                    };
                    match conn.write_frame(&bytes).await {
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

                // is_report=0 的 MO：原 msg_fmt=15→Encoding::Gbk；msg_id/recv_time 头字段走 adapter 默认（语义无关）。
                let unified = UnifiedMessage::Deliver(UnifiedDeliver {
                    src: Address::plain(src),
                    dest: Address::plain(STRESS_TEST_CLIENT_ID),
                    content: content.as_bytes().to_vec(),
                    encoding: Encoding::Gbk,
                    concat: None,
                    extra: ProtocolExtra::None,
                    tlvs: vec![],
                });
                let seq = rand_u32();
                let bytes = match SmgpAdapter.encode(&unified, Sequence::Plain(seq)) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::debug!("Failed to encode MO: {:?}", e);
                        continue;
                    }
                };
                match conn.write_frame(&bytes).await {
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
        ).with_protocol(Protocol::Smgp).with_window_size(2048).with_log_level(tracing::Level::WARN));

        let mut conn = None;
        for retry in 0..50 {
            match ClientBuilder::new(endpoint.clone(), client_state.clone(), SmgpDecoder)
                .client_config(ClientConfig::new())
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
