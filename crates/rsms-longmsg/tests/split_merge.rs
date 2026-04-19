//! Long message splitting and merging tests

use rsms_longmsg::split::SmsAlphabet;
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter};

#[test]
fn short_message_not_split() {
    let mut splitter = LongMessageSplitter::new();
    let content = b"Hello, this is a short message!";

    let frames = splitter.split(content, SmsAlphabet::GSM7);

    assert_eq!(frames.len(), 1);
    assert!(!frames[0].has_udhi);
}

#[test]
fn long_message_split_into_multiple_segments() {
    let mut splitter = LongMessageSplitter::new();
    let content = vec![0x41u8; 200];

    let frames = splitter.split(&content, SmsAlphabet::GSM7);

    assert!(frames.len() > 1);
    for frame in &frames {
        assert!(frame.has_udhi);
        assert!(frame.total_segments > 1);
    }
}

#[test]
fn split_segments_have_sequential_numbers() {
    let mut splitter = LongMessageSplitter::new();
    let content = vec![0x42u8; 300];

    let frames = splitter.split(&content, SmsAlphabet::GSM7);

    let ref_id = frames[0].reference_id;
    let total = frames[0].total_segments;

    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(frame.reference_id, ref_id);
        assert_eq!(frame.total_segments, total);
        assert_eq!(frame.segment_number, (i + 1) as u8);
    }
}

#[test]
fn merge_single_frame_returns_message() {
    let mut merger = LongMessageMerger::new();
    let frame = LongMessageFrame::new(0, 1, 1, b"Hello".to_vec(), false, None);

    let result = merger.add_frame(frame).unwrap();

    assert!(result.is_some());
    assert_eq!(result.unwrap(), b"Hello");
}

#[test]
fn merge_multiple_frames_from_splitter() {
    let mut merger = LongMessageMerger::new();
    let content = b"Hello World Test Message";

    let mut splitter = LongMessageSplitter::new();
    let frames = splitter.split(content, SmsAlphabet::GSM7);

    for frame in frames {
        let result = merger.add_frame(frame).unwrap();
        if result.is_some() {
            assert_eq!(result.unwrap(), content);
        }
    }
}

#[test]
fn merge_pending_frames_until_all_arrive() {
    let mut merger = LongMessageMerger::new();

    let frame1 = LongMessageFrame::new(5, 2, 1, vec![0x41, 0x41], true, None);
    let frame2 = LongMessageFrame::new(5, 2, 2, vec![0x42, 0x42], true, None);

    let result1 = merger.add_frame(frame1).unwrap();
    assert!(result1.is_none());
    assert_eq!(merger.pending_count(), 1);

    let result2 = merger.add_frame(frame2).unwrap();
    assert!(result2.is_some());
    assert_eq!(merger.pending_count(), 0);
}

#[test]
fn ucs2_long_message_gets_split_with_udhi() {
    let mut splitter = LongMessageSplitter::new();
    let content: Vec<u8> = vec![0x00; 100];

    let frames = splitter.split(&content, SmsAlphabet::UCS2);

    assert!(frames.len() > 1);
    for frame in &frames {
        assert!(frame.has_udhi);
    }
}

#[test]
fn ascii_message_split() {
    let mut splitter = LongMessageSplitter::new();
    let content = b"Test message for ASCII splitting";

    let frames = splitter.split(content, SmsAlphabet::ASCII);

    assert!(frames.len() >= 1);
}

#[test]
fn binary_message_split() {
    let mut splitter = LongMessageSplitter::new();
    let content: Vec<u8> = (0..200u8).collect();

    let frames = splitter.split(&content, SmsAlphabet::Binary);

    for frame in &frames {
        assert!(frame.has_udhi);
    }
}
