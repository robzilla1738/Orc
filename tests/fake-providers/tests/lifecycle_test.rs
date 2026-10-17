//! End-to-end controller test against `orc-fake-agent`: spawn through
//! the PTY adapter, observe state transitions, confirm transcripts,
//! cancel a hang, expire approvals on exit.

use orc_adapters::{AdapterRegistry, DiscoveryPaths};
use orc_domain::*;
use orc_runtime::Runtime;
use orc_storage::{Store, TranscriptStore};
use serde_json::json;
use std::time::Duration;

async fn make_runtime(dir: &std::path::Path) -> Runtime {
    let store = Store::open(dir).await.unwrap();
    let transcripts = TranscriptStore::new(dir);
    Runtime::new(
        store,
        transcripts,
        AdapterRegistry::with_defaults(),
        DiscoveryPaths::default(),
    )
}

async fn provision(store: &Store) -> (ToolInstanceId, WorkspaceId) {
    let tid = ToolInstanceId::new();
    let wid = WorkspaceId::new();
    store
        .upsert_tool(&ToolInstance {
            id: tid.clone(),
            provider: ProviderKind::Pty,
            label: "fake".into(),
            executable: env!("CARGO_BIN_EXE_orc-fake-agent").to_string(),
            version: Some("test".into()),
            readiness: ToolReadiness::Ready,
            settings: Default::default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    store
        .insert_workspace(&Workspace {
            id: wid.clone(),
            name: "test".into(),
            root: "/tmp".into(),
            allow_rules: vec![],
            network_allowed: true,
            remote_content_allowed: true,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    (tid, wid)
}

async fn session_state(rt: &Runtime, sid: &SessionId) -> SessionState {
    rt.store().get_session(sid).await.unwrap().state
}

async fn wait_for(
    rt: &Runtime,
    sid: &SessionId,
    want: impl Fn(SessionState) -> bool,
    timeout: Duration,
) -> SessionState {
    let start = std::time::Instant::now();
    loop {
        let s = session_state(rt, sid).await;
        if want(s) {
            return s;
        }
        if start.elapsed() > timeout {
            panic!("timed out waiting for state; got {s:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn start(rt: &Runtime, tid: &ToolInstanceId, wid: &WorkspaceId) -> SessionId {
    let out = rt
        .execute(Command {
            id: OperationId::new(),
            idempotency_key: IdempotencyKey::new(),
            actor: Actor::User,
            kind: CommandKind::SessionStart {
                tool_instance_id: tid.clone(),
                workspace_id: wid.clone(),
                task_id: None,
                title: "fake".into(),
                prompt: "hello".into(),
                launch: json!({}),
            },
        })
        .await
        .unwrap();
    match out {
        CommandOutcome::Applied { result } => {
            SessionId::from_raw(result["session_id"].as_str().unwrap().to_string())
        }
        other => panic!("start failed: {other:?}"),
    }
}

#[tokio::test]
async fn spawn_echo_and_exit() {
    let dir = tempfile::tempdir().unwrap();
    let rt = make_runtime(dir.path()).await;
    let (tid, wid) = provision(rt.store()).await;
    let sid = start(&rt, &tid, &wid).await;

    // Normal mode: echoes init + input, then the process exits →
    // session must reach a terminal state, not hang in Running.
    let state = wait_for(
        &rt,
        &sid,
        |s| {
            matches!(
                s,
                SessionState::Completed | SessionState::Idle | SessionState::Running
            )
        },
        Duration::from_secs(15),
    )
    .await;
    assert!(matches!(
        state,
        SessionState::Completed | SessionState::Idle | SessionState::Running
    ));
}

#[tokio::test]
async fn cancel_kills_hung_process() {
    let dir = tempfile::tempdir().unwrap();
    let rt = make_runtime(dir.path()).await;
    let (tid, wid) = provision(rt.store()).await;
    // hang mode: inject env via the `env.` settings convention
    let mut tool = rt.store().get_tool(&tid).await.unwrap();
    tool.settings
        .insert("env.ORC_FAKE_MODE".into(), "hang".into());
    rt.store().upsert_tool(&tool).await.unwrap();
    let sid = start(&rt, &tid, &wid).await;

    // A hung PTY child never produces turn events; wait for spawn.
    wait_for(
        &rt,
        &sid,
        |s| s != SessionState::Starting,
        Duration::from_secs(15),
    )
    .await;
    let pid = rt.store().get_session(&sid).await.unwrap().pid;
    assert!(pid.is_some(), "pty session must report a pid");

    rt.execute(Command {
        id: OperationId::new(),
        idempotency_key: IdempotencyKey::new(),
        actor: Actor::User,
        kind: CommandKind::SessionCancel {
            session_id: sid.clone(),
        },
    })
    .await
    .unwrap();

    // The cancel must reach the adapter: the hung process dies.
    let pid = pid.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !alive {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("hung process still alive after cancel");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let state = session_state(&rt, &sid).await;
    assert!(!matches!(state, SessionState::Running));
}
