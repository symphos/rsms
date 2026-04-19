//! Reference ID generator for long messages.
//!
//! Each endpoint should have its own generator to avoid cross-endpoint collisions.

use std::sync::atomic::{AtomicU64, Ordering};

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
            counter: AtomicU64::new(rand_u64()),
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

fn rand_u64() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    nanos as u64
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
}
