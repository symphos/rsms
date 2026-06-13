//! Long message frame structure with UDH (User Data Header) support.

use rsms_core::RsmsError;

/// 8-bit 级联短信 UDH 的信息元素标识符（IEI = 0x00）。
pub const UDH_IEI_CONCAT_8BIT: u8 = 0x00;
/// 16-bit 级联短信 UDH 的信息元素标识符（IEI = 0x08）。
pub const UDH_IEI_CONCAT_16BIT: u8 = 0x08;
/// 8-bit UDH 头总字节数（UDHL 前缀 1B + IEI 1B + IEDL 1B + ref 1B + total 1B + seq 1B = 6B）。
pub const UDH_HEADER_LEN: usize = 6;
/// 8-bit 引用 ID 的 UDH 头总长度（同 `UDH_HEADER_LEN`）。
pub const UDH_HEADER_8BIT_LEN: usize = 6;
/// 16-bit 引用 ID 的 UDH 头总长度（比 8-bit 多 1 字节存放高字节）。
pub const UDH_HEADER_16BIT_LEN: usize = 7;

/// 用户数据头（UDH）结构，携带长短信级联所需的引用 ID 和分段信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdhHeader {
    /// 信息元素标识符：`UDH_IEI_CONCAT_8BIT`(0x00) 或 `UDH_IEI_CONCAT_16BIT`(0x08)。
    pub iei: u8,
    /// 信息元素数据长度（8-bit 为 3，16-bit 为 4）。
    pub iedl: u8,
    /// 长短信引用 ID，同一条长短信的所有分段共享同一引用 ID。
    pub reference_id: u16,
    /// 该长短信的总分段数。
    pub total_segments: u8,
    /// 当前帧在长短信中的分段序号（从 1 开始）。
    pub segment_number: u8,
}

impl UdhHeader {
    /// 构造 8-bit 引用 ID 的 UDH（适用于 reference_id <= 255）。
    pub fn new_8bit(reference_id: u8, total_segments: u8, segment_number: u8) -> Self {
        Self {
            iei: UDH_IEI_CONCAT_8BIT,
            iedl: 3,
            reference_id: reference_id as u16,
            total_segments,
            segment_number,
        }
    }

    /// 构造 16-bit 引用 ID 的 UDH（适用于 reference_id > 255）。
    pub fn new_16bit(reference_id: u16, total_segments: u8, segment_number: u8) -> Self {
        Self {
            iei: UDH_IEI_CONCAT_16BIT,
            iedl: 4,
            reference_id,
            total_segments,
            segment_number,
        }
    }

    /// 将 UDH 序列化为字节序列（不含前置 UDHL 字节）。
    pub fn to_bytes(&self) -> Vec<u8> {
        if self.iei == UDH_IEI_CONCAT_16BIT {
            vec![
                self.iei,
                self.iedl,
                (self.reference_id >> 8) as u8,
                self.reference_id as u8,
                self.total_segments,
                self.segment_number,
            ]
        } else {
            vec![
                self.iei,
                self.iedl,
                self.reference_id as u8,
                self.total_segments,
                self.segment_number,
            ]
        }
    }

    /// 从字节切片解析 UDH（字节切片不含前置 UDHL，直接从 IEI 开始）。
    ///
    /// 返回 `Err` 若字节不足或格式非法。
    pub fn from_bytes(b: &[u8]) -> Result<Self, RsmsError> {
        if b.len() < UDH_HEADER_8BIT_LEN {
            return Err(RsmsError::Codec("UDH header too short".into()));
        }

        let iei = b[0];
        if iei == UDH_IEI_CONCAT_16BIT {
            if b.len() < UDH_HEADER_16BIT_LEN {
                return Err(RsmsError::Codec("16-bit UDH header too short".into()));
            }
            Ok(Self {
                iei: b[0],
                iedl: b[1],
                reference_id: u16::from_be_bytes([b[2], b[3]]),
                total_segments: b[4],
                segment_number: b[5],
            })
        } else {
            Ok(Self {
                iei: b[0],
                iedl: b[1],
                reference_id: b[2] as u16,
                total_segments: b[3],
                segment_number: b[4],
            })
        }
    }
}

/// 长短信的单个分段帧，包含完整的分段元数据和带 UDH 头的原始内容字节。
#[derive(Debug, Clone)]
pub struct LongMessageFrame {
    /// 长短信引用 ID，与 UDH 中的 `reference_id` 一致。
    pub reference_id: u16,
    /// 该长短信的总分段数。
    pub total_segments: u8,
    /// 当前帧的分段序号（从 1 开始）。
    pub segment_number: u8,
    /// 完整的帧内容字节，包含 UDH 头（若 `has_udhi` 为 true）和实际 payload。
    pub content: Vec<u8>,
    /// 标识 `content` 是否以 UDH 头开头。
    pub has_udhi: bool,
    /// 解析出的 UDH 结构，仅在 `has_udhi` 为 true 时有值。
    pub udh: Option<UdhHeader>,
}

impl LongMessageFrame {
    /// 构造一个长短信分段帧。
    pub fn new(
        reference_id: u16,
        total_segments: u8,
        segment_number: u8,
        content: Vec<u8>,
        has_udhi: bool,
        udh: Option<UdhHeader>,
    ) -> Self {
        Self {
            reference_id,
            total_segments,
            segment_number,
            content,
            has_udhi,
            udh,
        }
    }

    /// 返回该帧所属长短信分组的唯一键（`reference_id-total_segments`），
    /// 用于 `LongMessageMerger` 中的分组索引。
    pub fn unique_id(&self) -> String {
        format!("{}-{}", self.reference_id, self.total_segments)
    }
}

/// UDH 解析工具集，提供从原始帧内容中提取或剥离 UDH 头的静态方法。
pub struct UdhParser;

impl UdhParser {
    /// 尝试从帧内容的开头解析 UDH 头。
    ///
    /// 返回 `Some((header, udh_total_len))`，其中 `udh_total_len` 为含 UDHL 前缀字节的完整 UDH 占用字节数；
    /// 返回 `None` 若内容为空、UDHL 为 0 或长度不足。
    pub fn extract_udh(content: &[u8]) -> Option<(UdhHeader, usize)> {
        if content.is_empty() {
            return None;
        }
        let udhl = content[0] as usize;
        if udhl == 0 || content.len() < 1 + udhl {
            return None;
        }
        let udh_bytes = &content[1..1 + udhl];
        let header = UdhParser::parse_concat_udh(udh_bytes)?;
        Some((header, 1 + udhl))
    }

    fn parse_concat_udh(udh: &[u8]) -> Option<UdhHeader> {
        if udh.is_empty() {
            return None;
        }
        let iei = udh[0];
        if iei == UDH_IEI_CONCAT_16BIT {
            if udh.len() < 6 {
                return None;
            }
            Some(UdhHeader {
                iei: udh[0],
                iedl: udh[1],
                reference_id: u16::from_be_bytes([udh[2], udh[3]]),
                total_segments: udh[4],
                segment_number: udh[5],
            })
        } else if iei == UDH_IEI_CONCAT_8BIT {
            if udh.len() < 5 {
                return None;
            }
            Some(UdhHeader {
                iei: udh[0],
                iedl: udh[1],
                reference_id: udh[2] as u16,
                total_segments: udh[3],
                segment_number: udh[4],
            })
        } else {
            None
        }
    }

    /// 去除帧内容开头的 UDH 头，返回纯 payload 字节。
    ///
    /// 若内容不含 UDH（`extract_udh` 返回 `None`），原样返回。
    pub fn strip_udh(content: &[u8]) -> Vec<u8> {
        if let Some((_, udh_len)) = Self::extract_udh(content) {
            content[udh_len..].to_vec()
        } else {
            content.to_vec()
        }
    }

    /// 判断帧内容是否以有效的 UDH 头开头。
    pub fn has_udhi(content: &[u8]) -> bool {
        Self::extract_udh(content).is_some()
    }
}
