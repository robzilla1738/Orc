//! ACP adapter — Agent Client Protocol v1 over stdio JSON-RPC. This is
//! the expansion path: any ACP-compatible agent plugs in here without
//! interface changes. Stable-v1 surface only; negotiated capabilities
//! come from `initialize`.

use crate::error::{AdapterError, AdapterResult};
use crate::jsonrpc::{ref_to_value, Incoming, StdioJsonRpc};
use crate::traits::*;
use async_trait::async_trait;
use orc_domain::*;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};

pub struct AcpAdapter;

impl AcpAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AcpAdapter {
    fn default() -> Self {
        Self::new()
    }
}

struct AcpCore {
    rpc: StdioJsonRpc,
    acp_session_id: Mutex<Option<String>>,
}

#[async_trait]
impl AgentAdapter for AcpAdapter {
    fn provider(&self) -> ProviderKind {
        ProviderKind::Acp
    }

    fn base_capabilities(&self) -> Capabilities {
        let mut c = Capabilities::all_unknown();
        c.streaming = Support::supported();
        c.structured_approvals = Support::supported();
        c.structured_questions = Support::Supported {
            constraint: Some("permission/options requests".into()),
        };
        c.cancel = Support::supported();
        // Everything else depends on the agent's negotiated capabilities
        // — reported per-session after initialize.
        c
    }

    async fn spawn(&self, spec: SpawnSpec) -> AdapterResult<AdapterSession> {
        // executable is e.g. `npx <agent>` or a binary; spec.args carries
        // the agent command line after the executable.
        let mut cmd = Command::new(&spec.executable);
        for arg in &spec.args {
            cmd.arg(arg);
        }
        cmd.current_dir(&spec.cwd)
            .envs(spec.env.iter().cloned())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| AdapterError::Spawn(e.to_string()))?;
        let pid = child.id();
        let started_at = chrono::Utc::now();
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let (incoming_tx, mut incoming_rx) = mpsc::channel::<Incoming>(512);
        let (rpc, _reader) = StdioJsonRpc::start(stdin, stdout, incoming_tx);
        let (event_tx, event_rx) = mpsc::channel::<AdapterEvent>(512);
        let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<AdapterControl>(64);

        {
            let event_tx = event_tx.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt, BufReader};
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let _ = event_tx
                        .send(AdapterEvent::Output {
                            stream: Stream::Stderr,
                            text: line,
                        })
                        .await;
                }
            });
        }

        let core = Arc::new(AcpCore {
            rpc: rpc.clone(),
            acp_session_id: Mutex::new(None),
        });

        // Notifications + agent→client requests.
        {
            let event_tx = event_tx.clone();
            let core = core.clone();
            tokio::spawn(async move {
                while let Some(msg) = incoming_rx.recv().await {
                    match msg {
                        Incoming::Notification { method, params } => {
                            if method == "session/update" {
                                handle_update(&params, &event_tx).await;
                            }
                        }
                        Incoming::Request { id, method, params } => {
                            handle_agent_request(id, &method, &params, &event_tx, &core).await;
                        }
                    }
                }
                let _ = event_tx
                    .send(AdapterEvent::Exited {
                        code: None,
                        reason: Some("agent closed stdio".into()),
                    })
                    .await;
            });
        }

        // Control loop.
        {
            let core = core.clone();
            tokio::spawn(async move {
                while let Some(cmd) = ctrl_rx.recv().await {
                    match cmd {
                        AdapterControl::Send { text } | AdapterControl::Steer { text } => {
                            if let Some(sid) = core.acp_session_id.lock().await.clone() {
                                // session/prompt is a request; the turn
                                // ends when its result arrives.
                                let _ = core
                                    .rpc
                                    .call(
                                        "session/prompt",
                                        json!({
                                            "sessionId": sid,
                                            "prompt": [{"type": "text", "text": text}],
                                        }),
                                    )
                                    .await;
                            }
                        }
                        AdapterControl::Answer {
                            request_ref,
                            answer,
                        } => {
                            let result = match answer {
                                AdapterAnswer::Approve { option_id } => json!({
                                    "outcome": {
                                        "outcome": "selected",
                                        "optionId": option_id.unwrap_or_else(|| "allow".into()),
                                    }
                                }),
                                AdapterAnswer::Deny { .. } => json!({
                                    "outcome": { "outcome": "cancelled" }
                                }),
                                AdapterAnswer::Text(t) => json!({
                                    "outcome": { "outcome": "selected", "optionId": t }
                                }),
                            };
                            let _ = core.rpc.respond(ref_to_value(&request_ref), result);
                        }
                        AdapterControl::Cancel => {
                            if let Some(sid) = core.acp_session_id.lock().await.clone() {
                                let _ = core
                                    .rpc
                                    .notify("session/cancel", json!({ "sessionId": sid }));
                            }
                        }
                        AdapterControl::Shutdown => {}
                    }
                }
            });
        }

        tokio::spawn(async move {
            let _ = child.wait().await;
        });

        // Negotiate.
        let init = rpc
            .call(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {
                        "fs": { "readTextFile": false, "writeTextFile": false },
                        "terminal": false,
                    },
                    "clientInfo": { "name": "orc", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;

        let caps = self_caps_from_initialize(&init);

        let new_res = rpc
            .call("session/new", json!({ "cwd": spec.cwd, "mcpServers": [] }))
            .await?;
        let acp_id = new_res
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| AdapterError::Protocol("session/new returned no sessionId".into()))?
            .to_string();
        *core.acp_session_id.lock().await = Some(acp_id.clone());

        // Negotiated per-session; nothing further to sharpen here.

        let _ = event_tx
            .send(AdapterEvent::Started {
                native_id: Some(acp_id),
            })
            .await;

        if !spec.prompt.is_empty() {
            let core2 = core.clone();
            tokio::spawn(async move {
                let sid = core2.acp_session_id.lock().await.clone();
                if let Some(sid) = sid {
                    let res = core2
                        .rpc
                        .call(
                            "session/prompt",
                            json!({
                                "sessionId": sid,
                                "prompt": [{"type": "text", "text": spec.prompt}],
                            }),
                        )
                        .await;
                    let stop = res
                        .as_ref()
                        .ok()
                        .and_then(|r| r.get("stopReason"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let _ = core2
                        .rpc
                        .notify("internal/turnFinished", json!({ "stopReason": stop }));
                }
            });
        }

        Ok(AdapterSession {
            pid,
            process_started_at: Some(started_at),
            native_id: None,
            capabilities: caps,
            events: event_rx,
            control: ctrl_tx,
        })
    }

    async fn import(&self, spec: ImportSpec) -> AdapterResult<AdapterSession> {
        // ACP `session/load` requires agent capability `loadSession`.
        // We spawn normally first — the negotiation decides.
        let _ = spec;
        Err(AdapterError::Unsupported(
            "import requires the agent to declare loadSession; connect via spawn first".into(),
        ))
    }
}

fn self_caps_from_initialize(init: &Value) -> Capabilities {
    let mut c = Capabilities::all_unknown();
    let agent_caps = init.get("agentCapabilities").cloned().unwrap_or(json!({}));
    let supports = |key: &str| {
        agent_caps
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };

    c.streaming = Support::supported();
    c.structured_approvals = Support::supported();
    c.structured_questions = Support::supported();
    c.cancel = Support::supported();
    c.resume = if supports("loadSession") {
        Support::supported()
    } else {
        Support::unsupported("agent does not advertise loadSession")
    };
    c.import_session = c.resume.clone();
    c.mid_turn_steer = Support::Unknown;
    c.usage_reporting = if supports("usage") {
        Support::supported()
    } else {
        Support::unsupported("not advertised")
    };
    c.list_sessions = Support::unsupported("ACP has no session-list call");
    c.fork = Support::unsupported("not in ACP v1");
    c.reconnect = Support::unsupported("stdio process per session");
    c.artifact_reporting = Support::Unknown;
    c
}

async fn handle_update(params: &Value, event_tx: &mpsc::Sender<AdapterEvent>) {
    let update = match params.get("update") {
        Some(u) => u,
        None => return,
    };
    let kind = update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or("");
    match kind {
        "agent_message_chunk" | "agent_thought_chunk" => {
            if let Some(text) = update
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(Value::as_str)
            {
                let _ = event_tx
                    .send(AdapterEvent::Output {
                        stream: Stream::Agent,
                        text: text.to_string(),
                    })
                    .await;
            }
        }
        "tool_call" | "tool_call_update" => {
            let title = update
                .get("title")
                .or_else(|| update.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let _ = event_tx
                .send(AdapterEvent::Meaningful {
                    summary: format!("tool: {title}"),
                })
                .await;
        }
        "plan" => {
            let _ = event_tx
                .send(AdapterEvent::Meaningful {
                    summary: "plan updated".into(),
                })
                .await;
        }
        _ => {}
    }
}

async fn handle_agent_request(
    id: Value,
    method: &str,
    params: &Value,
    event_tx: &mpsc::Sender<AdapterEvent>,
    core: &Arc<AcpCore>,
) {
    match method {
        "session/request_permission" => {
            let title = params
                .get("toolCall")
                .and_then(|t| t.get("title"))
                .and_then(Value::as_str)
                .unwrap_or("permission requested")
                .to_string();
            let options: Vec<String> = params
                .get("options")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|o| o.get("name").and_then(Value::as_str).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let _ = event_tx
                .send(AdapterEvent::ApprovalRequested {
                    request_ref: id.to_string(),
                    action: ProposedAction {
                        kind: ActionKind::Other,
                        display: format!("{title} ({})", options.join(" / ")),
                        args: params.clone(),
                    },
                })
                .await;
        }
        // Agent asks us for fs/terminal work we don't provide — answer
        // with a JSON-RPC error so the agent can adapt.
        _ => {
            let _ = core
                .rpc
                .respond_error(id, -32601, "capability not provided by Orc");
        }
    }
}
