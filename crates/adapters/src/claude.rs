//! Claude adapter — the documented headless CLI surface:
//! `claude -p --output-format stream-json --input-format stream-json`.
//!
//! Honest boundary: without the optional Agent SDK bridge there is no
//! structured approval channel; `structured_approvals` is reported
//! Unsupported and permission prompts are not faked. Concurrent work
//! targets explicit session ids via `--resume`, never "latest".

use crate::error::{AdapterError, AdapterResult};
use crate::traits::*;
use async_trait::async_trait;
use orc_domain::*;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

pub struct ClaudeAdapter;

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn capabilities(with_bridge: bool) -> Capabilities {
        let mut c = Capabilities::all_unknown();
        c.list_sessions = Support::unsupported("the CLI does not list sessions");
        c.import_session = Support::Supported {
            constraint: Some("attach via --resume; control is turn-scoped".into()),
        };
        c.resume = Support::Supported {
            constraint: Some("explicit session id via --resume".into()),
        };
        c.fork = Support::Supported {
            constraint: Some("--fork-session on resume".into()),
        };
        c.streaming = Support::supported();
        c.structured_approvals = if with_bridge {
            Support::Supported {
                constraint: Some("via the TypeScript SDK bridge".into()),
            }
        } else {
            Support::unsupported("requires the optional Claude Agent SDK bridge (bridges/claude)")
        };
        c.structured_questions = Support::unsupported("headless mode has no question channel");
        c.cancel = Support::supported();
        c.mid_turn_steer = Support::Supported {
            constraint: Some("stream-json input between/within turns".into()),
        };
        c.reconnect = Support::unsupported("a new CLI process is spawned per turn series");
        c.usage_reporting = Support::supported();
        c.artifact_reporting = Support::unsupported("artifacts are inferred from file ops");
        c
    }

    fn build_command(spec: &SpawnSpec, resume_native_id: Option<&str>, fork: bool) -> Command {
        let mut cmd = Command::new(&spec.executable);
        // Headless + structured I/O.
        cmd.arg("-p")
            .arg(&spec.prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--input-format")
            .arg("stream-json")
            .arg("--verbose");
        // Launch knobs (all user/workspace-level choices).
        if let Some(model) = spec.launch.get("model").and_then(Value::as_str) {
            cmd.arg("--model").arg(model);
        }
        if let Some(mode) = spec.launch.get("permission_mode").and_then(Value::as_str) {
            cmd.arg("--permission-mode").arg(mode);
        }
        if let Some(tools) = spec.launch.get("allowed_tools").and_then(Value::as_str) {
            cmd.arg("--allowedTools").arg(tools);
        }
        if let Some(id) = resume_native_id {
            cmd.arg("--resume").arg(id);
            if fork {
                cmd.arg("--fork-session");
            }
        }
        for arg in &spec.args {
            cmd.arg(arg);
        }
        cmd.current_dir(&spec.cwd)
            .envs(spec.env.iter().cloned())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        cmd
    }
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentAdapter for ClaudeAdapter {
    fn provider(&self) -> ProviderKind {
        ProviderKind::Claude
    }

    fn base_capabilities(&self) -> Capabilities {
        Self::capabilities(false)
    }

    async fn spawn(&self, spec: SpawnSpec) -> AdapterResult<AdapterSession> {
        let resume = spec
            .launch
            .get("resume")
            .and_then(Value::as_str)
            .map(str::to_string);
        let fork = spec
            .launch
            .get("fork")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut cmd = Self::build_command(&spec, resume.as_deref(), fork);
        let child = cmd
            .spawn()
            .map_err(|e| AdapterError::Spawn(e.to_string()))?;
        Ok(wire_claude(
            child,
            spec.launch
                .get("sdk_bridge")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ))
    }

    async fn import(&self, spec: ImportSpec) -> AdapterResult<AdapterSession> {
        // Attach by resuming the native conversation with an empty
        // follow-up prompt placeholder — the session is user-drivable
        // from then on via stream-json input.
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
            launch: spec.launch.clone(),
        };
        let fork = spec
            .launch
            .get("fork")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut cmd = Self::build_command(&spawn, Some(&spec.native_id), fork);
        let child = cmd
            .spawn()
            .map_err(|e| AdapterError::Spawn(e.to_string()))?;
        Ok(wire_claude(child, false))
    }
}

fn wire_claude(mut child_owned: tokio::process::Child, with_bridge: bool) -> AdapterSession {
    let pid = child_owned.id();
    let started_at = chrono::Utc::now();
    let stdin = child_owned.stdin.take().expect("piped stdin");
    let stdout = child_owned.stdout.take().expect("piped stdout");
    let stderr = child_owned.stderr.take().expect("piped stderr");

    let (event_tx, event_rx) = mpsc::channel(512);
    let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<AdapterControl>(64);

    // Stdout → structured events.
    {
        let event_tx = event_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut announced = false;
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let msg: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                for ev in translate(&msg, &mut announced) {
                    if event_tx.send(ev).await.is_err() {
                        return;
                    }
                }
            }
            let _ = event_tx
                .send(AdapterEvent::Exited {
                    code: None,
                    reason: Some("stdout closed".into()),
                })
                .await;
        });
    }

    // Stderr → transcript.
    {
        let event_tx = event_tx.clone();
        tokio::spawn(async move {
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

    // Control → stdin stream-json / kill.
    {
        let mut stdin = stdin;
        let (kill_tx, mut kill_rx) = mpsc::channel::<bool>(1);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(cmd) = ctrl_rx.recv() => {
                        match cmd {
                            AdapterControl::Send { text } | AdapterControl::Steer { text } => {
                                let frame = serde_json::json!({
                                    "type": "user",
                                    "message": {
                                        "role": "user",
                                        "content": [{"type": "text", "text": text}],
                                    },
                                });
                                if stdin
                                    .write_all(frame.to_string().as_bytes())
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                let _ = stdin.write_all(b"\n").await;
                                let _ = stdin.flush().await;
                            }
                            AdapterControl::Answer { .. } => {}
                            AdapterControl::Cancel => {
                                let _ = kill_tx.send(true).await;
                            }
                            AdapterControl::Shutdown => {
                                let _ = kill_tx.send(true).await;
                            }
                        }
                    }
                    else => break,
                }
            }
        });
        // The child is shared between the kill watcher and the reaper:
        // whichever path ends it, the other just waits.
        let child_owned = std::sync::Arc::new(tokio::sync::Mutex::new(child_owned));
        {
            let child = child_owned.clone();
            tokio::spawn(async move {
                if kill_rx.recv().await.is_some() {
                    let _ = child.lock().await.kill().await;
                }
            });
        }
        // Reap the child so zombies don't accumulate.
        tokio::spawn(async move {
            let _ = child_owned.lock().await.wait().await;
        });
    }

    AdapterSession {
        pid,
        process_started_at: Some(started_at),
        native_id: None,
        capabilities: ClaudeAdapter::capabilities(with_bridge),
        events: event_rx,
        control: ctrl_tx,
    }
}

/// One stream-json line → adapter events. Pure so recorded provider
/// streams (tests/fixtures/*.jsonl) can replay through this exact path.
fn translate(msg: &Value, announced: &mut bool) -> Vec<AdapterEvent> {
    let mut out = Vec::new();
    match msg.get("type").and_then(Value::as_str) {
        Some("system") => {
            if !*announced {
                *announced = true;
                let native_id = msg
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                out.push(AdapterEvent::Started { native_id });
            }
        }
        Some("assistant") => {
            if let Some(text) = extract_text(msg) {
                out.push(AdapterEvent::Output {
                    stream: Stream::Agent,
                    text,
                });
            }
        }
        Some("control_request") => {
            // Provider asks permission — a structured approval, bound to
            // this exact request.
            let req = msg.get("request").cloned().unwrap_or(Value::Null);
            let tool = req
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let kind = match tool {
                "Bash" => ActionKind::Exec,
                "Write" | "Edit" | "MultiEdit" => ActionKind::WriteFile,
                "WebFetch" | "WebSearch" => ActionKind::Network,
                _ => ActionKind::Other,
            };
            let input = req.get("input").cloned().unwrap_or(Value::Null);
            let display = input
                .get("command")
                .or_else(|| input.get("file_path"))
                .or_else(|| input.get("url"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| tool.to_string());
            out.push(AdapterEvent::ApprovalRequested {
                request_ref: msg
                    .get("request_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                action: ProposedAction {
                    kind,
                    display,
                    args: input,
                },
            });
        }
        Some("result") => {
            let subtype = msg
                .get("subtype")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let ok = subtype.starts_with("success");
            if let Some(usage) = msg.get("usage") {
                let cost = msg.get("total_cost_usd").and_then(Value::as_f64);
                out.push(AdapterEvent::Usage {
                    usage: orc_domain::Usage {
                        input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
                        output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
                        cost_usd: cost,
                        source: UsageSource::ProviderReported,
                        reported_at: chrono::Utc::now(),
                    },
                });
            }
            let reason = msg
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or(subtype)
                .to_string();
            out.push(AdapterEvent::TurnFinished {
                native_turn_id: None,
                reason,
                ok,
            });
        }
        _ => {}
    }
    out
}

fn extract_text(msg: &Value) -> Option<String> {
    let content = msg.get("message")?.get("content")?.as_array()?;
    let mut out = String::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(t) = block.get("text").and_then(Value::as_str) {
                out.push_str(t);
            }
        }
        if block.get("type").and_then(Value::as_str) == Some("tool_use") {
            if let Some(name) = block.get("name").and_then(Value::as_str) {
                out.push_str(&format!("\n[tool: {name}]\n"));
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replay a recorded provider stream through `translate` — the same
    /// code path production uses.
    fn replay(path: &str) -> Vec<AdapterEvent> {
        let raw = std::fs::read_to_string(path).expect("fixture");
        let mut announced = false;
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .flat_map(|l| {
                let v: Value = serde_json::from_str(l).expect("fixture line is json");
                translate(&v, &mut announced)
            })
            .collect()
    }

    #[test]
    fn replays_basic_turn() {
        let evs = replay("../../tests/fixtures/claude_stream_basic.jsonl");
        assert!(matches!(
            evs[0],
            AdapterEvent::Started { native_id: Some(ref id) } if id == "fixture-session-1"
        ));
        let outputs: Vec<_> = evs
            .iter()
            .filter(|e| matches!(e, AdapterEvent::Output { .. }))
            .collect();
        assert_eq!(outputs.len(), 3, "two text blocks + tool_use line");
        assert!(evs.iter().any(|e| matches!(
            e,
            AdapterEvent::Usage { usage } if usage.input_tokens == Some(1820)
        )));
        assert!(matches!(
            evs.last(),
            Some(AdapterEvent::TurnFinished { ok: true, .. })
        ));
    }

    #[test]
    fn replays_approval_bound_to_request() {
        let evs = replay("../../tests/fixtures/claude_stream_approval.jsonl");
        let approval = evs.iter().find_map(|e| match e {
            AdapterEvent::ApprovalRequested {
                request_ref,
                action,
            } => Some((request_ref, action)),
            _ => None,
        });
        let (request_ref, action) = approval.expect("control_request → approval");
        assert_eq!(request_ref, "req_7f3a");
        assert_eq!(action.kind, ActionKind::Exec);
        assert!(action.display.contains("migrate"));
    }
}
