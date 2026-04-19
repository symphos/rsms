//! Long message frame structure with UDH (User Data Header) support.

use rsms_core::RsmsError;

pub const UDH_IEI_CONCAT_8BIT: u8 = 0x00;
pub const UDH_IEI_CONCAT_16BIT: u8 = 0x08;
pub const UDH_HEADER_LEN: usize = 6;
pub const UDH_HEADER_8BIT_LEN: usize = 6;
pub const UDH_HEADER_16BIT_LEN: usize = 7;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdhHeader {
    pub iei: u8,
    pub iedl: u8,
    pub reference_id: u16,
    pub total_segments: u8,
    pub segment_number: u8,
}

impl UdhHeader {
    pub fn new_8bit(reference_id: u8, total_segments: u8, segment_number: u8) -> Self {
        Self {
            iei: UDH_IEI_CONCAT_8BIT,
            iedl: 3,
            reference_id: reference_id as u16,
            total_segments,
            segment_number,
        }
    }

    pub fn new_16bit(reference_id: u16, total_segments: u8, segment_number: u8) -> Self {
        Self {
            iei: UDH_IEI_CONCAT_16BIT,
            iedl: 4,
            reference_id,
            total_segments,
            segment_number,
        }
    }

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

#[derive(Debug, Clone)]
pub struct LongMessageFrame {
    pub reference_id: u16,
    pub total_segments: u8,
    pub segment_number: u8,
    pub content: Vec<u8>,
    pub has_udhi: bool,
    pub udh: Option<UdhHeader>,
}

impl LongMessageFrame {
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

    pub fn unique_id(&self) -> String {
        format!("{}-{}", self.reference_id, self.total_segments)
    }
}

pub struct UdhParser;

impl UdhParser {
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

    pub fn strip_udh(content: &[u8]) -> Vec<u8> {
        if let Some((_, udh_len)) = Self::extract_udh(content) {
            content[udh_len..].to_vec()
        } else {
            content.to_vec()
        }
    }

    pub fn has_udhi(content: &[u8]) -> bool {
        Self::extract_udh(content).is_some()
    }
}
