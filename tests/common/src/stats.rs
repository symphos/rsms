use std::sync::atomic::AtomicU64;
use std::sync::RwLock;
use tokio::time::Instant;

pub struct TestStats {
    pub submit_sent: AtomicU64,
    pub submit_resp_received: AtomicU64,
    pub submit_errors: AtomicU64,
    pub report_received: AtomicU64,
    pub report_matched: AtomicU64,
    pub mo_received: AtomicU64,
    pub mo_sent: AtomicU64,
    pub start_time: RwLock<Option<Instant>>,
    pub end_time: RwLock<Option<Instant>>,
}

impl TestStats {
    pub fn new() -> Self {
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

    pub fn start(&self) {
        *self.start_time.write().unwrap() = Some(Instant::now());
    }

    pub fn end(&self) {
        *self.end_time.write().unwrap() = Some(Instant::now());
    }

    pub fn elapsed_secs(&self) -> f64 {
        let start = self.start_time.read().unwrap();
        if let Some(start) = *start {
            if let Some(end) = *self.end_time.read().unwrap() {
                return (end - start).as_secs_f64();
            }
            return start.elapsed().as_secs_f64();
        }
        0.0
    }
}

impl Default for TestStats {
    fn default() -> Self {
        Self::new()
    }
}
