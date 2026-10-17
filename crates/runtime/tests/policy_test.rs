//! Policy: who may do what — actors, readiness, allow rules.

use orc_domain::*;
use orc_runtime::policy::{Decision, PolicyEngine};

fn tool(readiness: ToolReadiness, provider: ProviderKind) -> ToolInstance {
    ToolInstance {
        id: ToolInstanceId::new(),
        provider,
        label: "tool".into(),
        executable: "/bin/true".into(),
        version: None,
        readiness,
        settings: Default::default(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn ws() -> Workspace {
    Workspace {
        id: WorkspaceId::new(),
        name: "w".into(),
        root: "/tmp".into(),
        allow_rules: vec![],
        network_allowed: true,
        remote_content_allowed: true,
        created_at: chrono::Utc::now(),
    }
}

#[test]
fn unready_tool_cannot_start() {
    let t = tool(ToolReadiness::Installed, ProviderKind::Codex);
    assert!(matches!(
        PolicyEngine::check_session_start(&ws(), &t, Actor::User),
        Decision::Deny(_)
    ));
}

#[test]
fn pty_is_always_startable() {
    let t = tool(ToolReadiness::Installed, ProviderKind::Pty);
    assert!(matches!(
        PolicyEngine::check_session_start(&ws(), &t, Actor::User),
        Decision::Allow
    ));
}

#[test]
fn agent_actor_needs_approval() {
    let t = tool(ToolReadiness::Ready, ProviderKind::Codex);
    assert!(matches!(
        PolicyEngine::check_session_start(&ws(), &t, Actor::Agent),
        Decision::RequireApproval
    ));
}

#[test]
fn allow_rules_short_circuit_approvals() {
    let mut w = ws();
    w.allow_rules.push(AllowRule {
        action_kind: ActionKind::Exec,
        pattern: "cargo test".into(),
        created_at: chrono::Utc::now(),
    });
    let pa = PendingAction {
        kind: ActionKind::Exec,
        display: "cargo test --workspace".into(),
        args: serde_json::json!({}),
        args_hash: String::new(),
        provider_ref: None,
    };
    assert!(matches!(
        PolicyEngine::check_approval(&w, &pa),
        Decision::Allow
    ));
    let other = PendingAction {
        display: "rm -rf /".into(),
        ..pa
    };
    assert!(matches!(
        PolicyEngine::check_approval(&w, &other),
        Decision::RequireApproval
    ));
}

#[test]
fn agent_cannot_answer_approvals() {
    let item = AttentionItem {
        id: AttentionId::new(),
        session_id: None,
        run_id: None,
        workspace_id: WorkspaceId::new(),
        kind: AttentionKind::Approval,
        status: AttentionStatus::Open,
        title: "x".into(),
        body: "x".into(),
        pending_action: None,
        choices: vec![],
        provider_request_ref: None,
        expires_at: None,
        expires_with_session: false,
        created_at: chrono::Utc::now(),
        resolved_at: None,
        resolution: None,
    };
    assert!(matches!(
        PolicyEngine::check_attention_answer(&item, Actor::Agent),
        Decision::Deny(_)
    ));
    assert!(matches!(
        PolicyEngine::check_attention_answer(&item, Actor::User),
        Decision::Allow
    ));
}
