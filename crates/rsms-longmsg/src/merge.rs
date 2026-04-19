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
}

impl LongMessageMerger {
    pub fn new() -> Self {
        Self {
            pending_frames: HashMap::new(),
            ttl: Duration::from_secs(60),
        }
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            pending_frames: HashMap::new(),
            ttl,
        }
    }

    pub fn cleanup_expired(&mut self) {
        self.pending_frames
            .retain(|_, entry| !entry.is_expired(self.ttl));
    }

    pub fn add_frame(&mut self, frame: LongMessageFrame) -> Result<Option<Vec<u8>>, RsmsError> {
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
