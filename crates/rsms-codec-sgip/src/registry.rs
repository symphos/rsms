use crate::codec::{CodecError, Decodable, Pdu, PduHeader};
use crate::datatypes::{
    Bind, BindResp, Deliver, DeliverResp, Report, ReportResp, Submit, SubmitResp, Trace, TraceResp,
    Unbind, UnbindResp,
};

pub struct PduRegistry {
    entries: std::collections::HashMap<u32, fn(PduHeader, &[u8]) -> Result<Pdu, CodecError>>,
}

impl PduRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            entries: std::collections::HashMap::new(),
        };
        register_all_pdus(&mut registry);
        registry
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
}

impl Default for PduRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register_all_pdus(registry: &mut PduRegistry) {
    registry.register::<Bind>();
    registry.register::<BindResp>();
    registry.register::<Submit>();
    registry.register::<SubmitResp>();
    registry.register::<Deliver>();
    registry.register::<DeliverResp>();
    registry.register::<Report>();
    registry.register::<ReportResp>();
    registry.register::<Unbind>();
    registry.register::<UnbindResp>();
    registry.register::<Trace>();
    registry.register::<TraceResp>();
}
