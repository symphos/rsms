use std::sync::Arc;
use std::time::{Duration, Instant};
use dashmap::DashMap;
use tokio::sync::RwLock;
use tokio::time;
use tracing::warn;

pub mod cmpp;
pub mod smgp;
pub mod sgip;
pub mod smpp;

pub use cmpp::{CmppSubmit, CmppDeliver, CmppTransactionManager};
pub use smgp::{SmgpSubmit, SmgpDeliver, SmgpTransactionManager};
pub use sgip::{SgipSubmit, SgipDeliver, SgipTransactionManager};
pub use smpp::{SmppSubmit, SmppDeliver, SmppTransactionManager};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionStatus {
    Pending,
    RespReceived,
    Timeout,
    SendFailed,
}

#[derive(Debug, Clone)]
pub struct SubmitInfo {
    pub sequence_id: u32,
    pub msg_id: Option<String>,
    pub dest_id: String,
    pub src_id: String,
    pub content: Vec<u8>,
    pub protocol: &'static str,
    pub submit_time: Instant,
}

impl SubmitInfo {
    pub fn new(
        sequence_id: u32,
        dest_id: String,
        src_id: String,
        content: Vec<u8>,
        protocol: &'static str,
    ) -> Self {
        Self {
            sequence_id,
            msg_id: None,
            dest_id,
            src_id,
            content,
            protocol,
            submit_time: Instant::now(),
        }
    }

    pub fn with_msg_id(
        sequence_id: u32,
        msg_id: String,
        dest_id: String,
        src_id: String,
        content: Vec<u8>,
        protocol: &'static str,
    ) -> Self {
        Self {
            sequence_id,
            msg_id: Some(msg_id),
            dest_id,
            src_id,
            content,
            protocol,
            submit_time: Instant::now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReportInfo {
    pub msg_id: String,
    pub src_terminal_id: String,
    pub stat: String,
}

#[derive(Debug, Clone)]
pub struct MoInfo {
    pub dest_id: String,
    pub src_terminal_id: String,
    pub content: Vec<u8>,
}

#[async_trait::async_trait]
pub trait MessageCallback: Send + Sync {
    async fn on_submit_resp_ok(&self, info: &SubmitInfo);
    async fn on_submit_resp_error(&self, info: &SubmitInfo, result: u32);
    async fn on_submit_resp_timeout(&self, info: &SubmitInfo);
    async fn on_send_failed(&self, sequence_id: u32, error: &str);
    async fn on_report(&self, report: &ReportInfo, original_submit: &SubmitInfo);
    async fn on_mo(&self, mo: &MoInfo);
}

#[derive(Clone)]
pub struct TransactionManager {
    inner: Arc<Inner>,
}

struct Inner {
    // 用 DashMap 替代全局写锁的 RwLock<HashMap>：每条 submit/resp/report 不再串行化到单把锁。
    // 主表的键在 resp 前为 sequence_id 字符串、resp 后改为 msg_id。
    transactions: DashMap<String, TransactionEntry>,
    seq_to_msg_id: DashMap<u32, String>,
    check_interval: Duration,
    resp_timeout: Duration,
    running: Arc<RwLock<bool>>,
    callback: RwLock<Option<Arc<dyn MessageCallback>>>,
}

#[derive(Clone)]
struct TransactionEntry {
    info: SubmitInfo,
    status: TransactionStatus,
}

impl TransactionManager {
    pub fn new(resp_timeout_secs: u64) -> Self {
        Self {
            inner: Arc::new(Inner {
                transactions: DashMap::new(),
                seq_to_msg_id: DashMap::new(),
                check_interval: Duration::from_secs(1),
                resp_timeout: Duration::from_secs(resp_timeout_secs),
                running: Arc::new(RwLock::new(false)),
                callback: RwLock::new(None),
            }),
        }
    }

    pub async fn set_callback(&self, callback: Option<Arc<dyn MessageCallback>>) {
        *self.inner.callback.write().await = callback;
    }

    pub async fn add_submit_transaction(&self, info: SubmitInfo) {
        let key = info.sequence_id.to_string();
        let entry = TransactionEntry {
            info,
            status: TransactionStatus::Pending,
        };
        self.inner.transactions.insert(key, entry);
    }

    pub async fn on_submit_resp(&self, sequence_id: u32, msg_id: Option<String>, result: u32) {
        let key = sequence_id.to_string();
        let entry = if let Some(mid) = msg_id {
            // 取出当前 entry 的克隆并释放分片锁，然后**先按 msg_id 落键、再删 seq 键**，
            // 保证不存在“两键瞬时都不在”的窗口，避免并发 on_report 漏匹配。
            let Some(mut tx) = self.inner.transactions.get(&key).map(|r| r.clone()) else {
                return;
            };
            tx.info.msg_id = Some(mid.clone());
            tx.status = TransactionStatus::RespReceived;
            self.inner.seq_to_msg_id.insert(sequence_id, mid.clone());
            self.inner.transactions.insert(mid, tx.clone());
            self.inner.transactions.remove(&key);
            tx
        } else {
            // 没有 msg_id：原地更新状态，仍以 sequence_id 为键
            let Some(mut r) = self.inner.transactions.get_mut(&key) else {
                return;
            };
            r.status = TransactionStatus::RespReceived;
            r.clone()
        };

        let callback = self.inner.callback.read().await;
        if let Some(ref cb) = *callback {
            if result == 0 {
                cb.on_submit_resp_ok(&entry.info).await;
            } else {
                cb.on_submit_resp_error(&entry.info, result).await;
            }
        }
    }

    pub async fn on_send_failed(&self, sequence_id: u32, error: &str) {
        let Some((_, entry)) = self.inner.transactions.remove(&sequence_id.to_string()) else {
            return;
        };

        let callback = self.inner.callback.read().await;
        if let Some(ref cb) = *callback {
            cb.on_send_failed(entry.info.sequence_id, error).await;
        }
    }

    pub async fn on_report(&self, report: ReportInfo) {
        let entry = if let Some(tx) = self.inner.transactions.get(&report.msg_id) {
            Some(tx.clone())
        } else {
            // 兜底：扫描 seq_to_msg_id 找到 msg_id 对应的事务
            let mut found = None;
            for kv in self.inner.seq_to_msg_id.iter() {
                if let Some(tx) = self.inner.transactions.get(kv.value())
                    && tx.info.msg_id.as_deref() == Some(report.msg_id.as_str()) {
                        found = Some(tx.clone());
                        break;
                    }
            }
            found
        };

        if let Some(entry) = entry {
            // 匹配成功后清理两表，避免长生命周期客户端无界增长（msg_id 键条目 + seq 映射）
            if let Some(mid) = entry.info.msg_id.as_deref() {
                self.inner.transactions.remove(mid);
            }
            self.inner.seq_to_msg_id.remove(&entry.info.sequence_id);

            let callback = self.inner.callback.read().await;
            if let Some(ref cb) = *callback {
                cb.on_report(&report, &entry.info).await;
            }
        }
    }

    pub async fn on_mo(&self, mo: MoInfo) {
        let callback = self.inner.callback.read().await;
        if let Some(ref cb) = *callback {
            cb.on_mo(&mo).await;
        }
    }

    pub async fn start_timeout_checker(self: Arc<Self>) {
        let running = self.inner.running.clone();
        *running.write().await = true;

        let inner = self.inner.clone();
        
        tokio::spawn(async move {
            let mut interval = time::interval(inner.check_interval);
            
            while *running.read().await {
                interval.tick().await;
                Self::check_timeouts(&inner).await;
            }
        });
    }

    pub async fn stop_timeout_checker(&self) {
        *self.inner.running.write().await = false;
    }

    async fn check_timeouts(inner: &Inner) {
        let now = Instant::now();
        let mut timed_out = Vec::new();

        // 先收集过期键（克隆），释放分片迭代锁后再逐个 remove，避免迭代期写冲突
        let keys_to_remove: Vec<String> = inner
            .transactions
            .iter()
            .filter(|e| {
                e.status == TransactionStatus::Pending
                    && now.duration_since(e.info.submit_time) > inner.resp_timeout
            })
            .map(|e| e.key().clone())
            .collect();

        for key in keys_to_remove {
            if let Some((_, tx)) = inner.transactions.remove(&key) {
                timed_out.push(tx.info);
            }
        }

        let callback = inner.callback.read().await;
        for info in timed_out {
            warn!(sequence_id = info.sequence_id, "SubmitResp timeout");
            if let Some(ref cb) = *callback {
                cb.on_submit_resp_timeout(&info).await;
            }
        }
    }

    pub async fn transaction_count(&self) -> usize {
        self.inner.transactions.len()
    }

    pub async fn pending_count(&self) -> usize {
        self.inner
            .transactions
            .iter()
            .filter(|e| e.status == TransactionStatus::Pending)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct CountingCallback {
        resp_ok: AtomicUsize,
        reports: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl MessageCallback for CountingCallback {
        async fn on_submit_resp_ok(&self, _: &SubmitInfo) {
            self.resp_ok.fetch_add(1, Ordering::Relaxed);
        }
        async fn on_submit_resp_error(&self, _: &SubmitInfo, _: u32) {}
        async fn on_submit_resp_timeout(&self, _: &SubmitInfo) {}
        async fn on_send_failed(&self, _: u32, _: &str) {}
        async fn on_report(&self, _: &ReportInfo, _: &SubmitInfo) {
            self.reports.fetch_add(1, Ordering::Relaxed);
        }
        async fn on_mo(&self, _: &MoInfo) {}
    }

    fn submit_info(seq: u32) -> SubmitInfo {
        SubmitInfo::new(seq, "dest".to_string(), "src".to_string(), vec![], "cmpp")
    }

    fn report(msg_id: &str) -> ReportInfo {
        ReportInfo {
            msg_id: msg_id.to_string(),
            src_terminal_id: "x".to_string(),
            stat: "DELIVRD".to_string(),
        }
    }

    #[tokio::test]
    async fn resp_rekeys_to_msg_id_and_report_matches() {
        let tm = TransactionManager::new(10);
        let cb = Arc::new(CountingCallback::default());
        tm.set_callback(Some(cb.clone() as Arc<dyn MessageCallback>)).await;

        tm.add_submit_transaction(submit_info(1)).await;
        tm.on_submit_resp(1, Some("MSGID-1".to_string()), 0).await;
        tm.on_report(report("MSGID-1")).await;

        assert_eq!(cb.resp_ok.load(Ordering::Relaxed), 1);
        assert_eq!(cb.reports.load(Ordering::Relaxed), 1, "报告应按 msg_id 匹配成功");
        // 报告匹配后应清理该条目（修复内存泄漏），事务表清空
        assert_eq!(tm.transaction_count().await, 0, "报告匹配后应清理事务条目");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_add_resp_report_no_loss() {
        let tm = TransactionManager::new(30);
        let cb = Arc::new(CountingCallback::default());
        tm.set_callback(Some(cb.clone() as Arc<dyn MessageCallback>)).await;

        let n = 1000u32;
        let mut handles = Vec::new();
        for i in 0..n {
            let tm = tm.clone();
            handles.push(tokio::spawn(async move {
                tm.add_submit_transaction(submit_info(i)).await;
                tm.on_submit_resp(i, Some(format!("M-{i}")), 0).await;
                tm.on_report(report(&format!("M-{i}"))).await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        // 并发下 DashMap 不得死锁/丢匹配：所有 resp 与 report 都应被回调
        assert_eq!(cb.resp_ok.load(Ordering::Relaxed), n as usize);
        assert_eq!(
            cb.reports.load(Ordering::Relaxed),
            n as usize,
            "并发下所有报告都应匹配成功（零漏匹配）"
        );
    }
}
