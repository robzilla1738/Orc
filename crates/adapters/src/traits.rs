//! The adapter contract: an agent session with explicitly supported
//! operations. The runtime speaks `AdapterControl`; adapters emit
//! `AdapterEvent`. Everything else is provider detail.

use crate::error::AdapterResult;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use orc_domain::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

/// Launch parameters — provider-specific knobs live in `launch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub session_id: SessionId,
    /// Resolved executable path (or base URL for server providers).
    pub executable: String,
    /// Extra CLI args from the tool instance config.
    pub args: Vec<String>,
    /// Working directory (workspace root).
    pub cwd: String,
    /// Sanitized child environment.
    pub env: Vec<(String, String)>,
    /// The user's instruction for the first turn.
    pub prompt: String,
    /// Provider launch settings (model, permission mode, …).
    pub launch: Value,
}

/// Attach/import parameters for a provider-native conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportSpec {
    pub session_id: SessionId,
    pub executable: String,
    pub cwd: String,
    pub native_id: String,
    pub launch: Value,
}

/// A live adapter session. `control` serializes owner operations;
/// `events` streams everything the provider emits.
pub struct AdapterSession {
    /// OS pid when Orc spawned a process (or attached to one whose pid
    /// is verified against `process_started_at`).
    pub pid: Option<u32>,
    /// Process start instant — guards pid reuse.
    pub process_started_at: Option<DateTime<Utc>>,
    /// Provider-native conversation id once known.
    pub native_id: Option<String>,
    /// Negotiated capabilities for this connection.
    pub capabilities: Capabilities,
    pub events: mpsc::Receiver<AdapterEvent>,
    pub control: mpsc::Sender<AdapterControl>,
}

/// Operations the runtime asks a session to perform.
#[derive(Debug)]
pub enum AdapterControl {
    /// User input for the next/current turn.
    Send { text: String },
    /// Mid-turn steering where supported.
    Steer { text: String },
    /// Answer a structured request (question or approval).
    Answer {
        request_ref: String,
        answer: AdapterAnswer,
    },
    /// Cancel the running turn (or the process where that's all there is).
    Cancel,
    /// Orderly shutdown of the adapter.
    Shutdown,
}

/// The answer to send back to a provider request.
#[derive(Debug, Clone)]
pub enum AdapterAnswer {
    /// Text or choice for a question.
    Text(String),
    /// Approve the pending action (optionally with a provider option id).
    Approve { option_id: Option<String> },
    /// Deny the pending action.
    Deny { reason: Option<String> },
}

/// Everything a provider can tell us.
#[derive(Debug, Clone)]
pub enum AdapterEvent {
    /// Process/protocol is up. `native_id` once the provider assigns one.
    Started { native_id: Option<String> },
    /// Raw or item text for the transcript.
    Output { stream: Stream, text: String },
    /// One-line meaningful update for list views ("editing auth.rs").
    Meaningful { summary: String },
    /// Structured permission request — a *fact* the UI can trust.
    ApprovalRequested {
        request_ref: String,
        action: ProposedAction,
    },
    /// Structured question from the provider.
    Question {
        request_ref: String,
        prompt: String,
        choices: Vec<String>,
    },
    /// Authentication expired / failed inside the provider.
    AuthFailed { message: String },
    /// A turn finished. Distinguishes "agent stopped talking" from
    /// "task verified".
    TurnFinished {
        native_turn_id: Option<String>,
        reason: String,
        ok: bool,
    },
    /// The underlying process/connection ended.
    Exited {
        code: Option<i32>,
        reason: Option<String>,
    },
    /// Usage counters when the provider reports them.
    Usage { usage: Usage },
    /// An explicit artifact was produced (with provider provenance).
    Artifact { label: String, path: String },
    /// Something went wrong inside the adapter itself.
    Error { message: String },
}

/// The action a provider wants permission for — before Orc binds it to
/// workspace/run context into a `PendingAction`.
#[derive(Debug, Clone)]
pub struct ProposedAction {
    pub kind: ActionKind,
    pub display: String,
    pub args: Value,
}

#[async_trait]
pub trait AgentAdapter: Send + Sync {
    fn provider(&self) -> ProviderKind;

    /// Capabilities this adapter can provide at best case — the actual
    /// session's may be narrower after negotiation.
    fn base_capabilities(&self) -> Capabilities;

    /// Launch a new managed session.
    async fn spawn(&self, spec: SpawnSpec) -> AdapterResult<AdapterSession>;

    /// Import/attach to an existing native conversation. Default:
    /// unsupported — adapters opt in honestly.
    async fn import(&self, _spec: ImportSpec) -> AdapterResult<AdapterSession> {
        Err(crate::error::AdapterError::Unsupported(
            "this adapter does not support importing conversations".into(),
        ))
    }
}
