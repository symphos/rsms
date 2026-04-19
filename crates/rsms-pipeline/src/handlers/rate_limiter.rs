//! Rate limit configuration

use std::sync::atomic::{AtomicU64, Ordering};

/// Rate limit mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitMode {
    Global,
    PerConnection,
    PerUser,
}

/// Rate limit configuration
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub qps: u64,
    pub burst: u64,
    pub mode: LimitMode,
}

impl RateLimitConfig {
    pub fn new(qps: u64, burst: u64) -> Self {
        Self {
            qps,
            burst,
            mode: LimitMode::PerConnection,
        }
    }

    pub fn global(qps: u64, burst: u64) -> Self {
        Self {
            qps,
            burst,
            mode: LimitMode::Global,
        }
    }
}

/// Simple rate limiter (not Clone, use Arc for sharing)
pub struct RateLimiter {
    tokens: AtomicU64,
    capacity: u64,
    refill_rate: u64,
    last_refill: AtomicU64,
}

impl RateLimiter {
    pub fn new(qps: u64, burst: u64) -> Self {
        Self {
            tokens: AtomicU64::new(burst),
            capacity: burst,
            refill_rate: qps,
            last_refill: AtomicU64::new(Self::now_ms()),
        }
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    fn refill(&self) {
        loop {
            let now = Self::now_ms();
            let last = self.last_refill.load(Ordering::Acquire);

            if now <= last {
                break;
            }

            let elapsed = now.saturating_sub(last);

            if elapsed == 0 {
                break;
            }

            let added = elapsed.saturating_mul(self.refill_rate) / 1000;
            let current = self.tokens.load(Ordering::Acquire);
            let new_tokens = current.saturating_add(added).min(self.capacity);

            if self
                .tokens
                .compare_exchange_weak(current, new_tokens, Ordering::Release, Ordering::Acquire)
                .is_ok()
            {
                self.last_refill.store(now, Ordering::Release);
                break;
            }
        }
    }

    pub fn try_acquire(&self) -> bool {
        self.refill();

        loop {
            let current = self.tokens.load(Ordering::Acquire);
            if current == 0 {
                return false;
            }

            if self
                .tokens
                .compare_exchange(current, current - 1, Ordering::Release, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }
}
