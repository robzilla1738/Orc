//! `orc-ipc` — the private boundary between UI/CLI and the daemon.
//! Local sockets on Unix, named pipes on Windows. Current-user access
//! control, an authenticated handshake, and protocol-version
//! negotiation — no unrestricted local HTTP surface.

pub mod auth;
pub mod client;
pub mod error;
pub mod proto;
pub mod server;

pub use client::IpcClient;
pub use error::{IpcError, IpcResult};
pub use proto::*;
