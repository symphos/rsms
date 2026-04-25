//! SGIP 1.2 (Short Gateway Interface Protocol) codec implementation.
//!
//! SGIP is used by China Unicom for SMS transmission.

pub mod codec;
pub mod datatypes;
pub mod frame;
pub mod message;

pub use codec::{CodecError, Decodable, Encodable, Pdu, PduHeader, MAX_PDU_SIZE};
pub use datatypes::{
    Bind, BindResp, CommandId, CommandStatus, Deliver, DeliverResp, Report, ReportResp,
    SgipSequence, Submit, SubmitResp, Trace, TraceResp, Unbind, UnbindResp,
};
pub use frame::{decode_frames, FrameDecoder};
pub use message::{decode_message, SgipMessage};
