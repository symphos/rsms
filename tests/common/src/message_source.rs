use async_trait::async_trait;
use rsms_connector::protocol::{MessageSource, MessageItem};
use rsms_core::Result;

pub struct TestMessageSource;

#[async_trait]
impl MessageSource for TestMessageSource {
    async fn fetch(&self, _account: &str, _batch_size: usize) -> Result<Vec<MessageItem>> {
        Ok(vec![])
    }
}
