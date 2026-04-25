//! SMGP (Short Message Gateway Protocol) codec implementation.
//!
//! SMGP is used by China Telecom for SMS transmission.

pub mod auth;
pub mod codec;
pub mod datatypes;
pub mod frame;
pub mod message;

pub use auth::{compute_login_auth, compute_server_auth, verify_server_auth};
pub use codec::{CodecError, Decodable, Encodable, Pdu, PduHeader, MAX_PDU_SIZE};
pub use datatypes::{
    ActiveTest, ActiveTestResp, CommandId, CommandStatus, Deliver, DeliverResp, Exit, ExitResp,
    Login, LoginResp, Query, QueryResp, SmgpMsgId, Submit, SubmitResp,
};
pub use frame::{decode_frames, FrameDecoder};
pub use message::{decode_message, SmgpMessage};
