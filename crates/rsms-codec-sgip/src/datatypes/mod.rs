mod bind;
mod command_id;
mod deliver;
mod sgip_sequence;
mod submit;
mod unbind;

pub use bind::{Bind, BindResp};
pub use command_id::{CommandId, CommandStatus};
pub use deliver::{Deliver, DeliverResp, Report, ReportResp, Trace, TraceResp};
pub use sgip_sequence::SgipSequence;
pub use submit::{Submit, SubmitResp};
pub use unbind::{Unbind, UnbindResp};

use crate::PduRegistry;

pub fn register_all_pdus(registry: &mut PduRegistry) {
    registry.register::<Bind>();
    registry.register::<BindResp>();
    registry.register::<Unbind>();
    registry.register::<UnbindResp>();
    registry.register::<Submit>();
    registry.register::<SubmitResp>();
    registry.register::<Deliver>();
    registry.register::<DeliverResp>();
    registry.register::<Report>();
    registry.register::<ReportResp>();
    registry.register::<Trace>();
    registry.register::<TraceResp>();
}
