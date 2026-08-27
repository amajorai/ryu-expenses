use std::sync::Arc;

use crate::store::ExpenseStore;

pub const PLUGIN_ID: &str = "@ryu/expenses";
pub const DEFAULT_PORT: u16 = 8017;

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub token: Option<String>,
}

impl Config {
    pub fn from_env(port: u16) -> Self {
        Self {
            port,
            token: std::env::var("RYU_EXT_TOKEN")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub store: ExpenseStore,
    pub config: Arc<Config>,
}

impl AppState {
    pub fn new(store: ExpenseStore, config: Config) -> Self {
        Self {
            store,
            config: Arc::new(config),
        }
    }
}

pub fn bearer_ok(provided: Option<&str>, expected: Option<&str>) -> bool {
	ryu_sidecar_runtime::token_ok(provided, expected)
}
