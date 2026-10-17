//! Storage: event append, projection, cursor replay, intent ledger.

use orc_domain::*;
use orc_storage::Store;

fn session(id: &SessionId, tid: &ToolInstanceId, wid: &WorkspaceId) -> Session {
    let now = chrono::Utc::now();
    Session {
        id: id.clone(),
        workspace_id: wid.clone(),
        task_id: None,
        tool_instance_id: tid.clone(),
        provider: ProviderKind::Codex,
        ownership: Ownership::Managed,
        native_id: None,
        title: "auth work".into(),
        state: SessionState::Starting,
        status_detail: None,
        capabilities: Capabilities::all_unknown(),
        pid: None,
        process_started_at: None,
        last_meaningful_update: None,
        usage: None,
        last_event_seq: 0,
        created_at: now,
        updated_at: now,
        activity_started_at: None,
    }
}

#[tokio::test]
async fn append_projects_session_and_replays() {
    let store = Store::open_in_memory().await.unwrap();
    let sid = SessionId::new();
    let tid = ToolInstanceId::new();
    let wid = WorkspaceId::new();

    store
        .upsert_session(&session(&sid, &tid, &wid))
        .await
        .unwrap();
    store
        .append(NewEvent::new(
            EventSource::Runtime,
            EventPayload::SessionCreated {
                session_id: sid.clone(),
                ownership: Ownership::Managed,
                title: "auth work".into(),
            },
        ))
        .await
        .unwrap();
    store
        .append(
            NewEvent::new(
                EventSource::Runtime,
                EventPayload::SessionStateChanged {
                    session_id: sid.clone(),
                    from: SessionState::Starting,
                    to: SessionState::Running,
                    reason: "spawned".into(),
                    inferred: false,
                },
            )
            .for_session(sid.clone()),
        )
        .await
        .unwrap();

    let snap = store.snapshot().await.unwrap();
    let sess = snap.sessions.iter().find(|s| s.id == sid).unwrap();
    assert_eq!(sess.state, SessionState::Running);
    assert_eq!(sess.title, "auth work");
    assert!(matches!(
        sess.status_detail,
        Some(StatusDetail::Fact { .. })
    ));

    // Cursor replay: second event only.
    let events = store.events_since(1, 100).await.unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0].payload,
        EventPayload::SessionStateChanged { .. }
    ));
}

#[tokio::test]
async fn intent_ledger_dedupes_and_resolves() {
    let store = Store::open_in_memory().await.unwrap();
    let key = IdempotencyKey::new();
    let op = OperationId::new();

    store
        .record_intent(&Command {
            id: op,
            idempotency_key: key.clone(),
            actor: Actor::User,
            kind: CommandKind::TaskCreate {
                workspace_id: WorkspaceId::new(),
                title: "t".into(),
                description: None,
                steps: vec![],
            },
        })
        .await
        .unwrap();
    assert!(store.intent_seen(&key).await.unwrap());

    let outcome = CommandOutcome::Applied {
        result: serde_json::json!({"session_id": "s-1"}),
    };
    store.resolve_intent(&key, &outcome).await.unwrap();
    assert!(store.pending_intents().await.unwrap().is_empty());
}

#[tokio::test]
async fn attention_items_project_and_expire() {
    let store = Store::open_in_memory().await.unwrap();
    let sid = SessionId::new();
    let wid = WorkspaceId::new();
    let aid = AttentionId::new();
    let _ = &wid;
    let pa = PendingAction {
        kind: ActionKind::Exec,
        display: "rm -rf x".into(),
        args: serde_json::json!({"cmd": "rm -rf x"}),
        args_hash: String::new(),
        provider_ref: None,
    };

    store
        .append(NewEvent::new(
            EventSource::Adapter,
            EventPayload::AttentionRaised {
                attention_id: aid.clone(),
                kind: AttentionKind::Approval,
                session_id: Some(sid.clone()),
                workspace_id: wid.clone(),
                run_id: None,
                title: "Approve?".into(),
                body: "rm".into(),
                pending_action: Some(pa),
                choices: vec![],
                provider_request_ref: Some("r1".into()),
                expires_with_session: true,
            },
        ))
        .await
        .unwrap();

    let open = store
        .list_attention(Some(AttentionStatus::Open))
        .await
        .unwrap();
    assert_eq!(open.len(), 1);

    store.expire_attention_for_session(&sid).await.unwrap();
    let open = store
        .list_attention(Some(AttentionStatus::Open))
        .await
        .unwrap();
    assert!(open.is_empty());
    let item = store.get_attention(&aid).await.unwrap();
    assert_eq!(item.status, AttentionStatus::Expired);
}
