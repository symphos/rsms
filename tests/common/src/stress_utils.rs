use crate::StressMockMessageSource;
use crate::TestStats;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

pub fn rand_u32() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    nanos.wrapping_mul(0x5851F42D4C957F2D_u64 as u32)
}

pub fn format_timestamp(use_4_digit_year: bool) -> String {
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
    let y = days_since_epoch / 365 + 2000;
    let yday = days_since_epoch % 365;
    let mth = yday / 30;
    let d = yday % 30 + 1;
    if use_4_digit_year {
        format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}",
            y, mth + 1, d, h, m, sec
        )
    } else {
        format!(
            "{:02}{:02}{:02}{:02}{:02}{:02}",
            (y as u64) % 100, mth + 1, d, h, m, sec
        )
    }
}

pub struct StressTestResults {
    pub warmup_secs: f64,
    pub stress_secs: f64,
    pub total_secs: f64,
    pub submit_sent: u64,
    pub submit_resp: u64,
    pub submit_errors: u64,
    pub report_sent: u64,
    pub report_received: u64,
    pub report_matched: u64,
    pub mo_sent: u64,
    pub mo_received: u64,
}

impl StressTestResults {
    pub fn from_stats(
        stats: &TestStats,
        report_sent: u64,
        mo_sent: u64,
        warmup_secs: f64,
        total_secs: f64,
    ) -> Self {
        Self {
            warmup_secs,
            stress_secs: stats.elapsed_secs(),
            total_secs,
            submit_sent: stats.submit_sent.load(Ordering::Relaxed),
            submit_resp: stats.submit_resp_received.load(Ordering::Relaxed),
            submit_errors: stats.submit_errors.load(Ordering::Relaxed),
            report_sent,
            report_received: stats.report_received.load(Ordering::Relaxed),
            report_matched: stats.report_matched.load(Ordering::Relaxed),
            mo_sent,
            mo_received: stats.mo_received.load(Ordering::Relaxed),
        }
    }
}

pub fn print_stress_results(results: &StressTestResults, protocol_name: &str, subtitle: &str) {
    let stress_secs = results.stress_secs;
    let total_secs = results.total_secs;

    println!();
    println!("==========================================");
    println!("{} {} Results", protocol_name, subtitle);
    println!("==========================================");
    println!("[时间]");
    println!("  预热耗时:   {:.2}s (建连+认证)", results.warmup_secs);
    println!("  压测时间:   {:.2}s", stress_secs);
    println!(
        "  收尾耗时:   {:.2}s",
        total_secs - results.warmup_secs - stress_secs
    );
    println!("  程序总时间: {:.2}s", total_secs);
    println!();
    println!("[Client -> Server: MT Submit]");
    println!("  Sent: {}", results.submit_sent);
    println!("  Resp: {}", results.submit_resp);
    println!("  Errors: {}", results.submit_errors);
    println!();
    println!("[Server -> Client: Report]");
    println!("  Sent: {}", results.report_sent);
    println!("  Received: {}", results.report_received);
    println!("  MsgId Matched: {}", results.report_matched);
    println!(
        "  Pending (unmatched): {}",
        results.submit_resp.saturating_sub(results.report_matched)
    );
    println!();
    println!("[Server -> Client: MO]");
    println!("  Sent: {}", results.mo_sent);
    println!("  Received: {}", results.mo_received);
    println!();
    println!("[TPS - 压测时间({:.1}s)]", stress_secs);
    println!(
        "  MT Submit:  {:.1}",
        results.submit_sent as f64 / stress_secs
    );
    println!(
        "  SubmitResp: {:.1}",
        results.submit_resp as f64 / stress_secs
    );
    println!(
        "  Report:     {:.1}",
        results.report_received as f64 / stress_secs
    );
    println!(
        "  MO:         {:.1}",
        results.mo_received as f64 / stress_secs
    );
    println!();
    println!("[TPS - 程序总时间({:.1}s)]", total_secs);
    println!(
        "  MT Submit:  {:.1}",
        results.submit_sent as f64 / total_secs
    );
    println!(
        "  SubmitResp: {:.1}",
        results.submit_resp as f64 / total_secs
    );
    println!(
        "  Report:     {:.1}",
        results.report_received as f64 / total_secs
    );
    println!(
        "  MO:         {:.1}",
        results.mo_received as f64 / total_secs
    );
    println!("==========================================\n");
}

pub async fn drain_wait_submit_resp(stats: &TestStats, timeout: Duration) {
    let start = tokio::time::Instant::now();
    loop {
        let sent = stats.submit_sent.load(Ordering::Relaxed);
        let resp = stats.submit_resp_received.load(Ordering::Relaxed);
        if resp >= sent || start.elapsed() > timeout {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn drain_wait_queue_and_reports_single(
    stats: &TestStats,
    msg_source: &StressMockMessageSource,
    account: &str,
    report_sent: u64,
    timeout: Duration,
) {
    let start = tokio::time::Instant::now();
    loop {
        let queue_len = msg_source.queue_len(account).await;
        let sent = stats.submit_sent.load(Ordering::Relaxed);
        if (queue_len == 0 && report_sent >= sent) || start.elapsed() > timeout {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn drain_wait_queue_and_reports_multi(
    stats: &TestStats,
    msg_source: &StressMockMessageSource,
    total_report_sent: u64,
    timeout: Duration,
) {
    let start = tokio::time::Instant::now();
    loop {
        let queue_len = msg_source.total_queue_len().await;
        let sent = stats.submit_sent.load(Ordering::Relaxed);
        if (queue_len == 0 && total_report_sent >= sent) || start.elapsed() > timeout {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub async fn drain_wait_final_single(
    stats: &TestStats,
    report_sent: u64,
    mo_sent: u64,
    timeout: Duration,
) {
    let start = tokio::time::Instant::now();
    loop {
        let report_ok = stats.report_received.load(Ordering::Relaxed) >= report_sent;
        let mo_ok = stats.mo_received.load(Ordering::Relaxed) >= mo_sent;
        if (report_ok && mo_ok) || start.elapsed() > timeout {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn drain_wait_final_multi(
    stats: &TestStats,
    total_report_sent: u64,
    total_mo_sent: u64,
    timeout: Duration,
) {
    let start = tokio::time::Instant::now();
    loop {
        let report_ok = stats.report_received.load(Ordering::Relaxed) >= total_report_sent;
        let mo_ok = stats.mo_received.load(Ordering::Relaxed) >= total_mo_sent;
        if (report_ok && mo_ok) || start.elapsed() > timeout {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub fn spawn_stats_monitor(
    stats: Arc<TestStats>,
    msg_source: Arc<StressMockMessageSource>,
    protocol_name: &str,
    duration_secs: u64,
    print_interval_secs: u64,
    account: Option<String>,
) -> tokio::task::JoinHandle<()> {
    let protocol_name = protocol_name.to_string();
    tokio::spawn(async move {
        let mut last_submit = 0u64;
        let mut last_submit_resp = 0u64;
        let mut last_report = 0u64;
        let mut last_mo = 0u64;

        loop {
            tokio::time::sleep(Duration::from_secs(print_interval_secs)).await;

            let submit_sent = stats.submit_sent.load(Ordering::Relaxed);
            let submit_resp = stats.submit_resp_received.load(Ordering::Relaxed);
            let reports = stats.report_received.load(Ordering::Relaxed);
            let mo_recv = stats.mo_received.load(Ordering::Relaxed);
            let queue_len = if let Some(ref acc) = account {
                msg_source.queue_len(acc).await
            } else {
                msg_source.total_queue_len().await
            };

            let elapsed = stats.elapsed_secs();
            let interval = print_interval_secs as f64;
            let submit_rate = (submit_sent - last_submit) as f64 / interval;
            let resp_rate = (submit_resp - last_submit_resp) as f64 / interval;
            let report_rate_val = (reports - last_report) as f64 / interval;
            let mo_rate_val = (mo_recv - last_mo) as f64 / interval;

            println!("\n--- {} Stats at {:.0}s ---", protocol_name, elapsed);
            println!("  MT Submit: {} ({:.0} msg/s)", submit_sent, submit_rate);
            println!(
                "  Submit Resp: {} ({:.0} msg/s)",
                submit_resp, resp_rate
            );
            println!("  Report RX: {} ({:.0} msg/s)", reports, report_rate_val);
            println!("  MO RX: {} ({:.0} msg/s)", mo_recv, mo_rate_val);
            println!("  Queue Pending: {}", queue_len);

            last_submit = submit_sent;
            last_submit_resp = submit_resp;
            last_report = reports;
            last_mo = mo_recv;

            if elapsed >= duration_secs as f64 {
                break;
            }
        }
    })
}
