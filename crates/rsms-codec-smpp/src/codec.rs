use bytes::{Buf, BufMut, Bytes, BytesMut};
use rsms_core::{CstringError, RawPdu, RsmsError};
use std::io::Cursor;
use thiserror::Error;

use crate::datatypes::{
    BindReceiver, BindReceiverResp, BindTransceiver, BindTransceiverResp, BindTransmitter,
    BindTransmitterResp, CancelSm, CancelSmResp, DeliverSm, DeliverSmResp, EnquireLink,
    EnquireLinkResp, GenericNack, QuerySm, QuerySmResp, SubmitSm, SubmitSmResp, Unbind, UnbindResp,
};
use crate::datatypes::{CommandId, CommandStatus};

pub const MAX_PDU_SIZE: u32 = 65536;

/// SMPP PDU 公共头（16 字节）：长度 + 命令字 + 状态码 + 序列号。
///
/// 注意 SMPP 头比 CMPP/SMGP 多 4 字节（`command_status`），`sequence_id` 偏移为 12。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PduHeader {
    /// PDU 总字节数（含头部 16 字节）。
    pub command_length: u32,
    /// 命令字，标识 PDU 类型（SubmitSm、DeliverSm 等）。
    pub command_id: CommandId,
    /// 命令状态码，请求 PDU 须为 0（`ESME_ROK`），响应 PDU 携带操作结果。
    pub command_status: CommandStatus,
    /// 请求/响应匹配用的序列号，0 和 0xFFFFFFFF 为保留值不可使用。
    pub sequence_number: u32,
}

impl PduHeader {
    pub const SIZE: usize = 16;

    pub fn decode(buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < Self::SIZE {
            return Err(CodecError::Incomplete);
        }
        let command_length = buf.try_get_u32().map_err(|_| CodecError::Incomplete)?;
        let command_id_raw = buf.try_get_u32().map_err(|_| CodecError::Incomplete)?;
        let command_id = CommandId::try_from(command_id_raw)
            .map_err(|_| CodecError::InvalidCommandId(command_id_raw))?;
        let command_status_raw = buf.try_get_u32().map_err(|_| CodecError::Incomplete)?;
        let command_status = CommandStatus::try_from(command_status_raw)
            .map_err(|_| CodecError::InvalidCommandStatus(command_status_raw))?;
        let sequence_number = buf.try_get_u32().map_err(|_| CodecError::Incomplete)?;

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

/// PDU body 编码能力，由各具体 PDU 类型实现。
pub trait Encodable {
    /// 将 body 字段写入 `buf`，不含协议头。
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError>;
    /// 返回 body 编码后的字节数（不含头部），用于预分配缓冲区。
    fn encoded_size(&self) -> usize;
    /// 将完整 PDU（含头部）序列化为 `Bytes`，适用于独立构造 PDU 的场景。
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

/// PDU body 解码能力，由各具体 PDU 类型实现。
pub trait Decodable: Sized {
    /// 从 `buf` 中解码 body（头部已由调用方解析为 `header`）。
    fn decode(header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError>;
    /// 返回该 PDU 类型对应的命令字。
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

impl From<CstringError> for CodecError {
    fn from(e: CstringError) -> Self {
        match e {
            CstringError::Incomplete => CodecError::Incomplete,
            CstringError::FieldTooLong { field, max_len } => CodecError::FieldValidation {
                field,
                reason: format!("exceeds maximum length of {} bytes", max_len - 1),
            },
        }
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
    rsms_core::decode_cstring(buf, max_len, field_name).map_err(Into::into)
}

pub fn encode_cstring(buf: &mut BytesMut, value: &str, max_len: usize) -> Result<(), CodecError> {
    rsms_core::encode_cstring(buf, value, max_len).map_err(Into::into)
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

    /// 响应 PDU 的头部 command_status（操作结果码，0=ESME_ROK）。
    /// 请求/无状态 PDU 恒为 0。供 `to_pdu_bytes` 写入头部。
    pub fn command_status(&self) -> u32 {
        match self {
            Pdu::SubmitSmResp(p) => p.command_status,
            Pdu::BindTransmitterResp(p) => p.command_status,
            Pdu::BindReceiverResp(p) => p.command_status,
            Pdu::BindTransceiverResp(p) => p.command_status,
            _ => 0,
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
        buf[8..12].copy_from_slice(&self.command_status().to_be_bytes()); // 响应 PDU 写回结果码,其余恒 0
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
