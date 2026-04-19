use crate::{EncodedPdu, RawPdu};

#[derive(Debug, Clone)]
pub struct Frame {
    pub command_id: u32,
    pub sequence_id: u32,
    pub data: RawPdu,
}

impl Frame {
    pub fn new(command_id: u32, sequence_id: u32, data: RawPdu) -> Self {
        Self {
            command_id,
            sequence_id,
            data,
        }
    }

    pub fn data_as_slice(&self) -> &[u8] {
        self.data.as_bytes()
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl EncodedPdu for Frame {
    fn as_bytes(&self) -> &[u8] {
        self.data.as_bytes()
    }

    fn sequence_id(&self) -> Option<u32> {
        Some(self.sequence_id)
    }

    fn command_id(&self) -> Option<u32> {
        Some(self.command_id)
    }
}

impl From<RawPdu> for Frame {
    fn from(pdu: RawPdu) -> Self {
        let slice = pdu.as_bytes();
        if slice.len() >= 12 {
            let command_id = u32::from_be_bytes([slice[4], slice[5], slice[6], slice[7]]);
            let sequence_id = u32::from_be_bytes([slice[8], slice[9], slice[10], slice[11]]);
            Self::new(command_id, sequence_id, pdu)
        } else {
            Self::new(0, 0, pdu)
        }
    }
}
