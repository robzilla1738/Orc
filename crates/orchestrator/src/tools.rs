//! The typed tool surface — a restricted client of the runtime. Same
//! command service as buttons and the CLI; no shell access.

use crate::backend::ToolDef;
use orc_domain::*;
use orc_runtime::Runtime;
use serde_json::{json, Value};

/// Every tool the model may propose.
pub fn tool_defs() -> Vec<ToolDef> {
    let obj = |props: Value, required: &[&str]| {
        json!({
            "type": "object",
            "properties": props,
            "required": required,
        })
    };
    vec![
        ToolDef {
            name: "tools.list",
            description: "List installed agent tools and their readiness.",
            input_schema: obj(json!({}), &[]),
        },
        ToolDef {
            name: "workspaces.list",
            description: "List workspaces.",
            input_schema: obj(json!({}), &[]),
        },
        ToolDef {
            name: "sessions.list",
            description: "List sessions with state, ownership, and latest update.",
            input_schema: obj(
                json!({
                    "workspace_id": {"type": "string"},
                    "active_only": {"type": "boolean"},
                }),
                &[],
            ),
        },
        ToolDef {
            name: "sessions.inspect",
            description: "Get one session's full state, runs, and recent output preview.",
            input_schema: obj(json!({"session_id": {"type": "string"}}), &["session_id"]),
        },
        ToolDef {
            name: "sessions.start",
            description: "Start a managed agent session. Returns a session id immediately.",
            input_schema: obj(
                json!({
                    "tool_instance_id": {"type": "string"},
                    "workspace_id": {"type": "string"},
                    "title": {"type": "string"},
                    "prompt": {"type": "string"},
                    "task_id": {"type": "string"},
                    "launch": {"type": "object"},
                }),
                &["tool_instance_id", "workspace_id", "title", "prompt"],
            ),
        },
        ToolDef {
            name: "sessions.send",
            description: "Send input to a session's next turn.",
            input_schema: obj(
                json!({"session_id": {"type": "string"}, "text": {"type": "string"}}),
                &["session_id", "text"],
            ),
        },
        ToolDef {
            name: "sessions.cancel",
            description: "Cancel a session's running turn.",
            input_schema: obj(json!({"session_id": {"type": "string"}}), &["session_id"]),
        },
        ToolDef {
            name: "sessions.resume",
            description: "Resume a persisted session where the adapter supports it.",
            input_schema: obj(json!({"session_id": {"type": "string"}}), &["session_id"]),
        },
        ToolDef {
            name: "attention.list",
            description: "List open attention items (approvals, questions, auth failures).",
            input_schema: obj(json!({}), &[]),
        },
        ToolDef {
            name: "attention.answer_question",
            description: "Answer a QUESTION item with text or a choice. Approvals need a human — never call this for them.",
            input_schema: obj(
                json!({
                    "attention_id": {"type": "string"},
                    "text": {"type": "string"},
                    "choice": {"type": "integer"},
                }),
                &["attention_id"],
            ),
        },
        ToolDef {
            name: "tasks.create",
            description: "Create a task with an optional dependency graph of steps.",
            input_schema: obj(
                json!({
                    "workspace_id": {"type": "string"},
                    "title": {"type": "string"},
                    "description": {"type": "string"},
                }),
                &["workspace_id", "title"],
            ),
        },
        ToolDef {
            name: "artifacts.read",
            description: "Read a session's registered artifacts.",
            input_schema: obj(json!({"session_id": {"type": "string"}}), &["session_id"]),
        },
    ]
}

/// Validate + dispatch one tool call. Side-effecting tools become
/// `Command`s on the runtime (actor: Orchestrator); reads query state.
pub async fn execute(runtime: &Runtime, name: &str, input: &Value) -> Result<Value, CommandError> {
    let cmd = |kind: CommandKind| Command {
        id: OperationId::new(),
        idempotency_key: IdempotencyKey::new(),
        actor: Actor::Orchestrator,
        kind,
    };
    match name {
        "tools.list" => {
            let tools = runtime.store().list_tools().await.map_err(into_cmd)?;
            Ok(json!(tools))
        }
        "workspaces.list" => {
            let ws = runtime.store().list_workspaces().await.map_err(into_cmd)?;
            Ok(json!(ws))
        }
        "sessions.list" => {
            let mut sessions = runtime.store().list_sessions().await.map_err(into_cmd)?;
            if input
                .get("active_only")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                sessions.retain(|s| s.state.is_active());
            }
            if let Some(ws) = input.get("workspace_id").and_then(Value::as_str) {
                sessions.retain(|s| s.workspace_id.as_str() == ws);
            }
            Ok(json!(sessions))
        }
        "sessions.inspect" => {
            let id = SessionId::from_raw(req_str(input, "session_id")?);
            let session = runtime.store().get_session(&id).await.map_err(into_cmd)?;
            let mut tail = String::new();
            for stream in [Stream::Agent, Stream::Stdout] {
                if let Ok((_, bytes)) = runtime.transcripts().tail(&id, stream, 8 * 1024).await {
                    tail.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
            let runs = runtime
                .store()
                .list_runs()
                .await
                .map_err(into_cmd)?
                .into_iter()
                .filter(|r| r.session_id == id)
                .collect::<Vec<_>>();
            Ok(json!({"session": session, "runs": runs, "recent_output": tail}))
        }
        "sessions.start" => {
            let kind = CommandKind::SessionStart {
                tool_instance_id: ToolInstanceId::from_raw(req_str(input, "tool_instance_id")?),
                workspace_id: WorkspaceId::from_raw(req_str(input, "workspace_id")?),
                task_id: input
                    .get("task_id")
                    .and_then(Value::as_str)
                    .map(TaskId::from_raw),
                title: req_str(input, "title")?,
                prompt: req_str(input, "prompt")?,
                launch: input.get("launch").cloned().unwrap_or(json!({})),
            };
            exec(runtime, cmd(kind)).await
        }
        "sessions.send" => {
            let kind = CommandKind::SessionSend {
                session_id: SessionId::from_raw(req_str(input, "session_id")?),
                text: req_str(input, "text")?,
            };
            exec(runtime, cmd(kind)).await
        }
        "sessions.cancel" => {
            let kind = CommandKind::SessionCancel {
                session_id: SessionId::from_raw(req_str(input, "session_id")?),
            };
            exec(runtime, cmd(kind)).await
        }
        "sessions.resume" => {
            let kind = CommandKind::SessionResume {
                session_id: SessionId::from_raw(req_str(input, "session_id")?),
            };
            exec(runtime, cmd(kind)).await
        }
        "attention.list" => {
            let items = runtime
                .store()
                .list_attention(Some(AttentionStatus::Open))
                .await
                .map_err(into_cmd)?;
            Ok(json!(items))
        }
        "attention.answer_question" => {
            // The orchestrator may only answer questions — never grant
            // approvals on its own authority.
            let att_id = AttentionId::from_raw(req_str(input, "attention_id")?);
            let item = runtime
                .store()
                .get_attention(&att_id)
                .await
                .map_err(into_cmd)?;
            if item.kind != AttentionKind::Question {
                return Err(CommandError::PolicyDenied(
                    "orchestrator may only answer questions; approvals need a human".into(),
                ));
            }
            let answer = if let Some(i) = input.get("choice").and_then(Value::as_u64) {
                Answer::Choice { index: i as u32 }
            } else {
                Answer::Text {
                    text: req_str(input, "text")?,
                }
            };
            let kind = CommandKind::AttentionAnswer {
                attention_id: att_id,
                answer,
            };
            exec(runtime, cmd(kind)).await
        }
        "tasks.create" => {
            let kind = CommandKind::TaskCreate {
                workspace_id: WorkspaceId::from_raw(req_str(input, "workspace_id")?),
                title: req_str(input, "title")?,
                description: input
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                steps: vec![],
            };
            exec(runtime, cmd(kind)).await
        }
        "artifacts.read" => {
            let id = SessionId::from_raw(req_str(input, "session_id")?);
            let snap = runtime.store().snapshot().await.map_err(into_cmd)?;
            let artifacts: Vec<_> = snap
                .sessions
                .iter()
                .find(|s| s.id == id)
                .map(|_| ())
                .into_iter()
                .collect();
            let _ = artifacts;
            // Artifacts live in the events' projections; list via scan.
            let events = runtime
                .store()
                .events_since(0, 10_000)
                .await
                .map_err(into_cmd)?;
            let arts: Vec<Value> = events
                .iter()
                .filter_map(|e| match &e.payload {
                    EventPayload::ArtifactRegistered {
                        artifact_id,
                        session_id,
                        label,
                        path,
                        ..
                    } if session_id.as_ref() == Some(&id) => Some(json!({
                        "id": artifact_id, "label": label, "path": path,
                    })),
                    _ => None,
                })
                .collect();
            Ok(json!(arts))
        }
        _ => Err(CommandError::InvalidInput(format!("unknown tool {name}"))),
    }
}

async fn exec(runtime: &Runtime, cmd: Command) -> Result<Value, CommandError> {
    runtime
        .execute(cmd)
        .await
        .map(|outcome| serde_json::to_value(outcome).unwrap_or(Value::Null))
        .map_err(|e| CommandError::Internal(e.to_string()))
}

fn req_str(input: &Value, key: &str) -> Result<String, CommandError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| CommandError::InvalidInput(format!("missing '{key}'")))
}

fn into_cmd(e: orc_storage::StorageError) -> CommandError {
    CommandError::Internal(e.to_string())
}
