//! Long message merging from segments.

use crate::frame::{LongMessageFrame, UdhParser};
use crate::segment_bitmap::SegmentBitmap;
use rsms_core::RsmsError;
use std::collections::HashMap;
use std::time::{Duration, Instant};

struct PendingEntry {
    frames: Vec<LongMessageFrame>,
    bitmap: SegmentBitmap,
    created_at: Instant,
}

impl PendingEntry {
    fn new(total_segments: u8) -> Self {
        Self {
            frames: Vec::new(),
            bitmap: SegmentBitmap::new(total_segments),
            created_at: Instant::now(),
        }
    }

    fn is_expired(&self, ttl: Duration) -> bool {
        self.created_at.elapsed() > ttl
    }

    fn add_frame(&mut self, frame: LongMessageFrame) -> bool {
        if self.bitmap.is_received(frame.segment_number) {
            return false; // duplicate
        }
        self.bitmap.mark_received(frame.segment_number);
        self.frames.push(frame);
        true
    }

    fn is_complete(&self) -> bool {
        self.bitmap.is_complete()
    }
}

pub struct LongMessageMerger {
    pending_frames: HashMap<String, PendingEntry>,
    ttl: Duration,
    last_cleanup: Instant,
}

impl LongMessageMerger {
    pub fn new() -> Self {
        Self {
            pending_frames: HashMap::new(),
            ttl: Duration::from_secs(60),
            last_cleanup: Instant::now(),
        }
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            pending_frames: HashMap::new(),
            ttl,
            last_cleanup: Instant::now(),
        }
    }

    pub fn cleanup_expired(&mut self) {
        self.pending_frames
            .retain(|_, entry| !entry.is_expired(self.ttl));
        self.last_cleanup = Instant::now();
    }

    pub fn add_frame(&mut self, frame: LongMessageFrame) -> Result<Option<Vec<u8>>, RsmsError> {
        // 惰性清理：距上次清理超过 ttl 即回收过期未完成分片，避免永不收齐的分片无界堆积导致 OOM。
        // 与框架「不缓存 MsgId 避免 OOM」的设计哲学一致——merger 同样必须有界。
        if self.last_cleanup.elapsed() >= self.ttl {
            self.cleanup_expired();
        }

        let key = frame.unique_id();

        if frame.total_segments == 1 && !frame.has_udhi {
            return Ok(Some(frame.content));
        }

        let entry = self
            .pending_frames
            .entry(key.clone())
            .or_insert_with(|| PendingEntry::new(frame.total_segments));

        let is_duplicate = !entry.add_frame(frame);
        if is_duplicate {
            return Ok(None);
        }

        if entry.is_complete() {
            let sorted: Vec<LongMessageFrame> = {
                let mut v = self.pending_frames.remove(&key).unwrap().frames;
                v.sort_by_key(|f| f.segment_number);
                v
            };

            let mut result = Vec::new();
            for f in sorted {
                let content = UdhParser::strip_udh(&f.content);
                result.extend_from_slice(&content);
            }
            Ok(Some(result))
        } else {
            Ok(None)
        }
    }

    pub fn pending_count(&self) -> usize {
        self.pending_frames.len()
    }

    pub fn clear(&mut self) {
        self.pending_frames.clear();
    }

    pub fn set_ttl(&mut self, ttl: Duration) {
        self.ttl = ttl;
    }
}

impl Default for LongMessageMerger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reference_id::ReferenceIdGenerator;
    use crate::split::{LongMessageSplitter, SmsAlphabet};
    use std::sync::Arc;
    use std::thread::sleep;

    fn multi_segment_frames(ref_id: u64, fill: u8) -> Vec<LongMessageFrame> {
        let mut splitter =
            LongMessageSplitter::with_generator(Arc::new(ReferenceIdGenerator::with_value(ref_id)));
        let frames = splitter.split(&vec![fill; 400], SmsAlphabet::GSM7);
        assert!(frames.len() >= 2, "测试数据应被分成多段");
        frames
    }

    /// 显式 `cleanup_expired` 应回收过期的未完成分片。
    #[test]
    fn cleanup_expired_removes_stale_incomplete_entries() {
        let frames = multi_segment_frames(300, b'X');
        let mut merger = LongMessageMerger::with_ttl(Duration::from_millis(30));
        assert!(merger.add_frame(frames[0].clone()).unwrap().is_none());
        assert_eq!(merger.pending_count(), 1);
        sleep(Duration::from_millis(60));
        merger.cleanup_expired();
        assert_eq!(merger.pending_count(), 0, "过期未完成分片应被回收");
    }

    /// `add_frame` 必须惰性触发清理，否则永不收齐的分片会无界堆积导致 OOM。
    #[test]
    fn add_frame_triggers_lazy_cleanup_of_stale_entries() {
        let a = multi_segment_frames(300, b'X');
        let b = multi_segment_frames(900, b'Y');
        let mut merger = LongMessageMerger::with_ttl(Duration::from_millis(30));
        merger.add_frame(a[0].clone()).unwrap(); // 不完整，留在 pending
        assert_eq!(merger.pending_count(), 1);
        sleep(Duration::from_millis(60)); // 超过 ttl
        merger.add_frame(b[0].clone()).unwrap(); // 应触发惰性清理 + 加入新 entry
        assert_eq!(
            merger.pending_count(),
            1,
            "过期的 a 应被惰性清理，仅余新加入的 b"
        );
    }
}
