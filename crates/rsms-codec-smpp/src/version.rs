#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmppVersion {
    V34 = 0x34,
    V50 = 0x50,
}

impl SmppVersion {
    pub fn from_interface_version(v: u8) -> Option<Self> {
        match v {
            0x34 | 0x33 | 0x32 => Some(SmppVersion::V34),
            0x50 | 0x51 => Some(SmppVersion::V50),
            _ => None,
        }
    }

    pub fn source_addr_size(self) -> usize {
        match self {
            SmppVersion::V34 => 21,
            SmppVersion::V50 => 65,
        }
    }

    pub fn destination_addr_size(self) -> usize {
        match self {
            SmppVersion::V34 => 21,
            SmppVersion::V50 => 65,
        }
    }

    pub fn password_size(self) -> usize {
        match self {
            SmppVersion::V34 => 9,
            SmppVersion::V50 => 65,
        }
    }

    pub fn system_type_size(self) -> usize {
        match self {
            SmppVersion::V34 => 13,
            SmppVersion::V50 => 32,
        }
    }

    pub fn service_type_size(self) -> usize {
        match self {
            SmppVersion::V34 => 6,
            SmppVersion::V50 => 16,
        }
    }
}

pub const SMPP_V34_SOURCE_ADDR_SIZE: usize = 21;
pub const SMPP_V34_DEST_ADDR_SIZE: usize = 21;
pub const SMPP_V34_PASSWORD_SIZE: usize = 9;
pub const SMPP_V34_SYSTEM_TYPE_SIZE: usize = 13;
pub const SMPP_V34_SERVICE_TYPE_SIZE: usize = 6;

pub const SMPP_V50_SOURCE_ADDR_SIZE: usize = 65;
pub const SMPP_V50_DEST_ADDR_SIZE: usize = 65;
pub const SMPP_V50_PASSWORD_SIZE: usize = 65;
pub const SMPP_V50_SYSTEM_TYPE_SIZE: usize = 32;
pub const SMPP_V50_SERVICE_TYPE_SIZE: usize = 16;
