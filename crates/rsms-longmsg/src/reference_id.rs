//! Reference ID generator for long messages.
//!
//! 理想用法是**每账号/端点持有一个持久生成器**（经 `LongMessageSplitter::with_generator`），
//! 使该账号的 reference 单调递增、互不撞号。若按「每条长短信新建一个 splitter」的常见写法，
//! 各生成器的**起始值**由进程级种子分发器保证互不相同（见 `next_seed`），从而避免起始 reference 撞号
//! 导致发往同一手机的两条长短信被错误拼接。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// Generator for 16-bit reference IDs used in long SMS messages.
///
/// Uses the lower 16 bits of an AtomicU64 counter, providing up to 65535
/// unique reference IDs per endpoint before wrapping.
#[derive(Debug)]
pub struct ReferenceIdGenerator {
    counter: AtomicU64,
}

impl ReferenceIdGenerator {
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(next_seed()),
        }
    }

    pub fn with_value(value: u64) -> Self {
        Self {
            counter: AtomicU64::new(value),
        }
    }

    pub fn next_reference_id(&self) -> u16 {
        self.counter.fetch_add(1, Ordering::Relaxed) as u16
    }

    pub fn current(&self) -> u16 {
        self.counter.load(Ordering::Relaxed) as u16
    }

    pub fn reset(&self) {
        self.counter.store(0, Ordering::Relaxed);
    }
}

impl Default for ReferenceIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// 进程级种子分发器：为每个 `ReferenceIdGenerator::new()` 分发一个互不相同且分散的起始值。
///
/// - 进程内：每次 `new()` 把计数器叠加一个黄金比例素数步长（与 2^16 互质 → 低 16 位满周期、
///   连续新建的生成器起始 reference 互不相同，直到 65536 个才回绕）。
/// - 进程间/重启：起点再混入一次性的纳秒时间种子，使不同进程的 reference 范围也错开。
///
/// 这样即便业务按「每条长短信新建一个 splitter」也不会因起始 reference 撞号而导致手机端拼接错乱。
fn next_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    // 一次性时间基（全纳秒，跨进程/重启有足够熵）。
    static TIME_BASE: OnceLock<u64> = OnceLock::new();
    // 进程级分发计数。
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let base = *TIME_BASE.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    });
    // 0x9E37_79B1：32-bit 黄金比例素数；为奇数 → 与 2^16 互质 → 低 16 位满周期均匀铺开。
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    base.wrapping_add(n.wrapping_mul(0x9E37_79B1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reference_id_wrapping() {
        let generator = ReferenceIdGenerator::with_value(u64::MAX - 2);

        let id1 = generator.next_reference_id();
        let id2 = generator.next_reference_id();
        let id3 = generator.next_reference_id();

        assert_eq!(id1, u16::MAX - 2);
        assert_eq!(id2, u16::MAX - 1);
        assert_eq!(id3, u16::MAX);
        assert_eq!(generator.next_reference_id(), 0);
    }

    #[test]
    fn test_sequential_ids() {
        let generator = ReferenceIdGenerator::new();

        let id1 = generator.next_reference_id();
        let id2 = generator.next_reference_id();

        assert_eq!(id2, id1.wrapping_add(1));
    }

    /// 进程级种子分发器：连续新建的多个生成器起始 reference 必须互不相同，
    /// 否则「每条长短信新建一个 splitter」时两条长短信会以相同 reference 发往同一手机 → 拼接错乱。
    #[test]
    fn fresh_generators_have_distinct_starting_references() {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        // 1000 < 65536，黄金比例素数步长保证低 16 位在此范围内确定性互不相同。
        for _ in 0..1000 {
            let start = ReferenceIdGenerator::new().current();
            assert!(
                seen.insert(start),
                "连续新建的生成器起始 reference 出现撞号: {start}"
            );
        }
    }
}
