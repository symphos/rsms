use std::collections::HashMap;
use std::sync::OnceLock;

use crate::codec::{CodecError, Decodable, Pdu, PduHeader};
use crate::datatypes::{
    ActiveTest, ActiveTestResp, Cancel, CancelResp, CmppVersion, Connect, ConnectResp, Deliver,
    DeliverResp, DeliverV20, Query, QueryResp, Submit, SubmitResp, SubmitV20, Terminate,
    TerminateResp,
};

pub struct PduRegistry {
    version: CmppVersion,
    entries: HashMap<u32, fn(PduHeader, &[u8]) -> Result<Pdu, CodecError>>,
}

static REGISTRY_CACHE: [OnceLock<PduRegistry>; 2] = [OnceLock::new(), OnceLock::new()];

impl PduRegistry {
    pub fn new(version: CmppVersion) -> Self {
        let mut registry = Self {
            version,
            entries: HashMap::new(),
        };
        registry.register_all_pdus();
        registry
    }

    pub fn for_version(version: CmppVersion) -> &'static Self {
        match version {
            CmppVersion::V20 => REGISTRY_CACHE[0].get_or_init(|| Self::new(version)),
            CmppVersion::V30 => REGISTRY_CACHE[1].get_or_init(|| Self::new(version)),
        }
    }

    pub fn register<F>(&mut self)
    where
        F: Decodable + Into<Pdu>,
    {
        let cmd_id = F::command_id() as u32;
        self.entries.insert(cmd_id, |header, buf| {
            let pdu = F::decode(header, &mut std::io::Cursor::new(buf))?;
            Ok(pdu.into())
        });
    }

    pub fn dispatch(&self, header: PduHeader, buf: &[u8]) -> Result<Pdu, CodecError> {
        let cmd_id = header.command_id as u32;
        let constructor = self
            .entries
            .get(&cmd_id)
            .ok_or(CodecError::InvalidCommandId(cmd_id))?;
        constructor(header, buf)
    }

    fn register_all_pdus(&mut self) {
        self.register::<Connect>();
        self.register::<ConnectResp>();
        self.register::<Terminate>();
        self.register::<TerminateResp>();
        self.register::<SubmitResp>();
        self.register::<DeliverResp>();
        self.register::<Query>();
        self.register::<QueryResp>();
        self.register::<Cancel>();
        self.register::<CancelResp>();
        self.register::<ActiveTest>();
        self.register::<ActiveTestResp>();

        match self.version {
            CmppVersion::V20 => {
                self.register::<SubmitV20>();
                self.register::<DeliverV20>();
            }
            CmppVersion::V30 => {
                self.register::<Submit>();
                self.register::<Deliver>();
            }
        }
    }
}

impl Default for PduRegistry {
    fn default() -> Self {
        Self::new(CmppVersion::V30)
    }
}
