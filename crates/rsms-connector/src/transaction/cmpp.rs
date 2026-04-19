use rsms_codec_cmpp::Submit;

#[derive(Debug, Clone)]
pub struct CmppSubmit {
    pub inner: Submit,
}

impl CmppSubmit {
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
            "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            self.inner.msg_id[0],
            self.inner.msg_id[1],
            self.inner.msg_id[2],
            self.inner.msg_id[3],
            self.inner.msg_id[4],
            self.inner.msg_id[5],
            self.inner.msg_id[6],
            self.inner.msg_id[7]
        )
    }

    pub fn dest_id(&self) -> String {
        self.inner
            .dest_terminal_ids
            .first()
            .cloned()
            .unwrap_or_default()
    }

    pub fn src_id(&self) -> String {
        self.inner.src_id.clone()
    }

    pub fn content(&self) -> Vec<u8> {
        self.inner.msg_content.clone()
    }

    pub fn protocol_name(&self) -> &'static str {
        "CMPP"
    }
}

impl Default for CmppSubmit {
    fn default() -> Self {
        Self::new(Submit::new())
    }
}

impl From<CmppSubmit> for (String, String, String, Vec<u8>, &'static str) {
    fn from(s: CmppSubmit) -> Self {
        (
            s.msg_id(),
            s.dest_id(),
            s.src_id(),
            s.content(),
            s.protocol_name(),
        )
    }
}

#[derive(Debug, Clone)]
pub struct CmppDeliver {
    pub inner: rsms_codec_cmpp::Deliver,
}

impl CmppDeliver {
    pub fn new(inner: rsms_codec_cmpp::Deliver) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &rsms_codec_cmpp::Deliver {
        &self.inner
    }

    pub fn into_inner(self) -> rsms_codec_cmpp::Deliver {
        self.inner
    }

    pub fn is_report(&self) -> bool {
        self.inner.registered_delivery == 1
    }

    pub fn msg_id(&self) -> String {
        format!(
            "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            self.inner.msg_id[0],
            self.inner.msg_id[1],
            self.inner.msg_id[2],
            self.inner.msg_id[3],
            self.inner.msg_id[4],
            self.inner.msg_id[5],
            self.inner.msg_id[6],
            self.inner.msg_id[7]
        )
    }

    pub fn src_terminal_id(&self) -> String {
        self.inner.src_terminal_id.clone()
    }

    pub fn content(&self) -> Vec<u8> {
        self.inner.msg_content.clone()
    }
}

impl Default for CmppDeliver {
    fn default() -> Self {
        Self::new(rsms_codec_cmpp::Deliver::new())
    }
}

impl From<rsms_codec_cmpp::Deliver> for CmppDeliver {
    fn from(d: rsms_codec_cmpp::Deliver) -> Self {
        CmppDeliver::new(d)
    }
}

impl From<CmppDeliver> for rsms_codec_cmpp::Deliver {
    fn from(d: CmppDeliver) -> Self {
        d.inner
    }
}

pub type CmppTransactionManager = super::TransactionManager;
