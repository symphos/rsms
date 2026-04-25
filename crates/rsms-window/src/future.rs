use std::time::Instant;

pub struct WindowFuture<K, P> {
    key: K,
    params: P,
    created_at: Instant,
    timeout: std::time::Duration,
}

impl<K, P> Clone for WindowFuture<K, P>
where
    K: Clone,
    P: Clone,
{
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            params: self.params.clone(),
            created_at: self.created_at,
            timeout: self.timeout,
        }
    }
}

impl<K, P> WindowFuture<K, P>
where
    P: Clone,
{
    pub fn new(key: K, params: P, timeout: std::time::Duration) -> Self {
        Self {
            key,
            params,
            created_at: Instant::now(),
            timeout,
        }
    }

    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn params(&self) -> &P {
        &self.params
    }

    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() > self.timeout
    }
}

impl<K, P> std::fmt::Debug for WindowFuture<K, P>
where
    K: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowFuture")
            .field("key", &self.key)
            .field("timeout", &self.timeout)
            .finish()
    }
}
