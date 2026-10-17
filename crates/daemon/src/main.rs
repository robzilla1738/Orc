//! `orcd` — the orchestration daemon. Owns sessions, permissions,
//! execution, persistence, and the private IPC surface. The desktop
//! app is a client; closing a window never kills work.

use async_trait::async_trait;
use clap::Parser;
use orc_adapters::{AdapterRegistry, DiscoveryPaths};
use orc_domain::{Event, PROTOCOL_VERSION};
use orc_ipc::auth;
use orc_ipc::proto::methods;
use orc_ipc::server::{serve, RequestHandler};
use orc_orchestrator::{AnthropicBackend, ChatService, MockBackend};
use orc_runtime::{reconcile, Observer, Runtime, Scheduler};
use orc_storage::{Store, TranscriptStore};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{info, warn};

#[derive(Parser)]
#[command(name = "orcd", about = "Orc orchestration daemon")]
struct Args {
    /// Override the runtime dir (data + socket + token).
    #[arg(long)]
    runtime_dir: Option<PathBuf>,
    /// Extra directories to probe for tools (repeatable).
    #[arg(long)]
    tool_path: Vec<PathBuf>,
    /// Log filter, e.g. "info,orc=debug".
    #[arg(long, default_value = "info")]
    log: String,
    /// Force the mock orchestrator backend (no model key needed).
    #[arg(long)]
    mock_model: bool,
    /// Observed-process scan cadence in seconds (0 disables).
    #[arg(long, default_value_t = 30)]
    observe_interval: u64,
}

struct Daemon {
    runtime: Runtime,
    chat: Arc<ChatService>,
    token: String,
}

#[async_trait]
impl RequestHandler for Daemon {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            methods::PING => Ok(json!({
                "pong": true,
                "protocol": PROTOCOL_VERSION,
                "version": env!("CARGO_PKG_VERSION"),
            })),
            methods::SNAPSHOT => self
                .runtime
                .snapshot()
                .await
                .map(|s| serde_json::to_value(s).unwrap_or(Value::Null))
                .map_err(|e| e.to_string()),
            methods::EVENTS_SINCE => {
                let since = params.get("since_seq").and_then(Value::as_i64).unwrap_or(0);
                let limit = params
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(10_000) as u32;
                self.runtime
                    .store()
                    .events_since(since, limit)
                    .await
                    .map(|ev| serde_json::to_value(ev).unwrap_or(Value::Null))
                    .map_err(|e| e.to_string())
            }
            methods::COMMAND => {
                let cmd: orc_domain::Command =
                    serde_json::from_value(params).map_err(|e| e.to_string())?;
                self.runtime
                    .execute(cmd)
                    .await
                    .map(|o| serde_json::to_value(o).unwrap_or(Value::Null))
                    .map_err(|e| e.to_string())
            }
            methods::DISCOVER => self
                .runtime
                .discover_tools()
                .await
                .map(|d| {
                    serde_json::to_value(
                        d.iter()
                            .map(|v| {
                                json!({
                                    "tool": v.tool,
                                    "readiness_note": v.readiness_note,
                                })
                            })
                            .collect::<Vec<_>>(),
                    )
                    .unwrap_or(Value::Null)
                })
                .map_err(|e| e.to_string()),
            methods::OBSERVE => self
                .runtime
                .execute(orc_domain::Command {
                    id: orc_domain::OperationId::new(),
                    idempotency_key: orc_domain::IdempotencyKey::new(),
                    actor: orc_domain::Actor::User,
                    kind: orc_domain::CommandKind::ObserveScan,
                })
                .await
                .map(|o| serde_json::to_value(o).unwrap_or(Value::Null))
                .map_err(|e| e.to_string()),
            methods::CHAT_SEND => {
                let text = params
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing 'text'".to_string())?;
                let reply = self.chat.handle(text).await;
                Ok(json!({
                    "message_id": reply.message_id,
                    "text": reply.text,
                    "operations": reply.operations,
                    "results": reply.results,
                    "offline": reply.offline,
                }))
            }
            methods::CHAT_HISTORY => self
                .runtime
                .store()
                .chat_history(100)
                .await
                .map(|h| serde_json::to_value(h).unwrap_or(Value::Null))
                .map_err(|e| e.to_string()),
            methods::TRANSCRIPT_TAIL => {
                #[derive(serde::Deserialize)]
                struct P {
                    session_id: String,
                    stream: orc_domain::Stream,
                    max_bytes: Option<u64>,
                }
                let p: P = serde_json::from_value(params).map_err(|e| e.to_string())?;
                let (offset, bytes) = self
                    .runtime
                    .transcripts()
                    .tail(
                        &orc_domain::SessionId::from_raw(p.session_id),
                        p.stream,
                        p.max_bytes.unwrap_or(64 * 1024),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!({
                    "offset": offset,
                    "text": String::from_utf8_lossy(&bytes),
                }))
            }
            methods::DIAGNOSTICS => {
                let snapshot = self.runtime.snapshot().await.map_err(|e| e.to_string())?;
                Ok(json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "protocol": PROTOCOL_VERSION,
                    "sessions": snapshot.sessions.len(),
                    "open_attention": snapshot.attention.iter()
                        .filter(|a| a.status == orc_domain::AttentionStatus::Open)
                        .count(),
                    "tools": snapshot.tools.len(),
                    "workspaces": snapshot.workspaces.len(),
                    "pending_intents": snapshot.pending_commands.len(),
                }))
            }
            methods::SHUTDOWN => {
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    std::process::exit(0);
                });
                Ok(json!({"shutting_down": true}))
            }
            other => Err(format!("unknown method {other}")),
        }
    }

    fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<Event> {
        self.runtime.bus().subscribe()
    }

    async fn events_since(&self, since_seq: i64, limit: u32) -> Result<Vec<Event>, String> {
        self.runtime
            .store()
            .events_since(since_seq, limit)
            .await
            .map_err(|e| e.to_string())
    }

    fn expected_token(&self) -> String {
        self.token.clone()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let runtime_dir = args.runtime_dir.unwrap_or_else(auth::runtime_dir);
    std::fs::create_dir_all(&runtime_dir)?;

    // Logs → file + stderr; never secrets.
    let log_path = runtime_dir.join("orcd.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| args.log.clone().into()),
        )
        .with_writer(move || file.try_clone().expect("log file"))
        .with_ansi(false)
        .init();

    let token = auth::load_or_create_token(&runtime_dir)?;
    std::fs::write(
        runtime_dir.join(auth::PID_NAME),
        std::process::id().to_string(),
    )?;
    info!(dir = %runtime_dir.display(), "orcd starting");

    let store = Store::open(&runtime_dir).await?;
    let transcripts = TranscriptStore::new(&runtime_dir);
    let adapters = AdapterRegistry::with_defaults();
    let runtime = Runtime::new(
        store,
        transcripts,
        adapters,
        DiscoveryPaths {
            extra: args.tool_path,
        },
    );

    // Crash recovery before serving: unresolved intents → unknown;
    // orphaned sessions → Unknown.
    let recovered = reconcile::reconcile(runtime.store(), runtime.bus()).await;
    if recovered > 0 {
        info!(recovered, "reconciled pre-existing state");
    }

    let backend: Arc<dyn orc_orchestrator::ModelBackend> = if args.mock_model
        || std::env::var("ORC_MODEL_BACKEND").ok().as_deref() == Some("mock")
    {
        Arc::new(MockBackend::default())
    } else {
        match AnthropicBackend::from_env() {
            Ok(b) => Arc::new(b),
            Err(e) => {
                warn!(error = %e, "model backend unavailable; chat falls back to offline replies");
                Arc::new(MockBackend::default())
            }
        }
    };
    let chat = Arc::new(ChatService::new(runtime.clone(), backend));

    let daemon = Arc::new(Daemon {
        runtime: runtime.clone(),
        chat,
        token,
    });

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Observed-process scanning on a timer.
    if args.observe_interval > 0 {
        let observer = Observer::new(runtime.store().clone(), runtime.bus().clone());
        tokio::spawn(async move {
            let mut tick =
                tokio::time::interval(std::time::Duration::from_secs(args.observe_interval));
            loop {
                tick.tick().await;
                observer.scan_once().await;
            }
        });
    }

    // Scheduler ticks + controller reaping.
    {
        let rt = runtime.clone();
        tokio::spawn(async move {
            let scheduler = Scheduler::new(rt.clone());
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tick.tick().await;
                rt.reap_controllers().await;
                scheduler.tick().await;
            }
        });
    }

    // Signals: graceful daemon stop — sessions persist, controllers
    // shut down in order, no side effects half-dispatched.
    {
        let rt = runtime.clone();
        let shutdown_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let mut term = signal(SignalKind::terminate()).expect("signal");
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    _ = term.recv() => {},
                }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
            info!("shutting down gracefully");
            let _ = shutdown_tx.send(true);
            rt.shutdown_all().await;
            std::process::exit(0);
        });
    }

    let sock = auth::socket_path(&runtime_dir);
    serve(&sock, daemon, shutdown_rx).await?;
    Ok(())
}
