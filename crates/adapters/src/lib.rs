//! `orc-adapters` — provider integrations built around capabilities,
//! not provider names. Each adapter owns a spawn/attach contract and
//! reports what it can actually do.

pub mod acp;
pub mod claude;
pub mod codex;
pub mod discover;
pub mod error;
pub mod jsonrpc;
pub mod observed;
pub mod opencode;
pub mod pty;
pub mod traits;

pub use discover::{Discoverer, DiscoveryPaths};
pub use error::{AdapterError, AdapterResult};
pub use observed::{ObservedProcess, ObservedScanner};
pub use traits::*;

use orc_domain::ProviderKind;
use std::collections::HashMap;
use std::sync::Arc;

/// Every adapter the daemon can dispatch to, keyed by provider.
#[derive(Default, Clone)]
pub struct AdapterRegistry {
    adapters: HashMap<ProviderKind, Arc<dyn AgentAdapter>>,
}

impl AdapterRegistry {
    pub fn with_defaults() -> Self {
        let mut r = Self::default();
        r.register(Arc::new(pty::PtyAdapter::new()));
        r.register(Arc::new(claude::ClaudeAdapter::new()));
        r.register(Arc::new(codex::CodexAdapter::new()));
        r.register(Arc::new(acp::AcpAdapter::new()));
        r.register(Arc::new(opencode::OpenCodeAdapter::new()));
        r
    }

    pub fn register(&mut self, adapter: Arc<dyn AgentAdapter>) {
        self.adapters.insert(adapter.provider(), adapter);
    }

    pub fn get(&self, provider: ProviderKind) -> Option<Arc<dyn AgentAdapter>> {
        self.adapters.get(&provider).cloned()
    }
}
