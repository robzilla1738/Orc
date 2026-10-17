//! `ModelBackend` — provider-neutral model calls. rig-style surface,
//! Orc-owned validation. The orchestrator's model config is separate
//! from worker-agent credentials: an installed coding-agent
//! subscription does not fund the orchestration model.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("model unavailable: {0}")]
    Unavailable(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("bad model response: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    /// Text + tool call/result blocks, provider-agnostic.
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Value,
        is_error: bool,
    },
}

/// One model turn's output.
#[derive(Debug, Clone)]
pub struct Completion {
    pub blocks: Vec<Block>,
    /// `stop_reason` — "tool_use" means the model wants another round.
    pub stop_reason: String,
}

/// A callable tool the model may propose.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema for the input object.
    pub input_schema: Value,
}

#[async_trait]
pub trait ModelBackend: Send + Sync {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Completion, BackendError>;
}

// ---------------------------------------------------------------------
// Anthropic-compatible backend
// ---------------------------------------------------------------------

pub struct AnthropicBackend {
    base: String,
    api_key: String,
    model: String,
    client: reqwest::Client,
    max_tokens: u32,
}

impl AnthropicBackend {
    /// `ORC_MODEL_API_KEY`, `ORC_MODEL_BASE` (default Anthropic),
    /// `ORC_MODEL` (default claude-sonnet-4-5).
    pub fn from_env() -> Result<Self, BackendError> {
        let api_key = std::env::var("ORC_MODEL_API_KEY").map_err(|_| {
            BackendError::Unavailable(
                "ORC_MODEL_API_KEY unset — configure an API key or a local model".into(),
            )
        })?;
        Ok(Self {
            base: std::env::var("ORC_MODEL_BASE")
                .unwrap_or_else(|_| "https://api.anthropic.com".into()),
            api_key,
            model: std::env::var("ORC_MODEL").unwrap_or_else(|_| "claude-sonnet-4-5".into()),
            client: reqwest::Client::new(),
            max_tokens: 2048,
        })
    }
}

#[async_trait]
impl ModelBackend for AnthropicBackend {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Completion, BackendError> {
        let msgs: Vec<Value> = messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                    "content": m.blocks.iter().map(|b| match b {
                        Block::Text { text } => serde_json::json!({"type":"text","text":text}),
                        Block::ToolUse { id, name, input } => serde_json::json!({
                            "type":"tool_use","id":id,"name":name,"input":input
                        }),
                        Block::ToolResult { tool_use_id, content, is_error } => serde_json::json!({
                            "type":"tool_result","tool_use_id":tool_use_id,
                            "content":content,"is_error":is_error
                        }),
                    }).collect::<Vec<_>>()
                })
            })
            .collect();
        let tool_list: Vec<Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();
        let res = self
            .client
            .post(format!("{}/v1/messages", self.base.trim_end_matches('/')))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&serde_json::json!({
                "model": self.model,
                "max_tokens": self.max_tokens,
                "system": system,
                "messages": msgs,
                "tools": tool_list,
            }))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| BackendError::Unavailable(e.to_string()))?
            .json::<Value>()
            .await?;

        let mut blocks = Vec::new();
        for item in res
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            match item.get("type").and_then(Value::as_str) {
                Some("text") => blocks.push(Block::Text {
                    text: item
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                }),
                Some("tool_use") => blocks.push(Block::ToolUse {
                    id: item
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    input: item.get("input").cloned().unwrap_or(Value::Null),
                }),
                _ => {}
            }
        }
        Ok(Completion {
            blocks,
            stop_reason: res
                .get("stop_reason")
                .and_then(Value::as_str)
                .unwrap_or("end_turn")
                .to_string(),
        })
    }
}

// ---------------------------------------------------------------------
// Mock backend — deterministic replies for tests/dev without a key.
// ---------------------------------------------------------------------

/// A scripted backend: first response is a tool call (if `tool_call`
/// set), subsequent replies echo `reply`. Used by tests and by
/// `ORC_MODEL_BACKEND=mock` development.
pub struct MockBackend {
    pub tool_call: Option<(String, Value)>,
    pub reply: String,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self {
            tool_call: None,
            reply: "Mock orchestrator (no model configured).".into(),
        }
    }
}

#[async_trait]
impl ModelBackend for MockBackend {
    async fn complete(
        &self,
        _system: &str,
        messages: &[Message],
        _tools: &[ToolDef],
    ) -> Result<Completion, BackendError> {
        // If the last message is a tool result, the tool loop is done —
        // return prose.
        let last_was_tool_result = messages
            .last()
            .map(|m| {
                m.blocks
                    .iter()
                    .any(|b| matches!(b, Block::ToolResult { .. }))
            })
            .unwrap_or(false);
        if let (Some((name, input)), false) = (&self.tool_call, last_was_tool_result) {
            return Ok(Completion {
                blocks: vec![Block::ToolUse {
                    id: "mock-call-1".into(),
                    name: name.clone(),
                    input: input.clone(),
                }],
                stop_reason: "tool_use".into(),
            });
        }
        Ok(Completion {
            blocks: vec![Block::Text {
                text: self.reply.clone(),
            }],
            stop_reason: "end_turn".into(),
        })
    }
}
