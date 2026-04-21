use rsms_core::IdGenerator;
use std::sync::atomic::{AtomicU64, AtomicU32, Ordering};

pub struct SimpleIdGenerator {
    msg_id_counter: AtomicU64,
    sequence_id_counter: AtomicU32,
}

impl SimpleIdGenerator {
    pub fn new() -> Self {
        Self {
            msg_id_counter: AtomicU64::new(1),
            sequence_id_counter: AtomicU32::new(1),
        }
    }
}

impl Default for SimpleIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl IdGenerator for SimpleIdGenerator {
    fn next_msg_id(&self) -> u64 {
        self.msg_id_counter.fetch_add(1, Ordering::Relaxed)
    }

    fn next_sequence_id(&self) -> u32 {
        self.sequence_id_counter.fetch_add(1, Ordering::Relaxed)
    }
}
