use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
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
    transactions: RwLock<HashMap<String, TransactionEntry>>,
    seq_to_msg_id: RwLock<HashMap<u32, String>>,
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
                transactions: RwLock::new(HashMap::new()),
                seq_to_msg_id: RwLock::new(HashMap::new()),
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
        
        let mut transactions = self.inner.transactions.write().await;
        transactions.insert(key, entry);
    }

    pub async fn on_submit_resp(&self, sequence_id: u32, msg_id: Option<String>, result: u32) {
        let entry = {
            let mut transactions = self.inner.transactions.write().await;
            let key = sequence_id.to_string();
            
            if let Some(tx) = transactions.remove(&key) {
                if let Some(ref mid) = msg_id {
                    // 更新 seq_to_msg_id 映射
                    let mut seq_to_msg = self.inner.seq_to_msg_id.write().await;
                    seq_to_msg.insert(sequence_id, mid.clone());
                    
                    let mut new_entry = tx;
                    new_entry.info.msg_id = Some(mid.clone());
                    new_entry.status = TransactionStatus::RespReceived;
                    transactions.insert(mid.clone(), new_entry);
                    transactions.get(mid).cloned().unwrap()
                } else {
                    // 没有 msg_id，保持用 sequence_id 作为 key
                    let mut new_entry = tx;
                    new_entry.status = TransactionStatus::RespReceived;
                    transactions.insert(key.clone(), new_entry.clone());
                    new_entry
                }
            } else {
                return;
            }
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
        let entry = {
            let mut transactions = self.inner.transactions.write().await;
            if let Some(tx) = transactions.remove(&sequence_id.to_string()) {
                tx
            } else {
                return;
            }
        };

        let callback = self.inner.callback.read().await;
        if let Some(ref cb) = *callback {
            cb.on_send_failed(entry.info.sequence_id, error).await;
        }
    }

    pub async fn on_report(&self, report: ReportInfo) {
        let entry = {
            let transactions = self.inner.transactions.read().await;

            if let Some(tx) = transactions.get(&report.msg_id) {
                Some(tx.clone())
            } else {
                let seq_to_msg = self.inner.seq_to_msg_id.read().await;
                let mut found = None;
                for (_, msg_id) in seq_to_msg.iter() {
                    if let Some(tx) = transactions.get(msg_id) {
                        if tx.info.msg_id.as_deref() == Some(&report.msg_id) {
                            found = Some(tx.clone());
                            break;
                        }
                    }
                }
                found
            }
        };

        if let Some(entry) = entry {
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

        {
            let mut transactions = inner.transactions.write().await;
            let keys_to_remove: Vec<String> = transactions.iter()
                .filter(|(_, tx)| {
                    tx.status == TransactionStatus::Pending 
                        && now.duration_since(tx.info.submit_time) > inner.resp_timeout
                })
                .map(|(k, _)| k.clone())
                .collect();
            
            for key in keys_to_remove {
                if let Some(tx) = transactions.remove(&key) {
                    timed_out.push(tx.info);
                }
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
        self.inner.transactions.read().await.len()
    }

    pub async fn pending_count(&self) -> usize {
        let transactions = self.inner.transactions.read().await;
        transactions.values()
            .filter(|tx| tx.status == TransactionStatus::Pending)
            .count()
    }
}
