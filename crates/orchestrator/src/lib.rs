//! `orc-orchestrator` — the conversational surface. The model
//! interprets requests and proposes typed operations; the runtime
//! validates and owns the truth. The orchestrator has no shell access
//! and no approval authority of its own.

pub mod backend;
pub mod chat;
pub mod tools;

pub use backend::{AnthropicBackend, MockBackend, ModelBackend};
pub use chat::ChatService;
