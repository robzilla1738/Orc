//! Events — the immutable record of every observed or accepted change.
//!
//! Every event has an application-owned id, a monotonically increasing
//! sequence number, session/run references, a source, a timestamp, and a
//! schema version. Provider-native references are preserved inside the
//! payload where relevant.

use crate::attention::{Answer, AttentionKind, AttentionStatus, PendingAction};
use crate::capabilities::Ownership;
use crate::commands::{Actor, CommandKind, CommandOutcome};
use crate::entities::{RunState, SessionState, Stream, Usage};
use crate::ids::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A persisted event. `seq` is assigned by the store inside the write
/// transaction — it is the replay cursor for subscribers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    /// Monotonic sequence assigned at write time.
    pub seq: i64,
    pub schema_version: u16,
    pub at: DateTime<Utc>,
    pub source: EventSource,
    pub session_id: Option<SessionId>,
    pub run_id: Option<RunId>,
    pub payload: EventPayload,
}

/// What produced the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    /// A provider adapter (structured protocol or process observation).
    Adapter,
    /// The runtime itself (state transitions, policy, reconciliation).
    Runtime,
    /// A user/interface action.
    Interface,
    /// The chat orchestrator.
    Orchestrator,
    /// The task scheduler.
    Scheduler,
}

/// An event accepted but not yet sequenced — what callers hand the store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewEvent {
    pub schema_version: u16,
    pub source: EventSource,
    pub session_id: Option<SessionId>,
    pub run_id: Option<RunId>,
    pub payload: EventPayload,
}

impl NewEvent {
    pub fn new(source: EventSource, payload: EventPayload) -> Self {
        Self {
            schema_version: crate::SCHEMA_VERSION,
            source,
            session_id: None,
            run_id: None,
            payload,
        }
    }

    pub fn for_session(mut self, session: SessionId) -> Self {
        self.session_id = Some(session);
        self
    }

    pub fn for_run(mut self, run: RunId) -> Self {
        self.run_id = Some(run);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    // ---- command intents & outcomes --------------------------------
    /// Recorded BEFORE dispatching a side effect (idempotency).
    CommandIntent {
        operation_id: OperationId,
        idempotency_key: IdempotencyKey,
        actor: Actor,
        kind: CommandKind,
    },
    CommandResolved {
        operation_id: OperationId,
        idempotency_key: IdempotencyKey,
        outcome: CommandOutcome,
    },

    // ---- sessions ---------------------------------------------------
    SessionCreated {
        session_id: SessionId,
        ownership: Ownership,
        title: String,
    },
    SessionStateChanged {
        session_id: SessionId,
        from: SessionState,
        to: SessionState,
        /// Human-readable reason; `inferred: true` marks an inference.
        reason: String,
        inferred: bool,
    },
    SessionNativeIdAssigned {
        session_id: SessionId,
        native_id: String,
    },
    SessionUpdate {
        session_id: SessionId,
        /// Latest meaningful one-liner for list rendering.
        summary: String,
    },
    SessionUsage {
        session_id: SessionId,
        usage: Usage,
    },

    // ---- runs -------------------------------------------------------
    RunStarted {
        run_id: RunId,
        session_id: SessionId,
        instruction: String,
        attempt: u32,
    },
    RunStateChanged {
        run_id: RunId,
        session_id: SessionId,
        from: RunState,
        to: RunState,
        reason: String,
    },

    // ---- output ------------------------------------------------------
    /// Batched transcript append. Full bytes live in the transcript
    /// file; this event carries the span + preview for live subscribers.
    OutputAppended {
        session_id: SessionId,
        run_id: Option<RunId>,
        stream: Stream,
        offset: u64,
        len: u64,
        preview: String,
    },

    // ---- attention ---------------------------------------------------
    AttentionRaised {
        attention_id: AttentionId,
        session_id: Option<SessionId>,
        workspace_id: WorkspaceId,
        run_id: Option<RunId>,
        kind: AttentionKind,
        title: String,
        body: String,
        pending_action: Option<PendingAction>,
        choices: Vec<String>,
        provider_request_ref: Option<String>,
        expires_with_session: bool,
    },
    AttentionStatusChanged {
        attention_id: AttentionId,
        from: AttentionStatus,
        to: AttentionStatus,
        resolution: Option<Answer>,
    },

    // ---- artifacts ---------------------------------------------------
    ArtifactRegistered {
        artifact_id: ArtifactId,
        session_id: Option<SessionId>,
        run_id: Option<RunId>,
        label: String,
        path: String,
    },

    // ---- orchestrator ------------------------------------------------
    /// A chat message was appended (user or assistant).
    ChatMessage {
        message_id: String,
        role: String,
        /// Operation ids referenced by this message (for cards).
        operations: Vec<OperationId>,
        preview: String,
    },

    // ---- entity change notifications -------------------------------
    /// Non-event-sourced rows changed; subscribers refresh projections.
    TaskChanged {
        task_id: TaskId,
    },
    WorkspaceChanged {
        workspace_id: WorkspaceId,
    },
    ToolChanged {
        tool_instance_id: ToolInstanceId,
    },

    // ---- observed processes ------------------------------------------
    ObservedProcessSeen {
        pid: u32,
        exe: String,
        provider: crate::capabilities::ProviderKind,
        started_at: Option<DateTime<Utc>>,
        cwd: Option<String>,
    },
    ObservedProcessGone {
        pid: u32,
        exe: String,
    },
}

/// A subscriber-visible event (what crosses the IPC boundary).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ServerEvent {
    pub seq: i64,
    pub at: DateTime<Utc>,
    pub session_id: Option<SessionId>,
    pub run_id: Option<RunId>,
    pub payload: serde_json::Value,
}
