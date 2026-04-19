//! Heartbeat timer implementation for session keep-alive.

use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct HeartbeatConfig {
    pub interval: Duration,
    pub timeout: Duration,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
            timeout: Duration::from_secs(10),
        }
    }
}

impl HeartbeatConfig {
    pub fn new(interval: Duration, timeout: Duration) -> Self {
        Self { interval, timeout }
    }
}

#[derive(Debug)]
pub struct HeartbeatState {
    pub last_sent: std::time::Instant,
    pub last_response: std::time::Instant,
    pub consecutive_failures: u32,
}

impl HeartbeatState {
    pub fn new() -> Self {
        let now = std::time::Instant::now();
        Self {
            last_sent: now,
            last_response: now,
            consecutive_failures: 0,
        }
    }

    pub fn record_sent(&mut self) {
        self.last_sent = std::time::Instant::now();
    }

    pub fn record_response(&mut self) {
        self.last_response = std::time::Instant::now();
        self.consecutive_failures = 0;
    }

    pub fn is_timeout(&self, timeout: Duration) -> bool {
        self.last_response.elapsed() > timeout
    }

    pub fn should_retry(&self, max_retries: u32) -> bool {
        self.consecutive_failures < max_retries
    }
}

impl Default for HeartbeatState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct HeartbeatTimer {
    config: HeartbeatConfig,
    state: Arc<Mutex<HeartbeatState>>,
    running: std::sync::atomic::AtomicBool,
}

impl HeartbeatTimer {
    pub fn new(config: HeartbeatConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            state: Arc::new(Mutex::new(HeartbeatState::new())),
            running: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn start(&self) {
        self.running
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn stop(&self) {
        self.running
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn record_heartbeat_sent(&self) {
        let mut state = self.state.lock();
        state.record_sent();
    }

    pub fn record_heartbeat_response(&self) {
        let mut state = self.state.lock();
        state.record_response();
    }

    pub fn is_timed_out(&self) -> bool {
        let state = self.state.lock();
        state.is_timeout(self.config.timeout)
    }

    pub fn should_send_heartbeat(&self) -> bool {
        if !self.is_running() {
            return false;
        }

        let state = self.state.lock();
        state.last_sent.elapsed() >= self.config.interval
    }

    pub fn interval(&self) -> Duration {
        self.config.interval
    }

    pub fn timeout(&self) -> Duration {
        self.config.timeout
    }
}

impl Default for HeartbeatTimer {
    fn default() -> Self {
        Self {
            config: HeartbeatConfig::default(),
            state: Arc::new(Mutex::new(HeartbeatState::new())),
            running: std::sync::atomic::AtomicBool::new(false),
        }
    }
}
