//! `orc-domain` — the shared contract for the Orc session manager.
//!
//! Everything that crosses a process or crate boundary lives here:
//! entities, commands, events, capabilities, and the error vocabulary.
//! The boundary rule: the model interprets requests and proposes actions;
//! the runtime owns sessions, permissions, execution, and truth. These
//! types are what both sides of that boundary speak.

pub mod attention;
pub mod capabilities;
pub mod commands;
pub mod entities;
pub mod events;
pub mod ids;
pub mod operations;

pub use attention::*;
pub use capabilities::*;
pub use commands::*;
pub use entities::*;
pub use events::*;
pub use ids::*;
pub use operations::*;

/// Wire/durable schema version. Bump when any persisted or IPC-visible
/// shape changes incompatibly.
pub const SCHEMA_VERSION: u16 = 1;

/// IPC protocol version negotiated during the handshake.
pub const PROTOCOL_VERSION: u16 = 1;
