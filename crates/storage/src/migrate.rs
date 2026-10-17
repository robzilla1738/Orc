//! Embedded migrations, applied in order inside a transaction. Each
//! migration runs once, tracked in `schema_migrations`.

use crate::error::{StorageError, StorageResult};
use sqlx::SqlitePool;

const MIGRATIONS: &[(&str, &str)] = &[(
    "0001_init",
    r#"
        CREATE TABLE IF NOT EXISTS events (
            seq             INTEGER PRIMARY KEY AUTOINCREMENT,
            id              TEXT NOT NULL,
            at              TEXT NOT NULL,
            source          TEXT NOT NULL,
            session_id      TEXT,
            run_id          TEXT,
            schema_version  INTEGER NOT NULL,
            payload         TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id, seq);
        CREATE INDEX IF NOT EXISTS idx_events_run ON events(run_id, seq);

        CREATE TABLE IF NOT EXISTS workspaces (
            id                      TEXT PRIMARY KEY,
            name                    TEXT NOT NULL,
            root                    TEXT NOT NULL,
            allow_rules             TEXT NOT NULL DEFAULT '[]',
            network_allowed         INTEGER NOT NULL DEFAULT 0,
            remote_content_allowed  INTEGER NOT NULL DEFAULT 1,
            created_at              TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS tool_instances (
            id          TEXT PRIMARY KEY,
            provider    TEXT NOT NULL,
            label       TEXT NOT NULL,
            executable  TEXT NOT NULL,
            version     TEXT,
            readiness   TEXT NOT NULL,
            settings    TEXT NOT NULL DEFAULT '{}',
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS tasks (
            id          TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            title       TEXT NOT NULL,
            description TEXT,
            state       TEXT NOT NULL,
            steps       TEXT NOT NULL DEFAULT '[]',
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS sessions (
            id                      TEXT PRIMARY KEY,
            workspace_id            TEXT NOT NULL,
            task_id                 TEXT,
            tool_instance_id        TEXT NOT NULL,
            provider                TEXT NOT NULL,
            ownership               TEXT NOT NULL,
            native_id               TEXT,
            title                   TEXT NOT NULL,
            state                   TEXT NOT NULL,
            status_detail           TEXT,
            capabilities            TEXT NOT NULL,
            pid                     INTEGER,
            process_started_at      TEXT,
            last_meaningful_update  TEXT,
            usage                   TEXT,
            last_event_seq          INTEGER NOT NULL DEFAULT 0,
            activity_started_at     TEXT,
            created_at              TEXT NOT NULL,
            updated_at              TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_sessions_workspace ON sessions(workspace_id);
        CREATE INDEX IF NOT EXISTS idx_sessions_task ON sessions(task_id);
        CREATE UNIQUE INDEX idx_sessions_native
            ON sessions(tool_instance_id, native_id)
            WHERE native_id IS NOT NULL;

        CREATE TABLE IF NOT EXISTS runs (
            id          TEXT PRIMARY KEY,
            session_id  TEXT NOT NULL,
            native_id   TEXT,
            state       TEXT NOT NULL,
            instruction TEXT NOT NULL,
            attempt     INTEGER NOT NULL DEFAULT 1,
            started_at  TEXT NOT NULL,
            finished_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_runs_session ON runs(session_id);

        CREATE TABLE IF NOT EXISTS attention_items (
            id                    TEXT PRIMARY KEY,
            session_id            TEXT,
            run_id                TEXT,
            workspace_id          TEXT NOT NULL,
            kind                  TEXT NOT NULL,
            status                TEXT NOT NULL,
            title                 TEXT NOT NULL,
            body                  TEXT NOT NULL,
            pending_action        TEXT,
            choices               TEXT NOT NULL DEFAULT '[]',
            provider_request_ref  TEXT,
            expires_at            TEXT,
            expires_with_session  INTEGER NOT NULL DEFAULT 1,
            created_at            TEXT NOT NULL,
            resolved_at           TEXT,
            resolution            TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_attention_status ON attention_items(status);
        CREATE INDEX IF NOT EXISTS idx_attention_session ON attention_items(session_id);

        CREATE TABLE IF NOT EXISTS artifacts (
            id          TEXT PRIMARY KEY,
            run_id      TEXT,
            session_id  TEXT,
            kind        TEXT NOT NULL,
            label       TEXT NOT NULL,
            path        TEXT NOT NULL,
            sha256      TEXT,
            byte_len    INTEGER NOT NULL DEFAULT 0,
            created_at  TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_artifacts_session ON artifacts(session_id);

        -- Intent log: every side-effecting command recorded before
        -- dispatch, resolved after. Survives crashes for reconciliation.
        CREATE TABLE IF NOT EXISTS commands (
            operation_id    TEXT PRIMARY KEY,
            idempotency_key TEXT NOT NULL UNIQUE,
            actor           TEXT NOT NULL,
            kind            TEXT NOT NULL,
            outcome         TEXT,
            created_at      TEXT NOT NULL,
            resolved_at     TEXT
        );

        CREATE TABLE IF NOT EXISTS chat_messages (
            id          TEXT PRIMARY KEY,
            role        TEXT NOT NULL,
            content     TEXT NOT NULL,
            operations  TEXT NOT NULL DEFAULT '[]',
            at          TEXT NOT NULL
        );

        -- Session↔external-process observation (identity over name:
        -- pid + exe + start time, so stale pids are never signaled).
        CREATE TABLE IF NOT EXISTS observed_processes (
            pid         INTEGER NOT NULL,
            exe         TEXT NOT NULL,
            provider    TEXT NOT NULL,
            started_at  TEXT,
            cwd         TEXT,
            first_seen  TEXT NOT NULL,
            last_seen   TEXT NOT NULL,
            PRIMARY KEY (pid, started_at)
        );
        "#,
)];

pub async fn run(pool: &SqlitePool) -> StorageResult<()> {
    // Bootstrap the migrations table before checking it.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            name TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    for (name, sql) in MIGRATIONS {
        let applied: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE name = ?)")
                .bind(name)
                .fetch_one(pool)
                .await?;
        if applied {
            continue;
        }
        let mut tx = pool.begin().await?;
        for stmt in sql.split(';') {
            let stmt = stmt.trim();
            if stmt.is_empty() {
                continue;
            }
            sqlx::query(stmt)
                .execute(&mut *tx)
                .await
                .map_err(|e| StorageError::Migration {
                    step: name.to_string(),
                    reason: e.to_string(),
                })?;
        }
        sqlx::query("INSERT INTO schema_migrations (name, applied_at) VALUES (?, ?)")
            .bind(name)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        tracing::info!(migration = name, "applied schema migration");
    }
    Ok(())
}
