use rsms_core::RsmsError;

/// CMPP 消息头（12 字节，与 `CmppHeaderCodec` 一致：Total_Length + Command_Id + Sequence_Id）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmppHeader {
    /// 整包长度，含本头 12 字节
    pub total_length: u32,
    pub command_id: u32,
    pub sequence_id: u32,
}

pub const HEADER_LEN: usize = 12;

/// 解析一帧完整 PDU（`buf` 为单帧，从首字节起为 `Total_Length`）。
pub fn parse_pdu(buf: &[u8]) -> Result<(CmppHeader, &[u8]), RsmsError> {
    if buf.len() < HEADER_LEN {
        return Err(RsmsError::Codec("PDU shorter than header".into()));
    }
    let total_length = u32::from_be_bytes(buf[0..4].try_into().unwrap());
    if total_length as usize != buf.len() {
        return Err(RsmsError::Codec(format!(
            "total_length {total_length} != buffer len {}",
            buf.len()
        )));
    }
    let command_id = u32::from_be_bytes(buf[4..8].try_into().unwrap());
    let sequence_id = u32::from_be_bytes(buf[8..12].try_into().unwrap());
    let body = &buf[HEADER_LEN..];
    let expect_body = total_length
        .checked_sub(HEADER_LEN as u32)
        .ok_or_else(|| RsmsError::Codec("total_length underflow".into()))?
        as usize;
    if body.len() != expect_body {
        return Err(RsmsError::Codec(format!(
            "body len {} expected {}",
            body.len(),
            expect_body
        )));
    }
    Ok((
        CmppHeader {
            total_length,
            command_id,
            sequence_id,
        },
        body,
    ))
}
