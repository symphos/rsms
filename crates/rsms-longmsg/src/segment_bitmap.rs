//! Segment bitmap for tracking received segments in long SMS merging.

use std::collections::HashSet;

/// 长短信分段接收状态跟踪器，用于判断所有分段是否已全部到达。
#[derive(Debug, Clone)]
pub struct SegmentBitmap {
    total_segments: u8,
    received: HashSet<u8>,
}

impl SegmentBitmap {
    /// 创建一个跟踪 `total_segments` 个分段的位图。
    pub fn new(total_segments: u8) -> Self {
        Self {
            total_segments,
            received: HashSet::new(),
        }
    }

    /// 标记分段 `segment` 已收到。返回 `true` 表示首次标记，`false` 表示重复收到。
    pub fn mark_received(&mut self, segment: u8) -> bool {
        self.received.insert(segment)
    }

    /// 判断分段 `segment` 是否已被标记为收到。
    pub fn is_received(&self, segment: u8) -> bool {
        self.received.contains(&segment)
    }

    /// 判断所有分段是否均已收到（已收到数 >= 总分段数）。
    pub fn is_complete(&self) -> bool {
        self.received.len() >= self.total_segments as usize
    }

    /// 返回尚未收到的分段数量。
    pub fn missing_count(&self) -> usize {
        self.total_segments as usize - self.received.len()
    }

    /// 返回已收到的分段数量。
    pub fn received_count(&self) -> usize {
        self.received.len()
    }

    /// 返回所有已收到的分段序号，按升序排列。
    pub fn all_received(&self) -> Vec<u8> {
        let mut segments: Vec<u8> = self.received.iter().copied().collect();
        segments.sort();
        segments
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitmap_basic() {
        let mut bitmap = SegmentBitmap::new(3);
        assert!(!bitmap.is_complete());

        assert!(bitmap.mark_received(2));
        assert!(!bitmap.is_received(1));
        assert!(bitmap.is_received(2));

        bitmap.mark_received(1);
        assert!(!bitmap.is_complete());

        bitmap.mark_received(3);
        assert!(bitmap.is_complete());
    }

    #[test]
    fn test_duplicate_segment() {
        let mut bitmap = SegmentBitmap::new(3);
        assert!(bitmap.mark_received(1));
        assert!(!bitmap.mark_received(1)); // duplicate returns false
        assert_eq!(bitmap.received_count(), 1);
    }

    #[test]
    fn test_missing_count() {
        let mut bitmap = SegmentBitmap::new(5);
        assert_eq!(bitmap.missing_count(), 5);

        bitmap.mark_received(2);
        bitmap.mark_received(4);
        assert_eq!(bitmap.missing_count(), 3);
    }
}
