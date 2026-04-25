use crate::frame::LongMessageFrame;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant};

type FrameMap = HashMap<String, (Vec<LongMessageFrame>, Instant)>;

pub trait FrameCache: Send + Sync {
    fn put(&self, key: &str, frame: LongMessageFrame);
    fn get(&self, key: &str) -> Option<Vec<LongMessageFrame>>;
    fn remove(&self, key: &str);
    fn cleanup(&self);
}

pub struct InMemoryFrameCache {
    frames: Arc<RwLock<FrameMap>>,
    ttl: Duration,
}

impl InMemoryFrameCache {
    pub fn new(ttl_secs: u64) -> Self {
        let frames: Arc<RwLock<FrameMap>> =
            Arc::new(RwLock::new(HashMap::new()));
        let ttl = Duration::from_secs(ttl_secs);

        let frames_clone = frames.clone();
        let cleanup_ttl = ttl;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let mut g = frames_clone.write().await;
                let now = Instant::now();
                g.retain(|_, (_, last_update)| now.duration_since(*last_update) < cleanup_ttl);
            }
        });

        Self { frames, ttl }
    }
}

impl FrameCache for InMemoryFrameCache {
    fn put(&self, key: &str, frame: LongMessageFrame) {
        let frames = self.frames.clone();
        let key = key.to_string();
        let ttl = self.ttl;
        let _ = ttl;
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            let mut g = frames.write().await;
            let entry = g.entry(key).or_insert_with(|| (Vec::new(), Instant::now()));
            entry.0.push(frame);
            entry.1 = Instant::now();
        });
    }

    fn get(&self, key: &str) -> Option<Vec<LongMessageFrame>> {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async { self.frames.read().await.get(key).map(|(frames, _)| frames.clone()) })
    }

    fn remove(&self, key: &str) {
        let frames = self.frames.clone();
        let key = key.to_string();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            frames.write().await.remove(&key);
        });
    }

    fn cleanup(&self) {
        let frames = self.frames.clone();
        let ttl = self.ttl;
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
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
