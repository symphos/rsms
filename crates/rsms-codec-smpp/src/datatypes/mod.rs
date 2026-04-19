mod address;
mod command_id;
mod command_status;
mod data_coding;
mod tlv;

mod bind_receiver;
mod bind_transceiver;
mod bind_transmitter;
mod cancel_sm;
mod deliver_sm;
mod enquire_link;
mod generic_nack;
mod query_sm;
mod submit_sm;
mod unbind;

pub use address::Address;
pub use command_id::CommandId;
pub use command_status::CommandStatus;
pub use data_coding::DataCoding;
pub use tlv::{tags, Tlv};

pub use bind_receiver::{BindReceiver, BindReceiverResp};
pub use bind_transceiver::{BindTransceiver, BindTransceiverResp};
pub use bind_transmitter::{BindTransmitter, BindTransmitterResp};
pub use cancel_sm::{CancelSm, CancelSmResp};
pub use deliver_sm::{DeliverSm, DeliverSmResp};
pub use enquire_link::{EnquireLink, EnquireLinkResp};
pub use generic_nack::GenericNack;
pub use query_sm::{QuerySm, QuerySmResp};
pub use submit_sm::{SubmitSm, SubmitSmResp};
pub use unbind::{Unbind, UnbindResp};

use crate::PduRegistry;

pub fn register_all_pdus(registry: &mut PduRegistry) {
    registry.register::<BindTransmitter>();
    registry.register::<BindTransmitterResp>();
    registry.register::<BindReceiver>();
    registry.register::<BindReceiverResp>();
    registry.register::<BindTransceiver>();
    registry.register::<BindTransceiverResp>();
    registry.register::<SubmitSm>();
    registry.register::<SubmitSmResp>();
    registry.register::<DeliverSm>();
    registry.register::<DeliverSmResp>();
    registry.register::<EnquireLink>();
    registry.register::<EnquireLinkResp>();
    registry.register::<Unbind>();
    registry.register::<UnbindResp>();
    registry.register::<QuerySm>();
    registry.register::<QuerySmResp>();
    registry.register::<CancelSm>();
    registry.register::<CancelSmResp>();
    registry.register::<GenericNack>();
}
