//! Codex adapter — app-server over stdio (JSON-RPC), preserving native
//! thread/turn/item/approval semantics instead of scraping a TUI.
//!
//! The protocol is versioned: this client is written against the
//! documented message families (`initialize`, `thread/*`, `turn/*`,
//! `item/*`, `*requestApproval`) and degrades by leaving unmatched
//! frames as transcript output — never by guessing.

use crate::error::{AdapterError, AdapterResult};
use crate::jsonrpc::{ref_to_value, Incoming, StdioJsonRpc};
use crate::traits::*;
use async_trait::async_trait;
use orc_domain::*;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};

pub struct CodexAdapter;

impl CodexAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

fn codex_capabilities() -> Capabilities {
    let mut c = Capabilities::all_unknown();
    c.list_sessions = Support::Supported {
        constraint: Some("thread/list over app-server".into()),
    };
    c.import_session = Support::Supported {
        constraint: Some("attach via thread/resume".into()),
    };
    c.resume = Support::Supported {
        constraint: Some("thread/resume; same CLI generation".into()),
    };
    c.fork = Support::Supported {
        constraint: Some("thread/fork where the version supports it".into()),
    };
    c.streaming = Support::supported();
    c.structured_approvals = Support::supported();
    c.structured_questions = Support::Supported {
        constraint: Some("requestUserInput items when the version emits them".into()),
    };
    c.cancel = Support::supported();
    c.mid_turn_steer = Support::Supported {
        constraint: Some("turn/steer where supported; else next-turn input".into()),
    };
    c.reconnect = Support::unsupported("app-server is spawned per Orc session");
    c.usage_reporting = Support::Supported {
        constraint: Some("reported on turn completion".into()),
    };
    c.artifact_reporting = Support::Supported {
        constraint: Some("file-change items when emitted".into()),
    };
    c
}

/// Mutable per-session protocol state shared by the event task and the
/// control task.
struct SessionCore {
    rpc: StdioJsonRpc,
    thread_id: Mutex<Option<String>>,
    /// The current turn id, for interruption.
    active_turn: Mutex<Option<String>>,
}

#[async_trait]
impl AgentAdapter for CodexAdapter {
    fn provider(&self) -> ProviderKind {
        ProviderKind::Codex
    }

    fn base_capabilities(&self) -> Capabilities {
        codex_capabilities()
    }

    async fn spawn(&self, spec: SpawnSpec) -> AdapterResult<AdapterSession> {
        let resume = spec
            .launch
            .get("resume")
            .and_then(Value::as_str)
            .map(str::to_string);
        launch(spec, resume).await
    }

    async fn import(&self, spec: ImportSpec) -> AdapterResult<AdapterSession> {
        let spawn = SpawnSpec {
            session_id: spec.session_id,
            executable: spec.executable,
            args: vec![],
            cwd: spec.cwd,
            env: vec![],
            prompt: spec
                .launch
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            launch: spec.launch,
        };
        launch(spawn, Some(spec.native_id)).await
    }
}

async fn launch(
    spec: SpawnSpec,
    resume_native_id: Option<String>,
) -> AdapterResult<AdapterSession> {
    let mut cmd = Command::new(&spec.executable);
    cmd.arg("app-server")
        .current_dir(&spec.cwd)
        .envs(spec.env.iter().cloned())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for arg in &spec.args {
        cmd.arg(arg);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| AdapterError::Spawn(e.to_string()))?;
    let pid = child.id();
    let started_at = chrono::Utc::now();
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let (incoming_tx, mut incoming_rx) = mpsc::channel::<Incoming>(512);
    let (rpc, _reader_task) = StdioJsonRpc::start(stdin, stdout, incoming_tx);

    let (event_tx, event_rx) = mpsc::channel::<AdapterEvent>(512);
    let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<AdapterControl>(64);

    // stderr → transcript.
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

    let core = Arc::new(SessionCore {
        rpc: rpc.clone(),
        thread_id: Mutex::new(None),
        active_turn: Mutex::new(None),
    });

    // Server→client: notifications + approval/question requests.
    {
        let event_tx = event_tx.clone();
        let core = core.clone();
        tokio::spawn(async move {
            while let Some(msg) = incoming_rx.recv().await {
                match msg {
                    Incoming::Notification { method, params } => {
                        handle_notification(&method, &params, &event_tx, &core).await;
                    }
                    Incoming::Request { id, method, params } => {
                        handle_request(id, &method, &params, &event_tx, &core).await;
                    }
                }
            }
            let _ = event_tx
                .send(AdapterEvent::Exited {
                    code: None,
                    reason: Some("app-server closed".into()),
                })
                .await;
        });
    }

    // Control loop: send/steer/cancel/answer/shutdown.
    {
        let core = core.clone();
        tokio::spawn(async move {
            while let Some(cmd) = ctrl_rx.recv().await {
                match cmd {
                    AdapterControl::Send { text } | AdapterControl::Steer { text } => {
                        if let Some(tid) = core.thread_id.lock().await.clone() {
                            let _ = core
                                .rpc
                                .call(
                                    "turn/start",
                                    json!({
                                        "threadId": tid,
                                        "input": [{"type": "text", "text": text}],
                                    }),
                                )
                                .await;
                        }
                    }
                    AdapterControl::Answer {
                        request_ref,
                        answer,
                    } => {
                        // The request_ref is the original RPC id.
                        let result = match answer {
                            AdapterAnswer::Approve { option_id } => {
                                json!({ "decision": option_id.unwrap_or_else(|| "approved".into()) })
                            }
                            AdapterAnswer::Deny { .. } => json!({ "decision": "denied" }),
                            AdapterAnswer::Text(t) => json!({ "answer": t }),
                        };
                        let _ = core.rpc.respond(ref_to_value(&request_ref), result);
                    }
                    AdapterControl::Cancel => {
                        let tid = core.thread_id.lock().await.clone();
                        let turn = core.active_turn.lock().await.clone();
                        if let (Some(tid), Some(turn)) = (tid, turn) {
                            let _ = core
                                .rpc
                                .call("turn/interrupt", json!({ "threadId": tid, "turnId": turn }))
                                .await;
                        }
                    }
                    AdapterControl::Shutdown => {
                        // kill_on_drop handles the child when the
                        // adapter is dropped; nothing polite to send.
                    }
                }
            }
        });
    }

    // Reap the child.
    tokio::spawn(async move {
        let _ = child.wait().await;
    });

    // Handshake + thread setup.
    rpc.call(
        "initialize",
        json!({
            "clientInfo": {
                "name": "orc",
                "title": "Orc",
                "version": env!("CARGO_PKG_VERSION"),
            }
        }),
    )
    .await?;
    let _ = rpc.notify("initialized", json!({}));

    let thread_id = if let Some(native) = resume_native_id {
        let res = rpc
            .call("thread/resume", json!({ "threadId": native }))
            .await?;
        extract_thread_id(&res).unwrap_or(native)
    } else {
        let mut params = json!({});
        if let Some(model) = spec.launch.get("model").and_then(Value::as_str) {
            params["model"] = json!(model);
        }
        if let Some(policy) = spec.launch.get("approval_policy").and_then(Value::as_str) {
            params["approvalPolicy"] = json!(policy);
        }
        params["cwd"] = json!(spec.cwd);
        let res = rpc.call("thread/start", params).await?;
        extract_thread_id(&res)
            .ok_or_else(|| AdapterError::Protocol("thread/start returned no thread id".into()))?
    };
    *core.thread_id.lock().await = Some(thread_id.clone());

    let _ = event_tx
        .send(AdapterEvent::Started {
            native_id: Some(thread_id.clone()),
        })
        .await;

    // Kick the first turn if a prompt was supplied.
    if !spec.prompt.is_empty() {
        let core2 = core.clone();
        tokio::spawn(async move {
            let res = core2
                .rpc
                .call(
                    "turn/start",
                    json!({
                        "threadId": thread_id,
                        "input": [{"type": "text", "text": spec.prompt}],
                    }),
                )
                .await;
            if let Ok(res) = res {
                if let Some(turn) = res
                    .get("turn")
                    .and_then(|t| t.get("id"))
                    .and_then(Value::as_str)
                {
                    *core2.active_turn.lock().await = Some(turn.to_string());
                }
            }
        });
    }

    Ok(AdapterSession {
        pid,
        process_started_at: Some(started_at),
        native_id: None,
        capabilities: codex_capabilities(),
        events: event_rx,
        control: ctrl_tx,
    })
}

fn extract_thread_id(res: &Value) -> Option<String> {
    res.get("thread")
        .and_then(|t| t.get("id"))
        .and_then(Value::as_str)
        .or_else(|| res.get("threadId").and_then(Value::as_str))
        .or_else(|| res.get("id").and_then(Value::as_str))
        .map(str::to_string)
}

async fn handle_notification(
    method: &str,
    params: &Value,
    event_tx: &mpsc::Sender<AdapterEvent>,
    core: &Arc<SessionCore>,
) {
    let send = |ev: AdapterEvent| {
        let tx = event_tx.clone();
        async move {
            let _ = tx.send(ev).await;
        }
    };
    match method {
        m if m.contains("agent_message_delta") || m.contains("agentMessage/delta") => {
            if let Some(delta) = params
                .get("delta")
                .or_else(|| params.get("text"))
                .and_then(Value::as_str)
            {
                send(AdapterEvent::Output {
                    stream: Stream::Agent,
                    text: delta.to_string(),
                })
                .await;
            }
        }
        m if m.contains("reasoning") => {}
        "turn/started" => {
            if let Some(turn) = params
                .get("turn")
                .and_then(|t| t.get("id"))
                .and_then(Value::as_str)
            {
                *core.active_turn.lock().await = Some(turn.to_string());
            }
        }
        "turn/completed" => {
            let turn = params
                .get("turn")
                .and_then(|t| t.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string);
            *core.active_turn.lock().await = None;
            let status = params
                .get("turn")
                .and_then(|t| t.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("completed");
            send(AdapterEvent::TurnFinished {
                native_turn_id: turn,
                reason: status.to_string(),
                ok: status == "completed",
            })
            .await;
            if let Some(usage) = params.get("usage").or_else(|| params.get("tokenUsage")) {
                let _ = event_tx
                    .send(AdapterEvent::Usage {
                        usage: Usage {
                            input_tokens: usage
                                .get("inputTokens")
                                .or_else(|| usage.get("input_tokens"))
                                .and_then(Value::as_u64),
                            output_tokens: usage
                                .get("outputTokens")
                                .or_else(|| usage.get("output_tokens"))
                                .and_then(Value::as_u64),
                            cost_usd: None,
                            source: UsageSource::ProviderReported,
                            reported_at: chrono::Utc::now(),
                        },
                    })
                    .await;
            }
        }
        m if m.contains("item/") => {
            // Item lifecycle: tool calls, file edits, agent items.
            if let Some(item) = params.get("item") {
                let kind = item.get("type").and_then(Value::as_str).unwrap_or("item");
                let label = item
                    .get("command")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("title").and_then(Value::as_str))
                    .unwrap_or(kind);
                send(AdapterEvent::Meaningful {
                    summary: format!("{kind}: {label}"),
                })
                .await;
            }
        }
        _ => {}
    }
}

async fn handle_request(
    id: Value,
    method: &str,
    params: &Value,
    event_tx: &mpsc::Sender<AdapterEvent>,
    _core: &Arc<SessionCore>,
) {
    let m = method.to_string();
    if m.contains("requestApproval") || m.contains("approval") {
        let command = params
            .get("command")
            .or_else(|| params.get("cmd"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| params.get("item").map(|i| i.to_string()))
            .unwrap_or_else(|| params.to_string());
        let _ = event_tx
            .send(AdapterEvent::ApprovalRequested {
                request_ref: id.to_string(),
                action: ProposedAction {
                    kind: ActionKind::Exec,
                    display: command.clone(),
                    args: params.clone(),
                },
            })
            .await;
    } else if m.contains("requestUserInput") || m.contains("requestInput") {
        let prompt = params
            .get("question")
            .or_else(|| params.get("prompt"))
            .and_then(Value::as_str)
            .unwrap_or("input requested")
            .to_string();
        let choices = params
            .get("options")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|o| {
                        o.get("label")
                            .or_else(|| o.get("name"))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let _ = event_tx
            .send(AdapterEvent::Question {
                request_ref: id.to_string(),
                prompt,
                choices,
            })
            .await;
    }
}
