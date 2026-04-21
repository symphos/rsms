pub trait IdGenerator: Send + Sync + 'static {
    fn next_msg_id(&self) -> u64;
    fn next_sequence_id(&self) -> u32;
}
