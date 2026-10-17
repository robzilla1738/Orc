//! `orc` — headless control + diagnostics for the daemon.
//! Same IPC surface the desktop uses; `orc doctor` for environment
//! diagnostics.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use orc_domain::*;
use orc_ipc::{auth, proto::methods, IpcClient};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "orc", about = "Orc — local AI session manager")]
struct Cli {
    /// Daemon runtime dir override.
    #[arg(long, global = true)]
    runtime_dir: Option<std::path::PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Environment + daemon diagnostics.
    Doctor,
    /// Ping the daemon.
    Ping,
    /// Discover installed agent tools.
    Discover,
    /// Scan for observed external agent processes.
    Observe,
    /// Manage workspaces.
    Workspace {
        #[command(subcommand)]
        cmd: WorkspaceCmd,
    },
    /// List discovered tools.
    Tools,
    /// Manage sessions.
    Session {
        #[command(subcommand)]
        cmd: SessionCmd,
    },
    /// Attention inbox.
    Attention {
        #[command(subcommand)]
        cmd: AttentionCmd,
    },
    /// Create a task.
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    /// Send a chat message to the orchestrator.
    Chat { text: String },
    /// Tail a session transcript.
    Transcript {
        session: String,
        #[arg(long)]
        stream: Option<String>,
    },
    /// Gracefully stop the daemon.
    Stop,
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    Ls,
    Add {
        name: String,
        root: String,
    },
    Allow {
        workspace: String,
        kind: String,
        pattern: String,
    },
}

#[derive(Subcommand)]
enum SessionCmd {
    Ls,
    Start {
        tool: String,
        workspace: String,
        title: String,
        prompt: String,
    },
    Send {
        session: String,
        text: String,
    },
    Cancel {
        session: String,
    },
    Resume {
        session: String,
    },
    Inspect {
        session: String,
    },
}

#[derive(Subcommand)]
enum AttentionCmd {
    Ls,
    Answer {
        id: String,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        choice: Option<u32>,
        #[arg(long)]
        approve: bool,
        #[arg(long)]
        deny: bool,
    },
    Dismiss {
        id: String,
    },
}

#[derive(Subcommand)]
enum TaskCmd {
    Ls,
    New {
        workspace: String,
        title: String,
        #[arg(long)]
        description: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let dir = cli.runtime_dir.clone().unwrap_or_else(auth::runtime_dir);

    if matches!(cli.cmd, Cmd::Doctor) {
        return doctor(&dir).await;
    }

    let client = IpcClient::connect(&dir, "orc-cli")
        .await
        .context("cannot reach orcd — is it running? (`orcd` or `cargo run -p orcd`)")?;

    match cli.cmd {
        Cmd::Doctor => doctor(&dir).await,
        Cmd::Ping => show(client.call(methods::PING, json!({})).await),
        Cmd::Discover => show(client.call(methods::DISCOVER, json!({})).await),
        Cmd::Observe => show(client.call(methods::OBSERVE, json!({})).await),
        Cmd::Tools => {
            let snap = snapshot(&client).await?;
            println!("{}", serde_json::to_string_pretty(&snap["tools"])?);
            Ok(())
        }
        Cmd::Workspace { cmd } => match cmd {
            WorkspaceCmd::Ls => {
                let snap = snapshot(&client).await?;
                println!("{}", serde_json::to_string_pretty(&snap["workspaces"])?);
                Ok(())
            }
            WorkspaceCmd::Add { name, root } => {
                command(&client, CommandKind::WorkspaceCreate { name, root }).await
            }
            WorkspaceCmd::Allow {
                workspace,
                kind,
                pattern,
            } => {
                let action_kind = match kind.as_str() {
                    "exec" => ActionKind::Exec,
                    "write" | "write_file" => ActionKind::WriteFile,
                    "read" | "read_file" => ActionKind::ReadFile,
                    "network" => ActionKind::Network,
                    _ => ActionKind::Other,
                };
                command(
                    &client,
                    CommandKind::WorkspaceAddAllowRule {
                        workspace_id: WorkspaceId::from_raw(workspace),
                        action_kind,
                        pattern,
                    },
                )
                .await
            }
        },
        Cmd::Session { cmd } => match cmd {
            SessionCmd::Ls => {
                let snap = snapshot(&client).await?;
                println!("{}", serde_json::to_string_pretty(&snap["sessions"])?);
                Ok(())
            }
            SessionCmd::Start {
                tool,
                workspace,
                title,
                prompt,
            } => {
                command(
                    &client,
                    CommandKind::SessionStart {
                        tool_instance_id: ToolInstanceId::from_raw(tool),
                        workspace_id: WorkspaceId::from_raw(workspace),
                        task_id: None,
                        title,
                        prompt,
                        launch: json!({}),
                    },
                )
                .await
            }
            SessionCmd::Send { session, text } => {
                command(
                    &client,
                    CommandKind::SessionSend {
                        session_id: SessionId::from_raw(session),
                        text,
                    },
                )
                .await
            }
            SessionCmd::Cancel { session } => {
                command(
                    &client,
                    CommandKind::SessionCancel {
                        session_id: SessionId::from_raw(session),
                    },
                )
                .await
            }
            SessionCmd::Resume { session } => {
                command(
                    &client,
                    CommandKind::SessionResume {
                        session_id: SessionId::from_raw(session),
                    },
                )
                .await
            }
            SessionCmd::Inspect { session } => {
                let res = client
                    .call(
                        methods::TRANSCRIPT_TAIL,
                        json!({
                            "session_id": session,
                            "stream": "agent",
                            "max_bytes": 16384,
                        }),
                    )
                    .await?;
                println!("{}", serde_json::to_string_pretty(&res)?);
                Ok(())
            }
        },
        Cmd::Attention { cmd } => match cmd {
            AttentionCmd::Ls => {
                let snap = snapshot(&client).await?;
                let open: Vec<&Value> = snap["attention"]
                    .as_array()
                    .map(|a| a.iter().filter(|i| i["status"] == "open").collect())
                    .unwrap_or_default();
                println!("{}", serde_json::to_string_pretty(&open)?);
                Ok(())
            }
            AttentionCmd::Answer {
                id,
                text,
                choice,
                approve,
                deny,
            } => {
                let answer = if let Some(t) = text {
                    Answer::Text { text: t }
                } else if let Some(i) = choice {
                    Answer::Choice { index: i }
                } else if approve {
                    Answer::Approve {
                        scope: ApprovalScope::Once,
                    }
                } else if deny {
                    Answer::Deny { reason: None }
                } else {
                    anyhow::bail!("pass --text, --choice, --approve, or --deny");
                };
                command(
                    &client,
                    CommandKind::AttentionAnswer {
                        attention_id: AttentionId::from_raw(id),
                        answer,
                    },
                )
                .await
            }
            AttentionCmd::Dismiss { id } => {
                command(
                    &client,
                    CommandKind::AttentionDismiss {
                        attention_id: AttentionId::from_raw(id),
                    },
                )
                .await
            }
        },
        Cmd::Task { cmd } => match cmd {
            TaskCmd::Ls => {
                let snap = snapshot(&client).await?;
                println!("{}", serde_json::to_string_pretty(&snap["tasks"])?);
                Ok(())
            }
            TaskCmd::New {
                workspace,
                title,
                description,
            } => {
                command(
                    &client,
                    CommandKind::TaskCreate {
                        workspace_id: WorkspaceId::from_raw(workspace),
                        title,
                        description,
                        steps: vec![],
                    },
                )
                .await
            }
        },
        Cmd::Chat { text } => {
            let res = client
                .call(methods::CHAT_SEND, json!({ "text": text }))
                .await?;
            if let Some(t) = res.get("text").and_then(Value::as_str) {
                println!("{t}");
            }
            let ops = res.get("operations").and_then(Value::as_array);
            if let Some(ops) = ops {
                for op in ops {
                    println!("  → operation {}", op.as_str().unwrap_or("?"));
                }
            }
            Ok(())
        }
        Cmd::Transcript { session, stream } => {
            let stream = stream.unwrap_or_else(|| "agent".into());
            let res = client
                .call(
                    methods::TRANSCRIPT_TAIL,
                    json!({ "session_id": session, "stream": stream, "max_bytes": 65536 }),
                )
                .await?;
            if let Some(t) = res.get("text").and_then(Value::as_str) {
                print!("{t}");
            }
            Ok(())
        }
        Cmd::Stop => show(client.call(methods::SHUTDOWN, json!({})).await),
    }
}

async fn snapshot(client: &IpcClient) -> Result<Value> {
    Ok(client.call(methods::SNAPSHOT, json!({})).await?)
}

async fn command(client: &IpcClient, kind: CommandKind) -> Result<()> {
    let cmd = Command {
        id: OperationId::new(),
        idempotency_key: IdempotencyKey::new(),
        actor: Actor::User,
        kind,
    };
    let res = client
        .call(methods::COMMAND, serde_json::to_value(cmd)?)
        .await?;
    println!("{}", serde_json::to_string_pretty(&res)?);
    Ok(())
}

fn show(res: orc_ipc::IpcResult<Value>) -> Result<()> {
    match res {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v)?),
        Err(e) => anyhow::bail!("{e}"),
    }
    Ok(())
}

async fn doctor(dir: &std::path::Path) -> Result<()> {
    println!("Orc doctor");
    println!("  runtime dir: {}", dir.display());
    let sock = auth::socket_path(dir);
    println!(
        "  socket:      {} ({})",
        sock.display(),
        if sock.exists() { "present" } else { "missing" }
    );
    match IpcClient::connect(dir, "orc-doctor").await {
        Ok(c) => {
            let ping = c.call(methods::PING, json!({})).await;
            match ping {
                Ok(_) => println!(
                    "  daemon:      reachable (protocol {})",
                    c.server.protocol_version
                ),
                Err(e) => println!("  daemon:      socket present but ping failed: {e}"),
            }
            if let Ok(diag) = c.call(methods::DIAGNOSTICS, json!({})).await {
                println!("  diagnostics: {}", serde_json::to_string_pretty(&diag)?);
            }
        }
        Err(e) => println!("  daemon:      unreachable ({e})"),
    }
    // PATH gap check for the common desktop-vs-terminal divergence.
    for name in ["codex", "claude", "opencode"] {
        match which::which(name) {
            Ok(p) => println!("  tool:        {name} → {}", p.display()),
            Err(_) => println!("  tool:        {name} not on PATH"),
        }
    }
    Ok(())
}
