//! Wire protocol — newline-delimited JSON. One request → one response;
//! `events.subscribe` upgrades the connection into a push stream.

use orc_domain::{Command, ServerEvent, PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// Client → daemon greeting: authenticate + negotiate versions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u16,
    pub client: String,
    pub token: String,
}

/// Daemon → client greeting reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAck {
    pub protocol_version: u16,
    pub schema_version: u16,
    pub daemon_version: String,
    /// Server clock for display skew checks.
    pub server_time: chrono::DateTime<chrono::Utc>,
}

impl Hello {
    pub fn new(client: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            client: client.into(),
            token: token.into(),
        }
    }
}

/// Client → daemon request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Daemon → client response (correlated by `id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When true, this connection is now an event stream: subsequent
    /// frames are `ServerEvent`s, not `Response`s.
    #[serde(default)]
    pub streaming: bool,
}

/// RPC methods — every UI/CLI/orchestrator entry goes through these.
pub mod methods {
    /// Health + versions.
    pub const PING: &str = "ping";
    /// Snapshot: sessions, attention, tasks, tools, workspaces + cursor.
    pub const SNAPSHOT: &str = "snapshot.get";
    /// Events since a seq — non-subscribing replay.
    pub const EVENTS_SINCE: &str = "events.since";
    /// Subscribe: connection becomes an event stream.
    pub const EVENTS_SUBSCRIBE: &str = "events.subscribe";
    /// Execute a command (params = `Command`).
    pub const COMMAND: &str = "command.exec";
    /// Run tool discovery.
    pub const DISCOVER: &str = "discover.run";
    /// Scan observed processes.
    pub const OBSERVE: &str = "observe.scan";
    /// Chat message to the orchestrator → assistant reply payload.
    pub const CHAT_SEND: &str = "chat.send";
    /// Chat transcript.
    pub const CHAT_HISTORY: &str = "chat.history";
    /// Transcript tail for a session stream.
    pub const TRANSCRIPT_TAIL: &str = "transcript.tail";
    /// Daemon diagnostics (versions, socket, counts).
    pub const DIAGNOSTICS: &str = "diagnostics";
    /// Graceful daemon shutdown.
    pub const SHUTDOWN: &str = "daemon.shutdown";
}

/// Params for `transcript.tail`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct TranscriptTailParams {
    pub session_id: String,
    pub stream: orc_domain::Stream,
    #[serde(default = "default_tail")]
    pub max_bytes: u64,
}

fn default_tail() -> u64 {
    64 * 1024
}

/// A chat exchange for the wire.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ChatWire {
    pub id: String,
    pub role: String,
    /// Prose + operation cards as JSON.
    pub content: Value,
    pub operations: Vec<String>,
    pub at: chrono::DateTime<chrono::Utc>,
}

/// Pushed frame after `events.subscribe`.
pub type EventFrame = ServerEvent;

/// The command payload sent over the wire.
pub type CommandParams = Command;
