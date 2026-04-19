use async_trait::async_trait;
use rsms_connector::{
    AccountConfig, AccountConfigProvider, AuthCredentials, AuthHandler, AuthResult,
    MessageSource, MessageItem, ServerEventHandler,
};
use rsms_business::BusinessHandler;
use rsms_business::InboundContext;
use rsms_core::{ConnectionInfo, EncodedPdu, RawPdu, Result};
use rsms_codec_cmpp::{decode_message, CmppMessage};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Clone)]
pub struct MockAuthHandler {
    pub allow_list: Vec<String>,
    pub deny_list: Vec<String>,
    pub auth_count: Arc<AtomicUsize>,
}

impl MockAuthHandler {
    pub fn new() -> Self {
        Self {
            allow_list: Vec::new(),
            deny_list: Vec::new(),
            auth_count: Arc::new(AtomicUsize::new(0)),
        }
    }
    
    pub fn with_allow(mut self, account: &str) -> Self {
        self.allow_list.push(account.to_string());
        self
    }
    
    pub fn with_deny(mut self, account: &str) -> Self {
        self.deny_list.push(account.to_string());
        self
    }
}

#[async_trait]
impl AuthHandler for MockAuthHandler {
    fn name(&self) -> &'static str {
        "mock-auth-handler"
    }

    async fn authenticate(&self, client_id: &str, _credentials: AuthCredentials, _conn_info: &ConnectionInfo) -> Result<AuthResult> {
        self.auth_count.fetch_add(1, Ordering::Relaxed);

        if self.deny_list.contains(&client_id.to_string()) {
            return Ok(AuthResult::failure(1, "Account denied"));
        }

        if !self.allow_list.is_empty() && !self.allow_list.contains(&client_id.to_string()) {
            return Ok(AuthResult::failure(2, "Not in allow list"));
        }

        Ok(AuthResult::success(client_id))
    }
}

pub struct MockMessageSource {
    pub messages: Vec<Vec<u8>>,
    pub fetch_count: Arc<AtomicUsize>,
}

impl MockMessageSource {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            fetch_count: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl MessageSource for MockMessageSource {
    async fn fetch(&self, _account: &str, _batch_size: usize) -> Result<Vec<MessageItem>> {
        self.fetch_count.fetch_add(1, Ordering::Relaxed);
        Ok(self.messages.iter()
            .map(|m| MessageItem::Single(Arc::new(RawPdu::from_vec(m.clone())) as Arc<dyn EncodedPdu>))
            .collect())
    }
}

pub struct MockAccountConfigProvider {
    pub configs: std::collections::HashMap<String, AccountConfig>,
}

impl MockAccountConfigProvider {
    pub fn new() -> Self {
        Self {
            configs: std::collections::HashMap::new(),
        }
    }
}

#[async_trait]
impl AccountConfigProvider for MockAccountConfigProvider {
    async fn get_config(&self, account: &str) -> Result<AccountConfig> {
        if let Some(config) = self.configs.get(account) {
            return Ok(config.clone());
        }
        Ok(AccountConfig::new())
    }
}

pub struct MockServerEventHandler {
    pub connected_count: Arc<AtomicUsize>,
    pub disconnected_count: Arc<AtomicUsize>,
    pub authenticated_count: Arc<AtomicUsize>,
}

impl MockServerEventHandler {
    pub fn new() -> Self {
        Self {
            connected_count: Arc::new(AtomicUsize::new(0)),
            disconnected_count: Arc::new(AtomicUsize::new(0)),
            authenticated_count: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl ServerEventHandler for MockServerEventHandler {
    async fn on_connected(&self, _conn: &Arc<dyn rsms_connector::ProtocolConnection>) {
        self.connected_count.fetch_add(1, Ordering::Relaxed);
    }

    async fn on_disconnected(&self, _conn_id: u64, _account: Option<&str>) {
        self.disconnected_count.fetch_add(1, Ordering::Relaxed);
    }

    async fn on_authenticated(&self, _conn: &Arc<dyn rsms_connector::ProtocolConnection>, _account: &str) {
        self.authenticated_count.fetch_add(1, Ordering::Relaxed);
    }
}

pub struct TestBusinessHandler {
    pub submit_count: Arc<AtomicUsize>,
    pub deliver_count: Arc<AtomicUsize>,
}

impl TestBusinessHandler {
    pub fn new() -> Self {
        Self {
            submit_count: Arc::new(AtomicUsize::new(0)),
            deliver_count: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl BusinessHandler for TestBusinessHandler {
    fn name(&self) -> &'static str {
        "test-business-handler"
    }

    async fn on_inbound(&self, _ctx: &InboundContext, frame: &rsms_core::Frame) -> Result<()> {
        match decode_message(frame.data_as_slice()) {
            Ok(CmppMessage::SubmitV20 { .. }) | Ok(CmppMessage::SubmitV30 { .. }) => {
                self.submit_count.fetch_add(1, Ordering::Relaxed);
            }
            Ok(CmppMessage::DeliverV20 { .. }) | Ok(CmppMessage::DeliverV30 { .. }) => {
                self.deliver_count.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        Ok(())
    }
}

pub struct TestServerGuard {
    pub local_addr: SocketAddr,
}

impl TestServerGuard {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}