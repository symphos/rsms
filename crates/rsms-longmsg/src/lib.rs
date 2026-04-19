//! Long SMS handling: splitting and merging with UDH support.
//!
//! Implements TP-UDHI (User Data Header Indicator) for long SMS messages.
//! Supports both 7-bit (GSM) and UCS2 encoding.

pub mod frame;
pub mod reference_id;
pub mod segment_bitmap;
pub mod split;
pub mod merge;
pub mod cache;

pub use frame::{LongMessageFrame, UdhHeader, UdhParser};
pub use reference_id::ReferenceIdGenerator;
pub use segment_bitmap::SegmentBitmap;
pub use split::LongMessageSplitter;
pub use merge::LongMessageMerger;
pub use cache::{FrameCache, InMemoryFrameCache};
