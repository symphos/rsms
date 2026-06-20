use bytes::{Buf, BufMut, BytesMut};
use rsms_core::{decode_pstring, encode_pstring};
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Encodable, PduHeader};
use crate::datatypes::CommandId;

/// CMPP Deliver 请求（String 字段版）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deliver {
    pub msg_id: [u8; 8],
    pub dest_id: String,
    pub service_id: String,
    pub tppid: u8,
    pub tpudhi: u8,
    pub msg_fmt: u8,
    pub src_terminal_id: String,
    pub src_terminal_type: u8,
    pub registered_delivery: u8,
    pub msg_content: Vec<u8>,
    pub link_id: String,
}

impl Deliver {
    pub fn new() -> Self {
        Self {
            msg_id: [0u8; 8],
            dest_id: String::new(),
            service_id: String::new(),
            tppid: 0,
            tpudhi: 0,
            msg_fmt: 0,
            src_terminal_id: String::new(),
            src_terminal_type: 0,
            registered_delivery: 0,
            msg_content: Vec::new(),
            link_id: String::new(),
        }
    }
}

impl Default for Deliver {
    fn default() -> Self {
        Self::new()
    }
}

impl Encodable for Deliver {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_slice(&self.msg_id);
        encode_pstring(buf, &self.dest_id, 21, "dest_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "dest_id",
                reason: e,
            }
        })?;
        encode_pstring(buf, &self.service_id, 10, "service_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "service_id",
                reason: e,
            }
        })?;
        buf.put_u8(self.tppid);
        buf.put_u8(self.tpudhi);
        buf.put_u8(self.msg_fmt);
        encode_pstring(buf, &self.src_terminal_id, 32, "src_terminal_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "src_terminal_id",
                reason: e,
            }
        })?;
        buf.put_u8(self.src_terminal_type);
        buf.put_u8(self.registered_delivery);
        if self.msg_content.len() > u8::MAX as usize {
            return Err(CodecError::FieldValidation {
                field: "msg_content",
                reason: format!(
                    "正文长度 {} 超过 Msg_Length(u8) 上限 255",
                    self.msg_content.len()
                ),
            });
        }
        buf.put_u8(self.msg_content.len() as u8);
        buf.put_slice(&self.msg_content);
        encode_pstring(buf, &self.link_id, 20, "link_id").map_err(|e| {
            CodecError::FieldValidation {
                field: "link_id",
                reason: e,
            }
        })?;
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        8 + 21 + 10 + 1 + 1 + 1 + 32 + 1 + 1 + 1 + self.msg_content.len() + 20
    }
}

impl Decodable for Deliver {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        let body_len = (header.total_length - PduHeader::SIZE as u32) as usize;
        if buf.remaining() < body_len {
            return Err(CodecError::Incomplete);
        }

        let mut msg_id = [0u8; 8];
        buf.try_copy_to_slice(&mut msg_id)
            .map_err(|_| CodecError::Incomplete)?;
        let dest_id = decode_pstring(buf, 21).map_err(|_| CodecError::Incomplete)?;
        let service_id = decode_pstring(buf, 10).map_err(|_| CodecError::Incomplete)?;
        let tppid = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let tpudhi = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let msg_fmt = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let src_terminal_id = decode_pstring(buf, 32).map_err(|_| CodecError::Incomplete)?;
        let src_terminal_type = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let registered_delivery = buf.try_get_u8().map_err(|_| CodecError::Incomplete)?;
        let msg_length = buf.try_get_u8().map_err(|_| CodecError::Incomplete)? as usize;
        if buf.remaining() < msg_length {
            return Err(CodecError::Incomplete);
        }
        let mut msg_content = vec![0u8; msg_length];
        buf.try_copy_to_slice(&mut msg_content)
            .map_err(|_| CodecError::Incomplete)?;
        let link_id = decode_pstring(buf, 20).map_err(|_| CodecError::Incomplete)?;

        Ok(Deliver {
            msg_id,
            dest_id,
            service_id,
            tppid,
            tpudhi,
            msg_fmt,
            src_terminal_id,
            src_terminal_type,
            registered_delivery,
            msg_content,
            link_id,
        })
    }

    fn command_id() -> CommandId {
        CommandId::Deliver
    }
}

/// CMPP Deliver 响应
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliverResp {
    pub msg_id: [u8; 8],
    pub result: u32,
}

impl DeliverResp {
    pub const BODY_SIZE: usize = 8 + 4;
}

impl Encodable for DeliverResp {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_slice(&self.msg_id);
        buf.put_u32(self.result);
        Ok(())
    }

    fn encoded_size(&self) -> usize {
        Self::BODY_SIZE
    }
}

impl Decodable for DeliverResp {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if header.total_length != (PduHeader::SIZE + Self::BODY_SIZE) as u32 {
            return Err(CodecError::InvalidPduLength {
                length: header.total_length,
                min: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
                max: (PduHeader::SIZE + Self::BODY_SIZE) as u32,
            });
        }
        if buf.remaining() < Self::BODY_SIZE {
            return Err(CodecError::Incomplete);
        }
        let mut msg_id = [0u8; 8];
        buf.try_copy_to_slice(&mut msg_id)
            .map_err(|_| CodecError::Incomplete)?;
        let result = buf.try_get_u32().map_err(|_| CodecError::Incomplete)?;
        Ok(DeliverResp { msg_id, result })
    }

    fn command_id() -> CommandId {
        CommandId::DeliverResp
    }
}

/// CMPP 状态报告（从 Deliver 的 msg_content 二进制解析）
///
/// CMPP 状态报告是**固定二进制结构**（非文本 key:value）：
/// Msg_Id(8, 二进制) + Stat(7) + Submit_time(10) + Done_time(10) + Dest_terminal_Id(21) + SMSC_sequence(4, 二进制)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmppReport {
    pub msg_id: [u8; 8],
    pub stat: String,
    pub submit_time: String,
    pub done_time: String,
    pub dest_terminal_id: String,
    pub smsc_sequence: u32,
}

impl CmppReport {
    /// 解析 CMPP 状态报告（Deliver.msg_content 的原始字节）。
    ///
    /// 旧实现把整段当文本 `MsgId:.. Stat:..` 解析，与真实 CMPP 网关（如 lihuanghe/SMSGate）
    /// 的定长二进制报告不兼容——Msg_Id 是 8 字节二进制（含非 UTF-8 字节），整段 from_utf8 即失败。
    pub fn parse(data: &[u8]) -> Option<Self> {
        // 至少需到 Done_time（8+7+10+10=35）
        if data.len() < 35 {
            return None;
        }
        let mut msg_id = [0u8; 8];
        msg_id.copy_from_slice(&data[0..8]);

        let take = |slice: &[u8]| {
            String::from_utf8_lossy(slice)
                .trim_end_matches('\0')
                .trim()
                .to_string()
        };
        let stat = take(&data[8..15]);
        let submit_time = take(&data[15..25]);
        let done_time = take(&data[25..35]);
        // Dest_terminal_Id 21 字节（CMPP3.0），SMSC_sequence 4 字节；长度不足时尽力取。
        let dest_terminal_id = if data.len() >= 56 {
            take(&data[35..56])
        } else {
            take(&data[35..])
        };
        let smsc_sequence = if data.len() >= 60 {
            u32::from_be_bytes([data[56], data[57], data[58], data[59]])
        } else {
            0
        };

        if stat.is_empty() {
            return None;
        }

        Some(CmppReport {
            msg_id,
            stat,
            submit_time,
            done_time,
            dest_terminal_id,
            smsc_sequence,
        })
    }

    /// 编码为 CMPP 3.0 状态报告正文（定长 71 字节二进制）：
    /// Msg_Id(8) + Stat(7) + Submit_time(10) + Done_time(10) + Dest_terminal_Id(32) + SMSC_sequence(4)。
    /// 字符串字段按字节截断/补 0 到定长。真实 CMPP 网关（如 lihuanghe/SMSGate）按此定长解析；
    /// 旧 example 发自由文本（119B）会被对端按定长解致字段错位（cmos WARN: length 119 should be 71）。
    pub fn to_bytes(&self) -> Vec<u8> {
        fn fixed(src: &[u8], n: usize) -> Vec<u8> {
            let mut v = vec![0u8; n];
            let len = src.len().min(n);
            v[..len].copy_from_slice(&src[..len]);
            v
        }
        let mut b = Vec::with_capacity(71);
        b.extend_from_slice(&self.msg_id);
        b.extend_from_slice(&fixed(self.stat.as_bytes(), 7));
        b.extend_from_slice(&fixed(self.submit_time.as_bytes(), 10));
        b.extend_from_slice(&fixed(self.done_time.as_bytes(), 10));
        b.extend_from_slice(&fixed(self.dest_terminal_id.as_bytes(), 32));
        b.extend_from_slice(&self.smsc_sequence.to_be_bytes());
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Pdu;

    fn decode_pdu<T: Decodable>(bytes: &[u8]) -> Result<T, CodecError> {
        let mut cursor = Cursor::new(bytes);
        let header = PduHeader::decode(&mut cursor)?;
        T::decode(header, &mut cursor)
    }

    #[test]
    fn deliver_resp_roundtrip() {
        let resp = DeliverResp {
            msg_id: [0x87, 0x65, 0x43, 0x21, 0x00, 0x00, 0x00, 0x00],
            result: 0,
        };
        let bytes = Pdu::from(resp.clone()).to_pdu_bytes(1);
        let decoded = decode_pdu::<DeliverResp>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.msg_id, resp.msg_id);
        assert_eq!(decoded.result, resp.result);
    }

    #[test]
    fn deliver_mo_roundtrip() {
        let mut deliver = Deliver::new();
        deliver.msg_fmt = 0;
        deliver.registered_delivery = 0;
        deliver.msg_content = b"MO reply".to_vec();
        deliver.src_terminal_id = "13800138000".to_string();

        let bytes = Pdu::from(deliver).to_pdu_bytes(5);
        let decoded = decode_pdu::<Deliver>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.registered_delivery, 0);
        assert_eq!(decoded.msg_content, b"MO reply");
        assert_eq!(decoded.src_terminal_id, "13800138000");
    }

    #[test]
    fn deliver_string_fields_decode_correctly() {
        let mut deliver = Deliver::new();
        deliver.dest_id = "10655000000".to_string();
        deliver.service_id = "SMS".to_string();
        deliver.src_terminal_id = "13800138000".to_string();
        deliver.link_id = "ABC123".to_string();
        deliver.msg_content = b"Hello".to_vec();

        let bytes = Pdu::from(deliver).to_pdu_bytes(1);
        let decoded = decode_pdu::<Deliver>(&bytes.as_slice()).unwrap();
        assert_eq!(decoded.dest_id, "10655000000");
        assert_eq!(decoded.service_id, "SMS");
        assert_eq!(decoded.src_terminal_id, "13800138000");
        assert_eq!(decoded.link_id, "ABC123");
    }

    #[test]
    fn encode_oversized_msg_content_returns_err_not_truncate() {
        // 4A.6：Msg_Length 是 u8（上限 255）。正文 256 字节时旧代码静默截断为 0，
        // 修复后 encode 必须返回 Err。
        let mut deliver = Deliver::new();
        deliver.msg_content = vec![0x41u8; 256];
        let mut buf = BytesMut::new();
        assert!(deliver.encode(&mut buf).is_err());
    }

    #[test]
    fn deliver_status_report_parse() {
        // CMPP 报告定长二进制：MsgId(8) Stat(7) Submit(10) Done(10) Dest(21) SMSC(4)=60。
        // MsgId 含 0xf1 高位字节（旧 from_utf8 文本解析在此即失败）。
        let mut data = Vec::new();
        let msg_id = [0x67u8, 0x05, 0xf1, 0x03, 0x24, 0x66, 0x2f, 0x02];
        data.extend_from_slice(&msg_id);
        data.extend_from_slice(b"DELIVRD");
        data.extend_from_slice(b"0405120000");
        data.extend_from_slice(b"0405120100");
        let mut dest = b"13800138000".to_vec();
        dest.resize(21, 0);
        data.extend_from_slice(&dest);
        data.extend_from_slice(&42u32.to_be_bytes());
        let report = CmppReport::parse(&data).unwrap();
        assert_eq!(report.msg_id, msg_id);
        assert_eq!(report.stat, "DELIVRD");
        assert_eq!(report.submit_time, "0405120000");
        assert_eq!(report.dest_terminal_id, "13800138000");
        assert_eq!(report.smsc_sequence, 42);
    }

    #[test]
    fn deliver_status_report_invalid() {
        assert!(CmppReport::parse(b"short").is_none());
    }
}
