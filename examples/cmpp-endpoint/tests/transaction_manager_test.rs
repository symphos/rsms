use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use rsms_connector::MessageCallback;
use rsms_connector::{SubmitInfo, ReportInfo, MoInfo};

struct TestMessageCallback {
    submit_resp_ok_count: AtomicU64,
    submit_resp_error_count: AtomicU64,
    submit_resp_timeout_count: AtomicU64,
    send_failed_count: AtomicU64,
    report_count: AtomicU64,
    mo_count: AtomicU64,
}

impl TestMessageCallback {
    fn new() -> Self {
        Self {
            submit_resp_ok_count: Default::default(),
            submit_resp_error_count: Default::default(),
            submit_resp_timeout_count: Default::default(),
            send_failed_count: Default::default(),
            report_count: Default::default(),
            mo_count: Default::default(),
        }
    }
}

impl Default for TestMessageCallback {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl MessageCallback for TestMessageCallback {
    async fn on_submit_resp_ok(&self, info: &SubmitInfo) {
        self.submit_resp_ok_count.fetch_add(1, Ordering::Relaxed);
        tracing::info!("[Callback] on_submit_resp_ok: seq={}", info.sequence_id);
    }

    async fn on_submit_resp_error(&self, info: &SubmitInfo, result: u32) {
        self.submit_resp_error_count.fetch_add(1, Ordering::Relaxed);
        tracing::warn!("[Callback] on_submit_resp_error: seq={} result={}", info.sequence_id, result);
    }

    async fn on_submit_resp_timeout(&self, info: &SubmitInfo) {
        self.submit_resp_timeout_count.fetch_add(1, Ordering::Relaxed);
        tracing::warn!("[Callback] on_submit_resp_timeout: seq={}", info.sequence_id);
    }

    async fn on_send_failed(&self, sequence_id: u32, error: &str) {
        self.send_failed_count.fetch_add(1, Ordering::Relaxed);
        tracing::error!("[Callback] on_send_failed: seq={} error={}", sequence_id, error);
    }

    async fn on_report(&self, report: &ReportInfo, original_submit: &SubmitInfo) {
        self.report_count.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            "[Callback] on_report: msg_id={} stat={} original_seq={}",
            report.msg_id, report.stat, original_submit.sequence_id
        );
    }

    async fn on_mo(&self, mo: &MoInfo) {
        self.mo_count.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            "[Callback] on_mo: dest={} src={}",
            mo.dest_id, mo.src_terminal_id
        );
    }
}

#[tokio::test]
async fn test_submit_info_new() {
    let info = SubmitInfo::new(
        123,
        "13800138000".to_string(),
        "106900".to_string(),
        b"Hello".to_vec(),
        "CMPP",
    );
    
    assert_eq!(info.sequence_id, 123);
    assert_eq!(info.dest_id, "13800138000");
    assert_eq!(info.src_id, "106900");
    assert_eq!(info.content, b"Hello");
    assert_eq!(info.protocol, "CMPP");
    assert!(info.msg_id.is_none());
}

#[tokio::test]
async fn test_submit_info_with_msg_id() {
    let info = SubmitInfo::with_msg_id(
        123,
        "msg_abc".to_string(),
        "13800138000".to_string(),
        "106900".to_string(),
        b"Hello".to_vec(),
        "SMGP",
    );
    
    assert_eq!(info.sequence_id, 123);
    assert_eq!(info.msg_id, Some("msg_abc".to_string()));
    assert_eq!(info.protocol, "SMGP");
}

#[tokio::test]
async fn test_transaction_manager_add_and_submit_resp() {
    let tm = Arc::new(rsms_connector::TransactionManager::new(30));
    let callback = Arc::new(TestMessageCallback::new());
    tm.set_callback(Some(callback.clone())).await;
    
    let info = SubmitInfo::new(
        1001,
        "13800138000".to_string(),
        "106900".to_string(),
        b"Test".to_vec(),
        "CMPP",
    );
    tm.add_submit_transaction(info).await;
    
    assert_eq!(tm.pending_count().await, 1);
    
    tm.on_submit_resp(1001, Some("msg_123".to_string()), 0).await;
    
    assert_eq!(callback.submit_resp_ok_count.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn test_transaction_manager_submit_resp_error() {
    let tm = Arc::new(rsms_connector::TransactionManager::new(30));
    let callback = Arc::new(TestMessageCallback::new());
    tm.set_callback(Some(callback.clone())).await;
    
    let info = SubmitInfo::new(
        1002,
        "13800138000".to_string(),
        "106900".to_string(),
        b"Test".to_vec(),
        "CMPP",
    );
    tm.add_submit_transaction(info).await;
    
    tm.on_submit_resp(1002, Some("msg_456".to_string()), 1).await;
    
    assert_eq!(callback.submit_resp_error_count.load(Ordering::Relaxed), 1);
    assert_eq!(callback.submit_resp_ok_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn test_transaction_manager_send_failed() {
    let tm = Arc::new(rsms_connector::TransactionManager::new(30));
    let callback = Arc::new(TestMessageCallback::new());
    tm.set_callback(Some(callback.clone())).await;
    
    let info = SubmitInfo::new(
        1003,
        "13800138000".to_string(),
        "106900".to_string(),
        b"Test".to_vec(),
        "CMPP",
    );
    tm.add_submit_transaction(info).await;
    
    tm.on_send_failed(1003, "Connection closed").await;
    
    assert_eq!(callback.send_failed_count.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn test_transaction_manager_report() {
    let tm = Arc::new(rsms_connector::TransactionManager::new(30));
    let callback = Arc::new(TestMessageCallback::new());
    tm.set_callback(Some(callback.clone())).await;
    
    // 先添加事务（此时只有 sequence_id，msg_id 为 None）
    let info = SubmitInfo::new(
        1004,
        "13800138000".to_string(),
        "106900".to_string(),
        b"Test".to_vec(),
        "SMGP",
    );
    tm.add_submit_transaction(info).await;
    
    // 模拟收到 SubmitResp，更新 msg_id
    tm.on_submit_resp(1004, Some("msg_report_123".to_string()), 0).await;
    
    // 模拟收到 Report（用 msg_id 匹配）
    let report = ReportInfo {
        msg_id: "msg_report_123".to_string(),
        src_terminal_id: "13800138001".to_string(),
        stat: "DELIVRD".to_string(),
    };
    tm.on_report(report).await;
    
    assert_eq!(callback.report_count.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn test_transaction_manager_mo() {
    let tm = Arc::new(rsms_connector::TransactionManager::new(30));
    let callback = Arc::new(TestMessageCallback::new());
    tm.set_callback(Some(callback.clone())).await;
    
    let mo = MoInfo {
        dest_id: "106900".to_string(),
        src_terminal_id: "13800138000".to_string(),
        content: b"Hello MO".to_vec(),
    };
    tm.on_mo(mo).await;
    
    assert_eq!(callback.mo_count.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn test_report_info() {
    let report = ReportInfo {
        msg_id: "msg_123".to_string(),
        src_terminal_id: "13800138000".to_string(),
        stat: "DELIVRD".to_string(),
    };
    
    assert_eq!(report.msg_id, "msg_123");
    assert_eq!(report.stat, "DELIVRD");
}

#[tokio::test]
async fn test_mo_info() {
    let mo = MoInfo {
        dest_id: "106900".to_string(),
        src_terminal_id: "13800138000".to_string(),
        content: b"Test".to_vec(),
    };
    
    assert_eq!(mo.dest_id, "106900");
    assert_eq!(mo.src_terminal_id, "13800138000");
}
