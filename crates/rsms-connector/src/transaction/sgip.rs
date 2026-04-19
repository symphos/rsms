use rsms_codec_sgip::{Deliver, Submit};

#[derive(Debug, Clone)]
pub struct SgipSubmit {
    pub inner: Submit,
}

impl SgipSubmit {
    pub fn new(inner: Submit) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &Submit {
        &self.inner
    }

    pub fn into_inner(self) -> Submit {
        self.inner
    }

    pub fn dest_id(&self) -> String {
        self.inner.user_numbers.first().cloned().unwrap_or_default()
    }

    pub fn src_id(&self) -> String {
        self.inner.sp_number.clone()
    }

    pub fn content(&self) -> Vec<u8> {
        self.inner.message_content.clone()
    }

    pub fn protocol_name(&self) -> &'static str {
        "SGIP"
    }

    pub fn report_flag(&self) -> u8 {
        self.inner.report_flag
    }
}

impl Default for SgipSubmit {
    fn default() -> Self {
        Self::new(Submit::new())
    }
}

#[derive(Debug, Clone)]
pub struct SgipDeliver {
    pub inner: Deliver,
}

impl SgipDeliver {
    pub fn new(inner: Deliver) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &Deliver {
        &self.inner
    }

    pub fn into_inner(self) -> Deliver {
        self.inner
    }

    pub fn is_report(&self) -> bool {
        false
    }

    pub fn msg_id(&self) -> String {
        format!("{:021}{:021}", self.inner.sp_number, self.inner.user_number)
    }

    pub fn src_terminal_id(&self) -> String {
        self.inner.user_number.clone()
    }

    pub fn content(&self) -> Vec<u8> {
        self.inner.message_content.clone()
    }
}

impl Default for SgipDeliver {
    fn default() -> Self {
        Self::new(Deliver::new())
    }
}

impl From<Deliver> for SgipDeliver {
    fn from(d: Deliver) -> Self {
        SgipDeliver::new(d)
    }
}

impl From<SgipDeliver> for Deliver {
    fn from(d: SgipDeliver) -> Self {
        d.inner
    }
}

pub type SgipTransactionManager = super::TransactionManager;
