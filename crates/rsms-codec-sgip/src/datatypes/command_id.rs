use num_enum::TryFromPrimitive;

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u32)]
pub enum CommandId {
    Bind = 0x00000001,
    BindResp = 0x80000001,
    Unbind = 0x00000002,
    UnbindResp = 0x80000002,
    Submit = 0x00000003,
    SubmitResp = 0x80000003,
    Deliver = 0x00000004,
    DeliverResp = 0x80000004,
    Report = 0x00000005,
    ReportResp = 0x80000005,
    Trace = 0x00001000,
    TraceResp = 0x80001000,
}

impl CommandId {
    pub fn is_response(self) -> bool {
        matches!(
            self,
            CommandId::BindResp
                | CommandId::UnbindResp
                | CommandId::SubmitResp
                | CommandId::DeliverResp
                | CommandId::ReportResp
                | CommandId::TraceResp
        )
    }

    pub fn to_response(self) -> Self {
        match self {
            CommandId::Bind => CommandId::BindResp,
            CommandId::Unbind => CommandId::UnbindResp,
            CommandId::Submit => CommandId::SubmitResp,
            CommandId::Deliver => CommandId::DeliverResp,
            CommandId::Report => CommandId::ReportResp,
            CommandId::Trace => CommandId::TraceResp,
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
