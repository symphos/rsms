//! SmoothRateLimiter 实现，参考 Guava RateLimiter
//!
//! 核心思想：
//! 1. 维护 storedPermits（已存储但未使用的 permit）
//! 2. 当 RateLimiter 空闲时，storedPermits 增加（最大到 maxStoredPermits）
//! 3. 当需要 permit 时，先用 storedPermits，不够再用 fresh permits
//! 4. fresh permits 需要等待 stableIntervalMicros

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct SmoothRateLimiter {
    inner: Arc<RwLock<Inner>>,
}

struct Inner {
    stored_permits: f64,
    max_stored_permits: f64,
    stable_interval: Duration,
    next_free_permission: Instant,
    last_update_time: Instant,
}

impl SmoothRateLimiter {
    pub fn new(max_qps: u64) -> Self {
        let stable_interval = Duration::from_micros(1_000_000 / max_qps);
        let now = Instant::now();
        
        Self {
            inner: Arc::new(RwLock::new(Inner {
                stored_permits: max_qps as f64,
                max_stored_permits: max_qps as f64,
                stable_interval,
                next_free_permission: now,
                last_update_time: now,
            })),
        }
    }

    /// 阻塞获取 1 个 permit，返回是否成功
    pub async fn acquire(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        
        loop {
            let wait = {
                let mut inner = self.inner.write().await;
                inner.reserve_and_get_wait_length(1)
            };
            
            if wait.is_zero() {
                return true;
            }
            
            if wait > timeout {
                return false;
            }
            
            let sleep_time = wait.min(Duration::from_millis(100));
            tokio::time::sleep(sleep_time).await;
            
            if start.elapsed() >= timeout {
                return false;
            }
        }
    }

    /// 非阻塞尝试获取 permit
    pub async fn try_acquire(&self) -> bool {
        let wait = {
            let mut inner = self.inner.write().await;
            inner.reserve_and_get_wait_length(1)
        };
        wait.is_zero()
    }

    /// 获取当前可用 permit 数
    pub async fn available_permits(&self) -> f64 {
        let inner = self.inner.read().await;
        inner.stored_permits
    }

    /// 设置 QPS
    pub async fn set_rate(&self, permits_per_second: u64) {
        let mut inner = self.inner.write().await;
        inner.max_stored_permits = permits_per_second as f64;
        inner.stable_interval = Duration::from_micros(1_000_000 / permits_per_second);
        inner.stored_permits = permits_per_second as f64;
    }
}

impl Inner {
    fn resync(&mut self, now: Instant) {
        if now > self.next_free_permission {
            let elapsed = now.duration_since(self.last_update_time);
            let permits_to_add = elapsed.as_secs_f64() / self.stable_interval.as_secs_f64();
            self.stored_permits = (self.stored_permits + permits_to_add).min(self.max_stored_permits);
            self.last_update_time = now;
        }
    }
    
    fn reserve_and_get_wait_length(&mut self, permits: u64) -> Duration {
        let now = Instant::now();
        self.resync(now);
        
        let permits = permits as f64;
        let stored_permits_to_spend = permits.min(self.stored_permits);
        let fresh_permits_to_claim = permits - stored_permits_to_spend;
        
        let wait = Duration::from_secs_f64(fresh_permits_to_claim * self.stable_interval.as_secs_f64());
        
        if wait > Duration::ZERO {
            if self.next_free_permission <= now {
                self.next_free_permission = now + wait;
            } else {
                self.next_free_permission += wait;
            }
        } else {
            self.next_free_permission = now;
        }
        
        self.stored_permits -= stored_permits_to_spend;
        
        if wait > Duration::ZERO && self.next_free_permission > now {
            self.next_free_permission - now
        } else {
            Duration::ZERO
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[tokio::test]
    async fn test_basic_rate_limiting() {
        let limiter = SmoothRateLimiter::new(100);
        
        for _ in 0..10 {
            let result = limiter.acquire(Duration::from_secs(1)).await;
            assert!(result, "Should be able to acquire within 1 second");
        }
    }

    #[tokio::test]
    async fn test_burst() {
        let limiter = SmoothRateLimiter::new(100);
        
        let result = limiter.acquire(Duration::from_millis(100)).await;
        assert!(result, "First acquire should succeed immediately");
    }

    #[tokio::test]
    async fn test_high_qps_rate_limiting() {
        let limiter = SmoothRateLimiter::new(1000);
        
        let start = Instant::now();
        let mut count = 0u64;
        let target = 3000;
        
        loop {
            let timeout = Duration::from_secs(5);
            if limiter.acquire(timeout).await {
                count += 1;
                if count >= target {
                    break;
                }
            } else {
                break;
            }
        }
        
        let elapsed = start.elapsed().as_secs_f64();
        let actual_qps = count as f64 / elapsed;
        
        println!("High QPS test: count={}, elapsed={:.2}s, actual_qps={:.1}", count, elapsed, actual_qps);
        
        assert!(count >= target as u64 * 80 / 100, 
            "Expected at least 80% of target, got {} out of {} ({:.1} QPS)", 
            count, target, actual_qps);
    }
}
