//! The daemon side of the IPC boundary.

use crate::auth;
use crate::error::{IpcError, IpcResult};
use crate::proto::*;
use async_trait::async_trait;
use interprocess::local_socket::{
    tokio::{prelude::*, Stream},
    GenericFilePath, ListenerOptions, ToFsName,
};
use orc_domain::{Event, ServerEvent, PROTOCOL_VERSION, SCHEMA_VERSION};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

/// What the daemon must provide to serve requests.
#[async_trait]
pub trait RequestHandler: Send + Sync {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, String>;
    /// A live event subscription; `since_seq` is the replay cursor.
    fn subscribe_events(&self) -> broadcast::Receiver<Event>;
    async fn events_since(&self, since_seq: i64, limit: u32) -> Result<Vec<Event>, String>;
    /// Daemon auth token presented during hello.
    fn expected_token(&self) -> String;
    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
}

/// Bind the listener. Caller picks the filesystem path on Unix; Windows
/// uses a namespaced pipe.
pub fn socket_name(path: &Path) -> IpcResult<interprocess::local_socket::Name<'static>> {
    #[cfg(unix)]
    {
        // Stale socket files are safe to replace.
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        path.to_path_buf()
            .to_fs_name::<GenericFilePath>()
            .map_err(|e| IpcError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, e)))
    }
    #[cfg(windows)]
    {
        let _ = path;
        return "orc-daemon"
            .to_ns_name::<GenericNamespaced>()
            .map_err(|e| IpcError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, e)));
    }
}

/// Accept loop — each connection runs `serve_conn`.
pub async fn serve(
    sock_path: &Path,
    handler: Arc<dyn RequestHandler>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> IpcResult<()> {
    let name = socket_name(sock_path)?;
    let listener = ListenerOptions::new()
        .name(name)
        .create_tokio()
        .map_err(IpcError::Io)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(sock_path, std::fs::Permissions::from_mode(0o600));
    }
    info!(path = %sock_path.display(), "ipc listener up");

    loop {
        tokio::select! {
            res = listener.accept() => {
                match res {
                    Ok(stream) => {
                        let handler = handler.clone();
                        tokio::spawn(async move {
                            if let Err(e) = serve_conn(stream, handler).await {
                                debug!(error = %e, "ipc connection ended");
                            }
                        });
                    }
                    Err(e) => {
                        warn!(error = %e, "accept failed");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    info!("ipc listener shutting down");
                    return Ok(());
                }
            }
        }
    }
}

/// One connection: hello → request/response loop (or an event stream).
async fn serve_conn(stream: Stream, handler: Arc<dyn RequestHandler>) -> IpcResult<()> {
    let (read, mut write) = stream.split();
    let mut reader = BufReader::with_capacity(64 * 1024, read);

    // ---- handshake ----------------------------------------------------
    let mut line = String::new();
    let n = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        reader.read_line(&mut line),
    )
    .await
    .map_err(|_| IpcError::Timeout)??;
    if n == 0 {
        return Err(IpcError::Closed);
    }
    let hello: Hello = serde_json::from_str(line.trim())
        .map_err(|e| IpcError::Handshake(format!("bad hello: {e}")))?;

    if hello.protocol_version != PROTOCOL_VERSION {
        let _ = write_json(
            &mut write,
            &serde_json::json!({
                "error": format!("protocol mismatch: client {} server {}",
                                 hello.protocol_version, PROTOCOL_VERSION),
            }),
        )
        .await;
        return Err(IpcError::VersionMismatch {
            client: hello.protocol_version,
            server: PROTOCOL_VERSION,
        });
    }
    if !auth::check_token(&handler.expected_token(), &hello.token) {
        let _ = write_json(&mut write, &serde_json::json!({"error": "bad token"})).await;
        return Err(IpcError::Handshake("auth token rejected".into()));
    }
    write_json(
        &mut write,
        &HelloAck {
            protocol_version: PROTOCOL_VERSION,
            schema_version: SCHEMA_VERSION,
            daemon_version: handler.version().to_string(),
            server_time: chrono::Utc::now(),
        },
    )
    .await?;
    debug!(client = %hello.client, "ipc client connected");

    // ---- request loop --------------------------------------------------
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }
        let req: Request = match serde_json::from_str(line.trim()) {
            Ok(r) => r,
            Err(e) => {
                write_json(
                    &mut write,
                    &Response {
                        id: 0,
                        result: None,
                        error: Some(format!("bad request: {e}")),
                        streaming: false,
                    },
                )
                .await?;
                continue;
            }
        };

        if req.method == methods::EVENTS_SUBSCRIBE {
            // Switch into push mode — no more responses on this conn.
            write_json(
                &mut write,
                &Response {
                    id: req.id,
                    result: Some(serde_json::json!({"subscribed": true})),
                    error: None,
                    streaming: true,
                },
            )
            .await?;
            let since = req
                .params
                .get("since_seq")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            return stream_events(since, &mut write, &handler).await;
        }

        let handler = handler.clone();
        match handler.handle(&req.method, req.params).await {
            Ok(result) => {
                write_json(
                    &mut write,
                    &Response {
                        id: req.id,
                        result: Some(result),
                        error: None,
                        streaming: false,
                    },
                )
                .await?;
            }
            Err(error) => {
                write_json(
                    &mut write,
                    &Response {
                        id: req.id,
                        result: None,
                        error: Some(error),
                        streaming: false,
                    },
                )
                .await?;
            }
        }
    }
}

/// Push live events; on lag, tell the client to resync.
async fn stream_events(
    since: i64,
    write: &mut (impl AsyncWriteExt + Unpin),
    handler: &Arc<dyn RequestHandler>,
) -> IpcResult<()> {
    // Replay the gap first so subscribers never miss history.
    for ev in handler
        .events_since(since, 10_000)
        .await
        .unwrap_or_default()
    {
        write_json(write, &to_frame(&ev)).await?;
    }
    let mut rx = handler.subscribe_events();
    loop {
        match rx.recv().await {
            Ok(ev) => {
                write_json(write, &to_frame(&ev)).await?;
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                let frame = ServerEvent {
                    seq: -1,
                    at: chrono::Utc::now(),
                    session_id: None,
                    run_id: None,
                    payload: serde_json::json!({
                        "type": "resync_required",
                        "lagged": n,
                    }),
                };
                write_json(write, &frame).await?;
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

fn to_frame(ev: &Event) -> ServerEvent {
    ServerEvent {
        seq: ev.seq,
        at: ev.at,
        session_id: ev.session_id.clone(),
        run_id: ev.run_id.clone(),
        payload: serde_json::to_value(&ev.payload).unwrap_or(Value::Null),
    }
}

async fn write_json(
    w: &mut (impl AsyncWriteExt + Unpin),
    v: &impl serde::Serialize,
) -> IpcResult<()> {
    let mut s = serde_json::to_string(v)?;
    s.push('\n');
    w.write_all(s.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}
