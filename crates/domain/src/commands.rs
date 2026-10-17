//! Commands — every operation the runtime accepts, regardless of which
//! surface (chat, button, CLI, MCP) proposed it. One operation, one
//! policy implementation.

use crate::attention::Answer;
use crate::entities::{ArtifactKind, Stream};
use crate::ids::*;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A validated request to do something. `idempotency_key` is recorded
/// before dispatch; the `actor` records which surface proposed it.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct Command {
    pub id: OperationId,
    pub idempotency_key: IdempotencyKey,
    pub actor: Actor,
    pub kind: CommandKind,
}

/// Who initiated a command — surfaced in the UI and audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    /// A human through the interface or CLI.
    User,
    /// The chat orchestrator, on the user's behalf.
    Orchestrator,
    /// The scheduler, from a workflow dependency.
    Scheduler,
    /// Another agent, through a scoped worker interface.
    Agent,
    /// Internal maintenance/reconciliation.
    System,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandKind {
    // ---- sessions ---------------------------------------------------
    /// Launch a new managed session.
    SessionStart {
        tool_instance_id: ToolInstanceId,
        workspace_id: WorkspaceId,
        task_id: Option<TaskId>,
        title: String,
        prompt: String,
        /// Provider-specific launch settings (model, approval policy…).
        launch: serde_json::Value,
    },
    /// Import a provider-native conversation.
    SessionImport {
        tool_instance_id: ToolInstanceId,
        workspace_id: WorkspaceId,
        native_id: String,
        title: String,
    },
    /// Send input to a session's next turn (starts a Run when idle).
    SessionSend {
        session_id: SessionId,
        text: String,
    },
    /// Mid-turn steering input, where the adapter supports it.
    SessionSteer {
        session_id: SessionId,
        text: String,
    },
    /// Cancel the current turn/process (capability-gated).
    SessionCancel {
        session_id: SessionId,
    },
    /// Resume a persisted conversation (capability-gated).
    SessionResume {
        session_id: SessionId,
    },
    /// Fork a conversation (capability-gated).
    SessionFork {
        session_id: SessionId,
    },
    /// Detach: stop tracking control state without killing the process
    /// where ownership allows it.
    SessionDetach {
        session_id: SessionId,
    },

    // ---- attention --------------------------------------------------
    /// Answer a question or approve/deny a bound action.
    AttentionAnswer {
        attention_id: AttentionId,
        answer: Answer,
    },
    /// Dismiss an item that is no longer relevant (e.g. stale inferred
    /// prompt). Never usable to shortcut an approval.
    AttentionDismiss {
        attention_id: AttentionId,
    },

    // ---- tasks / workspaces ----------------------------------------
    TaskCreate {
        workspace_id: WorkspaceId,
        title: String,
        description: Option<String>,
        steps: Vec<crate::entities::WorkflowStep>,
    },
    TaskUpdate {
        task_id: TaskId,
        state: crate::entities::TaskState,
    },
    WorkspaceCreate {
        name: String,
        root: String,
    },
    WorkspaceAddAllowRule {
        workspace_id: WorkspaceId,
        action_kind: crate::entities::ActionKind,
        pattern: String,
    },

    // ---- artifacts --------------------------------------------------
    ArtifactRegister {
        session_id: Option<SessionId>,
        run_id: Option<RunId>,
        kind: ArtifactKind,
        label: String,
        path: String,
    },

    // ---- maintenance ------------------------------------------------
    /// Rescan for observed external agent processes.
    ObserveScan,
    /// Record that output was appended to a transcript (internal).
    #[serde(skip)]
    TranscriptAppend {
        session_id: SessionId,
        stream: Stream,
        offset: u64,
        len: u64,
        preview: String,
    },
}

/// What a command produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandOutcome {
    /// Fully applied; `result` carries the created/changed entity ids.
    Applied { result: serde_json::Value },
    /// Accepted but its effect could not be confirmed — do NOT retry
    /// blindly; reconcile first.
    OutcomeUnknown { reason: String },
    /// Policy denied it (no human was asked).
    Denied { reason: String },
    /// Validation failed before dispatch.
    Rejected { reason: String },
}

/// Validation error kinds — mapped onto IPC/CLI/orchestrator uniformly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("operation requires capability `{capability}`, which is {status}")]
    CapabilityMissing { capability: String, status: String },
    #[error("policy denied: {0}")]
    PolicyDenied(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("internal error: {0}")]
    Internal(String),
}
