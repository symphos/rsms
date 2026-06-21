//! Long message splitting and merging tests

use rsms_longmsg::split::SmsAlphabet;
use rsms_longmsg::{LongMessageFrame, LongMessageMerger, LongMessageSplitter, ReferenceIdGenerator};
use std::sync::Arc;

/// 串号回归：两个不同发送方各发一条长短信，恰好 reference 与分段数相同、分段交错到达。
/// merger 必须按发送方分桶，否则第二条的分段会被当重复丢弃 / 两条内容串合（手机端拼接异常的同源问题）。
#[test]
fn different_senders_same_reference_do_not_cross_merge() {
    let content_a = vec![0xAAu8; 400];
    let content_b = vec![0xBBu8; 400];
    // 强制两条长短信使用相同 reference（=1）与相同分段数，模拟两部手机各自从小值起的 reference 撞号。
    let mut sa = LongMessageSplitter::with_generator(Arc::new(ReferenceIdGenerator::with_value(1)));
    let mut sb = LongMessageSplitter::with_generator(Arc::new(ReferenceIdGenerator::with_value(1)));
    let fa = sa.split(&content_a, SmsAlphabet::GSM7);
    let fb = sb.split(&content_b, SmsAlphabet::GSM7);
    assert!(fa.len() >= 2, "测试前提：内容应被拆为多段");
    assert_eq!(fa.len(), fb.len());
    assert_eq!(fa[0].reference_id, fb[0].reference_id, "测试前提：两条 reference 相同");

    let mut merger = LongMessageMerger::new();
    let mut done_a = None;
    let mut done_b = None;
    // 交错喂入：A1, B1, A2, B2, ...
    for i in 0..fa.len() {
        if let Some(m) = merger.add_frame("13800000001", fa[i].clone()).unwrap() {
            done_a = Some(m);
        }
        if let Some(m) = merger.add_frame("13800000002", fb[i].clone()).unwrap() {
            done_b = Some(m);
        }
    }

    let da = done_a.expect("发送方 A 的长短信应独立合齐");
    let db = done_b.expect("发送方 B 的长短信应独立合齐（不应被 A 的同 reference 顶掉）");
    assert_eq!(da, content_a, "A 内容不应混入 B 的分段");
    assert_eq!(db, content_b, "B 内容不应混入 A 的分段");
}

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

    let result = merger.add_frame("s", frame).unwrap();

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
        let result = merger.add_frame("s", frame).unwrap();
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

    let result1 = merger.add_frame("s", frame1).unwrap();
    assert!(result1.is_none());
    assert_eq!(merger.pending_count(), 1);

    let result2 = merger.add_frame("s", frame2).unwrap();
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
