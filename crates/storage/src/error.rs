use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Sql(#[from] sqlx::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("entity not found: {0}")]
    NotFound(String),

    #[error("migration failed at {step}: {reason}")]
    Migration { step: String, reason: String },
}

pub type StorageResult<T> = Result<T, StorageError>;
