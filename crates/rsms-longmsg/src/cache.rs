//! Frame cache for long message segment storage.

use crate::frame::LongMessageFrame;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant};

pub trait FrameCache: Send + Sync {
    fn put(&self, key: &str, frame: LongMessageFrame);
    fn get(&self, key: &str) -> Option<Vec<LongMessageFrame>>;
    fn remove(&self, key: &str);
    fn cleanup(&self);
}

pub struct InMemoryFrameCache {
    frames: Arc<RwLock<HashMap<String, (Vec<LongMessageFrame>, Instant)>>>,
    ttl: Duration,
}

impl InMemoryFrameCache {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            frames: Arc::new(RwLock::new(HashMap::new())),
            ttl: Duration::from_secs(ttl_secs),
        }
    }
}

impl FrameCache for InMemoryFrameCache {
    fn put(&self, key: &str, frame: LongMessageFrame) {
        let frames = self.frames.clone();
        let key = key.to_string();
        let ttl = self.ttl;
        tokio::spawn(async move {
            let mut g = frames.write().await;
            let entry = g.entry(key.clone()).or_insert_with(|| (Vec::new(), Instant::now()));
            entry.0.push(frame);
            entry.1 = Instant::now();
            drop(g);
            tokio::time::sleep(ttl).await;
            frames.write().await.remove(&key);
        });
    }

    fn get(&self, key: &str) -> Option<Vec<LongMessageFrame>> {
        self.frames.blocking_read().get(key).map(|(frames, _)| frames.clone())
    }

    fn remove(&self, key: &str) {
        let frames = self.frames.clone();
        let key = key.to_string();
        tokio::spawn(async move {
            frames.write().await.remove(&key);
        });
    }

    fn cleanup(&self) {
        let frames = self.frames.clone();
        let ttl = self.ttl;
        tokio::spawn(async move {
            let mut g = frames.write().await;
            let now = Instant::now();
            g.retain(|_, (_, last_update)| now.duration_since(*last_update) < ttl);
        });
    }
}

impl Default for InMemoryFrameCache {
    fn default() -> Self {
        Self::new(300)
    }
}
