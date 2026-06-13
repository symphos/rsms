use bytes::Buf;
use rsms_core::RsmsError;
use std::io::Cursor;

use crate::codec::decode_cstring;
use crate::datatypes::{
    BindReceiver, BindReceiverResp, BindTransceiver, BindTransceiverResp, BindTransmitter,
    BindTransmitterResp, CancelSm, CancelSmResp, CommandId, DeliverSm, DeliverSmResp, EnquireLink,
    EnquireLinkResp, GenericNack, QuerySm, QuerySmResp, SubmitSm, SubmitSmResp, Unbind, UnbindResp,
};
use crate::submit_decode::{decode_deliver_sm, decode_submit_sm};
use crate::version::SmppVersion;

#[derive(Debug, Clone, PartialEq)]
pub enum SmppMessage {
    BindTransmitter(BindTransmitter),
    BindTransmitterResp(BindTransmitterResp),
    BindReceiver(BindReceiver),
    BindReceiverResp(BindReceiverResp),
    BindTransceiver(BindTransceiver),
    BindTransceiverResp(BindTransceiverResp),
    SubmitSm(SubmitSm),
    SubmitSmResp(SubmitSmResp),
    DeliverSm(DeliverSm),
    DeliverSmResp(DeliverSmResp),
    EnquireLink(EnquireLink),
    EnquireLinkResp(EnquireLinkResp),
    Unbind(Unbind),
    UnbindResp(UnbindResp),
    QuerySm(QuerySm),
    QuerySmResp(QuerySmResp),
    CancelSm(CancelSm),
    CancelSmResp(CancelSmResp),
    GenericNack(GenericNack),
    Unknown { command_id: u32, body: Vec<u8> },
}

/// 解码一条 SMPP PDU 字节序列为 `SmppMessage`，版本默认为 V3.4。
///
/// 等价于 `decode_message_with_version(buf, None)`。
pub fn decode_message(buf: &[u8]) -> Result<SmppMessage, RsmsError> {
    decode_message_with_version(buf, None)
}

/// 解码一条 SMPP PDU 字节序列为 `SmppMessage`，并指定协议版本。
///
/// SMPP 头部为 16 字节（比 CMPP/SMGP 多 4 字节的 `command_status`）。
/// `version` 影响 `SubmitSm`/`DeliverSm` 中部分可选字段的最大长度限制（V5.0 放宽）；
/// 传入 `None` 时默认按 V3.4 解码。
/// 未知命令字映射为 `SmppMessage::Unknown`，不返回错误。
pub fn decode_message_with_version(
    buf: &[u8],
    version: Option<SmppVersion>,
) -> Result<SmppMessage, RsmsError> {
    if buf.len() < 16 {
        return Err(RsmsError::Codec(
            "buffer too short for SMPP header".to_string(),
        ));
    }

    let mut cursor = Cursor::new(buf);
    let command_length = cursor
        .try_get_u32()
        .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
    let command_id_raw = cursor
        .try_get_u32()
        .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
    let _command_status = cursor
        .try_get_u32()
        .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
    let _sequence_number = cursor
        .try_get_u32()
        .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;

    if !(16..=100_000).contains(&command_length) {
        return Err(RsmsError::Codec(format!(
            "invalid SMPP length: {}",
            command_length
        )));
    }

    let body = if buf.len() > 16 {
        // command_length 来自对端，可能大于实际收到的字节数；钳制到 buf.len() 防止切片越界 panic
        let body_end = (command_length as usize).min(buf.len());
        buf[16..body_end].to_vec()
    } else {
        vec![]
    };

    let command_id = match CommandId::try_from(command_id_raw) {
        Ok(id) => id,
        Err(_) => {
            return Ok(SmppMessage::Unknown {
                command_id: command_id_raw,
                body,
            });
        }
    };

    let msg = match command_id {
        CommandId::BIND_TRANSMITTER => {
            let mut cursor = Cursor::new(body.as_slice());
            let system_id = decode_cstring(&mut cursor, 16, "system_id")?;
            let password = decode_cstring(&mut cursor, 9, "password")?;
            let system_type = decode_cstring(&mut cursor, 13, "system_type")?;
            let interface_version = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let addr_ton = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let addr_npi = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let address_range = decode_cstring(&mut cursor, 16, "address_range")?;
            SmppMessage::BindTransmitter(BindTransmitter {
                system_id,
                password,
                system_type,
                interface_version,
                addr_ton,
                addr_npi,
                address_range,
            })
        }
        CommandId::BIND_TRANSMITTER_RESP => {
            let mut cursor = Cursor::new(body.as_slice());
            let system_id = decode_cstring(&mut cursor, 16, "system_id")?;
            let sc_interface_version = if cursor.remaining() > 0 {
                cursor
                    .try_get_u8()
                    .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?
            } else {
                0x34
            };
            SmppMessage::BindTransmitterResp(BindTransmitterResp {
                system_id,
                sc_interface_version,
            })
        }
        CommandId::BIND_RECEIVER => {
            let mut cursor = Cursor::new(body.as_slice());
            let system_id = decode_cstring(&mut cursor, 16, "system_id")?;
            let password = decode_cstring(&mut cursor, 9, "password")?;
            let system_type = decode_cstring(&mut cursor, 13, "system_type")?;
            let interface_version = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let addr_ton = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let addr_npi = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let address_range = decode_cstring(&mut cursor, 16, "address_range")?;
            SmppMessage::BindReceiver(BindReceiver {
                system_id,
                password,
                system_type,
                interface_version,
                addr_ton,
                addr_npi,
                address_range,
            })
        }
        CommandId::BIND_RECEIVER_RESP => {
            let mut cursor = Cursor::new(body.as_slice());
            let system_id = decode_cstring(&mut cursor, 16, "system_id")?;
            let sc_interface_version = if cursor.remaining() > 0 {
                cursor
                    .try_get_u8()
                    .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?
            } else {
                0x34
            };
            SmppMessage::BindReceiverResp(BindReceiverResp {
                system_id,
                sc_interface_version,
            })
        }
        CommandId::BIND_TRANSCEIVER => {
            let mut cursor = Cursor::new(body.as_slice());
            let system_id = decode_cstring(&mut cursor, 16, "system_id")?;
            let password = decode_cstring(&mut cursor, 9, "password")?;
            let system_type = decode_cstring(&mut cursor, 13, "system_type")?;
            let interface_version = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let addr_ton = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let addr_npi = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let address_range = decode_cstring(&mut cursor, 16, "address_range")?;
            SmppMessage::BindTransceiver(BindTransceiver {
                system_id,
                password,
                system_type,
                interface_version,
                addr_ton,
                addr_npi,
                address_range,
            })
        }
        CommandId::BIND_TRANSCEIVER_RESP => {
            let mut cursor = Cursor::new(body.as_slice());
            let system_id = decode_cstring(&mut cursor, 16, "system_id")?;
            let sc_interface_version = if cursor.remaining() > 0 {
                cursor
                    .try_get_u8()
                    .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?
            } else {
                0x34
            };
            SmppMessage::BindTransceiverResp(BindTransceiverResp {
                system_id,
                sc_interface_version,
            })
        }
        CommandId::SUBMIT_SM => {
            let submit = decode_submit_sm(version, command_length, &body)?;
            SmppMessage::SubmitSm(submit)
        }
        CommandId::SUBMIT_SM_RESP => {
            let mut cursor = Cursor::new(body.as_slice());
            let message_id = decode_cstring(&mut cursor, 65, "message_id")?;
            SmppMessage::SubmitSmResp(SubmitSmResp { message_id })
        }
        CommandId::DELIVER_SM => {
            let deliver = decode_deliver_sm(version, command_length, &body)?;
            SmppMessage::DeliverSm(deliver)
        }
        CommandId::DELIVER_SM_RESP => {
            let mut cursor = Cursor::new(body.as_slice());
            let message_id = decode_cstring(&mut cursor, 65, "message_id")?;
            SmppMessage::DeliverSmResp(DeliverSmResp { message_id })
        }
        CommandId::ENQUIRE_LINK => SmppMessage::EnquireLink(EnquireLink),
        CommandId::ENQUIRE_LINK_RESP => SmppMessage::EnquireLinkResp(EnquireLinkResp),
        CommandId::UNBIND => SmppMessage::Unbind(Unbind),
        CommandId::UNBIND_RESP => SmppMessage::UnbindResp(UnbindResp),
        CommandId::QUERY_SM => {
            let mut cursor = Cursor::new(body.as_slice());
            if body.len() < 3 {
                return Err(RsmsError::Codec("QuerySm body too short".to_string()));
            }
            let message_id = decode_cstring(&mut cursor, 65, "message_id")?;
            let source_addr_ton = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let source_addr_npi = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let source_addr = decode_cstring(&mut cursor, 21, "source_addr")?;
            SmppMessage::QuerySm(QuerySm {
                message_id,
                source_addr_ton,
                source_addr_npi,
                source_addr,
            })
        }
        CommandId::QUERY_SM_RESP => {
            let mut cursor = Cursor::new(body.as_slice());
            if body.is_empty() {
                return Err(RsmsError::Codec("QuerySmResp body too short".to_string()));
            }
            let message_id = decode_cstring(&mut cursor, 65, "message_id")?;
            let message_state = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let message_state_text = if cursor.remaining() > 0 {
                Some(decode_cstring(&mut cursor, 20, "message_state_text")?)
            } else {
                None
            };
            SmppMessage::QuerySmResp(QuerySmResp {
                message_id,
                message_state,
                message_state_text,
            })
        }
        CommandId::CANCEL_SM => {
            let mut cursor = Cursor::new(body.as_slice());
            if body.len() < 4 {
                return Err(RsmsError::Codec("CancelSm body too short".to_string()));
            }
            let service_type = decode_cstring(&mut cursor, 6, "service_type")?;
            let message_id = decode_cstring(&mut cursor, 65, "message_id")?;
            let source_addr_ton = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let source_addr_npi = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let source_addr = decode_cstring(&mut cursor, 21, "source_addr")?;
            let dest_addr_ton = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let dest_addr_npi = cursor
                .try_get_u8()
                .map_err(|_| RsmsError::Codec("incomplete PDU".to_string()))?;
            let destination_addr = decode_cstring(&mut cursor, 21, "destination_addr")?;
            SmppMessage::CancelSm(CancelSm {
                service_type,
                message_id,
                source_addr_ton,
                source_addr_npi,
                source_addr,
                dest_addr_ton,
                dest_addr_npi,
                destination_addr,
            })
        }
        CommandId::CANCEL_SM_RESP => SmppMessage::CancelSmResp(CancelSmResp),
        CommandId::GENERIC_NACK => SmppMessage::GenericNack(GenericNack::new()),
        _ => SmppMessage::Unknown {
            command_id: command_id_raw,
            body,
        },
    };

    Ok(msg)
}

#[cfg(test)]
mod fuzz_tests {
    use super::*;

    /// A truncated/short body on any body-carrying PDU must never panic the
    /// decoder. We build a valid 16-byte SMPP header followed by N zero body
    /// bytes (for N in 0..48), set command_length = 16 + N and the command_id,
    /// then ensure `decode_message_with_version` returns (Ok or Err) without
    /// panicking, for both SMPP v3.4 and v5.0.
    #[test]
    fn decode_message_short_body_never_panics() {
        // Command ids that carry a body and run a real decode path.
        let command_ids: &[u32] = &[
            CommandId::BIND_TRANSMITTER as u32,
            CommandId::BIND_TRANSMITTER_RESP as u32,
            CommandId::BIND_RECEIVER as u32,
            CommandId::BIND_RECEIVER_RESP as u32,
            CommandId::BIND_TRANSCEIVER as u32,
            CommandId::BIND_TRANSCEIVER_RESP as u32,
            CommandId::SUBMIT_SM as u32,
            CommandId::SUBMIT_SM_RESP as u32,
            CommandId::DELIVER_SM as u32,
            CommandId::DELIVER_SM_RESP as u32,
            CommandId::DATA_SM as u32,
        ];

        for &cmd_id in command_ids {
            for n in 0u32..48 {
                let total_len = 16 + n;
                let mut pdu = vec![0u8; total_len as usize];
                // SMPP header is big-endian: length, id, status, sequence.
                pdu[0..4].copy_from_slice(&total_len.to_be_bytes());
                pdu[4..8].copy_from_slice(&cmd_id.to_be_bytes());
                // command_status = 0 (already zero)
                // sequence_number must be non-zero / non-0xFFFFFFFF for some
                // paths; the live message decoder ignores it, but set a sane
                // value anyway.
                pdu[12..16].copy_from_slice(&1u32.to_be_bytes());

                // Must not panic for either version. Result may be Ok or Err.
                let _ = decode_message_with_version(&pdu, Some(SmppVersion::V34));
                let _ = decode_message_with_version(&pdu, Some(SmppVersion::V50));
            }
        }
    }
}
