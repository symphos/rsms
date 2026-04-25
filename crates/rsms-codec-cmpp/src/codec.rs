use bytes::{Buf, BufMut, BytesMut};
use rsms_core::{CstringError, RawPdu, RsmsError};
use std::io::Cursor;
use thiserror::Error;

use crate::datatypes::{
    ActiveTest, ActiveTestResp, Cancel, CancelResp, CommandId, CommandStatus, Connect, ConnectResp,
    Deliver, DeliverResp, DeliverV20, Query, QueryResp, Submit, SubmitResp, SubmitV20, Terminate,
    TerminateResp,
};

pub const MAX_PDU_SIZE: u32 = 65536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PduHeader {
    pub total_length: u32,
    pub command_id: CommandId,
    pub sequence_id: u32,
}

impl PduHeader {
    pub const SIZE: usize = 12;

    pub fn decode(buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < Self::SIZE {
            return Err(CodecError::Incomplete);
        }
        let total_length = buf.get_u32();
        let command_id_raw = buf.get_u32();
        let command_id = CommandId::try_from(command_id_raw)
            .map_err(|_| CodecError::InvalidCommandId(command_id_raw))?;
        let sequence_id = buf.get_u32();

        if total_length < Self::SIZE as u32 {
            return Err(CodecError::InvalidPduLength {
                length: total_length,
                min: Self::SIZE as u32,
                max: MAX_PDU_SIZE,
            });
        }
        if total_length > MAX_PDU_SIZE {
            return Err(CodecError::InvalidPduLength {
                length: total_length,
                min: Self::SIZE as u32,
                max: MAX_PDU_SIZE,
            });
        }

        Ok(PduHeader {
            total_length,
            command_id,
            sequence_id,
        })
    }

    pub fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u32(self.total_length);
        buf.put_u32(self.command_id as u32);
        buf.put_u32(self.sequence_id);
        Ok(())
    }
}

pub trait Encodable {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError>;
    fn encoded_size(&self) -> usize;
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

    #[error("Invalid PDU length: {length}, must be {min}-{max}")]
    InvalidPduLength { length: u32, min: u32, max: u32 },

    #[error("Field '{field}' validation failed: {reason}")]
    FieldValidation { field: &'static str, reason: String },

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
            CodecError::FieldValidation { .. } => CommandStatus::ESME_RINVCMDID,
            CodecError::Incomplete => CommandStatus::ESME_RSYSERR,
            CodecError::Io(_) => CommandStatus::ESME_RSYSERR,
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
    Connect(Connect),
    ConnectResp(ConnectResp),
    Terminate(Terminate),
    TerminateResp(TerminateResp),
    Submit(Submit),
    SubmitResp(SubmitResp),
    Deliver(Deliver),
    DeliverResp(DeliverResp),
    SubmitV20(SubmitV20),
    DeliverV20(DeliverV20),
    Query(Query),
    QueryResp(QueryResp),
    Cancel(Cancel),
    CancelResp(CancelResp),
    ActiveTest(ActiveTest),
    ActiveTestResp(ActiveTestResp),
}

impl Pdu {
    pub fn command_id(&self) -> CommandId {
        match self {
            Pdu::Connect(_) => CommandId::Connect,
            Pdu::ConnectResp(_) => CommandId::ConnectResp,
            Pdu::Terminate(_) => CommandId::Terminate,
            Pdu::TerminateResp(_) => CommandId::TerminateResp,
            Pdu::Submit(_) => CommandId::Submit,
            Pdu::SubmitResp(_) => CommandId::SubmitResp,
            Pdu::Deliver(_) => CommandId::Deliver,
            Pdu::DeliverResp(_) => CommandId::DeliverResp,
            Pdu::SubmitV20(_) => CommandId::Submit,
            Pdu::DeliverV20(_) => CommandId::Deliver,
            Pdu::Query(_) => CommandId::Query,
            Pdu::QueryResp(_) => CommandId::QueryResp,
            Pdu::Cancel(_) => CommandId::Cancel,
            Pdu::CancelResp(_) => CommandId::CancelResp,
            Pdu::ActiveTest(_) => CommandId::ActiveTest,
            Pdu::ActiveTestResp(_) => CommandId::ActiveTestResp,
        }
    }

    pub fn to_pdu_bytes(&self, sequence_number: u32) -> RawPdu {
        let mut buf = BytesMut::new();

        buf.resize(12, 0);

        self.encode(&mut buf).expect("encoding should not fail");

        let total_len = buf.len() as u32;
        buf[0..4].copy_from_slice(&total_len.to_be_bytes());
        buf[4..8].copy_from_slice(&(self.command_id() as u32).to_be_bytes());
        buf[8..12].copy_from_slice(&sequence_number.to_be_bytes());

        RawPdu::new(buf.freeze())
    }
}

impl Encodable for Pdu {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        match self {
            Pdu::Connect(p) => p.encode(buf),
            Pdu::ConnectResp(p) => p.encode(buf),
            Pdu::Terminate(p) => p.encode(buf),
            Pdu::TerminateResp(p) => p.encode(buf),
            Pdu::Submit(p) => p.encode(buf),
            Pdu::SubmitResp(p) => p.encode(buf),
            Pdu::Deliver(p) => p.encode(buf),
            Pdu::DeliverResp(p) => p.encode(buf),
            Pdu::SubmitV20(p) => crate::datatypes::encode_pdu_submit_v20(buf, p),
            Pdu::DeliverV20(p) => crate::datatypes::encode_pdu_deliver_v20(buf, p),
            Pdu::Query(p) => p.encode(buf),
            Pdu::QueryResp(p) => p.encode(buf),
            Pdu::Cancel(p) => p.encode(buf),
            Pdu::CancelResp(p) => p.encode(buf),
            Pdu::ActiveTest(p) => p.encode(buf),
            Pdu::ActiveTestResp(p) => p.encode(buf),
        }
    }

    fn encoded_size(&self) -> usize {
        match self {
            Pdu::Connect(p) => p.encoded_size(),
            Pdu::ConnectResp(p) => p.encoded_size(),
            Pdu::Terminate(p) => p.encoded_size(),
            Pdu::TerminateResp(p) => p.encoded_size(),
            Pdu::Submit(p) => p.encoded_size(),
            Pdu::SubmitResp(p) => p.encoded_size(),
            Pdu::Deliver(p) => p.encoded_size(),
            Pdu::DeliverResp(p) => p.encoded_size(),
            Pdu::SubmitV20(p) => crate::datatypes::encoded_size_v20(p) + 8,
            Pdu::DeliverV20(p) => crate::datatypes::encoded_size_deliver_v20(p) + 8,
            Pdu::Query(p) => p.encoded_size(),
            Pdu::QueryResp(p) => p.encoded_size(),
            Pdu::Cancel(p) => p.encoded_size(),
            Pdu::CancelResp(p) => p.encoded_size(),
            Pdu::ActiveTest(p) => p.encoded_size(),
            Pdu::ActiveTestResp(p) => p.encoded_size(),
        }
    }
}

impl From<Connect> for Pdu {
    fn from(p: Connect) -> Self {
        Pdu::Connect(p)
    }
}
impl From<ConnectResp> for Pdu {
    fn from(p: ConnectResp) -> Self {
        Pdu::ConnectResp(p)
    }
}
impl From<Submit> for Pdu {
    fn from(p: Submit) -> Self {
        Pdu::Submit(p)
    }
}
impl From<SubmitResp> for Pdu {
    fn from(p: SubmitResp) -> Self {
        Pdu::SubmitResp(p)
    }
}
impl From<Deliver> for Pdu {
    fn from(p: Deliver) -> Self {
        Pdu::Deliver(p)
    }
}
impl From<DeliverResp> for Pdu {
    fn from(p: DeliverResp) -> Self {
        Pdu::DeliverResp(p)
    }
}
impl From<ActiveTest> for Pdu {
    fn from(p: ActiveTest) -> Self {
        Pdu::ActiveTest(p)
    }
}
impl From<ActiveTestResp> for Pdu {
    fn from(p: ActiveTestResp) -> Self {
        Pdu::ActiveTestResp(p)
    }
}
impl From<Terminate> for Pdu {
    fn from(p: Terminate) -> Self {
        Pdu::Terminate(p)
    }
}
impl From<TerminateResp> for Pdu {
    fn from(p: TerminateResp) -> Self {
        Pdu::TerminateResp(p)
    }
}
impl From<Query> for Pdu {
    fn from(p: Query) -> Self {
        Pdu::Query(p)
    }
}
impl From<QueryResp> for Pdu {
    fn from(p: QueryResp) -> Self {
        Pdu::QueryResp(p)
    }
}
impl From<Cancel> for Pdu {
    fn from(p: Cancel) -> Self {
        Pdu::Cancel(p)
    }
}
impl From<CancelResp> for Pdu {
    fn from(p: CancelResp) -> Self {
        Pdu::CancelResp(p)
    }
}

impl From<SubmitV20> for Pdu {
    fn from(p: SubmitV20) -> Self {
        Pdu::SubmitV20(p)
    }
}

impl From<DeliverV20> for Pdu {
    fn from(p: DeliverV20) -> Self {
        Pdu::DeliverV20(p)
    }
}
