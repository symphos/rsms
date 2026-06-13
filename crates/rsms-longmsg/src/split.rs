//! Long message splitting into segments.

use crate::frame::{LongMessageFrame, UDH_HEADER_16BIT_LEN, UDH_HEADER_8BIT_LEN};
use crate::reference_id::ReferenceIdGenerator;
use std::sync::Arc;

/// GSM 7-bit 编码单条短信最大字符数（160 字符）。
pub const GSM_7BIT_SINGLE_MAX: usize = 160;
/// GSM 7-bit 编码长短信每段最大字符数（含 UDH 头后剩余 153 字符）。
pub const GSM_7BIT_MULTI_MAX: usize = 153;
/// UCS-2 编码单条短信最大字符数（70 个 Unicode 字符）。
pub const UCS2_SINGLE_MAX: usize = 70;
/// UCS-2 编码长短信每段最大字符数（含 UDH 头后剩余 67 个 Unicode 字符）。
pub const UCS2_MULTI_MAX: usize = 67;

/// TP-UDHI 标志位掩码，用于 TP-UD 首字节中标识用户数据头的存在。
pub const TP_UDHI_MASK: u8 = 0x40;

/// 短信编码字母表，决定分段时的字节上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmsAlphabet {
    /// GSM 7-bit 默认字母表。
    GSM7,
    /// ASCII（与 GSM 7-bit 使用相同分段限制）。
    ASCII,
    /// UCS-2（UTF-16BE），每个汉字/Emoji 占 2 字节。
    UCS2,
    /// 二进制内容，每段最多 134 字节。
    Binary,
}

/// 长短信分割器，将超长内容按协议规范切割为带 UDH 头的多段帧。
///
/// 每个实例持有一个引用 ID 生成器；引用 ID 超过 255 时自动切换到 16-bit UDH，
/// 低于等于 255 时使用 8-bit UDH，分段 payload 长度相应调整以满足 GSM 03.40。
pub struct LongMessageSplitter {
    generator: Arc<ReferenceIdGenerator>,
}

impl LongMessageSplitter {
    /// 创建一个使用默认引用 ID 生成器（随机初始值）的分割器。
    pub fn new() -> Self {
        Self {
            generator: Arc::new(ReferenceIdGenerator::new()),
        }
    }

    /// 使用指定的引用 ID 生成器创建分割器，常用于测试中固定引用 ID。
    pub fn with_generator(generator: Arc<ReferenceIdGenerator>) -> Self {
        Self { generator }
    }

    /// 将 `content` 按 `alphabet` 对应的字节上限切割为 `LongMessageFrame` 序列。
    ///
    /// 若内容未超出单段上限，返回包含单个无 UDH 帧的 `Vec`。
    /// 否则自动生成引用 ID 并构造带 UDH 的多段帧，各段长度严格不超过协议上限。
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
        // UDH 实际宽度取决于 ref_id：>255 用 16-bit(7B)，否则 8-bit(6B)。分段 payload 必须按
        // 实际 UDH 宽度扣减，否则 16-bit 段 frame_content 会超出 max_per_segment 上限（违反 GSM 03.40）。
        let udh_len = if reference_id > 255 {
            UDH_HEADER_16BIT_LEN
        } else {
            UDH_HEADER_8BIT_LEN
        };
        let payload_per_segment = max_per_segment - udh_len;
        let total_segments = content.len().div_ceil(payload_per_segment) as u8;

        let mut frames = Vec::with_capacity(total_segments as usize);
        for i in 0..total_segments {
            let start = (i as usize) * payload_per_segment;
            let end = (start + payload_per_segment).min(content.len());
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

    /// 与 `split` 相同，但直接返回各段的原始字节（含 UDH 头），不含 `LongMessageFrame` 元数据。
    ///
    /// 适合直接将字节写入 PDU `msg_content` 字段的场景。
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{UDH_HEADER_16BIT_LEN, UDH_HEADER_8BIT_LEN, UDH_IEI_CONCAT_16BIT};
    use crate::reference_id::ReferenceIdGenerator;
    use std::sync::Arc;

    fn assert_segments_within_limit(frames: &[LongMessageFrame], max: usize) {
        assert!(frames.len() > 1, "内容应被分成多段");
        for f in frames {
            assert!(
                f.content.len() <= max,
                "分段 content 长度 {} 超过上限 {}（segment {}/{}）",
                f.content.len(),
                max,
                f.segment_number,
                f.total_segments
            );
        }
    }

    /// 16-bit UDH（ref_id>255，7 字节头）分段 content 不得超过协议上限。
    #[test]
    fn split_16bit_udh_segment_within_gsm_limit() {
        let g = Arc::new(ReferenceIdGenerator::with_value(300)); // >255 → 16-bit
        let mut splitter = LongMessageSplitter::with_generator(g);
        let frames = splitter.split(&vec![b'A'; 500], SmsAlphabet::GSM7);
        assert_eq!(frames[0].reference_id, 300);
        assert_eq!(frames[0].udh.as_ref().unwrap().iei, UDH_IEI_CONCAT_16BIT);
        assert_segments_within_limit(&frames, GSM_7BIT_MULTI_MAX);
    }

    /// 8-bit UDH（ref_id<=255，6 字节头）分段同样在上限内（回归保护）。
    #[test]
    fn split_8bit_udh_segment_within_gsm_limit() {
        let g = Arc::new(ReferenceIdGenerator::with_value(10)); // <=255 → 8-bit
        let mut splitter = LongMessageSplitter::with_generator(g);
        let frames = splitter.split(&vec![b'B'; 500], SmsAlphabet::GSM7);
        assert_segments_within_limit(&frames, GSM_7BIT_MULTI_MAX);
    }

    /// UCS2 16-bit 分段在上限内。
    #[test]
    fn split_16bit_udh_segment_within_ucs2_limit() {
        let g = Arc::new(ReferenceIdGenerator::with_value(400));
        let mut splitter = LongMessageSplitter::with_generator(g);
        let frames = splitter.split(&vec![b'C'; 300], SmsAlphabet::UCS2);
        assert_segments_within_limit(&frames, UCS2_MULTI_MAX);
    }

    /// 去掉每段 UDH 头后拼接应还原原文（无字节丢失/重叠/超长）。
    #[test]
    fn split_16bit_roundtrip_payload_preserved() {
        let g = Arc::new(ReferenceIdGenerator::with_value(300));
        let mut splitter = LongMessageSplitter::with_generator(g);
        let content: Vec<u8> = (0..500u32).map(|i| (i % 251) as u8).collect();
        let frames = splitter.split(&content, SmsAlphabet::GSM7);
        let mut reassembled = Vec::new();
        for f in &frames {
            // frame.content 头部宽度 = UDHL 前缀(1) + UDH 主体，即 8-bit 6 字节 / 16-bit 7 字节。
            let udh_len = if f.reference_id > 255 {
                UDH_HEADER_16BIT_LEN
            } else {
                UDH_HEADER_8BIT_LEN
            };
            reassembled.extend_from_slice(&f.content[udh_len..]);
        }
        assert_eq!(reassembled, content);
    }
}
