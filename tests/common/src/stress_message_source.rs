use async_trait::async_trait;
use dashmap::DashMap;
use rsms_connector::protocol::{MessageItem, MessageSource};
use rsms_core::{EncodedPdu, RawPdu, Result};
use std::collections::VecDeque;
use std::sync::Arc;

pub struct StressMockMessageSource {
    queues: Arc<DashMap<String, VecDeque<MessageItem>>>,
}

impl StressMockMessageSource {
    pub fn new() -> Self {
        Self {
            queues: Arc::new(DashMap::new()),
        }
    }

    pub async fn push(&self, account: &str, bytes: Vec<u8>) -> Result<()> {
        let mut queue = self
            .queues
            .entry(account.to_string())
            .or_insert_with(VecDeque::new);
        queue.push_back(
            MessageItem::Single(Arc::new(RawPdu::from_vec(bytes)) as Arc<dyn EncodedPdu>)
        );
        Ok(())
    }

    pub async fn push_item(&self, account: &str, bytes: Vec<u8>) {
        let mut queue = self
            .queues
            .entry(account.to_string())
            .or_insert_with(VecDeque::new);
        queue.push_back(
            MessageItem::Single(Arc::new(RawPdu::from_vec(bytes)) as Arc<dyn EncodedPdu>)
        );
    }

    pub async fn push_group(&self, account: &str, items: Vec<Vec<u8>>) -> Result<()> {
        let mut queue = self
            .queues
            .entry(account.to_string())
            .or_insert_with(VecDeque::new);
        queue.push_back(MessageItem::Group {
            items: items
                .into_iter()
                .map(|i| Arc::new(RawPdu::from_vec(i)) as Arc<dyn EncodedPdu>)
                .collect(),
        });
        Ok(())
    }

    pub async fn fetch_bytes(&self, account: &str, batch_size: usize) -> Vec<Vec<u8>> {
        if let Some(mut queue) = self.queues.get_mut(account) {
            let mut batch = Vec::with_capacity(batch_size);
            for _ in 0..batch_size {
                if let Some(msg_item) = queue.pop_front() {
                    match msg_item {
                        MessageItem::Single(d) => batch.push(d.as_bytes().to_vec()),
                        MessageItem::Group { .. } => continue,
                    }
                } else {
                    break;
                }
            }
            batch
        } else {
            Vec::new()
        }
    }

    pub async fn queue_len(&self, account: &str) -> usize {
        self.queues.get(account).map(|q| q.len()).unwrap_or(0)
    }

    pub async fn total_queue_len(&self) -> usize {
        self.queues.iter().map(|r| r.value().len()).sum()
    }
}

impl Default for StressMockMessageSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MessageSource for StressMockMessageSource {
    async fn fetch(&self, account: &str, batch_size: usize) -> Result<Vec<MessageItem>> {
        if let Some(mut queue) = self.queues.get_mut(account) {
            let mut batch = Vec::with_capacity(batch_size);
            for _ in 0..batch_size {
                if let Some(item) = queue.pop_front() {
                    batch.push(item);
                } else {
                    break;
                }
            }
            Ok(batch)
        } else {
            Ok(Vec::new())
        }
    }
}
