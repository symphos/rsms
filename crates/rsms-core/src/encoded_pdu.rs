use bytes::Bytes;

pub trait EncodedPdu: Send + Sync {
    fn as_bytes(&self) -> &[u8];
    fn sequence_id(&self) -> Option<u32>;
    fn command_id(&self) -> Option<u32>;
}

impl EncodedPdu for std::sync::Arc<dyn EncodedPdu> {
    fn as_bytes(&self) -> &[u8] {
        (**self).as_bytes()
    }

    fn sequence_id(&self) -> Option<u32> {
        (**self).sequence_id()
    }

    fn command_id(&self) -> Option<u32> {
        (**self).command_id()
    }
}

#[derive(Debug, Clone)]
pub struct RawPdu {
    data: Bytes,
}

impl RawPdu {
    pub fn new(data: Bytes) -> Self {
        Self { data }
    }

    pub fn from_vec(data: Vec<u8>) -> Self {
        Self { data: data.into() }
    }

    pub fn as_bytes_ref(&self) -> &Bytes {
        &self.data
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.data.to_vec()
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }
}

impl EncodedPdu for RawPdu {
    fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    fn sequence_id(&self) -> Option<u32> {
        if self.data.len() >= 12 {
            Some(u32::from_be_bytes([
                self.data[8],
                self.data[9],
                self.data[10],
                self.data[11],
            ]))
        } else {
            None
        }
    }

    fn command_id(&self) -> Option<u32> {
        if self.data.len() >= 8 {
            Some(u32::from_be_bytes([
                self.data[4],
                self.data[5],
                self.data[6],
                self.data[7],
            ]))
        } else {
            None
        }
    }
}

impl From<Bytes> for RawPdu {
    fn from(data: Bytes) -> Self {
        Self::new(data)
    }
}

impl From<Vec<u8>> for RawPdu {
    fn from(data: Vec<u8>) -> Self {
        Self::from_vec(data)
    }
}
