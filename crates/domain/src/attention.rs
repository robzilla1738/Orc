//! Attention items: the inbox of decisions that need a human.
//!
//! An approval is bound to the exact pending action — its kind, args,
//! workspace, originating run, and provider request reference — so a
//! granted answer can never authorize a changed command. Ordinary
//! answers ("use PostgreSQL") and permission grants ("allow production
//! migration") are different kinds of input and stay separate.

use crate::entities::ActionKind;
use crate::ids::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Something that requires a human: approvals, questions, failed auth,
/// review requests, uncertain outcomes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct AttentionItem {
    pub id: AttentionId,
    pub session_id: Option<SessionId>,
    pub run_id: Option<RunId>,
    pub workspace_id: WorkspaceId,
    pub kind: AttentionKind,
    pub status: AttentionStatus,
    /// One-line summary for inbox rendering.
    pub title: String,
    /// Full prompt/details.
    pub body: String,
    /// For approvals: the action the decision is bound to.
    pub pending_action: Option<PendingAction>,
    /// Choices offered by the provider for questions, if any.
    pub choices: Vec<String>,
    /// Provider-native request reference, for correlating the answer.
    pub provider_request_ref: Option<String>,
    /// When this item stops being answerable. `None` = no expiry, but
    /// items die with their provider — see `expires_with_session`.
    pub expires_at: Option<DateTime<Utc>>,
    /// If true, the item is invalidated when `session_id` dies.
    pub expires_with_session: bool,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    /// The recorded answer when resolved.
    pub resolution: Option<Answer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    /// Provider asked a question (structured).
    Question,
    /// Provider requested permission for an action (structured).
    Approval,
    /// Authentication expired or failed.
    AuthFailed,
    /// An agent asked for a human review.
    ReviewRequest,
    /// A side effect was dispatched but its outcome is unknown.
    UncertainOutcome,
    /// A model-interpreted prompt that *looks* like a question from a
    /// PTY session — never silently trusted as an approval.
    InferredPrompt,
}

impl AttentionKind {
    pub fn label(self) -> &'static str {
        match self {
            AttentionKind::Question => "Question",
            AttentionKind::Approval => "Approval",
            AttentionKind::AuthFailed => "Authentication failed",
            AttentionKind::ReviewRequest => "Review requested",
            AttentionKind::UncertainOutcome => "Outcome unknown",
            AttentionKind::InferredPrompt => "Possible question (unverified)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AttentionStatus {
    Open,
    Answered,
    /// The request expired or its provider died.
    Expired,
    /// Replaced by a newer request for the same operation.
    Superseded,
}

/// The exact action a pending approval is bound to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct PendingAction {
    pub kind: ActionKind,
    /// Display form, e.g. the command line or file path.
    pub display: String,
    /// Canonicalized arguments, hashed into `args_hash`.
    pub args: serde_json::Value,
    /// sha256 over canonical args + workspace + run + provider ref.
    /// An approval for hash H authorizes only an action with hash H.
    pub args_hash: String,
    /// Provider-native reference (e.g. Codex approval id).
    pub provider_ref: Option<String>,
}

impl PendingAction {
    /// Bind an approval to a specific action instance.
    pub fn compute_hash(
        kind: ActionKind,
        args: &serde_json::Value,
        workspace_id: &WorkspaceId,
        run_id: Option<&RunId>,
        provider_ref: Option<&str>,
    ) -> String {
        use sha2::{Digest, Sha256};
        let canonical = serde_json::json!({
            "kind": kind,
            "args": args,
            "workspace": workspace_id.as_str(),
            "run": run_id.map(|r| r.as_str()),
            "provider_ref": provider_ref,
        });
        let mut h = Sha256::new();
        h.update(canonical.to_string().as_bytes());
        base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            h.finalize(),
        )
    }
}

/// How the human answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    /// Free text (question answers only — never a permission grant).
    Text { text: String },
    /// Picked one of `choices` by index.
    Choice { index: u32 },
    /// Permission granted for the bound action.
    Approve { scope: ApprovalScope },
    /// Permission denied.
    Deny { reason: Option<String> },
}

/// How long/far an approval extends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    /// This one action only.
    Once,
    /// This action for the rest of the run.
    ForRun,
    /// Add an allow rule for this workspace (pattern derived from the
    /// action's display form).
    WorkspaceRule,
}
