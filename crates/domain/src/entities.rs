//! Core entities. A session is not a process; a completed turn is not a
//! completed task; a provider saying "done" is not the same as the task
//! outcome being verified.

use crate::capabilities::{Capabilities, Ownership, ProviderKind};
use crate::ids::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// A configured installation: executable, environment, and provider
/// connection details for one agent tool on this machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct ToolInstance {
    pub id: ToolInstanceId,
    pub provider: ProviderKind,
    /// Human label, e.g. "Codex CLI 0.47".
    pub label: String,
    /// Resolved absolute path of the executable (symlinks resolved).
    /// For HTTP providers this is the base URL.
    pub executable: String,
    /// Detected version string, when a safe probe produced one.
    pub version: Option<String>,
    /// Discovery/readiness state. "Found on disk" is not "ready".
    pub readiness: ToolReadiness,
    /// Extra provider-specific settings (e.g. OpenCode auth header name,
    /// Claude bridge availability).
    pub settings: BTreeMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Distinct facts in the installed→ready chain. Each step is earned
/// separately so the UI can show exactly where a tool stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolReadiness {
    /// Executable found on disk.
    Installed,
    /// Executable launches and reports a compatible version.
    Compatible,
    /// Credentials/account state confirmed usable.
    Authenticated,
    /// Passed a probe session — usable for real work.
    Ready,
}

impl ToolReadiness {
    pub fn rank(self) -> u8 {
        match self {
            ToolReadiness::Installed => 0,
            ToolReadiness::Compatible => 1,
            ToolReadiness::Authenticated => 2,
            ToolReadiness::Ready => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ToolReadiness::Installed => "Installed",
            ToolReadiness::Compatible => "Compatible",
            ToolReadiness::Authenticated => "Authenticated",
            ToolReadiness::Ready => "Ready",
        }
    }
}

/// The working scope and its policies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    /// Absolute path of the working root.
    pub root: String,
    /// Scoped allow rules the policy engine evaluates, e.g.
    /// "allow this workspace's test command".
    pub allow_rules: Vec<AllowRule>,
    /// Whether agents in this workspace may use the network.
    pub network_allowed: bool,
    /// Whether content may leave the device (non-local inference).
    pub remote_content_allowed: bool,
    pub created_at: DateTime<Utc>,
}

/// A narrow, explicit permission rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct AllowRule {
    /// What kind of action this rule covers.
    pub action_kind: ActionKind,
    /// Prefix the command/path must match, e.g. "cargo test".
    pub pattern: String,
    pub created_at: DateTime<Utc>,
}

/// The user's desired outcome. Tasks own sessions' shared goal; a task
/// completes when its acceptance condition is met, not when an agent
/// stops talking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Task {
    pub id: TaskId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub description: Option<String>,
    pub state: TaskState,
    /// Ordered workflow steps with dependencies.
    pub steps: Vec<WorkflowStep>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Open,
    InProgress,
    Blocked,
    Done,
    Abandoned,
}

/// One step in a task's dependency graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct WorkflowStep {
    pub id: StepId,
    pub title: String,
    /// Steps that must reach `Done` before this step can start.
    pub prerequisites: Vec<StepId>,
    /// Session currently executing this step, if dispatched.
    pub session_id: Option<SessionId>,
    /// Input artifacts produced by earlier steps.
    pub inputs: Vec<ArtifactId>,
    /// Expected output description used in cross-agent handoffs.
    pub expected_output: Option<String>,
    /// Instruction dispatched to the session executing this step.
    pub instruction: Option<String>,
    /// Bounded retry policy.
    pub max_attempts: u32,
    pub attempts: u32,
    pub state: StepState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Ready,
    Running,
    Done,
    Failed,
    Skipped,
}

/// A continuing conversation with a particular agent. Not a process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Session {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub task_id: Option<TaskId>,
    pub tool_instance_id: ToolInstanceId,
    pub provider: ProviderKind,
    pub ownership: Ownership,
    /// Provider-native conversation/thread id, when the adapter knows it.
    pub native_id: Option<String>,
    pub title: String,
    pub state: SessionState,
    /// Why the state is what it is — a fact ("waiting for approval",
    /// from a protocol request) or a labeled inference ("may be stalled").
    pub status_detail: Option<StatusDetail>,
    /// Capabilities negotiated for this session's connection.
    pub capabilities: Capabilities,
    /// Provider-reported pid when managed (killing by pid is guarded by
    /// `started_at` so a stale pid is never signaled).
    pub pid: Option<u32>,
    /// OS start time of the managed process, used to validate `pid`.
    pub process_started_at: Option<DateTime<Utc>>,
    /// Latest meaningful update for list rendering.
    pub last_meaningful_update: Option<String>,
    /// Usage counters — distinguished by source.
    pub usage: Option<Usage>,
    /// Last persisted event seq touching this session.
    pub last_event_seq: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the current activity began (for elapsed-time display).
    pub activity_started_at: Option<DateTime<Utc>>,
}

/// Fact vs. inference. The interface renders these differently.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StatusDetail {
    /// Came from the provider protocol — e.g. a permission request.
    Fact { message: String },
    /// An Orc-side inference — e.g. "no output for 5m".
    Inference { message: String },
}

/// Session lifecycle. `Unknown` exists for post-crash states the daemon
/// could not reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Starting,
    Idle,
    Running,
    WaitingForInput,
    WaitingForApproval,
    Completed,
    Failed,
    Cancelled,
    /// Daemon recovered but could not determine the truth.
    Unknown,
}

impl SessionState {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            SessionState::Starting
                | SessionState::Idle
                | SessionState::Running
                | SessionState::WaitingForInput
                | SessionState::WaitingForApproval
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            SessionState::Starting => "Starting",
            SessionState::Idle => "Idle",
            SessionState::Running => "Running",
            SessionState::WaitingForInput => "Waiting for input",
            SessionState::WaitingForApproval => "Waiting for approval",
            SessionState::Completed => "Completed",
            SessionState::Failed => "Failed",
            SessionState::Cancelled => "Cancelled",
            SessionState::Unknown => "Unknown",
        }
    }
}

/// One execution attempt / turn within a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Run {
    pub id: RunId,
    pub session_id: SessionId,
    /// Provider-native turn id, when known.
    pub native_id: Option<String>,
    pub state: RunState,
    /// The prompt/instruction that started this run.
    pub instruction: String,
    /// Attempt number for this run (bounded by step policy).
    pub attempt: u32,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Running,
    WaitingForApproval,
    Succeeded,
    Failed,
    Cancelled,
    /// Dispatched but acknowledgment never arrived — the dangerous case.
    OutcomeUnknown,
}

/// Usage counters. `source` distinguishes provider-reported numbers,
/// estimates, and unavailable data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub source: UsageSource,
    pub reported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    ProviderReported,
    Estimated,
    Unavailable,
}

/// A file, diff, report, or other output with provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Artifact {
    pub id: ArtifactId,
    pub run_id: Option<RunId>,
    pub session_id: Option<SessionId>,
    pub kind: ArtifactKind,
    /// Human label, e.g. "auth-middleware.patch".
    pub label: String,
    /// Path relative to the workspace root, or an Orc-managed path.
    pub path: String,
    /// Content hash (sha256, hex) for provenance.
    pub sha256: Option<String>,
    pub byte_len: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    File,
    Diff,
    Report,
    Transcript,
    Image,
    Other,
}

/// Output channel for session transcript data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    Stdout,
    Stderr,
    /// Structured provider items (agent messages, tool calls).
    Agent,
    /// Orc/system lines.
    System,
}

/// Action kinds the policy engine recognizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Exec,
    WriteFile,
    ReadFile,
    Network,
    SpawnSession,
    Delegate,
    SendMessage,
    Other,
}
