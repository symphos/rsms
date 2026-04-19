use std::fmt;

/// SMGP 10字节 MsgId 结构
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[derive(Default)]
pub struct SmgpMsgId {
    pub bytes: [u8; 10],
}

impl SmgpMsgId {
    pub fn new(bytes: [u8; 10]) -> Self {
        Self { bytes }
    }

    pub fn from_u64(value: u64) -> Self {
        let mut bytes = [0u8; 10];
        bytes[2..10].copy_from_slice(&value.to_be_bytes());
        Self { bytes }
    }

    pub fn to_u64(&self) -> u64 {
        u64::from_be_bytes([
            self.bytes[2],
            self.bytes[3],
            self.bytes[4],
            self.bytes[5],
            self.bytes[6],
            self.bytes[7],
            self.bytes[8],
            self.bytes[9],
        ])
    }

    pub fn to_bytes(&self) -> [u8; 10] {
        self.bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut arr = [0u8; 10];
        arr.copy_from_slice(&bytes[..10]);
        Self { bytes: arr }
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.iter().all(|&b| b == 0)
    }
}

impl fmt::Debug for SmgpMsgId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SmgpMsgId({})", hex::encode(&self.bytes))
    }
}

impl fmt::Display for SmgpMsgId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(&self.bytes))
    }
}


mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_id_from_bytes_roundtrip() {
        let original = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09];
        let msg_id = SmgpMsgId::from_bytes(&original);
        assert_eq!(msg_id.to_bytes(), original);
    }

    #[test]
    fn msg_id_from_u64_roundtrip() {
        let value: u64 = 0x123456789ABCDEF0;
        let msg_id = SmgpMsgId::from_u64(value);
        assert_eq!(msg_id.to_u64(), value);
    }

    #[test]
    fn msg_id_is_empty() {
        let empty = SmgpMsgId::default();
        assert!(empty.is_empty());
        let non_empty = SmgpMsgId::from_u64(1);
        assert!(!non_empty.is_empty());
    }
}
