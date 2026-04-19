//! Segment bitmap for tracking received segments in long SMS merging.

use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct SegmentBitmap {
    total_segments: u8,
    received: HashSet<u8>,
}

impl SegmentBitmap {
    pub fn new(total_segments: u8) -> Self {
        Self {
            total_segments,
            received: HashSet::new(),
        }
    }

    pub fn mark_received(&mut self, segment: u8) -> bool {
        self.received.insert(segment)
    }

    pub fn is_received(&self, segment: u8) -> bool {
        self.received.contains(&segment)
    }

    pub fn is_complete(&self) -> bool {
        self.received.len() >= self.total_segments as usize
    }

    pub fn missing_count(&self) -> usize {
        self.total_segments as usize - self.received.len()
    }

    pub fn received_count(&self) -> usize {
        self.received.len()
    }

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
