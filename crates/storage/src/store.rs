//! The store: event log + current-state projections in one transaction.

use crate::error::{StorageError, StorageResult};
use crate::migrate;
use orc_domain::*;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use ts_rs::TS;

/// Serializes writer access. WAL gives readers concurrency; writes stay
/// single-file so event `seq` ordering is total.
#[derive(Clone)]
pub struct Store {
    pub(crate) pool: SqlitePool,
    data_dir: PathBuf,
}

/// Point-in-time view for the interface and orchestrator.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub cursor: i64,
    pub sessions: Vec<Session>,
    pub runs: Vec<Run>,
    pub attention: Vec<AttentionItem>,
    pub tasks: Vec<Task>,
    pub tools: Vec<ToolInstance>,
    pub workspaces: Vec<Workspace>,
    pub pending_commands: Vec<PendingIntent>,
}

/// A command intent with no recorded outcome — reconciled at startup.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct PendingIntent {
    pub operation_id: OperationId,
    pub idempotency_key: IdempotencyKey,
    pub actor: Actor,
    pub kind: CommandKind,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Store {
    /// Open (or create) the store under `data_dir`. `data_dir/orc.db`.
    pub async fn open(data_dir: &Path) -> StorageResult<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("orc.db");
        Self::open_path(&db_path, data_dir).await
    }

    pub async fn open_path(db_path: &Path, data_dir: &Path) -> StorageResult<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(30));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;
        migrate::run(&pool).await?;
        Ok(Self {
            pool,
            data_dir: data_dir.to_path_buf(),
        })
    }

    /// An in-memory store for tests.
    pub async fn open_in_memory() -> StorageResult<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(":memory:")
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        migrate::run(&pool).await?;
        Ok(Self {
            pool,
            data_dir: std::env::temp_dir(),
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Current event sequence (snapshot cursor).
    pub async fn cursor(&self) -> StorageResult<i64> {
        let seq: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM events")
            .fetch_one(&self.pool)
            .await?;
        Ok(seq)
    }

    // ------------------------------------------------------------------
    // Events
    // ------------------------------------------------------------------

    /// Append one event and update projections atomically.
    pub async fn append(&self, new: NewEvent) -> StorageResult<Event> {
        let mut tx = self.pool.begin().await?;
        let ev = insert_event(&mut tx, new).await?;
        apply_projection(&mut tx, &ev).await?;
        bump_session_seq(&mut tx, &ev).await?;
        tx.commit().await?;
        Ok(ev)
    }

    /// Batched append — one transaction for many events (output bursts).
    pub async fn append_batch(&self, news: Vec<NewEvent>) -> StorageResult<Vec<Event>> {
        if news.is_empty() {
            return Ok(vec![]);
        }
        let mut tx = self.pool.begin().await?;
        let mut out = Vec::with_capacity(news.len());
        for new in news {
            let ev = insert_event(&mut tx, new).await?;
            apply_projection(&mut tx, &ev).await?;
            bump_session_seq(&mut tx, &ev).await?;
            out.push(ev);
        }
        tx.commit().await?;
        Ok(out)
    }

    /// Events after `since_seq`, ascending — replay for subscribers.
    pub async fn events_since(&self, since_seq: i64, limit: u32) -> StorageResult<Vec<Event>> {
        let rows = sqlx::query(
            "SELECT seq, id, at, source, session_id, run_id, schema_version, payload
             FROM events WHERE seq > ? ORDER BY seq ASC LIMIT ?",
        )
        .bind(since_seq)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_event).collect()
    }

    // ------------------------------------------------------------------
    // Command intent log (outcome-unknown reconciliation)
    // ------------------------------------------------------------------

    /// Record a command intent BEFORE dispatch. Unique idempotency key —
    /// a duplicate key returns `Conflict` instead of double-dispatch.
    pub async fn record_intent(&self, cmd: &Command) -> StorageResult<()> {
        sqlx::query(
            "INSERT INTO commands (operation_id, idempotency_key, actor, kind, outcome, created_at)
             VALUES (?, ?, ?, ?, NULL, ?)",
        )
        .bind(cmd.id.as_str())
        .bind(cmd.idempotency_key.as_str())
        .bind(serde_json::to_string(&cmd.actor)?)
        .bind(serde_json::to_string(&cmd.kind)?)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// True if this idempotency key was already recorded (dedupe retries).
    pub async fn intent_seen(&self, key: &IdempotencyKey) -> StorageResult<bool> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM commands WHERE idempotency_key = ?)")
                .bind(key.as_str())
                .fetch_one(&self.pool)
                .await?;
        Ok(exists)
    }

    pub async fn resolve_intent(
        &self,
        key: &IdempotencyKey,
        outcome: &CommandOutcome,
    ) -> StorageResult<()> {
        sqlx::query("UPDATE commands SET outcome = ?, resolved_at = ? WHERE idempotency_key = ?")
            .bind(serde_json::to_string(outcome)?)
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(key.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Intents never resolved — crash candidates to reconcile.
    pub async fn pending_intents(&self) -> StorageResult<Vec<PendingIntent>> {
        let rows = sqlx::query(
            "SELECT operation_id, idempotency_key, actor, kind, created_at
             FROM commands WHERE outcome IS NULL ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(PendingIntent {
                    operation_id: OperationId::from_raw(r.get::<String, _>(0)),
                    idempotency_key: IdempotencyKey::from_str(r.get::<String, _>(1).as_str())
                        .map_err(|e| StorageError::NotFound(e.to_string()))?,
                    actor: serde_json::from_str(&r.get::<String, _>(2))?,
                    kind: serde_json::from_str(&r.get::<String, _>(3))?,
                    created_at: parse_dt(&r.get::<String, _>(4)),
                })
            })
            .collect()
    }

    // ------------------------------------------------------------------
    // Snapshot & queries
    // ------------------------------------------------------------------

    pub async fn snapshot(&self) -> StorageResult<Snapshot> {
        Ok(Snapshot {
            cursor: self.cursor().await?,
            sessions: self.list_sessions().await?,
            runs: self.list_runs().await?,
            attention: self.list_attention(None).await?,
            tasks: self.list_tasks().await?,
            tools: self.list_tools().await?,
            workspaces: self.list_workspaces().await?,
            pending_commands: self.pending_intents().await?,
        })
    }

    pub async fn list_sessions(&self) -> StorageResult<Vec<Session>> {
        let rows = sqlx::query("SELECT * FROM sessions ORDER BY updated_at DESC")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_session).collect()
    }

    pub async fn get_session(&self, id: &SessionId) -> StorageResult<Session> {
        let row = sqlx::query("SELECT * FROM sessions WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StorageError::NotFound(format!("session {id}")))?;
        row_to_session(&row)
    }

    pub async fn list_runs(&self) -> StorageResult<Vec<Run>> {
        let rows = sqlx::query("SELECT * FROM runs ORDER BY started_at DESC LIMIT 500")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_run).collect()
    }

    pub async fn list_attention(
        &self,
        status: Option<AttentionStatus>,
    ) -> StorageResult<Vec<AttentionItem>> {
        let rows = match status {
            Some(s) => {
                sqlx::query(
                    "SELECT * FROM attention_items WHERE status = ? ORDER BY created_at DESC",
                )
                .bind(status_name(s))
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query("SELECT * FROM attention_items ORDER BY created_at DESC LIMIT 500")
                    .fetch_all(&self.pool)
                    .await?
            }
        };
        rows.iter().map(row_to_attention).collect()
    }

    pub async fn get_attention(&self, id: &AttentionId) -> StorageResult<AttentionItem> {
        let row = sqlx::query("SELECT * FROM attention_items WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StorageError::NotFound(format!("attention {id}")))?;
        row_to_attention(&row)
    }

    pub async fn list_tasks(&self) -> StorageResult<Vec<Task>> {
        let rows = sqlx::query("SELECT * FROM tasks ORDER BY updated_at DESC LIMIT 500")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_task).collect()
    }

    pub async fn get_task(&self, id: &TaskId) -> StorageResult<Task> {
        let row = sqlx::query("SELECT * FROM tasks WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StorageError::NotFound(format!("task {id}")))?;
        row_to_task(&row)
    }

    pub async fn list_tools(&self) -> StorageResult<Vec<ToolInstance>> {
        let rows = sqlx::query("SELECT * FROM tool_instances ORDER BY provider, label")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_tool).collect()
    }

    pub async fn get_tool(&self, id: &ToolInstanceId) -> StorageResult<ToolInstance> {
        let row = sqlx::query("SELECT * FROM tool_instances WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StorageError::NotFound(format!("tool {id}")))?;
        row_to_tool(&row)
    }

    /// Upsert a discovered tool instance by executable path.
    pub async fn upsert_tool(&self, tool: &ToolInstance) -> StorageResult<()> {
        sqlx::query(
            "INSERT INTO tool_instances
                (id, provider, label, executable, version, readiness, settings, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                label=excluded.label, version=excluded.version,
                readiness=excluded.readiness, settings=excluded.settings,
                executable=excluded.executable, updated_at=excluded.updated_at",
        )
        .bind(tool.id.as_str())
        .bind(provider_name(tool.provider))
        .bind(&tool.label)
        .bind(&tool.executable)
        .bind(&tool.version)
        .bind(readiness_name(tool.readiness))
        .bind(serde_json::to_string(&tool.settings)?)
        .bind(tool.created_at.to_rfc3339())
        .bind(tool.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Find a registered tool by executable path (dedupe discovery).
    pub async fn find_tool_by_executable(
        &self,
        executable: &str,
    ) -> StorageResult<Option<ToolInstance>> {
        let row = sqlx::query("SELECT * FROM tool_instances WHERE executable = ? LIMIT 1")
            .bind(executable)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(row_to_tool).transpose()
    }

    pub async fn list_workspaces(&self) -> StorageResult<Vec<Workspace>> {
        let rows = sqlx::query("SELECT * FROM workspaces ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_workspace).collect()
    }

    pub async fn get_workspace(&self, id: &WorkspaceId) -> StorageResult<Workspace> {
        let row = sqlx::query("SELECT * FROM workspaces WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StorageError::NotFound(format!("workspace {id}")))?;
        row_to_workspace(&row)
    }

    // ------------------------------------------------------------------
    // Direct writes used by setup flows (not event-sourced themselves —
    // events referencing them are what subscribers see).
    // ------------------------------------------------------------------

    pub async fn insert_workspace(&self, w: &Workspace) -> StorageResult<()> {
        sqlx::query(
            "INSERT INTO workspaces
                (id, name, root, allow_rules, network_allowed, remote_content_allowed, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(w.id.as_str())
        .bind(&w.name)
        .bind(&w.root)
        .bind(serde_json::to_string(&w.allow_rules)?)
        .bind(w.network_allowed)
        .bind(w.remote_content_allowed)
        .bind(w.created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_workspace_rules(&self, w: &Workspace) -> StorageResult<()> {
        sqlx::query(
            "UPDATE workspaces SET allow_rules = ?, network_allowed = ?,
             remote_content_allowed = ? WHERE id = ?",
        )
        .bind(serde_json::to_string(&w.allow_rules)?)
        .bind(w.network_allowed)
        .bind(w.remote_content_allowed)
        .bind(w.id.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Insert (or replace mutable fields of) a session row. The service
    /// writes the full entity; `SessionCreated`/`SessionStateChanged`
    /// events are the notification layer.
    pub async fn upsert_session(&self, s: &Session) -> StorageResult<()> {
        sqlx::query(
            "INSERT INTO sessions
                (id, workspace_id, task_id, tool_instance_id, provider, ownership,
                 native_id, title, state, status_detail, capabilities, pid,
                 process_started_at, last_meaningful_update, usage, last_event_seq,
                 activity_started_at, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                title=excluded.title, state=excluded.state,
                status_detail=excluded.status_detail,
                capabilities=excluded.capabilities,
                native_id=excluded.native_id, pid=excluded.pid,
                process_started_at=excluded.process_started_at,
                last_meaningful_update=excluded.last_meaningful_update,
                usage=excluded.usage, last_event_seq=excluded.last_event_seq,
                activity_started_at=excluded.activity_started_at,
                updated_at=excluded.updated_at",
        )
        .bind(s.id.as_str())
        .bind(s.workspace_id.as_str())
        .bind(s.task_id.as_ref().map(|t| t.as_str()))
        .bind(s.tool_instance_id.as_str())
        .bind(provider_name(s.provider))
        .bind(
            serde_json::to_string(&s.ownership)?
                .trim_matches('"')
                .to_string(),
        )
        .bind(&s.native_id)
        .bind(&s.title)
        .bind(
            serde_json::to_string(&s.state)?
                .trim_matches('"')
                .to_string(),
        )
        .bind(
            s.status_detail
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(serde_json::to_string(&s.capabilities)?)
        .bind(s.pid.map(|p| p as i64))
        .bind(s.process_started_at.map(|d| d.to_rfc3339()))
        .bind(&s.last_meaningful_update)
        .bind(s.usage.as_ref().map(serde_json::to_string).transpose()?)
        .bind(s.last_event_seq)
        .bind(s.activity_started_at.map(|d| d.to_rfc3339()))
        .bind(s.created_at.to_rfc3339())
        .bind(s.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_task(&self, t: &Task) -> StorageResult<()> {
        sqlx::query(
            "INSERT INTO tasks (id, workspace_id, title, description, state, steps, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                title=excluded.title, description=excluded.description,
                state=excluded.state, steps=excluded.steps, updated_at=excluded.updated_at",
        )
        .bind(t.id.as_str())
        .bind(t.workspace_id.as_str())
        .bind(&t.title)
        .bind(&t.description)
        .bind(serde_json::to_string(&t.state)?.trim_matches('"').to_string())
        .bind(serde_json::to_string(&t.steps)?)
        .bind(t.created_at.to_rfc3339())
        .bind(t.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_run(&self, id: &RunId) -> StorageResult<Run> {
        let row = sqlx::query("SELECT * FROM runs WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StorageError::NotFound(format!("run {id}")))?;
        row_to_run(&row)
    }

    /// Expire every open attention item bound to a dead session.
    pub async fn expire_attention_for_session(&self, session: &SessionId) -> StorageResult<u64> {
        let res = sqlx::query(
            "UPDATE attention_items SET status = 'expired', resolved_at = ?
             WHERE session_id = ? AND status = 'open' AND expires_with_session = 1",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(session.as_str())
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Sessions still marked active — reconcile at startup.
    pub async fn active_sessions(&self) -> StorageResult<Vec<Session>> {
        let rows = sqlx::query(
            "SELECT * FROM sessions WHERE state IN
             ('starting','idle','running','waiting_for_input','waiting_for_approval')",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_session).collect()
    }
}

// ----------------------------------------------------------------------
// row mappings
// ----------------------------------------------------------------------

fn provider_name(p: ProviderKind) -> &'static str {
    match p {
        ProviderKind::Codex => "codex",
        ProviderKind::Acp => "acp",
        ProviderKind::Claude => "claude",
        ProviderKind::OpenCode => "opencode",
        ProviderKind::Pty => "pty",
        ProviderKind::Orchestrator => "orchestrator",
    }
}

fn provider_from(s: &str) -> StorageResult<ProviderKind> {
    Ok(match s {
        "codex" => ProviderKind::Codex,
        "acp" => ProviderKind::Acp,
        "claude" => ProviderKind::Claude,
        "opencode" => ProviderKind::OpenCode,
        "pty" => ProviderKind::Pty,
        "orchestrator" => ProviderKind::Orchestrator,
        other => return Err(StorageError::NotFound(format!("provider {other}"))),
    })
}

fn readiness_name(r: ToolReadiness) -> &'static str {
    match r {
        ToolReadiness::Installed => "installed",
        ToolReadiness::Compatible => "compatible",
        ToolReadiness::Authenticated => "authenticated",
        ToolReadiness::Ready => "ready",
    }
}

fn readiness_from(s: &str) -> ToolReadiness {
    match s {
        "compatible" => ToolReadiness::Compatible,
        "authenticated" => ToolReadiness::Authenticated,
        "ready" => ToolReadiness::Ready,
        _ => ToolReadiness::Installed,
    }
}

fn status_name(s: AttentionStatus) -> &'static str {
    match s {
        AttentionStatus::Open => "open",
        AttentionStatus::Answered => "answered",
        AttentionStatus::Expired => "expired",
        AttentionStatus::Superseded => "superseded",
    }
}

fn attention_status_from(s: &str) -> AttentionStatus {
    match s {
        "answered" => AttentionStatus::Answered,
        "expired" => AttentionStatus::Expired,
        "superseded" => AttentionStatus::Superseded,
        _ => AttentionStatus::Open,
    }
}

fn parse_dt(s: &str) -> chrono::DateTime<chrono::Utc> {
    parse_dt_pub(s)
}

pub(crate) fn parse_dt_pub(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now())
}

fn opt_dt(s: Option<String>) -> Option<chrono::DateTime<chrono::Utc>> {
    s.as_deref().map(parse_dt_pub)
}

fn row_to_event(r: &sqlx::sqlite::SqliteRow) -> StorageResult<Event> {
    Ok(Event {
        id: EventId::from_raw(r.get::<String, _>("id")),
        seq: r.get::<i64, _>("seq"),
        schema_version: r.get::<i64, _>("schema_version") as u16,
        at: parse_dt(&r.get::<String, _>("at")),
        source: serde_json::from_str(&format!("\"{}\"", r.get::<String, _>("source")))?,
        session_id: r
            .get::<Option<String>, _>("session_id")
            .map(SessionId::from_raw),
        run_id: r.get::<Option<String>, _>("run_id").map(RunId::from_raw),
        payload: serde_json::from_str(&r.get::<String, _>("payload"))?,
    })
}

fn row_to_session(r: &sqlx::sqlite::SqliteRow) -> StorageResult<Session> {
    Ok(Session {
        id: SessionId::from_raw(r.get::<String, _>("id")),
        workspace_id: WorkspaceId::from_raw(r.get::<String, _>("workspace_id")),
        task_id: r.get::<Option<String>, _>("task_id").map(TaskId::from_raw),
        tool_instance_id: ToolInstanceId::from_raw(r.get::<String, _>("tool_instance_id")),
        provider: provider_from(&r.get::<String, _>("provider"))?,
        ownership: serde_json::from_str(&format!("\"{}\"", r.get::<String, _>("ownership")))?,
        native_id: r.get::<Option<String>, _>("native_id"),
        title: r.get::<String, _>("title"),
        state: serde_json::from_str(&format!("\"{}\"", r.get::<String, _>("state")))?,
        status_detail: r
            .get::<Option<String>, _>("status_detail")
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        capabilities: serde_json::from_str(&r.get::<String, _>("capabilities"))?,
        pid: r.get::<Option<i64>, _>("pid").map(|p| p as u32),
        process_started_at: opt_dt(r.get::<Option<String>, _>("process_started_at")),
        last_meaningful_update: r.get::<Option<String>, _>("last_meaningful_update"),
        usage: r
            .get::<Option<String>, _>("usage")
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        last_event_seq: r.get::<i64, _>("last_event_seq"),
        created_at: parse_dt(&r.get::<String, _>("created_at")),
        updated_at: parse_dt(&r.get::<String, _>("updated_at")),
        activity_started_at: opt_dt(r.get::<Option<String>, _>("activity_started_at")),
    })
}

fn row_to_run(r: &sqlx::sqlite::SqliteRow) -> StorageResult<Run> {
    Ok(Run {
        id: RunId::from_raw(r.get::<String, _>("id")),
        session_id: SessionId::from_raw(r.get::<String, _>("session_id")),
        native_id: r.get::<Option<String>, _>("native_id"),
        state: serde_json::from_str(&format!("\"{}\"", r.get::<String, _>("state")))?,
        instruction: r.get::<String, _>("instruction"),
        attempt: r.get::<i64, _>("attempt") as u32,
        started_at: parse_dt(&r.get::<String, _>("started_at")),
        finished_at: opt_dt(r.get::<Option<String>, _>("finished_at")),
    })
}

fn row_to_attention(r: &sqlx::sqlite::SqliteRow) -> StorageResult<AttentionItem> {
    Ok(AttentionItem {
        id: AttentionId::from_raw(r.get::<String, _>("id")),
        session_id: r
            .get::<Option<String>, _>("session_id")
            .map(SessionId::from_raw),
        run_id: r.get::<Option<String>, _>("run_id").map(RunId::from_raw),
        workspace_id: WorkspaceId::from_raw(r.get::<String, _>("workspace_id")),
        kind: serde_json::from_str(&format!("\"{}\"", r.get::<String, _>("kind")))?,
        status: attention_status_from(&r.get::<String, _>("status")),
        title: r.get::<String, _>("title"),
        body: r.get::<String, _>("body"),
        pending_action: r
            .get::<Option<String>, _>("pending_action")
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        choices: serde_json::from_str(&r.get::<String, _>("choices"))?,
        provider_request_ref: r.get::<Option<String>, _>("provider_request_ref"),
        expires_at: opt_dt(r.get::<Option<String>, _>("expires_at")),
        expires_with_session: r.get::<i64, _>("expires_with_session") != 0,
        created_at: parse_dt(&r.get::<String, _>("created_at")),
        resolved_at: opt_dt(r.get::<Option<String>, _>("resolved_at")),
        resolution: r
            .get::<Option<String>, _>("resolution")
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
    })
}

fn row_to_task(r: &sqlx::sqlite::SqliteRow) -> StorageResult<Task> {
    Ok(Task {
        id: TaskId::from_raw(r.get::<String, _>("id")),
        workspace_id: WorkspaceId::from_raw(r.get::<String, _>("workspace_id")),
        title: r.get::<String, _>("title"),
        description: r.get::<Option<String>, _>("description"),
        state: serde_json::from_str(&format!("\"{}\"", r.get::<String, _>("state")))?,
        steps: serde_json::from_str(&r.get::<String, _>("steps"))?,
        created_at: parse_dt(&r.get::<String, _>("created_at")),
        updated_at: parse_dt(&r.get::<String, _>("updated_at")),
    })
}

fn row_to_tool(r: &sqlx::sqlite::SqliteRow) -> StorageResult<ToolInstance> {
    Ok(ToolInstance {
        id: ToolInstanceId::from_raw(r.get::<String, _>("id")),
        provider: provider_from(&r.get::<String, _>("provider"))?,
        label: r.get::<String, _>("label"),
        executable: r.get::<String, _>("executable"),
        version: r.get::<Option<String>, _>("version"),
        readiness: readiness_from(&r.get::<String, _>("readiness")),
        settings: serde_json::from_str(&r.get::<String, _>("settings"))?,
        created_at: parse_dt(&r.get::<String, _>("created_at")),
        updated_at: parse_dt(&r.get::<String, _>("updated_at")),
    })
}

fn row_to_workspace(r: &sqlx::sqlite::SqliteRow) -> StorageResult<Workspace> {
    Ok(Workspace {
        id: WorkspaceId::from_raw(r.get::<String, _>("id")),
        name: r.get::<String, _>("name"),
        root: r.get::<String, _>("root"),
        allow_rules: serde_json::from_str(&r.get::<String, _>("allow_rules"))?,
        network_allowed: r.get::<i64, _>("network_allowed") != 0,
        remote_content_allowed: r.get::<i64, _>("remote_content_allowed") != 0,
        created_at: parse_dt(&r.get::<String, _>("created_at")),
    })
}

// ----------------------------------------------------------------------
// write path
// ----------------------------------------------------------------------

async fn insert_event(tx: &mut Transaction<'_, Sqlite>, new: NewEvent) -> StorageResult<Event> {
    let id = EventId::new();
    let at = chrono::Utc::now();
    let payload = serde_json::to_string(&new.payload)?;
    let source = serde_json::to_string(&new.source)?;
    let source = source.trim_matches('"').to_string();
    let row = sqlx::query(
        "INSERT INTO events (id, at, source, session_id, run_id, schema_version, payload)
         VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING seq",
    )
    .bind(id.as_str())
    .bind(at.to_rfc3339())
    .bind(source)
    .bind(new.session_id.as_ref().map(|s| s.as_str()))
    .bind(new.run_id.as_ref().map(|s| s.as_str()))
    .bind(new.schema_version as i64)
    .bind(payload.clone())
    .fetch_one(&mut **tx)
    .await?;
    Ok(Event {
        id,
        seq: row.get::<i64, _>("seq"),
        schema_version: new.schema_version,
        at,
        source: new.source,
        session_id: new.session_id,
        run_id: new.run_id,
        payload: new.payload,
    })
}

async fn bump_session_seq(tx: &mut Transaction<'_, Sqlite>, ev: &Event) -> StorageResult<()> {
    if let Some(sid) = &ev.session_id {
        sqlx::query("UPDATE sessions SET last_event_seq = ? WHERE id = ?")
            .bind(ev.seq)
            .bind(sid.as_str())
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// The projection: keep current-state tables in lockstep with events.
async fn apply_projection(tx: &mut Transaction<'_, Sqlite>, ev: &Event) -> StorageResult<()> {
    let now = ev.at.to_rfc3339();
    match &ev.payload {
        EventPayload::SessionCreated {
            session_id,
            ownership,
            title,
        } => {
            // Runtime inserts the full row via `upsert_session_row`;
            // this branch exists so replaying the log stays total.
            let _ = (session_id, ownership, title);
        }
        EventPayload::SessionStateChanged {
            session_id,
            to,
            reason,
            inferred,
            ..
        } => {
            let detail = serde_json::to_string(&if *inferred {
                StatusDetail::Inference {
                    message: reason.clone(),
                }
            } else {
                StatusDetail::Fact {
                    message: reason.clone(),
                }
            })?;
            let state = serde_json::to_string(to)?;
            sqlx::query(
                "UPDATE sessions SET state = ?, status_detail = ?, updated_at = ?,
                 activity_started_at = CASE WHEN ? IN ('running') THEN ? ELSE activity_started_at END
                 WHERE id = ?",
            )
            .bind(state.trim_matches('"'))
            .bind(detail)
            .bind(&now)
            .bind(state.trim_matches('"'))
            .bind(&now)
            .bind(session_id.as_str())
            .execute(&mut **tx)
            .await?;
        }
        EventPayload::SessionNativeIdAssigned {
            session_id,
            native_id,
        } => {
            sqlx::query("UPDATE sessions SET native_id = ?, updated_at = ? WHERE id = ?")
                .bind(native_id)
                .bind(&now)
                .bind(session_id.as_str())
                .execute(&mut **tx)
                .await?;
        }
        EventPayload::SessionUpdate {
            session_id,
            summary,
        } => {
            sqlx::query(
                "UPDATE sessions SET last_meaningful_update = ?, updated_at = ? WHERE id = ?",
            )
            .bind(summary)
            .bind(&now)
            .bind(session_id.as_str())
            .execute(&mut **tx)
            .await?;
        }
        EventPayload::SessionUsage { session_id, usage } => {
            sqlx::query("UPDATE sessions SET usage = ?, updated_at = ? WHERE id = ?")
                .bind(serde_json::to_string(usage)?)
                .bind(&now)
                .bind(session_id.as_str())
                .execute(&mut **tx)
                .await?;
        }
        EventPayload::RunStarted {
            run_id,
            session_id,
            instruction,
            attempt,
        } => {
            sqlx::query(
                "INSERT INTO runs (id, session_id, state, instruction, attempt, started_at)
                 VALUES (?, ?, 'running', ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET attempt = excluded.attempt",
            )
            .bind(run_id.as_str())
            .bind(session_id.as_str())
            .bind(instruction)
            .bind(*attempt as i64)
            .bind(&now)
            .execute(&mut **tx)
            .await?;
        }
        EventPayload::RunStateChanged { run_id, to, .. } => {
            let finished = matches!(
                to,
                RunState::Succeeded
                    | RunState::Failed
                    | RunState::Cancelled
                    | RunState::OutcomeUnknown
            );
            sqlx::query(
                "UPDATE runs SET state = ?, finished_at = CASE WHEN ? THEN ? ELSE finished_at END
                 WHERE id = ?",
            )
            .bind(serde_json::to_string(to)?.trim_matches('"').to_string())
            .bind(finished)
            .bind(&now)
            .bind(run_id.as_str())
            .execute(&mut **tx)
            .await?;
        }
        EventPayload::AttentionRaised {
            attention_id,
            session_id,
            workspace_id,
            run_id,
            kind,
            title,
            body,
            pending_action,
            choices,
            provider_request_ref,
            expires_with_session,
        } => {
            sqlx::query(
                "INSERT INTO attention_items
                 (id, session_id, run_id, workspace_id, kind, status, title, body,
                  pending_action, choices, provider_request_ref, expires_with_session, created_at)
                 VALUES (?, ?, ?, ?, ?, 'open', ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(attention_id.as_str())
            .bind(session_id.as_ref().map(|s| s.as_str()))
            .bind(run_id.as_ref().map(|s| s.as_str()))
            .bind(workspace_id.as_str())
            .bind(serde_json::to_string(kind)?.trim_matches('"').to_string())
            .bind(title)
            .bind(body)
            .bind(
                pending_action
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
            )
            .bind(serde_json::to_string(choices)?)
            .bind(provider_request_ref)
            .bind(*expires_with_session)
            .bind(&now)
            .execute(&mut **tx)
            .await?;
        }
        EventPayload::AttentionStatusChanged {
            attention_id,
            to,
            resolution,
            ..
        } => {
            sqlx::query(
                "UPDATE attention_items SET status = ?, resolved_at = ?, resolution = ? WHERE id = ?",
            )
            .bind(status_name(*to))
            .bind(&now)
            .bind(resolution.as_ref().map(serde_json::to_string).transpose()?)
            .bind(attention_id.as_str())
            .execute(&mut **tx)
            .await?;
        }
        EventPayload::ArtifactRegistered {
            artifact_id,
            session_id,
            run_id,
            label,
            path,
        } => {
            sqlx::query(
                "INSERT INTO artifacts (id, run_id, session_id, kind, label, path, created_at)
                 VALUES (?, ?, ?, 'other', ?, ?, ?)",
            )
            .bind(artifact_id.as_str())
            .bind(run_id.as_ref().map(|s| s.as_str()))
            .bind(session_id.as_ref().map(|s| s.as_str()))
            .bind(label)
            .bind(path)
            .bind(&now)
            .execute(&mut **tx)
            .await?;
        }
        EventPayload::ChatMessage {
            message_id,
            role,
            operations,
            preview,
        } => {
            sqlx::query(
                "INSERT INTO chat_messages (id, role, content, operations, at)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(message_id)
            .bind(role)
            .bind(preview)
            .bind(serde_json::to_string(
                &operations.iter().map(|o| o.as_str()).collect::<Vec<_>>(),
            )?)
            .bind(&now)
            .execute(&mut **tx)
            .await?;
        }
        EventPayload::ObservedProcessSeen {
            pid,
            exe,
            provider,
            started_at,
            cwd,
        } => {
            sqlx::query(
                "INSERT INTO observed_processes (pid, exe, provider, started_at, cwd, first_seen, last_seen)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(pid, started_at) DO UPDATE SET last_seen = excluded.last_seen",
            )
            .bind(*pid as i64)
            .bind(exe)
            .bind(provider_name(*provider))
            .bind(started_at.map(|d| d.to_rfc3339()))
            .bind(cwd)
            .bind(&now)
            .bind(&now)
            .execute(&mut **tx)
            .await?;
        }
        EventPayload::ObservedProcessGone { pid, exe } => {
            sqlx::query("DELETE FROM observed_processes WHERE pid = ? AND exe = ?")
                .bind(*pid as i64)
                .bind(exe)
                .execute(&mut **tx)
                .await?;
        }
        // Command intents are persisted via `record_intent` (they must
        // exist BEFORE the dispatch decision); task/workspace/tool rows
        // are direct writes. These event rows are notifications only.
        EventPayload::CommandIntent { .. }
        | EventPayload::CommandResolved { .. }
        | EventPayload::OutputAppended { .. }
        | EventPayload::TaskChanged { .. }
        | EventPayload::WorkspaceChanged { .. }
        | EventPayload::ToolChanged { .. } => {}
    }
    Ok(())
}
