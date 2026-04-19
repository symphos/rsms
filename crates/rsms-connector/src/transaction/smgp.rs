use rsms_codec_smgp::Submit;

#[derive(Debug, Clone)]
pub struct SmgpSubmit {
    pub inner: Submit,
}

impl SmgpSubmit {
    pub fn new(inner: Submit) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &Submit {
        &self.inner
    }

    pub fn into_inner(self) -> Submit {
        self.inner
    }

    pub fn msg_id(&self) -> String {
        format!(
            "{:032x}",
            u64::from_be_bytes([
                self.inner
                    .src_term_id
                    .as_bytes()
                    .get(0)
                    .copied()
                    .unwrap_or(b'0') as u8,
                self.inner
                    .dest_term_ids
                    .first()
                    .and_then(|s| s.as_bytes().get(0))
                    .copied()
                    .unwrap_or(b'0') as u8,
                self.inner.msg_content.first().copied().unwrap_or(b'0') as u8,
                self.inner.msg_content.len().min(5) as u8,
                0,
                0,
                0,
                0
            ])
        )
    }

    pub fn dest_id(&self) -> String {
        self.inner
            .dest_term_ids
            .first()
            .cloned()
            .unwrap_or_default()
    }

    pub fn src_id(&self) -> String {
        self.inner.src_term_id.clone()
    }

    pub fn content(&self) -> Vec<u8> {
        self.inner.msg_content.clone()
    }

    pub fn protocol_name(&self) -> &'static str {
        "SMGP"
    }
}

impl Default for SmgpSubmit {
    fn default() -> Self {
        Self::new(Submit::new())
    }
}

#[derive(Debug, Clone)]
pub struct SmgpDeliver {
    pub inner: rsms_codec_smgp::Deliver,
}

impl SmgpDeliver {
    pub fn new(inner: rsms_codec_smgp::Deliver) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &rsms_codec_smgp::Deliver {
        &self.inner
    }

    pub fn into_inner(self) -> rsms_codec_smgp::Deliver {
        self.inner
    }

    pub fn is_report(&self) -> bool {
        self.inner.is_report == 1
    }

    pub fn msg_id(&self) -> String {
        format!(
            "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            self.inner.msg_id.bytes[0],
            self.inner.msg_id.bytes[1],
            self.inner.msg_id.bytes[2],
            self.inner.msg_id.bytes[3],
            self.inner.msg_id.bytes[4],
            self.inner.msg_id.bytes[5],
            self.inner.msg_id.bytes[6],
            self.inner.msg_id.bytes[7],
            self.inner.msg_id.bytes[8],
            self.inner.msg_id.bytes[9]
        )
    }

    pub fn src_terminal_id(&self) -> String {
        self.inner.src_term_id.clone()
    }

    pub fn content(&self) -> Vec<u8> {
        self.inner.msg_content.clone()
    }
}

impl Default for SmgpDeliver {
    fn default() -> Self {
        Self::new(rsms_codec_smgp::Deliver::new())
    }
}

impl From<rsms_codec_smgp::Deliver> for SmgpDeliver {
    fn from(d: rsms_codec_smgp::Deliver) -> Self {
        SmgpDeliver::new(d)
    }
}

impl From<SmgpDeliver> for rsms_codec_smgp::Deliver {
    fn from(d: SmgpDeliver) -> Self {
        d.inner
    }
}

pub type SmgpTransactionManager = super::TransactionManager;
