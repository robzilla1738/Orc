//! Operation handles — what chat renders as cards and what callers get
//! back immediately for long-running work.

use crate::commands::{Actor, CommandOutcome};
use crate::ids::OperationId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A dispatched operation's live handle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Operation {
    pub id: OperationId,
    pub actor: Actor,
    /// Human-readable verb, e.g. "Start Codex session".
    pub verb: String,
    pub state: OperationState,
    pub outcome: Option<CommandOutcome>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Dispatched,
    Applied,
    Rejected,
    Denied,
    OutcomeUnknown,
}
