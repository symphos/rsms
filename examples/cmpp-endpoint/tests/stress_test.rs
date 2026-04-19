use async_trait::async_trait;
use rsms_connector::{
    AuthCredentials, AuthHandler, AuthResult,
    AccountConfig, AccountConfigProvider, CmppDecoder, connect,
    protocol::{MessageSource, MessageItem},
};
use rsms_connector::client::{ClientContext, ClientConfig, ClientHandler};
use rsms_core::{ConnectionInfo, EncodedPdu, RawPdu, EndpointConfig, Frame, Result};
use rsms_codec_cmpp::{
    decode_message, CmppMessage, Pdu, Connect, Deliver, DeliverResp,
    CommandId, Submit, SubmitResp, SubmitV20,
};
use rsms_codec_cmpp::auth::compute_connect_auth;
use dashmap::DashMap;
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

#[allow(dead_code)]
struct TestStats {
    submit_sent: AtomicU64,
    submit_resp_received: AtomicU64,
    submit_errors: AtomicU64,
    report_received: AtomicU64,
    report_matched: AtomicU64,
    mo_received: AtomicU64,
    mo_sent: AtomicU64,
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
            mo_sent: AtomicU64::new(0),
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

struct MockMessageSource {
    queues: Arc<DashMap<String, VecDeque<MessageItem>>>,
}

impl MockMessageSource {
    fn new() -> Self {
        Self {
            queues: Arc::new(DashMap::new()),
        }
    }

    async fn push_item(&self, account: &str, item: ReportItem) {
        let mut queue = self.queues.entry(account.to_string()).or_insert_with(VecDeque::new);
        queue.push_back(MessageItem::Single(Arc::new(RawPdu::from_vec(item.to_bytes())) as Arc<dyn EncodedPdu>));
    }

    async fn fetch_items(&self, account: &str, batch_size: usize) -> Vec<ReportItem> {
        if let Some(mut queue) = self.queues.get_mut(account) {
            let mut batch = Vec::with_capacity(batch_size);
            for _ in 0..batch_size {
                if let Some(msg_item) = queue.pop_front() {
                    let data = match msg_item {
                        MessageItem::Single(d) => d,
                        MessageItem::Group { .. } => continue,
                    };
                    if let Some(item) = ReportItem::from_bytes(data.as_bytes()) {
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

    async fn queue_len(&self, account: &str) -> usize {
        self.queues.get(account).map(|q| q.len()).unwrap_or(0)
    }

    async fn push(&self, account: &str, item: Vec<u8>) -> Result<()> {
        let mut queue = self.queues.entry(account.to_string()).or_insert_with(VecDeque::new);
        queue.push_back(MessageItem::Single(Arc::new(RawPdu::from_vec(item)) as Arc<dyn EncodedPdu>));
        Ok(())
    }

    async fn push_group(&self, account: &str, items: Vec<Vec<u8>>) -> Result<()> {
        let mut queue = self.queues.entry(account.to_string()).or_insert_with(VecDeque::new);
        queue.push_back(MessageItem::Group {
            items: items.into_iter()
                .map(|i| Arc::new(RawPdu::from_vec(i)) as Arc<dyn EncodedPdu>)
                .collect(),
        });
        Ok(())
    }
}

#[async_trait]
impl MessageSource for MockMessageSource {
    async fn fetch(&self, account: &str, batch_size: usize) -> Result<Vec<MessageItem>> {
        if let Some(mut queue) = self.queues.get_mut(account) {
            let mut batch = Vec::with_capacity(batch_size);
            for _ in 0..batch_size {
                if let Some(item) = queue.pop_front() {
                    batch.push(item);
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

#[allow(dead_code)]
struct ClientState {
    connected: AtomicBool,
    connect_status: Mutex<Option<u32>>,
    seq: AtomicU64,
    pending_seqs: Arc<RwLock<VecDeque<u32>>>,
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
            pending_seqs: Arc::new(RwLock::new(VecDeque::new())),
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

    pub fn build_submit_pdu(&self, src: &str, dst: &str, content: &str) -> (RawPdu, u32) {
        if self.version == CMPP_VERSION_2_0 {
            return self.build_submit_pdu_v20(src, dst, content);
        }
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
        let seq = self.next_seq();
        (pdu.to_pdu_bytes(seq), seq)
    }

    pub fn build_submit_pdu_v20(&self, src: &str, dst: &str, content: &str) -> (RawPdu, u32) {
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
                tracing::trace!("[Client] SubmitResp received, stored msg_id: {:02x?}", msg_id);
            } else {
                self.stats.submit_errors.fetch_add(1, Ordering::Relaxed);
            }
        } else if cmd_id == CommandId::Deliver as u32 {
            if let Ok(msg) = decode_message(pdu) {
                match msg {
                    CmppMessage::DeliverV20 { deliver: d, .. } => {
                        if d.registered_delivery == 1 {
                            let content = String::from_utf8_lossy(&d.msg_content).to_string();
                            if let Some(report_msg_id) = Self::parse_msg_id_from_report(&content) {
                                tracing::trace!("[Client] Received report: {:02x?}", report_msg_id);
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
                            tracing::trace!("[Client] Received MO from {}: {:02x?}",
                                d.src_terminal_id, &d.msg_content);
                        }
                    }
                    CmppMessage::DeliverV30 { deliver: d, .. } => {
                        if d.registered_delivery == 1 {
                            let content = String::from_utf8_lossy(&d.msg_content).to_string();
                            if let Some(report_msg_id) = Self::parse_msg_id_from_report(&content) {
                                tracing::trace!("[Client] Received report: {:02x?}", report_msg_id);
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
                            tracing::trace!("[Client] Received MO from {}: {:02x?}",
                                d.src_terminal_id, &d.msg_content);
                        }
                    }
                    _ => {}
                }
            }
            let resp = DeliverResp {
                msg_id: [0u8; 8],
                result: 0,
            };
            let resp_pdu: Pdu = resp.into();
            ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;
        }

        Ok(())
    }
}

impl ClientState {
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
    msg_source: Arc<MockMessageSource>,
}

impl ServerHandler {
    #[allow(dead_code)]
    fn new(msg_source: Arc<MockMessageSource>) -> Self {
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
                CmppMessage::SubmitV20 { submit, .. } => {
                    let count = self.submit_count.fetch_add(1, Ordering::Relaxed);
                    let mut msg_id = [0u8; 8];
                    msg_id[4] = ((count >> 24) & 0xFF) as u8;
                    msg_id[5] = ((count >> 16) & 0xFF) as u8;
                    msg_id[6] = ((count >> 8) & 0xFF) as u8;
                    msg_id[7] = (count & 0xFF) as u8;

                    tracing::trace!("[Server] Received SubmitV20 #{}", count + 1);

                    let resp = SubmitResp {
                        msg_id,
                        result: 0,
                    };
                    let resp_pdu: Pdu = resp.into();
                    ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;

                    let dest_id = submit.dest_terminal_ids.first().cloned().unwrap_or_default();
                    self.msg_source.push_item(STRESS_TEST_ACCOUNT, ReportItem {
                        msg_id,
                        conn_id: ctx.conn.id(),
                        dest_id,
                    }).await;
                }
                CmppMessage::SubmitV30 { submit, .. } => {
                    let count = self.submit_count.fetch_add(1, Ordering::Relaxed);
                    let mut msg_id = [0u8; 8];
                    msg_id[4] = ((count >> 24) & 0xFF) as u8;
                    msg_id[5] = ((count >> 16) & 0xFF) as u8;
                    msg_id[6] = ((count >> 8) & 0xFF) as u8;
                    msg_id[7] = (count & 0xFF) as u8;

                    tracing::trace!("[Server] Received SubmitV30 #{}", count + 1);

                    let resp = SubmitResp {
                        msg_id,
                        result: 0,
                    };
                    let resp_pdu: Pdu = resp.into();
                    ctx.conn.write_frame(resp_pdu.to_pdu_bytes(frame.sequence_id).as_slice()).await?;

                    let dest_id = submit.dest_terminal_ids.first().cloned().unwrap_or_default();
                    self.msg_source.push_item(STRESS_TEST_ACCOUNT, ReportItem {
                        msg_id,
                        conn_id: ctx.conn.id(),
                        dest_id,
                    }).await;
                }
                CmppMessage::DeliverResp { .. } => {
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
    accounts: HashMap<String, String>,
}

impl PasswordAuthHandler {
    pub fn new() -> Self {
        Self {
            accounts: HashMap::new(),
        }
    }

    pub fn add_account(mut self, account: &str, password: &str) -> Self {
        self.accounts.insert(account.to_string(), password.to_string());
        self
    }
}

#[async_trait]
impl AuthHandler for PasswordAuthHandler {
    fn name(&self) -> &'static str {
        "password-auth"
    }

    async fn authenticate(&self, _client_id: &str, credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        if let AuthCredentials::Cmpp {
            source_addr,
            authenticator_source,
            timestamp,
            ..
        } = credentials
        {
            if let Some(password) = self.accounts.get(&source_addr) {
                let expected = compute_connect_auth(&source_addr, password, timestamp);
                if expected == authenticator_source {
                    return Ok(AuthResult::success(&source_addr));
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

struct MockAccountConfigProvider;

#[async_trait]
impl AccountConfigProvider for MockAccountConfigProvider {
    async fn get_config(&self, _account: &str) -> Result<AccountConfig> {
        Ok(AccountConfig::new()
            .with_max_qps(5000)
            .with_window_size(2048))
    }
}

use rsms_connector::serve;

async fn start_test_server(
    biz_handler: Arc<dyn rsms_business::BusinessHandler>,
) -> Result<(u16, Arc<rsms_connector::AccountPool>, tokio::task::JoinHandle<()>)> {
    let cfg = Arc::new(EndpointConfig::new(
        "stress-test-server",
        "127.0.0.1",
        0,
        500,
        60,
    ).with_log_level(tracing::Level::WARN));
    let auth = Arc::new(PasswordAuthHandler::new().add_account(STRESS_TEST_ACCOUNT, STRESS_TEST_PASSWORD));
    let server = serve(
        cfg,
        vec![biz_handler],
        Some(auth),
        None,
        Some(Arc::new(MockAccountConfigProvider) as Arc<dyn AccountConfigProvider>),
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

fn rand_u32() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    nanos.wrapping_mul(0x5851F42D4C957F2D_u64 as u32)
}

fn simple_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let s = now % (24 * 60 * 60);
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    let now_days = now / (24 * 60 * 60);
    let days_since_epoch = now_days % (365 * 100);
    let y = days_since_epoch / 365;
    let y = y + 2000;
    let yday = days_since_epoch % 365;
    let mth = yday / 30;
    let d = yday % 30 + 1;
    format!("{:02}{:02}{:02}{:02}{:02}{:02}", (y as u64) % 100, mth + 1, d, h, m, sec)
}

async fn report_generator_task(
    msg_source: Arc<MockMessageSource>,
    account_pool: Arc<rsms_connector::AccountPool>,
    report_sent: Arc<AtomicU64>,
    target_rate: f64,
) {
    let inter_msg_interval = Duration::from_secs_f64(1.0 / target_rate);
    let mut interval = tokio::time::interval(inter_msg_interval);

    loop {
        interval.tick().await;

        let items = msg_source.fetch_items(STRESS_TEST_ACCOUNT, 100).await;

        for item in items {
            if let Some(acc) = account_pool.get(STRESS_TEST_ACCOUNT).await {
                if let Some(conn) = acc.get_connection_by_id(item.conn_id).await {
                    let now = simple_timestamp();
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
    msg_source: Arc<MockMessageSource>,
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
            Ok(_) => {
                stats.submit_sent.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                stats.submit_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
        msg_count += 1;
    }
}

#[tokio::test]
async fn stress_test_cmpp30_mt_mo() {
    run_stress_test(CMPP_VERSION_3_0).await;
}

#[tokio::test]
async fn stress_test_cmpp20_mt_mo() {
    run_stress_test(CMPP_VERSION_2_0).await;
}

async fn run_stress_test(version: u8) {
    let total_start = Instant::now();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let version_name = if version == CMPP_VERSION_3_0 { "3.0" } else { "2.0" };

    println!("\n");
    println!("==========================================");
    println!("CMPP {} Stress Test Configuration", version_name);
    println!("==========================================");
    println!("CMPP Version: {}", version_name);
    println!("Account: {}", STRESS_TEST_ACCOUNT);
    println!("Connections: {}", STRESS_TEST_CONNECTIONS);
    println!("Target Rate: {} msg/s", STRESS_TEST_RATE);
    println!("  - MT (Submit): ~{} msg/s", STRESS_TEST_RATE * 0.6);
    println!("  - Report: ~{} msg/s", STRESS_TEST_RATE * 0.2);
    println!("  - MO: ~{} msg/s", STRESS_TEST_RATE * 0.2);
    println!("Duration: {} seconds", STRESS_TEST_DURATION_SECS);
    println!("==========================================\n");

    let stats = Arc::new(TestStats::new());
    let msg_source = Arc::new(MockMessageSource::new());
    let server_handler = Arc::new(ServerHandler::new(msg_source.clone()));
    let (port, account_pool, server_handle) = start_test_server(server_handler.clone()).await.unwrap();

    tracing::info!("Server started on port {}", port);

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

    for i in 0..STRESS_TEST_CONNECTIONS {
        let client_state = Arc::new(ClientState::new(stats.clone(), version));
        let endpoint = Arc::new(EndpointConfig::new(
            "stress-client",
            "127.0.0.1",
            port,
            1024,
            30,
        ).with_window_size(2048).with_log_level(tracing::Level::WARN));

        let conn = connect(
            endpoint,
            client_state.clone(),
            CmppDecoder,
            Some(ClientConfig::new()),
            Some(msg_source.clone() as Arc<dyn MessageSource>),
            None,
        )
        .await
        .expect("connect");

        let connect_pdu = client_state.build_connect_pdu();
        conn.send_request(connect_pdu).await.expect("send connect");

        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut retries = 0;
        while !client_state.is_connected() && retries < 10 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            retries += 1;
        }

        assert!(client_state.is_connected(), "Connection {} failed after {} retries", i, retries);
        tracing::info!("Client {} connected", i);

        client_conns.push(conn.clone());

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let producer_handle = tokio::spawn(mt_producer_task(
        msg_source.clone(),
        stats.clone(),
        mt_rate,
        "stress-client".to_string(),
        version,
    ));

    let warmup_secs = total_start.elapsed().as_secs_f64();
    stats.start();

    let monitor_handle = tokio::spawn({
        let stats = stats.clone();
        let msg_source = msg_source.clone();
        async move {
            let mut last_submit = 0u64;
            let mut last_submit_resp = 0u64;
            let mut last_report = 0u64;
            let mut last_mo = 0u64;

            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;

                let submit_sent = stats.submit_sent.load(Ordering::Relaxed);
                let submit_resp = stats.submit_resp_received.load(Ordering::Relaxed);
                let reports = stats.report_received.load(Ordering::Relaxed);
                let mo_recv = stats.mo_received.load(Ordering::Relaxed);
                let queue_len = msg_source.queue_len(STRESS_TEST_ACCOUNT).await;

                let elapsed = stats.elapsed_secs();
                let submit_rate = (submit_sent - last_submit) as f64;
                let resp_rate = (submit_resp - last_submit_resp) as f64;
                let report_rate_val = (reports - last_report) as f64;
                let mo_rate_val = (mo_recv - last_mo) as f64;

                println!("\n--- CMPP Stats at {:.1}s ---", elapsed);
                println!("  MT Submit: {} ({:.0} msg/s)", submit_sent, submit_rate);
                println!("  Submit Resp: {} ({:.0} msg/s)", submit_resp, resp_rate);
                println!("  Report RX: {} ({:.0} msg/s)", reports, report_rate_val);
                println!("  MO RX: {} ({:.0} msg/s)", mo_recv, mo_rate_val);
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

    // 阶段1：停止发送端
    producer_handle.abort();

    // 阶段2：等待所有 SubmitResp 回齐
    let mut drain_start = Instant::now();
    loop {
        let sent = stats.submit_sent.load(Ordering::Relaxed);
        let resp = stats.submit_resp_received.load(Ordering::Relaxed);
        if resp >= sent || drain_start.elapsed() > Duration::from_secs(10) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 阶段3：等待 msg_source 队列排空 + Report 全部发出
    drain_start = Instant::now();
    loop {
        let queue_len = msg_source.queue_len(STRESS_TEST_ACCOUNT).await;
        let sent = stats.submit_sent.load(Ordering::Relaxed);
        let report_sent_now = report_sent.load(Ordering::Relaxed);
        if (queue_len == 0 && report_sent_now >= sent) || drain_start.elapsed() > Duration::from_secs(15) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 阶段4：停止 Report/MO 生成器
    report_gen_handle.abort();
    mo_gen_handle.abort();

    // 阶段5：等待 Report/MO 全部收齐
    drain_start = Instant::now();
    loop {
        let report_sent_now = report_sent.load(Ordering::Relaxed);
        let report_recv_now = stats.report_received.load(Ordering::Relaxed);
        let mo_sent_now = mo_sent.load(Ordering::Relaxed);
        let mo_recv_now = stats.mo_received.load(Ordering::Relaxed);
        let report_ok = report_recv_now >= report_sent_now;
        let mo_ok = mo_recv_now >= mo_sent_now;
        if (report_ok && mo_ok) || drain_start.elapsed() > Duration::from_secs(5) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    monitor_handle.abort();

    let total_secs = total_start.elapsed().as_secs_f64();
    let stress_secs = stats.elapsed_secs();

    let submit_sent = stats.submit_sent.load(Ordering::Relaxed);
    let submit_resp = stats.submit_resp_received.load(Ordering::Relaxed);
    let submit_errors = stats.submit_errors.load(Ordering::Relaxed);
    let report_sent_count = report_sent.load(Ordering::Relaxed);
    let report_recv = stats.report_received.load(Ordering::Relaxed);
    let report_matched = stats.report_matched.load(Ordering::Relaxed);
    let mo_sent_count = mo_sent.load(Ordering::Relaxed);
    let mo_recv = stats.mo_received.load(Ordering::Relaxed);

    println!("\n");
    println!("==========================================");
    println!("CMPP {} Stress Test Results", version_name);
    println!("==========================================");
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
    println!("  Sent: {}", report_sent_count);
    println!("  Received: {}", report_recv);
    println!("  MsgId Matched: {}", report_matched);
    println!("  Pending (unmatched): {}", submit_resp.saturating_sub(report_matched));
    println!("");
    println!("[Server -> Client: MO]");
    println!("  Sent: {}", mo_sent_count);
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
    println!("==========================================\n");

    server_handle.abort();

    let actual_mt_qps = submit_sent as f64 / stress_secs;
    let actual_report_qps = report_recv as f64 / stress_secs;
    let actual_mo_qps = mo_recv as f64 / stress_secs;

    let expected_min_mt = (mt_rate * (STRESS_TEST_DURATION_SECS as f64) * 0.4) as u64;
    let _expected_min_report = (report_rate * (STRESS_TEST_DURATION_SECS as f64) * 0.3) as u64;
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

#[tokio::test]
async fn stress_test_cmpp30_5connections() {
    run_stress_test_with_connections(CMPP_VERSION_3_0, 5).await;
}

async fn run_stress_test_with_connections(version: u8, num_connections: usize) {
    let total_start = Instant::now();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let version_name = if version == CMPP_VERSION_3_0 { "3.0" } else { "2.0" };

    println!("\n");
    println!("==========================================");
    println!("CMPP {} Stress Test - {} Connections", version_name, num_connections);
    println!("==========================================");
    println!("CMPP Version: {}", version_name);
    println!("Account: {}", STRESS_TEST_ACCOUNT);
    println!("Connections: {}", num_connections);
    println!("Target Rate: {} msg/s", STRESS_TEST_RATE);
    println!("  - MT (Submit): ~{} msg/s", STRESS_TEST_RATE * 0.6);
    println!("  - Report: ~{} msg/s", STRESS_TEST_RATE * 0.2);
    println!("  - MO: ~{} msg/s", STRESS_TEST_RATE * 0.2);
    println!("Duration: {} seconds", STRESS_TEST_DURATION_SECS);
    println!("==========================================\n");

    let stats = Arc::new(TestStats::new());
    let msg_source = Arc::new(MockMessageSource::new());
    let server_handler = Arc::new(ServerHandler::new(msg_source.clone()));
    let (port, account_pool, server_handle) = start_test_server(server_handler.clone()).await.unwrap();

    tracing::info!("Server started on port {}", port);

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
        let client_state = Arc::new(ClientState::new(stats.clone(), version));
        let endpoint = Arc::new(EndpointConfig::new(
            "stress-client",
            "127.0.0.1",
            port,
            2048,
            30,
        ).with_window_size(2048).with_log_level(tracing::Level::WARN));

        let mut conn = None;
        for retry in 0..50 {
            match connect(
                endpoint.clone(),
                client_state.clone(),
                CmppDecoder,
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
                    tracing::warn!("Connection {} attempt {} failed: {:?}", i, retry, e);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }

        let conn = conn.expect("Failed to establish connection after retries");

        let connect_pdu = client_state.build_connect_pdu();
        conn.send_request(connect_pdu).await.expect("send connect");

        tokio::time::sleep(Duration::from_millis(100)).await;

        let mut retries = 0;
        while !client_state.is_connected() && retries < 30 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            retries += 1;
        }

        assert!(client_state.is_connected(), "Connection {} failed after {} retries", i, retries);
        tracing::info!("Client {} connected", i);

        client_conns.push(conn.clone());

        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let producer_handle = tokio::spawn(mt_producer_task(
        msg_source.clone(),
        stats.clone(),
        mt_rate,
        "stress-client".to_string(),
        version,
    ));

    let warmup_secs = total_start.elapsed().as_secs_f64();
    stats.start();

    let monitor_handle = tokio::spawn({
        let stats = stats.clone();
        let msg_source = msg_source.clone();
        async move {
            let mut last_submit = 0u64;
            let mut last_submit_resp = 0u64;
            let mut last_report = 0u64;
            let mut last_mo = 0u64;

            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;

                let submit_sent = stats.submit_sent.load(Ordering::Relaxed);
                let submit_resp = stats.submit_resp_received.load(Ordering::Relaxed);
                let reports = stats.report_received.load(Ordering::Relaxed);
                let mo_recv = stats.mo_received.load(Ordering::Relaxed);
                let queue_len = msg_source.queue_len(STRESS_TEST_ACCOUNT).await;

                let elapsed = stats.elapsed_secs();
                let submit_rate = (submit_sent - last_submit) as f64;
                let resp_rate = (submit_resp - last_submit_resp) as f64;
                let report_rate_val = (reports - last_report) as f64;
                let mo_rate_val = (mo_recv - last_mo) as f64;

                println!("\n--- CMPP Stats at {:.1}s ---", elapsed);
                println!("  MT Submit: {} ({:.0} msg/s)", submit_sent, submit_rate);
                println!("  Submit Resp: {} ({:.0} msg/s)", submit_resp, resp_rate);
                println!("  Report RX: {} ({:.0} msg/s)", reports, report_rate_val);
                println!("  MO RX: {} ({:.0} msg/s)", mo_recv, mo_rate_val);
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

    // 阶段1：停止发送端
    producer_handle.abort();

    // 阶段2：等待所有 SubmitResp 回齐
    let mut drain_start = Instant::now();
    loop {
        let sent = stats.submit_sent.load(Ordering::Relaxed);
        let resp = stats.submit_resp_received.load(Ordering::Relaxed);
        if resp >= sent || drain_start.elapsed() > Duration::from_secs(10) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 阶段3：等待 msg_source 队列排空 + Report 全部发出
    drain_start = Instant::now();
    loop {
        let queue_len = msg_source.queue_len(STRESS_TEST_ACCOUNT).await;
        let sent = stats.submit_sent.load(Ordering::Relaxed);
        let report_sent_now = report_sent.load(Ordering::Relaxed);
        if (queue_len == 0 && report_sent_now >= sent) || drain_start.elapsed() > Duration::from_secs(15) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 阶段4：停止 Report/MO 生成器
    report_gen_handle.abort();
    mo_gen_handle.abort();

    // 阶段5：等待 Report/MO 全部收齐
    drain_start = Instant::now();
    loop {
        let report_sent_now = report_sent.load(Ordering::Relaxed);
        let report_recv_now = stats.report_received.load(Ordering::Relaxed);
        let mo_sent_now = mo_sent.load(Ordering::Relaxed);
        let mo_recv_now = stats.mo_received.load(Ordering::Relaxed);
        let report_ok = report_recv_now >= report_sent_now;
        let mo_ok = mo_recv_now >= mo_sent_now;
        if (report_ok && mo_ok) || drain_start.elapsed() > Duration::from_secs(5) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    monitor_handle.abort();

    let total_secs = total_start.elapsed().as_secs_f64();
    let stress_secs = stats.elapsed_secs();

    let submit_sent = stats.submit_sent.load(Ordering::Relaxed);
    let submit_resp = stats.submit_resp_received.load(Ordering::Relaxed);
    let submit_errors = stats.submit_errors.load(Ordering::Relaxed);
    let report_sent_count = report_sent.load(Ordering::Relaxed);
    let report_recv = stats.report_received.load(Ordering::Relaxed);
    let report_matched = stats.report_matched.load(Ordering::Relaxed);
    let mo_sent_count = mo_sent.load(Ordering::Relaxed);
    let mo_recv = stats.mo_received.load(Ordering::Relaxed);

    println!("\n");
    println!("==========================================");
    println!("CMPP {} Stress Test Results ({} connections)", version_name, num_connections);
    println!("==========================================");
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
    println!("  Sent: {}", report_sent_count);
    println!("  Received: {}", report_recv);
    println!("  MsgId Matched: {}", report_matched);
    println!("  Pending (unmatched): {}", submit_resp.saturating_sub(report_matched));
    println!("");
    println!("[Server -> Client: MO]");
    println!("  Sent: {}", mo_sent_count);
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
    println!("==========================================\n");

    server_handle.abort();

    let actual_mt_qps = submit_sent as f64 / stress_secs;
    let actual_report_qps = report_recv as f64 / stress_secs;
    let actual_mo_qps = mo_recv as f64 / stress_secs;

    let expected_min_mt = (mt_rate * (STRESS_TEST_DURATION_SECS as f64) * 0.4) as u64;
    let _expected_min_report = (report_rate * (STRESS_TEST_DURATION_SECS as f64) * 0.3) as u64;
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
