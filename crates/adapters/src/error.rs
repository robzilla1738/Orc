use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("provider error: {0}")]
    Provider(String),

    #[error("protocol violation: {0}")]
    Protocol(String),

    #[error("not supported: {0}")]
    Unsupported(String),

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("spawn failed: {0}")]
    Spawn(String),

    #[error("channel closed")]
    Closed,
}

pub type AdapterResult<T> = Result<T, AdapterError>;
