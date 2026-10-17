//! Orchestrator chat transcript (separate from provider transcripts).

use crate::error::StorageResult;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub role: String,
    /// JSON: text + structured operation cards.
    pub content: serde_json::Value,
    pub operations: Vec<String>,
    pub at: DateTime<Utc>,
}

impl crate::Store {
    pub async fn append_chat_message(&self, msg: &ChatMessage) -> StorageResult<()> {
        sqlx::query(
            "INSERT INTO chat_messages (id, role, content, operations, at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&msg.id)
        .bind(&msg.role)
        .bind(serde_json::to_string(&msg.content)?)
        .bind(serde_json::to_string(&msg.operations)?)
        .bind(msg.at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn chat_history(&self, limit: u32) -> StorageResult<Vec<ChatMessage>> {
        let rows = sqlx::query(
            "SELECT id, role, content, operations, at FROM chat_messages
             ORDER BY at DESC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(ChatMessage {
                id: r.get::<String, _>("id"),
                role: r.get::<String, _>("role"),
                content: serde_json::from_str(&r.get::<String, _>("content"))?,
                operations: serde_json::from_str(&r.get::<String, _>("operations"))?,
                at: crate::store::parse_dt_pub(&r.get::<String, _>("at")),
            });
        }
        out.reverse();
        Ok(out)
    }
}
