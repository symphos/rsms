use async_trait::async_trait;
use rsms_connector::{AccountConfigProvider, AccountConfig};
use rsms_core::Result;

pub struct MockAccountConfigProvider {
    pub max_qps: u64,
    pub window_size: u16,
}

impl MockAccountConfigProvider {
    pub fn new() -> Self {
        Self {
            max_qps: 100,
            window_size: 16,
        }
    }

    pub fn with_limits(max_qps: u64, window_size: u16) -> Self {
        Self { max_qps, window_size }
    }

    pub fn stress_test() -> Self {
        Self {
            max_qps: 10000,
            window_size: 2048,
        }
    }
}

impl Default for MockAccountConfigProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AccountConfigProvider for MockAccountConfigProvider {
    async fn get_config(&self, _account: &str) -> Result<AccountConfig> {
        Ok(AccountConfig::new()
            .with_max_qps(self.max_qps)
            .with_window_size(self.window_size))
    }
}
