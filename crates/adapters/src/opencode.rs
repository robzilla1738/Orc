//! OpenCode adapter — the documented HTTP/SSE server interface.
//! Managed servers are bound locally with explicit authentication;
//! Orc never scrapes the terminal UI.

use crate::error::{AdapterError, AdapterResult};
use crate::traits::*;
use async_trait::async_trait;
use futures_util::StreamExt;
use orc_domain::*;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

pub struct OpenCodeAdapter {
    client: reqwest::Client,
}

impl OpenCodeAdapter {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for OpenCodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

fn opencode_capabilities() -> Capabilities {
    let mut c = Capabilities::all_unknown();
    c.list_sessions = Support::supported();
    c.import_session = Support::Supported {
        constraint: Some("attach by server session id".into()),
    };
    c.resume = Support::Supported {
        constraint: Some("server-side sessions persist on the server".into()),
    };
    c.fork = Support::unsupported("no fork endpoint");
    c.streaming = Support::supported();
    c.structured_approvals = Support::Supported {
        constraint: Some("permission reply endpoints".into()),
    };
    c.structured_questions = Support::Unknown;
    c.cancel = Support::supported();
    c.mid_turn_steer = Support::Supported {
        constraint: Some("messages queue against the session".into()),
    };
    c.reconnect = Support::Supported {
        constraint: Some("reconnects to the configured server".into()),
    };
    c.usage_reporting = Support::Supported {
        constraint: Some("reported on assistant messages".into()),
    };
    c.artifact_reporting = Support::unsupported("no artifact endpoint");
    c
}

#[async_trait]
impl AgentAdapter for OpenCodeAdapter {
    fn provider(&self) -> ProviderKind {
        ProviderKind::OpenCode
    }

    fn base_capabilities(&self) -> Capabilities {
        opencode_capabilities()
    }

    async fn spawn(&self, spec: SpawnSpec) -> AdapterResult<AdapterSession> {
        connect(self.client.clone(), &spec, None).await
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
        connect(self.client.clone(), &spawn, Some(spec.native_id)).await
    }
}

async fn connect(
    client: reqwest::Client,
    spec: &SpawnSpec,
    attach_native: Option<String>,
) -> AdapterResult<AdapterSession> {
    let base = spec.executable.trim_end_matches('/').to_string();
    let auth = spec
        .launch
        .get("auth_token")
        .and_then(Value::as_str)
        .map(str::to_string);

    let authed = |req: reqwest::RequestBuilder| {
        if let Some(t) = &auth {
            req.bearer_auth(t)
        } else {
            req
        }
    };

    // Create or attach the server session.
    let server_session = if let Some(native) = attach_native {
        native
    } else {
        let res = authed(client.post(format!("{base}/session")))
            .json(&json!({ "title": spec.launch.get("title").and_then(Value::as_str).unwrap_or("Orc session") }))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| AdapterError::Provider(e.to_string()))?
            .json::<Value>()
            .await?;
        res.get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| AdapterError::Protocol("POST /session returned no id".into()))?
            .to_string()
    };

    let (event_tx, event_rx) = mpsc::channel::<AdapterEvent>(512);
    let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<AdapterControl>(64);

    let server_session = Arc::new(server_session);
    let shutdown = Arc::new(Mutex::new(false));

    // SSE event stream task.
    {
        let client = client.clone();
        let base = base.clone();
        let auth = auth.clone();
        let event_tx = event_tx.clone();
        let server_session = server_session.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            loop {
                if *shutdown.lock().await {
                    break;
                }
                let mut req = client.get(format!("{base}/event"));
                if let Some(t) = &auth {
                    req = req.bearer_auth(t);
                }
                let res = match req.send().await {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = event_tx
                            .send(AdapterEvent::Error {
                                message: format!("event stream connect: {e}"),
                            })
                            .await;
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        continue;
                    }
                };
                let mut stream = res.bytes_stream();
                let mut buf = String::new();
                while let Some(chunk) = stream.next().await {
                    if *shutdown.lock().await {
                        return;
                    }
                    let chunk = match chunk {
                        Ok(c) => c,
                        Err(_) => break,
                    };
                    buf.push_str(&String::from_utf8_lossy(&chunk));
                    // SSE frames are separated by blank lines.
                    while let Some(idx) = buf.find("\n\n") {
                        let frame = buf[..idx].to_string();
                        buf = buf[idx + 2..].to_string();
                        handle_sse_frame(&frame, &server_session, &event_tx).await;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
    }

    // Control loop.
    {
        let client = client.clone();
        let base = base.clone();
        let auth = auth.clone();
        let server_session = server_session.clone();
        let shutdown = shutdown.clone();
        let event_tx = event_tx.clone();
        tokio::spawn(async move {
            while let Some(cmd) = ctrl_rx.recv().await {
                let authed = |req: reqwest::RequestBuilder| {
                    if let Some(t) = &auth {
                        req.bearer_auth(t)
                    } else {
                        req
                    }
                };
                match cmd {
                    AdapterControl::Send { text } | AdapterControl::Steer { text } => {
                        let _ = authed(
                            client.post(format!("{base}/session/{}/message", *server_session)),
                        )
                        .json(&json!({
                            "parts": [{ "type": "text", "text": text }]
                        }))
                        .send()
                        .await;
                    }
                    AdapterControl::Answer {
                        request_ref,
                        answer,
                    } => {
                        // OpenCode permission reply: permission id + reply.
                        let reply = match answer {
                            AdapterAnswer::Approve { option_id } => {
                                option_id.unwrap_or_else(|| "once".into())
                            }
                            AdapterAnswer::Deny { .. } => "reject".to_string(),
                            AdapterAnswer::Text(t) => t,
                        };
                        let _ = authed(client.post(format!(
                            "{base}/session/{}/permissions/{request_ref}/reply",
                            *server_session
                        )))
                        .json(&json!({ "reply": reply }))
                        .send()
                        .await;
                    }
                    AdapterControl::Cancel => {
                        let _ = authed(
                            client.post(format!("{base}/session/{}/abort", *server_session)),
                        )
                        .send()
                        .await;
                    }
                    AdapterControl::Shutdown => {
                        *shutdown.lock().await = true;
                        let _ = event_tx
                            .send(AdapterEvent::Exited {
                                code: None,
                                reason: Some("adapter shut down".into()),
                            })
                            .await;
                        break;
                    }
                }
            }
        });
    }

    let _ = event_tx
        .send(AdapterEvent::Started {
            native_id: Some((*server_session).clone()),
        })
        .await;

    if !spec.prompt.is_empty() {
        let client = client.clone();
        let base = base.clone();
        let auth = auth.clone();
        let sid = server_session.clone();
        let prompt = spec.prompt.clone();
        tokio::spawn(async move {
            let mut req = client.post(format!("{base}/session/{sid}/message"));
            if let Some(t) = &auth {
                req = req.bearer_auth(t);
            }
            let _ = req
                .json(&json!({ "parts": [{ "type": "text", "text": prompt }] }))
                .send()
                .await;
        });
    }

    Ok(AdapterSession {
        pid: None,
        process_started_at: None,
        native_id: None,
        capabilities: opencode_capabilities(),
        events: event_rx,
        control: ctrl_tx,
    })
}

async fn handle_sse_frame(
    frame: &str,
    server_session: &str,
    event_tx: &mpsc::Sender<AdapterEvent>,
) {
    let data = frame
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return;
    }
    let ev: Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return,
    };
    // Only forward events for our server-side session.
    let props = ev.get("properties").cloned().unwrap_or(json!({}));
    let sid_match = props
        .get("sessionID")
        .and_then(Value::as_str)
        .map(|s| s == server_session)
        .unwrap_or(true);
    if !sid_match {
        return;
    }
    match ev.get("type").and_then(Value::as_str) {
        Some("message.part.updated") | Some("message.updated") => {
            if let Some(text) = props
                .get("part")
                .and_then(|p| p.get("text"))
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
        Some("session.idle") | Some("message.completed") => {
            let _ = event_tx
                .send(AdapterEvent::TurnFinished {
                    native_turn_id: None,
                    reason: "idle".into(),
                    ok: true,
                })
                .await;
        }
        Some("permission.updated") | Some("permission.asked") => {
            let perm_id = props
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let title = props
                .get("title")
                .or_else(|| props.get("pattern"))
                .and_then(Value::as_str)
                .unwrap_or("permission requested")
                .to_string();
            let _ = event_tx
                .send(AdapterEvent::ApprovalRequested {
                    request_ref: perm_id,
                    action: ProposedAction {
                        kind: ActionKind::Other,
                        display: title,
                        args: props,
                    },
                })
                .await;
        }
        Some("session.error") | Some("server.error") => {
            let _ = event_tx
                .send(AdapterEvent::Error {
                    message: props.to_string(),
                })
                .await;
        }
        _ => {}
    }
}
