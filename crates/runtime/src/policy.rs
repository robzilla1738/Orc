//! The policy engine. One implementation for every surface; the model
//! can propose but never self-approve.

use orc_domain::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
    /// A human must decide — becomes an AttentionItem.
    RequireApproval,
}

pub struct PolicyEngine;

impl PolicyEngine {
    /// May `actor` start a session in this workspace on this tool?
    pub fn check_session_start(
        _workspace: &Workspace,
        tool: &ToolInstance,
        actor: Actor,
    ) -> Decision {
        // Tools must at least launch successfully. Pty always does.
        if tool.provider != ProviderKind::Pty
            && tool.readiness.rank() < ToolReadiness::Compatible.rank()
        {
            return Decision::Deny(format!(
                "{} is not ready ({}). Run discovery first.",
                tool.label,
                tool.readiness.label()
            ));
        }
        match actor {
            // Agents never spawn sessions on their own authority.
            Actor::Agent => Decision::RequireApproval,
            _ => Decision::Allow,
        }
    }

    /// Should this provider-raised approval go to the inbox, or is it
    /// covered by an explicit workspace allow rule?
    pub fn check_approval(workspace: &Workspace, action: &PendingAction) -> Decision {
        for rule in &workspace.allow_rules {
            if rule.action_kind == action.kind
                && action
                    .display
                    .to_lowercase()
                    .starts_with(&rule.pattern.to_lowercase())
            {
                return Decision::Allow;
            }
        }
        Decision::RequireApproval
    }

    /// Agent-originated answers are never allowed to grant themselves
    /// permissions.
    pub fn check_attention_answer(item: &AttentionItem, actor: Actor) -> Decision {
        if actor == Actor::Agent
            && matches!(
                item.kind,
                AttentionKind::Approval | AttentionKind::UncertainOutcome
            )
        {
            return Decision::Deny("agents cannot approve themselves; a human must answer".into());
        }
        if item.status != AttentionStatus::Open {
            return Decision::Deny(format!(
                "attention item is already {}",
                status_label(item.status)
            ));
        }
        Decision::Allow
    }

    /// Can this session perform `capability`?
    pub fn check_capability(session: &Session, capability: &str, support: &Support) -> Decision {
        match support {
            Support::Supported { .. } => Decision::Allow,
            Support::Unsupported { reason } => Decision::Deny(format!(
                "{capability} unsupported for {} session: {reason}",
                session.provider.label()
            )),
            Support::Unknown => Decision::Deny(format!(
                "{capability} status unknown for {} session",
                session.provider.label()
            )),
        }
    }
}

fn status_label(s: AttentionStatus) -> &'static str {
    match s {
        AttentionStatus::Open => "open",
        AttentionStatus::Answered => "answered",
        AttentionStatus::Expired => "expired",
        AttentionStatus::Superseded => "superseded",
    }
}
