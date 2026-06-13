//! 协议适配器登记表：把四协议 adapter 集中到唯一一处 `match protocol`。
//! 新增协议 = 在此加一臂 + 写它的 adapter，编排层其余位置零改动。

use rsms_codec_cmpp::adapter::CmppAdapter;
use rsms_codec_sgip::adapter::SgipAdapter;
use rsms_codec_smgp::adapter::SmgpAdapter;
use rsms_codec_smpp::adapter::SmppAdapter;
use rsms_core::Protocol;
use rsms_model::ProtocolAdapter;

/// 取协议对应的 adapter（零大小 unit struct，`'static` 提升）。
pub fn adapter_for(protocol: Protocol) -> &'static dyn ProtocolAdapter {
    match protocol {
        Protocol::Cmpp => &CmppAdapter,
        Protocol::Smgp => &SmgpAdapter,
        Protocol::Smpp => &SmppAdapter,
        Protocol::Sgip => &SgipAdapter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_adapter_reports_its_protocol() {
        for p in [Protocol::Cmpp, Protocol::Smgp, Protocol::Smpp, Protocol::Sgip] {
            assert_eq!(adapter_for(p).protocol(), p, "adapter_for({p:?}).protocol() 应自洽");
        }
    }
}
