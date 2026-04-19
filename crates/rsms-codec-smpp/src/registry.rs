use std::collections::HashMap;
use std::io::Cursor;

use crate::codec::{CodecError, Decodable, Pdu, PduHeader};
use crate::datatypes::CommandId;

pub type DecoderFn =
    Box<dyn Fn(PduHeader, &mut Cursor<&[u8]>) -> Result<Pdu, CodecError> + Send + Sync>;

pub struct PduRegistry {
    decoders: HashMap<CommandId, DecoderFn>,
}

impl PduRegistry {
    pub fn new() -> Self {
        Self {
            decoders: HashMap::new(),
        }
    }

    pub fn register<T: Decodable + Into<Pdu> + 'static>(&mut self) {
        let cmd_id = T::command_id();
        let decoder: DecoderFn =
            Box::new(move |header, buf| T::decode(header, buf).map(|p| p.into()));
        self.decoders.insert(cmd_id, decoder);
    }

    pub fn decode(&self, header: PduHeader, buf: &mut Cursor<&[u8]>) -> Result<Pdu, CodecError> {
        let decoder = self
            .decoders
            .get(&header.command_id)
            .ok_or(CodecError::UnknownCommand(header.command_id))?;
        decoder(header, buf)
    }
}

impl Default for PduRegistry {
    fn default() -> Self {
        let mut registry = Self::new();
        crate::datatypes::register_all_pdus(&mut registry);
        registry
    }
}
