use bytes::{Buf, BufMut, Bytes, BytesMut};
use rsms_core::RsmsError;
use std::io::Cursor;
use thiserror::Error;

use crate::datatypes::{
    Bind, BindResp, CommandId, CommandStatus, Deliver, DeliverResp, Report, ReportResp,
    SgipSequence, Submit, SubmitResp, Trace, TraceResp, Unbind, UnbindResp,
};

pub const MAX_PDU_SIZE: u32 = 65536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PduHeader {
    pub total_length: u32,
    pub command_id: CommandId,
    pub sequence: SgipSequence,
}

impl PduHeader {
    pub const SIZE: usize = 4 + 4 + 12;

    pub fn decode(buf: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        if buf.remaining() < Self::SIZE {
            return Err(CodecError::Incomplete);
        }
        let total_length = buf.get_u32();
        let command_id_raw = buf.get_u32();
        let command_id = CommandId::try_from(command_id_raw)
            .map_err(|_| CodecError::InvalidCommandId(command_id_raw))?;
        let sequence = SgipSequence::decode(buf).map_err(|_| CodecError::Incomplete)?;

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
            sequence,
        })
    }

    pub fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        buf.put_u32(self.total_length);
        buf.put_u32(self.command_id as u32);
        self.sequence.encode(buf);
        Ok(())
    }
}

pub trait Encodable: Decodable {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError>;
    fn encoded_size(&self) -> usize;
    fn to_pdu_bytes(&self, node_id: u32, timestamp: u32, number: u32) -> Bytes {
        let mut buf = BytesMut::with_capacity(PduHeader::SIZE + self.encoded_size());
        let sequence = SgipSequence::new(node_id, timestamp, number);
        let header = PduHeader {
            total_length: 0,
            command_id: Self::command_id(),
            sequence,
        };
        header
            .encode(&mut buf)
            .expect("header encoding should not fail");
        self.encode(&mut buf)
            .expect("body encoding should not fail");
        let total_len = buf.len() as u32;
        buf[0..4].copy_from_slice(&total_len.to_be_bytes());
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

#[derive(Debug, Clone, PartialEq)]
pub enum Pdu {
    Bind(Bind),
    BindResp(BindResp),
    Unbind(Unbind),
    UnbindResp(UnbindResp),
    Submit(Submit),
    SubmitResp(SubmitResp),
    Deliver(Deliver),
    DeliverResp(DeliverResp),
    Report(Report),
    ReportResp(ReportResp),
    Trace(Trace),
    TraceResp(TraceResp),
}

impl Pdu {
    pub fn command_id(&self) -> CommandId {
        match self {
            Pdu::Bind(_) => CommandId::Bind,
            Pdu::BindResp(_) => CommandId::BindResp,
            Pdu::Unbind(_) => CommandId::Unbind,
            Pdu::UnbindResp(_) => CommandId::UnbindResp,
            Pdu::Submit(_) => CommandId::Submit,
            Pdu::SubmitResp(_) => CommandId::SubmitResp,
            Pdu::Deliver(_) => CommandId::Deliver,
            Pdu::DeliverResp(_) => CommandId::DeliverResp,
            Pdu::Report(_) => CommandId::Report,
            Pdu::ReportResp(_) => CommandId::ReportResp,
            Pdu::Trace(_) => CommandId::Trace,
            Pdu::TraceResp(_) => CommandId::TraceResp,
        }
    }
}

impl From<Bind> for Pdu {
    fn from(p: Bind) -> Self {
        Pdu::Bind(p)
    }
}
impl From<BindResp> for Pdu {
    fn from(p: BindResp) -> Self {
        Pdu::BindResp(p)
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
impl From<Report> for Pdu {
    fn from(p: Report) -> Self {
        Pdu::Report(p)
    }
}
impl From<ReportResp> for Pdu {
    fn from(p: ReportResp) -> Self {
        Pdu::ReportResp(p)
    }
}
impl From<Trace> for Pdu {
    fn from(p: Trace) -> Self {
        Pdu::Trace(p)
    }
}
impl From<TraceResp> for Pdu {
    fn from(p: TraceResp) -> Self {
        Pdu::TraceResp(p)
    }
}
