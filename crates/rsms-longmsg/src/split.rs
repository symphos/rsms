//! Long message splitting into segments.

use crate::frame::{LongMessageFrame, UDH_HEADER_LEN};
use crate::reference_id::ReferenceIdGenerator;
use std::sync::Arc;

pub const GSM_7BIT_SINGLE_MAX: usize = 160;
pub const GSM_7BIT_MULTI_MAX: usize = 153;
pub const UCS2_SINGLE_MAX: usize = 70;
pub const UCS2_MULTI_MAX: usize = 67;

pub const TP_UDHI_MASK: u8 = 0x40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmsAlphabet {
    GSM7,
    ASCII,
    UCS2,
    Binary,
}

pub struct LongMessageSplitter {
    generator: Arc<ReferenceIdGenerator>,
}

impl LongMessageSplitter {
    pub fn new() -> Self {
        Self {
            generator: Arc::new(ReferenceIdGenerator::new()),
        }
    }

    pub fn with_generator(generator: Arc<ReferenceIdGenerator>) -> Self {
        Self { generator }
    }

    pub fn split(&mut self, content: &[u8], alphabet: SmsAlphabet) -> Vec<LongMessageFrame> {
        let max_per_segment = match alphabet {
            SmsAlphabet::GSM7 | SmsAlphabet::ASCII => GSM_7BIT_MULTI_MAX,
            SmsAlphabet::UCS2 => UCS2_MULTI_MAX,
            SmsAlphabet::Binary => 134,
        };

        if content.len() <= max_per_segment {
            return vec![LongMessageFrame::new(
                0,
                1,
                1,
                content.to_vec(),
                false,
                None,
            )];
        }

        let reference_id = self.generator.next_reference_id();
        let total_segments = ((content.len() + max_per_segment - UDH_HEADER_LEN - 1)
            / (max_per_segment - UDH_HEADER_LEN)) as u8;

        let mut frames = Vec::with_capacity(total_segments as usize);
        for i in 0..total_segments {
            let start = (i as usize) * (max_per_segment - UDH_HEADER_LEN);
            let end = (start + max_per_segment - UDH_HEADER_LEN).min(content.len());
            let segment_content = &content[start..end];

            let (udh, frame_content) = if reference_id > 255 {
                Self::build_16bit_udh_frame(reference_id, total_segments, i + 1, segment_content)
            } else {
                Self::build_8bit_udh_frame(
                    reference_id as u8,
                    total_segments,
                    i + 1,
                    segment_content,
                )
            };

            frames.push(LongMessageFrame::new(
                reference_id,
                total_segments,
                i + 1,
                frame_content,
                true,
                Some(udh),
            ));
        }

        frames
    }

    fn build_8bit_udh_frame(
        reference_id: u8,
        total_segments: u8,
        segment_number: u8,
        segment_content: &[u8],
    ) -> (crate::frame::UdhHeader, Vec<u8>) {
        let udh = crate::frame::UdhHeader::new_8bit(reference_id, total_segments, segment_number);
        let mut frame_content = vec![
            0x05,
            0x00,
            0x03,
            reference_id,
            total_segments,
            segment_number,
        ];
        frame_content.extend_from_slice(segment_content);
        (udh, frame_content)
    }

    fn build_16bit_udh_frame(
        reference_id: u16,
        total_segments: u8,
        segment_number: u8,
        segment_content: &[u8],
    ) -> (crate::frame::UdhHeader, Vec<u8>) {
        let udh = crate::frame::UdhHeader::new_16bit(reference_id, total_segments, segment_number);
        let mut frame_content = vec![
            0x06,
            0x08,
            0x04,
            (reference_id >> 8) as u8,
            reference_id as u8,
            total_segments,
            segment_number,
        ];
        frame_content.extend_from_slice(segment_content);
        (udh, frame_content)
    }

    pub fn split_into_pdu(&mut self, content: &[u8], alphabet: SmsAlphabet) -> Vec<Vec<u8>> {
        self.split(content, alphabet)
            .into_iter()
            .map(|f| f.content)
            .collect()
    }
}

impl Default for LongMessageSplitter {
    fn default() -> Self {
        Self::new()
    }
}
