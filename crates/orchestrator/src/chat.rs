//! The chat loop: user message → runtime state → model proposes typed
//! operations → validate/dispatch → operation cards + prose. Bounded:
//! max turns, budget caps, inference labels, never self-approving.

use crate::backend::{Block, Completion, Message, ModelBackend, Role};
use crate::tools;
use orc_domain::*;
use orc_runtime::Runtime;
use orc_storage::ChatMessage;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

/// Hard bound on model calls per user message.
const MAX_TURNS: usize = 8;

pub struct ChatService {
    runtime: Runtime,
    backend: Arc<dyn ModelBackend>,
}

/// What the interface renders: prose + operation cards.
#[derive(Debug, Clone)]
pub struct ChatReply {
    pub message_id: String,
    pub text: String,
    /// Operation ids dispatched (each renders as a card).
    pub operations: Vec<OperationId>,
    /// Tool call results for transparency.
    pub results: Vec<Value>,
    /// True when the model was unavailable and this is a static reply.
    pub offline: bool,
}

impl ChatService {
    pub fn new(runtime: Runtime, backend: Arc<dyn ModelBackend>) -> Self {
        Self { runtime, backend }
    }

    /// Handle one user message end-to-end.
    pub async fn handle(&self, user_text: &str) -> ChatReply {
        // Persist the user message.
        let user_id = Uuid::now_v7().simple().to_string();
        let _ = self
            .runtime
            .store()
            .append_chat_message(&ChatMessage {
                id: user_id.clone(),
                role: "user".into(),
                content: json!({"text": user_text}),
                operations: vec![],
                at: chrono::Utc::now(),
            })
            .await;
        let _ = self
            .runtime
            .store()
            .append(NewEvent::new(
                EventSource::Orchestrator,
                EventPayload::ChatMessage {
                    message_id: user_id,
                    role: "user".into(),
                    operations: vec![],
                    preview: user_text.chars().take(200).collect(),
                },
            ))
            .await;

        // Load history + live state for grounding.
        let history = self
            .runtime
            .store()
            .chat_history(40)
            .await
            .unwrap_or_default();
        let snapshot = self.runtime.store().snapshot().await.unwrap_or_default();
        let system = system_prompt(&snapshot);

        let mut messages: Vec<Message> = history
            .iter()
            .map(|m| Message {
                role: if m.role == "user" {
                    Role::User
                } else {
                    Role::Assistant
                },
                blocks: vec![Block::Text {
                    text: m
                        .content
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                }],
            })
            .collect();
        if messages.last().map(|m| m.role) != Some(Role::User) {
            messages.push(Message {
                role: Role::User,
                blocks: vec![Block::Text {
                    text: user_text.to_string(),
                }],
            });
        }

        let defs = tools::tool_defs();
        let mut operations = Vec::new();
        let mut results = Vec::new();
        let mut final_text = String::new();

        for _ in 0..MAX_TURNS {
            let completion = match self.backend.complete(&system, &messages, &defs).await {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, "model unavailable");
                    return self.offline_reply(user_text, &snapshot).await;
                }
            };
            final_text = text_of(&completion);

            let calls: Vec<(String, String, Value)> = completion
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::ToolUse { id, name, input } => {
                        Some((id.clone(), name.clone(), input.clone()))
                    }
                    _ => None,
                })
                .collect();
            if calls.is_empty() || completion.stop_reason != "tool_use" {
                break;
            }

            // Assistant turn includes the tool uses.
            messages.push(Message {
                role: Role::Assistant,
                blocks: completion.blocks.clone(),
            });

            // Execute each typed tool through the same command service.
            let mut tool_results = Vec::new();
            for (call_id, name, input) in calls {
                let res = tools::execute(&self.runtime, &name, &input).await;
                let (content, is_error) = match res {
                    Ok(v) => {
                        // Surface operation ids for cards.
                        if let Some(op) = v.get("operation_id").and_then(Value::as_str) {
                            operations.push(OperationId::from_raw(op.to_string()));
                        }
                        (v, false)
                    }
                    Err(e) => (json!({"error": e.to_string()}), true),
                };
                results.push(json!({"tool": name, "content": content}));
                tool_results.push(Block::ToolResult {
                    tool_use_id: call_id,
                    content,
                    is_error,
                });
            }
            messages.push(Message {
                role: Role::User,
                blocks: tool_results,
            });
        }

        let message_id = Uuid::now_v7().simple().to_string();
        let _ = self
            .runtime
            .store()
            .append_chat_message(&ChatMessage {
                id: message_id.clone(),
                role: "assistant".into(),
                content: json!({"text": final_text, "results": results}),
                operations: operations.iter().map(|o| o.as_str().to_string()).collect(),
                at: chrono::Utc::now(),
            })
            .await;
        let _ = self
            .runtime
            .store()
            .append(NewEvent::new(
                EventSource::Orchestrator,
                EventPayload::ChatMessage {
                    message_id: message_id.clone(),
                    role: "assistant".into(),
                    operations: operations.clone(),
                    preview: final_text.chars().take(200).collect(),
                },
            ))
            .await;

        ChatReply {
            message_id,
            text: final_text,
            operations,
            results,
            offline: false,
        }
    }

    /// Model-free fallback: the interface must still answer when
    /// inference is unavailable. Facts only — no fabrication.
    async fn offline_reply(&self, _text: &str, snapshot: &orc_storage::Snapshot) -> ChatReply {
        let active = snapshot
            .sessions
            .iter()
            .filter(|s| s.state.is_active())
            .count();
        let open_attention = snapshot
            .attention
            .iter()
            .filter(|a| a.status == AttentionStatus::Open)
            .count();
        let text = format!(
            "The orchestration model isn't configured, but the runtime is live. \
             {active} active session(s), {open_attention} open attention item(s). \
             Set ORC_MODEL_API_KEY (or ORC_MODEL_BASE for a local model) to enable \
             conversational control — everything remains operable from the panels."
        );
        let message_id = Uuid::now_v7().simple().to_string();
        let _ = self
            .runtime
            .store()
            .append_chat_message(&ChatMessage {
                id: message_id.clone(),
                role: "assistant".into(),
                content: json!({"text": text}),
                operations: vec![],
                at: chrono::Utc::now(),
            })
            .await;
        ChatReply {
            message_id,
            text,
            operations: vec![],
            results: vec![],
            offline: true,
        }
    }
}

fn text_of(c: &Completion) -> String {
    c.blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Facts and interpretations are presented differently — the prompt
/// carries the live snapshot so the model grounds its answers.
fn system_prompt(snapshot: &orc_storage::Snapshot) -> String {
    let state = serde_json::json!({
        "workspaces": snapshot.workspaces.iter().map(|w| json!({
            "id": w.id, "name": w.name, "root": w.root,
        })).collect::<Vec<_>>(),
        "tools": snapshot.tools.iter().map(|t| json!({
            "id": t.id, "provider": t.provider, "label": t.label,
            "readiness": t.readiness, "version": t.version,
        })).collect::<Vec<_>>(),
        "sessions": snapshot.sessions.iter().map(|s| json!({
            "id": s.id, "title": s.title, "provider": s.provider,
            "state": s.state, "ownership": s.ownership,
            "last_update": s.last_meaningful_update,
        })).collect::<Vec<_>>(),
        "open_attention": snapshot.attention.iter()
            .filter(|a| a.status == AttentionStatus::Open)
            .map(|a| json!({
                "id": a.id, "kind": a.kind, "title": a.title,
                "session_id": a.session_id,
            }))
            .collect::<Vec<_>>(),
    });
    format!(
        "You are Orc, a local AI session manager. The runtime owns the truth: \
         sessions, permissions, execution. You interpret requests and propose \
         typed operations via tools — you never claim an action happened unless \
         a tool result says so. Rules:\n\
         - Distinguish facts (protocol-reported) from inferences; label inference.\n\
         - Observed external processes are NOT controllable; say so.\n\
         - Never answer approval items; those are for humans. You MAY answer questions.\n\
         - Long operations return ids immediately; tell the user to watch the live view.\n\
         - When asked what's running, prefer sessions.list facts over memory.\n\n\
         Current runtime state:\n{}",
        serde_json::to_string_pretty(&state).unwrap_or_default()
    )
}
