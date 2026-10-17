//! Provider and capability model.
//!
//! The abstraction is "an agent session with explicitly supported
//! operations", never "a provider name". Every adapter reports what it
//! can actually do; `Unknown` is a first-class answer so nothing is
//! silently assumed.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Provider families Orc knows how to talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// OpenAI Codex CLI (app-server over stdio).
    Codex,
    /// Any agent speaking the Agent Client Protocol.
    Acp,
    /// Anthropic Claude Code CLI (headless stream-json, optional SDK bridge).
    Claude,
    /// OpenCode server (HTTP + SSE).
    OpenCode,
    /// Generic pseudo-terminal fallback — visibly limited.
    Pty,
    /// Synthetic provider for the Orc chat orchestrator itself.
    Orchestrator,
}

impl ProviderKind {
    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::Codex => "Codex",
            ProviderKind::Acp => "ACP",
            ProviderKind::Claude => "Claude",
            ProviderKind::OpenCode => "OpenCode",
            ProviderKind::Pty => "Terminal",
            ProviderKind::Orchestrator => "Orc",
        }
    }
}

/// Whether an operation is usable on a given connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Support {
    /// Verified or declared usable. `constraint` qualifies it when the
    /// support is partial, e.g. resume works only within one provider
    /// generation.
    Supported { constraint: Option<String> },
    /// Known to be unavailable for this adapter/installation.
    Unsupported { reason: String },
    /// Not yet determined — the UI must not present this as available.
    Unknown,
}

impl Support {
    pub fn supported() -> Self {
        Support::Supported { constraint: None }
    }

    pub fn unsupported(reason: impl Into<String>) -> Self {
        Support::Unsupported {
            reason: reason.into(),
        }
    }

    pub fn is_supported(&self) -> bool {
        matches!(self, Support::Supported { .. })
    }
}

/// Operations an adapter session may support. Probed/declared once per
/// connection and carried on the session so the UI and orchestrator make
/// decisions from facts, not provider marketing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Capabilities {
    /// Can the adapter list provider-native conversations?
    pub list_sessions: Support,
    /// Can an existing native conversation be imported into Orc?
    pub import_session: Support,
    /// Can a stored session be resumed as a continuing conversation?
    pub resume: Support,
    /// Can a session be forked into a new conversation?
    pub fork: Support,
    /// Does the adapter stream structured output items?
    pub streaming: Support,
    /// Does the provider surface approvals as structured requests?
    pub structured_approvals: Support,
    /// Does the provider surface questions as structured requests?
    pub structured_questions: Support,
    /// Can a running turn be cancelled?
    pub cancel: Support,
    /// Can user input be injected mid-turn?
    pub mid_turn_steer: Support,
    /// Can the adapter reconnect to a process it did not launch?
    pub reconnect: Support,
    /// Does the provider report token/cost usage?
    pub usage_reporting: Support,
    /// Does the provider report artifacts (files, diffs) explicitly?
    pub artifact_reporting: Support,
}

impl Capabilities {
    /// Everything unknown — the safe default for a detected but
    /// unprobed process.
    pub fn all_unknown() -> Self {
        Self {
            list_sessions: Support::Unknown,
            import_session: Support::Unknown,
            resume: Support::Unknown,
            fork: Support::Unknown,
            streaming: Support::Unknown,
            structured_approvals: Support::Unknown,
            structured_questions: Support::Unknown,
            cancel: Support::Unknown,
            mid_turn_steer: Support::Unknown,
            reconnect: Support::Unknown,
            usage_reporting: Support::Unknown,
            artifact_reporting: Support::Unknown,
        }
    }

    /// Generic terminal fallback: interactive I/O only.
    pub fn pty() -> Self {
        Self {
            list_sessions: Support::unsupported("a terminal process has no session registry"),
            import_session: Support::unsupported("nothing to import"),
            resume: Support::unsupported("terminal processes cannot be resumed"),
            fork: Support::unsupported("terminal processes cannot be forked"),
            streaming: Support::Supported {
                constraint: Some("raw byte stream only; no structured items".into()),
            },
            structured_approvals: Support::unsupported(
                "terminal text cannot be trusted as an approval request",
            ),
            structured_questions: Support::unsupported("no structured question channel"),
            cancel: Support::Supported {
                constraint: Some("SIGINT/SIGTERM only".into()),
            },
            mid_turn_steer: Support::Supported {
                constraint: Some("text is written to stdin; delivery is best-effort".into()),
            },
            reconnect: Support::unsupported("cannot reattach to a PTY after exit"),
            usage_reporting: Support::unsupported("no usage channel"),
            artifact_reporting: Support::unsupported("no artifact channel"),
        }
    }
}

/// Who owns a session — the single most important honesty boundary in
/// the product. Observation is never presented as control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    /// Orc launched it or holds a live control channel.
    Managed,
    /// A native conversation was imported; controls depend on adapter
    /// capabilities.
    Imported,
    /// A likely agent process was detected without any established
    /// control connection.
    Observed,
}

impl Ownership {
    pub fn label(self) -> &'static str {
        match self {
            Ownership::Managed => "Managed",
            Ownership::Imported => "Imported",
            Ownership::Observed => "Observed",
        }
    }
}
