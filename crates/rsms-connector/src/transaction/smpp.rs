use rsms_codec_smpp::SubmitSm;

#[derive(Debug, Clone)]
pub struct SmppSubmit {
    pub inner: SubmitSm,
}

impl SmppSubmit {
    pub fn new(inner: SubmitSm) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &SubmitSm {
        &self.inner
    }

    pub fn into_inner(self) -> SubmitSm {
        self.inner
    }

    pub fn msg_id(&self) -> String {
        // SMPP SubmitSm doesn't have msg_id - msg_id is in SubmitSmResp
        format!(
            "{:032x}",
            u64::from_be_bytes([
                self.inner
                    .source_addr
                    .as_bytes()
                    .first()
                    .copied()
                    .unwrap_or(b'0') as u8,
                self.inner
                    .destination_addr
                    .as_bytes()
                    .first()
                    .copied()
                    .unwrap_or(b'0') as u8,
                self.inner.short_message.first().copied().unwrap_or(b'0') as u8,
                self.inner.short_message.len().min(5) as u8,
                0,
                0,
                0,
                0
            ])
        )
    }

    pub fn dest_id(&self) -> String {
        self.inner.destination_addr.clone()
    }

    pub fn src_id(&self) -> String {
        self.inner.source_addr.clone()
    }

    pub fn content(&self) -> Vec<u8> {
        self.inner.short_message.clone()
    }

    pub fn protocol_name(&self) -> &'static str {
        "SMPP"
    }
}

#[derive(Debug, Clone)]
pub struct SmppDeliver {
    pub inner: rsms_codec_smpp::DeliverSm,
}

impl SmppDeliver {
    pub fn new(inner: rsms_codec_smpp::DeliverSm) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &rsms_codec_smpp::DeliverSm {
        &self.inner
    }

    pub fn into_inner(self) -> rsms_codec_smpp::DeliverSm {
        self.inner
    }

    pub fn is_report(&self) -> bool {
        // SMPP esm_class bit2 (0x04) = SMSC Delivery Receipt（投递状态报告）
        (self.inner.esm_class & 0x04) != 0
    }

    pub fn msg_id(&self) -> String {
        // SMPP DeliverSm doesn't have msg_id - use source_addr as identifier
        format!(
            "{:021}:{:021}",
            self.inner.source_addr, self.inner.destination_addr
        )
    }

    pub fn src_terminal_id(&self) -> String {
        self.inner.source_addr.clone()
    }

    pub fn content(&self) -> Vec<u8> {
        self.inner.short_message.clone()
    }
}

impl From<rsms_codec_smpp::DeliverSm> for SmppDeliver {
    fn from(d: rsms_codec_smpp::DeliverSm) -> Self {
        SmppDeliver::new(d)
    }
}

impl From<SmppDeliver> for rsms_codec_smpp::DeliverSm {
    fn from(d: SmppDeliver) -> Self {
        d.inner
    }
}

pub type SmppTransactionManager = super::TransactionManager;

#[cfg(test)]
mod tests {
    use super::*;
    use rsms_codec_smpp::DeliverSm;

    fn deliver_with_esm(esm_class: u8) -> SmppDeliver {
        SmppDeliver::new(DeliverSm {
            service_type: String::new(),
            source_addr_ton: 0,
            source_addr_npi: 0,
            source_addr: "10086".to_string(),
            dest_addr_ton: 0,
            dest_addr_npi: 0,
            destination_addr: "13800138000".to_string(),
            esm_class,
            protocol_id: 0,
            priority_flag: 0,
            schedule_delivery_time: String::new(),
            validity_period: String::new(),
            registered_delivery: 0,
            replace_if_present_flag: 0,
            data_coding: 0,
            sm_default_msg_id: 0,
            short_message: Vec::new(),
            tlvs: Vec::new(),
        })
    }

    #[test]
    fn is_report_detects_smsc_delivery_receipt_bit() {
        // 回归（P0.1）：esm_class bit2 (0x04) = SMSC 投递回执。
        // 旧实现 `(esm & 0x03) == 4` 恒为 false，导致 SMPP 报告永不被识别。
        assert!(deliver_with_esm(0x04).is_report());
        assert!(deliver_with_esm(0x04 | 0x01).is_report()); // 与其他位共存仍识别
        // 普通 MO / 普通消息：bit2 未置位
        assert!(!deliver_with_esm(0x00).is_report());
        assert!(!deliver_with_esm(0x01).is_report());
        assert!(!deliver_with_esm(0x03).is_report());
    }
}
