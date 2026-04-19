use bytes::{Buf, BufMut, BytesMut};
use rsms_core::{RawPdu, RsmsError};
use std::io::Cursor;
use thiserror::Error;

use crate::datatypes::{
    ActiveTest, ActiveTestResp, CommandId, CommandStatus, Deliver, DeliverResp, Exit, ExitResp,
    Login, LoginResp, Query, QueryResp, Submit, SubmitResp,
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
    Login(Login),
    LoginResp(LoginResp),
    Submit(Submit),
    SubmitResp(SubmitResp),
    Deliver(Deliver),
    DeliverResp(DeliverResp),
    Query(Query),
    QueryResp(QueryResp),
    ActiveTest(ActiveTest),
    ActiveTestResp(ActiveTestResp),
    Exit(Exit),
    ExitResp(ExitResp),
}

impl Pdu {
    pub fn command_id(&self) -> CommandId {
        match self {
            Pdu::Login(_) => CommandId::Login,
            Pdu::LoginResp(_) => CommandId::LoginResp,
            Pdu::Submit(_) => CommandId::Submit,
            Pdu::SubmitResp(_) => CommandId::SubmitResp,
            Pdu::Deliver(_) => CommandId::Deliver,
            Pdu::DeliverResp(_) => CommandId::DeliverResp,
            Pdu::Query(_) => CommandId::Query,
            Pdu::QueryResp(_) => CommandId::QueryResp,
            Pdu::ActiveTest(_) => CommandId::ActiveTest,
            Pdu::ActiveTestResp(_) => CommandId::ActiveTestResp,
            Pdu::Exit(_) => CommandId::Exit,
            Pdu::ExitResp(_) => CommandId::ExitResp,
        }
    }

    pub fn to_pdu_bytes(&self, sequence_id: u32) -> RawPdu {
        let mut buf = BytesMut::new();

        buf.resize(12, 0);

        match self {
            Pdu::Login(p) => p.encode(&mut buf).expect("encoding should not fail"),
            Pdu::LoginResp(p) => p.encode(&mut buf).expect("encoding should not fail"),
            Pdu::Submit(p) => p.encode(&mut buf).expect("encoding should not fail"),
            Pdu::SubmitResp(p) => p.encode(&mut buf).expect("encoding should not fail"),
            Pdu::Deliver(p) => p.encode(&mut buf).expect("encoding should not fail"),
            Pdu::DeliverResp(p) => p.encode(&mut buf).expect("encoding should not fail"),
            Pdu::Query(p) => p.encode(&mut buf).expect("encoding should not fail"),
            Pdu::QueryResp(p) => p.encode(&mut buf).expect("encoding should not fail"),
            Pdu::ActiveTest(p) => p.encode(&mut buf).expect("encoding should not fail"),
            Pdu::ActiveTestResp(p) => p.encode(&mut buf).expect("encoding should not fail"),
            Pdu::Exit(p) => p.encode(&mut buf).expect("encoding should not fail"),
            Pdu::ExitResp(p) => p.encode(&mut buf).expect("encoding should not fail"),
        };

        let total_len = buf.len() as u32;
        buf[0..4].copy_from_slice(&total_len.to_be_bytes());
        buf[4..8].copy_from_slice(&(self.command_id() as u32).to_be_bytes());
        buf[8..12].copy_from_slice(&sequence_id.to_be_bytes());

        RawPdu::new(buf.freeze())
    }
}

impl From<Login> for Pdu {
    fn from(p: Login) -> Self {
        Pdu::Login(p)
    }
}
impl From<LoginResp> for Pdu {
    fn from(p: LoginResp) -> Self {
        Pdu::LoginResp(p)
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
impl From<Exit> for Pdu {
    fn from(p: Exit) -> Self {
        Pdu::Exit(p)
    }
}
impl From<ExitResp> for Pdu {
    fn from(p: ExitResp) -> Self {
        Pdu::ExitResp(p)
    }
}
impl From<SubmitResp> for Pdu {
    fn from(p: SubmitResp) -> Self {
        Pdu::SubmitResp(p)
    }
}
impl From<DeliverResp> for Pdu {
    fn from(p: DeliverResp) -> Self {
        Pdu::DeliverResp(p)
    }
}
impl From<Submit> for Pdu {
    fn from(p: Submit) -> Self {
        Pdu::Submit(p)
    }
}
impl From<Deliver> for Pdu {
    fn from(p: Deliver) -> Self {
        Pdu::Deliver(p)
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

impl Encodable for Pdu {
    fn encode(&self, buf: &mut BytesMut) -> Result<(), CodecError> {
        match self {
            Pdu::Login(p) => p.encode(buf),
            Pdu::LoginResp(p) => p.encode(buf),
            Pdu::Submit(p) => p.encode(buf),
            Pdu::SubmitResp(p) => p.encode(buf),
            Pdu::Deliver(p) => p.encode(buf),
            Pdu::DeliverResp(p) => p.encode(buf),
            Pdu::Query(p) => p.encode(buf),
            Pdu::QueryResp(p) => p.encode(buf),
            Pdu::ActiveTest(p) => p.encode(buf),
            Pdu::ActiveTestResp(p) => p.encode(buf),
            Pdu::Exit(p) => p.encode(buf),
            Pdu::ExitResp(p) => p.encode(buf),
        }
    }

    fn encoded_size(&self) -> usize {
        match self {
            Pdu::Login(p) => p.encoded_size(),
            Pdu::LoginResp(p) => p.encoded_size(),
            Pdu::Submit(p) => p.encoded_size(),
            Pdu::SubmitResp(p) => p.encoded_size(),
            Pdu::Deliver(p) => p.encoded_size(),
            Pdu::DeliverResp(p) => p.encoded_size(),
            Pdu::Query(p) => p.encoded_size(),
            Pdu::QueryResp(p) => p.encoded_size(),
            Pdu::ActiveTest(p) => p.encoded_size(),
            Pdu::ActiveTestResp(p) => p.encoded_size(),
            Pdu::Exit(p) => p.encoded_size(),
            Pdu::ExitResp(p) => p.encoded_size(),
        }
    }
}
