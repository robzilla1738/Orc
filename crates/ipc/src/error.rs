use thiserror::Error;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("handshake failed: {0}")]
    Handshake(String),

    #[error("protocol version mismatch: client {client}, server {server}")]
    VersionMismatch { client: u16, server: u16 },

    #[error("daemon error: {0}")]
    Daemon(String),

    #[error("connection closed")]
    Closed,

    #[error("timeout")]
    Timeout,
}

pub type IpcResult<T> = Result<T, IpcError>;
