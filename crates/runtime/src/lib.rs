//! `orc-runtime` — the truth layer. Session controllers own per-session
//! serialization; the scheduler owns task dependencies; the policy
//! engine owns permissions; the command service is the single entry
//! every surface (chat, buttons, CLI, MCP) goes through.

pub mod bus;
pub mod controller;
pub mod observe;
pub mod policy;
pub mod reconcile;
pub mod scheduler;
pub mod service;

pub use bus::EventBus;
pub use observe::Observer;
pub use policy::{Decision, PolicyEngine};
pub use scheduler::Scheduler;
pub use service::{DiscoveredToolView, Runtime, RuntimeError};
