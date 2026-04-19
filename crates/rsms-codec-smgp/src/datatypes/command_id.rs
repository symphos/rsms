use num_enum::TryFromPrimitive;

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u32)]
pub enum CommandId {
    Login = 0x00000001,
    LoginResp = 0x80000001,
    Submit = 0x00000002,
    SubmitResp = 0x80000002,
    Deliver = 0x00000003,
    DeliverResp = 0x80000003,
    ActiveTest = 0x00000004,
    ActiveTestResp = 0x80000004,
    Exit = 0x00000006,
    ExitResp = 0x80000006,
    Query = 0x00000008,
    QueryResp = 0x80000008,
}

impl CommandId {
    pub fn is_response(self) -> bool {
        matches!(
            self,
            CommandId::LoginResp
                | CommandId::SubmitResp
                | CommandId::DeliverResp
                | CommandId::ActiveTestResp
                | CommandId::ExitResp
                | CommandId::QueryResp
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u32)]
#[allow(non_camel_case_types)]
pub enum CommandStatus {
    ESME_ROK = 0,
    ESME_RINVMSGLEN = 1,
    ESME_RINVCMDID = 2,
    ESME_RSYSERR = 8,
}

impl CommandStatus {
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}
