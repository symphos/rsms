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
        (self.inner.esm_class & 0x03) == 4
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
