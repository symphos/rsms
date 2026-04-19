use bytes::{Buf, BufMut, Bytes, BytesMut};
use rsms_core::{RawPdu, RsmsError};
use std::io::Cursor;
use thiserror::Error;

use crate::datatypes::{
    BindReceiver, BindReceiverResp, BindTransceiver, BindTransceiverResp, BindTransmitter,
    BindTransmitterResp, CancelSm, CancelSmResp, DeliverSm, DeliverSmResp, EnquireLink,
    EnquireLinkResp, GenericNack, QuerySm, QuerySmResp, SubmitSm, SubmitSmResp, Unbind, UnbindResp,
};
use crate::datatypes::{CommandId, CommandStatus};

pub const MAX_PDU_SIZE: u32 = 65536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PduHeader {
    pub command_length: u32,
    pub command_id: CommandId,
    pub command_status: CommandStatus,
    pub sequence_number: u32,
}

impl PduHeader {
    pub const SIZE: usize = 16;

    pub fn decode(buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < Self::SIZE {
            return Err(CodecError::Incomplete);
        }
        let command_length = buf.get_u32();
        let command_id_raw = buf.get_u32();
        let command_id = CommandId::try_from(command_id_raw)
            .map_err(|_| CodecError::InvalidCommandId(command_id_raw))?;
        let command_status_raw = buf.get_u32();
        let command_status = CommandStatus::try_from(command_status_raw)
            .map_err(|_| CodecError::InvalidCommandStatus(command_status_raw))?;
        let sequence_number = buf.get_u32();

        if command_length < Self::SIZE as u32 {
            return Err(CodecError::InvalidPduLength {
                length: command_length,
                min: Self::SIZE as u32,
                max: MAX_PDU_SIZE,
            });
        }
        if command_length > MAX_PDU_SIZE {
            return Err(CodecError::InvalidPduLength {
                length: command_length,
                min: Self::SIZE as u32,
                max: MAX_PDU_SIZE,
            });
        }
        if !command_id.is_response() && command_status != CommandStatus::ESME_ROK {
            return Err(CodecError::InvalidRequestStatus {
                command_id,
                command_status,
            });
        }
        if sequence_number == 0 || sequence_number == 0xFFFFFFFF {
            return Err(CodecError::ReservedSequenceNumber(sequence_number));
        }

        Ok(PduHeader {
            command_length,
            command_id,
            command_status,
            sequence_number,
        })
    }

    pub fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u32(self.command_length);
        buf.put_u32(self.command_id as u32);
        buf.put_u32(self.command_status as u32);
        buf.put_u32(self.sequence_number);
        Ok(())
    }
}

pub trait Encodable {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError>;
    fn encoded_size(&self) -> usize;
    fn to_bytes(&self) -> Bytes {
        let mut buf = BytesMut::new();
        self.encode(&mut buf).expect("encoding should not fail");
        if buf.len() >= 4 {
            let length = buf.len() as u32;
            buf[0..4].copy_from_slice(&length.to_be_bytes());
        }
        buf.freeze()
    }
}

pub trait Decodable: Sized {
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError>;
    fn command_id() -> CommandId;
}

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("Incomplete PDU: need more data")]
    Incomplete,

    #[error("Invalid command_id: {0:#x}")]
    InvalidCommandId(u32),

    #[error("Invalid command_status: {0:#x}")]
    InvalidCommandStatus(u32),

    #[error("Invalid PDU length: {length}, must be {min}-{max}")]
    InvalidPduLength { length: u32, min: u32, max: u32 },

    #[error("Request PDU {command_id:?} has non-zero status: {command_status:?}")]
    InvalidRequestStatus {
        command_id: CommandId,
        command_status: CommandStatus,
    },

    #[error("Reserved sequence number: {0} (0 and 0xFFFFFFFF are reserved)")]
    ReservedSequenceNumber(u32),

    #[error("Unknown command ID: {0:?}")]
    UnknownCommand(CommandId),

    #[error("Field '{field}' validation failed: {reason}")]
    FieldValidation { field: &'static str, reason: String },

    #[error("TLV parsing error: {0}")]
    TlvError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<CodecError> for RsmsError {
    fn from(e: CodecError) -> Self {
        RsmsError::Codec(e.to_string())
    }
}

impl CodecError {
    pub fn to_command_status(&self) -> CommandStatus {
        match self {
            CodecError::InvalidPduLength { .. } => CommandStatus::ESME_RINVMSGLEN,
            CodecError::InvalidCommandId(_) => CommandStatus::ESME_RINVCMDID,
            CodecError::InvalidCommandStatus(_) => CommandStatus::ESME_RSYSERR,
            CodecError::InvalidRequestStatus { .. } => CommandStatus::ESME_RSYSERR,
            CodecError::ReservedSequenceNumber(_) => CommandStatus::ESME_RSYSERR,
            CodecError::UnknownCommand(_) => CommandStatus::ESME_RINVCMDID,
            CodecError::FieldValidation { field, reason } => {
                if field.contains("addr") || field.contains("destination") {
                    CommandStatus::ESME_RINVDSTADR
                } else if reason.contains("too long") {
                    CommandStatus::ESME_RINVMSGLEN
                } else {
                    CommandStatus::ESME_RSYSERR
                }
            }
            CodecError::TlvError(_) => CommandStatus::ESME_RSYSERR,
            _ => CommandStatus::ESME_RSYSERR,
        }
    }
}

pub fn decode_cstring(
    buf: &mut Cursor<&[u8]>,
    max_len: usize,
    field_name: &'static str,
) -> Result<String, CodecError> {
    let mut string_bytes = Vec::new();
    let mut bytes_read = 0;

    while bytes_read < max_len {
        if !buf.has_remaining() {
            return Err(CodecError::Incomplete);
        }
        let byte = buf.get_u8();
        bytes_read += 1;
        if byte == 0 {
            return Ok(String::from_utf8_lossy(&string_bytes).into_owned());
        }
        string_bytes.push(byte);
    }

    Err(CodecError::FieldValidation {
        field: field_name,
        reason: format!("exceeds maximum length of {} bytes", max_len - 1),
    })
}

pub fn encode_cstring(buf: &mut BytesMut, value: &str, max_len: usize) -> Result<(), CodecError> {
    let bytes = value.as_bytes();
    if bytes.len() >= max_len {
        return Err(CodecError::FieldValidation {
            field: "cstring",
            reason: format!("string length {} exceeds max {}", bytes.len(), max_len - 1),
        });
    }
    buf.put(bytes);
    buf.put_u8(0);
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pdu {
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
    Unknown,
}

impl Pdu {
    pub fn command_id(&self) -> CommandId {
        match self {
            Pdu::BindTransmitter(_) => CommandId::BIND_TRANSMITTER,
            Pdu::BindTransmitterResp(_) => CommandId::BIND_TRANSMITTER_RESP,
            Pdu::BindReceiver(_) => CommandId::BIND_RECEIVER,
            Pdu::BindReceiverResp(_) => CommandId::BIND_RECEIVER_RESP,
            Pdu::BindTransceiver(_) => CommandId::BIND_TRANSCEIVER,
            Pdu::BindTransceiverResp(_) => CommandId::BIND_TRANSCEIVER_RESP,
            Pdu::SubmitSm(_) => CommandId::SUBMIT_SM,
            Pdu::SubmitSmResp(_) => CommandId::SUBMIT_SM_RESP,
            Pdu::DeliverSm(_) => CommandId::DELIVER_SM,
            Pdu::DeliverSmResp(_) => CommandId::DELIVER_SM_RESP,
            Pdu::EnquireLink(_) => CommandId::ENQUIRE_LINK,
            Pdu::EnquireLinkResp(_) => CommandId::ENQUIRE_LINK_RESP,
            Pdu::Unbind(_) => CommandId::UNBIND,
            Pdu::UnbindResp(_) => CommandId::UNBIND_RESP,
            Pdu::QuerySm(_) => CommandId::QUERY_SM,
            Pdu::QuerySmResp(_) => CommandId::QUERY_SM_RESP,
            Pdu::CancelSm(_) => CommandId::CANCEL_SM,
            Pdu::CancelSmResp(_) => CommandId::CANCEL_SM_RESP,
            Pdu::GenericNack(_) => CommandId::GENERIC_NACK,
            Pdu::Unknown => CommandId::GENERIC_NACK,
        }
    }
}

impl Encodable for Pdu {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        match self {
            Pdu::BindTransmitter(p) => p.encode(buf),
            Pdu::BindTransmitterResp(p) => p.encode(buf),
            Pdu::BindReceiver(p) => p.encode(buf),
            Pdu::BindReceiverResp(p) => p.encode(buf),
            Pdu::BindTransceiver(p) => p.encode(buf),
            Pdu::BindTransceiverResp(p) => p.encode(buf),
            Pdu::SubmitSm(p) => p.encode(buf),
            Pdu::SubmitSmResp(p) => p.encode(buf),
            Pdu::DeliverSm(p) => p.encode(buf),
            Pdu::DeliverSmResp(p) => p.encode(buf),
            Pdu::EnquireLink(p) => p.encode(buf),
            Pdu::EnquireLinkResp(p) => p.encode(buf),
            Pdu::Unbind(p) => p.encode(buf),
            Pdu::UnbindResp(p) => p.encode(buf),
            Pdu::QuerySm(p) => p.encode(buf),
            Pdu::QuerySmResp(p) => p.encode(buf),
            Pdu::CancelSm(p) => p.encode(buf),
            Pdu::CancelSmResp(p) => p.encode(buf),
            Pdu::GenericNack(p) => p.encode(buf),
            Pdu::Unknown => Ok(()),
        }
    }

    fn encoded_size(&self) -> usize {
        match self {
            Pdu::BindTransmitter(p) => p.encoded_size(),
            Pdu::BindTransmitterResp(p) => p.encoded_size(),
            Pdu::BindReceiver(p) => p.encoded_size(),
            Pdu::BindReceiverResp(p) => p.encoded_size(),
            Pdu::BindTransceiver(p) => p.encoded_size(),
            Pdu::BindTransceiverResp(p) => p.encoded_size(),
            Pdu::SubmitSm(p) => p.encoded_size(),
            Pdu::SubmitSmResp(p) => p.encoded_size(),
            Pdu::DeliverSm(p) => p.encoded_size(),
            Pdu::DeliverSmResp(p) => p.encoded_size(),
            Pdu::EnquireLink(p) => p.encoded_size(),
            Pdu::EnquireLinkResp(p) => p.encoded_size(),
            Pdu::Unbind(p) => p.encoded_size(),
            Pdu::UnbindResp(p) => p.encoded_size(),
            Pdu::QuerySm(p) => p.encoded_size(),
            Pdu::QuerySmResp(p) => p.encoded_size(),
            Pdu::CancelSm(p) => p.encoded_size(),
            Pdu::CancelSmResp(p) => p.encoded_size(),
            Pdu::GenericNack(p) => p.encoded_size(),
            Pdu::Unknown => 0,
        }
    }
}

impl Pdu {
    pub fn to_pdu_bytes(&self, sequence_number: u32) -> RawPdu {
        let mut buf = BytesMut::new();

        // Reserve space for header (will fill length later)
        buf.resize(16, 0);

        // Encode body
        self.encode(&mut buf).expect("encoding should not fail");

        // Fill header
        let total_len = buf.len() as u32;
        buf[0..4].copy_from_slice(&total_len.to_be_bytes());
        buf[4..8].copy_from_slice(&(self.command_id() as u32).to_be_bytes());
        buf[8..12].copy_from_slice(&0u32.to_be_bytes()); // command_status = 0
        buf[12..16].copy_from_slice(&sequence_number.to_be_bytes());

        RawPdu::new(buf.freeze())
    }
}

impl From<BindTransmitter> for Pdu {
    fn from(p: BindTransmitter) -> Self {
        Pdu::BindTransmitter(p)
    }
}
impl From<BindTransmitterResp> for Pdu {
    fn from(p: BindTransmitterResp) -> Self {
        Pdu::BindTransmitterResp(p)
    }
}
impl From<BindReceiver> for Pdu {
    fn from(p: BindReceiver) -> Self {
        Pdu::BindReceiver(p)
    }
}
impl From<BindReceiverResp> for Pdu {
    fn from(p: BindReceiverResp) -> Self {
        Pdu::BindReceiverResp(p)
    }
}
impl From<BindTransceiver> for Pdu {
    fn from(p: BindTransceiver) -> Self {
        Pdu::BindTransceiver(p)
    }
}
impl From<BindTransceiverResp> for Pdu {
    fn from(p: BindTransceiverResp) -> Self {
        Pdu::BindTransceiverResp(p)
    }
}
impl From<SubmitSm> for Pdu {
    fn from(p: SubmitSm) -> Self {
        Pdu::SubmitSm(p)
    }
}
impl From<SubmitSmResp> for Pdu {
    fn from(p: SubmitSmResp) -> Self {
        Pdu::SubmitSmResp(p)
    }
}
impl From<DeliverSm> for Pdu {
    fn from(p: DeliverSm) -> Self {
        Pdu::DeliverSm(p)
    }
}
impl From<DeliverSmResp> for Pdu {
    fn from(p: DeliverSmResp) -> Self {
        Pdu::DeliverSmResp(p)
    }
}
impl From<EnquireLink> for Pdu {
    fn from(p: EnquireLink) -> Self {
        Pdu::EnquireLink(p)
    }
}
impl From<EnquireLinkResp> for Pdu {
    fn from(p: EnquireLinkResp) -> Self {
        Pdu::EnquireLinkResp(p)
    }
}
impl From<Unbind> for Pdu {
    fn from(p: Unbind) -> Self {
        Pdu::Unbind(p)
    }
}
impl From<UnbindResp> for Pdu {
    fn from(p: UnbindResp) -> Self {
        Pdu::UnbindResp(p)
    }
}
impl From<QuerySm> for Pdu {
    fn from(p: QuerySm) -> Self {
        Pdu::QuerySm(p)
    }
}
impl From<QuerySmResp> for Pdu {
    fn from(p: QuerySmResp) -> Self {
        Pdu::QuerySmResp(p)
    }
}
impl From<CancelSm> for Pdu {
    fn from(p: CancelSm) -> Self {
        Pdu::CancelSm(p)
    }
}
impl From<CancelSmResp> for Pdu {
    fn from(p: CancelSmResp) -> Self {
        Pdu::CancelSmResp(p)
    }
}
impl From<GenericNack> for Pdu {
    fn from(p: GenericNack) -> Self {
        Pdu::GenericNack(p)
    }
}
