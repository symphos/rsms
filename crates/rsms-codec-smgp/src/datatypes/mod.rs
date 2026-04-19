mod active_test;
mod command_id;
mod deliver;
mod login;
mod msg_id;
mod submit;
pub mod tlv;

pub use active_test::{ActiveTest, ActiveTestResp, Exit, ExitResp};
pub use command_id::{CommandId, CommandStatus};
pub use deliver::{Deliver, DeliverResp, Query, QueryResp, SmgpReport};
pub use login::{Login, LoginResp};
pub use msg_id::SmgpMsgId;
pub use submit::{Submit, SubmitResp};
pub use tlv::{tlv_tags, OptionalParameters, Tlv};

use crate::PduRegistry;

pub fn register_all_pdus(registry: &mut PduRegistry) {
    registry.register::<Login>();
    registry.register::<LoginResp>();
    registry.register::<Submit>();
    registry.register::<SubmitResp>();
    registry.register::<Deliver>();
    registry.register::<DeliverResp>();
    registry.register::<Query>();
    registry.register::<QueryResp>();
    registry.register::<ActiveTest>();
    registry.register::<ActiveTestResp>();
    registry.register::<Exit>();
    registry.register::<ExitResp>();
}
