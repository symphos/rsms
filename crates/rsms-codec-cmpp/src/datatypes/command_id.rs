use num_enum::TryFromPrimitive;

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u32)]
pub enum CommandId {
    Connect = 0x00000001,
    ConnectResp = 0x80000001,
    Terminate = 0x00000002,
    TerminateResp = 0x80000002,
    Submit = 0x00000004,
    SubmitResp = 0x80000004,
    Deliver = 0x00000005,
    DeliverResp = 0x80000005,
    Query = 0x00000006,
    QueryResp = 0x80000006,
    Cancel = 0x00000007,
    CancelResp = 0x80000007,
    ActiveTest = 0x00000008,
    ActiveTestResp = 0x80000008,
}

impl CommandId {
    pub fn is_response(self) -> bool {
        matches!(
            self,
            CommandId::ConnectResp
                | CommandId::TerminateResp
                | CommandId::SubmitResp
                | CommandId::DeliverResp
                | CommandId::QueryResp
                | CommandId::CancelResp
                | CommandId::ActiveTestResp
        )
    }

    pub fn to_response(self) -> Self {
        match self {
            CommandId::Connect => CommandId::ConnectResp,
            CommandId::Terminate => CommandId::TerminateResp,
            CommandId::Submit => CommandId::SubmitResp,
            CommandId::Deliver => CommandId::DeliverResp,
            CommandId::Query => CommandId::QueryResp,
            CommandId::Cancel => CommandId::CancelResp,
            CommandId::ActiveTest => CommandId::ActiveTestResp,
            other => other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u32)]
#[allow(non_camel_case_types)]
pub enum CommandStatus {
    ESME_ROK = 0,
    ESME_RINVMSGLEN = 1,
    ESME_RINVCMDID = 2,
    ESME_RINVSYNTAX = 3,
    ESME_RSYSERR = 8,
}

impl CommandStatus {
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CmppVersion {
    V20 = 0x20,
    V30 = 0x30,
}

impl CmppVersion {
    pub fn from_wire(v: u8) -> Result<Self, &'static str> {
        match v {
            0x20 | 0x00 | 0x01 => Ok(CmppVersion::V20),
            0x30 => Ok(CmppVersion::V30),
            _ => Err("unknown CMPP version"),
        }
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn is_v20(&self) -> bool {
        matches!(self, CmppVersion::V20)
    }

    pub fn is_v30(&self) -> bool {
        matches!(self, CmppVersion::V30)
    }

    pub fn name(&self) -> &'static str {
        match self {
            CmppVersion::V20 => "CMPP 2.0",
            CmppVersion::V30 => "CMPP 3.0",
        }
    }

    pub fn is_compatible_with(self, other: CmppVersion) -> bool {
        self == other
    }

    pub fn negotiate(self, peer: CmppVersion) -> CmppVersion {
        if self.is_compatible_with(peer) {
            if self.to_u8() < peer.to_u8() {
                self
            } else {
                peer
            }
        } else {
            self
        }
    }
}
