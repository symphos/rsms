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

pub fn decode_message(buf: &[u8]) -> Result<SmppMessage, RsmsError> {
    decode_message_with_version(buf, None)
}

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
    let command_length = cursor.get_u32();
    let command_id_raw = cursor.get_u32();
    let _command_status = cursor.get_u32();
    let _sequence_number = cursor.get_u32();

    if !(16..=100_000).contains(&command_length) {
        return Err(RsmsError::Codec(format!(
            "invalid SMPP length: {}",
            command_length
        )));
    }

    let body = if buf.len() > 16 {
        buf[16..command_length as usize].to_vec()
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
            let interface_version = cursor.get_u8();
            let addr_ton = cursor.get_u8();
            let addr_npi = cursor.get_u8();
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
                cursor.get_u8()
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
            let interface_version = cursor.get_u8();
            let addr_ton = cursor.get_u8();
            let addr_npi = cursor.get_u8();
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
                cursor.get_u8()
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
            let interface_version = cursor.get_u8();
            let addr_ton = cursor.get_u8();
            let addr_npi = cursor.get_u8();
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
                cursor.get_u8()
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
            let source_addr_ton = cursor.get_u8();
            let source_addr_npi = cursor.get_u8();
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
            let message_state = cursor.get_u8();
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
            let source_addr_ton = cursor.get_u8();
            let source_addr_npi = cursor.get_u8();
            let source_addr = decode_cstring(&mut cursor, 21, "source_addr")?;
            let dest_addr_ton = cursor.get_u8();
            let dest_addr_npi = cursor.get_u8();
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
