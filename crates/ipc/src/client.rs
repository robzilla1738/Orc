//! The client side: connect, hello, multiplexed calls, subscriptions.

use crate::auth;
use crate::error::{IpcError, IpcResult};
use crate::proto::*;
use interprocess::local_socket::{
    tokio::{prelude::*, Stream},
    GenericFilePath, ToFsName,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex};

#[derive(Clone)]
pub struct IpcClient {
    writer: mpsc::UnboundedSender<Value>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>>,
    next_id: Arc<AtomicU64>,
    pub server: HelloAck,
}

impl IpcClient {
    /// Connect to a daemon under `runtime_dir`, performing the
    /// authenticated handshake and version negotiation.
    pub async fn connect(runtime_dir: &Path, client_name: &str) -> IpcResult<Self> {
        let token = auth::read_token(runtime_dir)?;
        let sock = auth::socket_path(runtime_dir);
        Self::connect_with(&sock, &token, client_name).await
    }

    pub async fn connect_with(sock_path: &Path, token: &str, client_name: &str) -> IpcResult<Self> {
        #[cfg(unix)]
        let name = sock_path
            .to_fs_name::<GenericFilePath>()
            .map_err(|e| IpcError::Handshake(e.to_string()))?;
        #[cfg(windows)]
        let name = {
            let _ = sock_path;
            "orc-daemon"
                .to_ns_name::<GenericNamespaced>()
                .map_err(|e| IpcError::Handshake(e.to_string()))?
        };

        let stream = Stream::connect(name)
            .await
            .map_err(|e| IpcError::Handshake(format!("connect {}: {e}", sock_path.display())))?;
        let (read, mut write) = stream.split();
        let mut reader = BufReader::with_capacity(64 * 1024, read);

        // Handshake.
        let hello = Hello::new(client_name, token);
        write_line(&mut write, &hello).await?;
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            reader.read_line(&mut line),
        )
        .await
        .map_err(|_| IpcError::Timeout)??;
        let v: Value = serde_json::from_str(line.trim())?;
        if let Some(err) = v.get("error") {
            return Err(IpcError::Handshake(err.to_string()));
        }
        let ack: HelloAck = serde_json::from_value(v)?;

        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Writer task.
        tokio::spawn(async move {
            while let Some(v) = rx.recv().await {
                if write_line(&mut write, &v).await.is_err() {
                    break;
                }
            }
        });

        // Reader task → resolve pending by id, forward event frames.
        let pending_reader = pending.clone();
        tokio::spawn(async move {
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let v: Value = match serde_json::from_str(line.trim()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(id) = v.get("id").and_then(Value::as_u64) {
                    if let Some(tx) = pending_reader.lock().await.remove(&id) {
                        let resp: Response = match serde_json::from_value(v) {
                            Ok(r) => r,
                            Err(e) => {
                                let _ = tx.send(Response {
                                    id,
                                    result: None,
                                    error: Some(e.to_string()),
                                    streaming: false,
                                });
                                continue;
                            }
                        };
                        let _ = tx.send(resp);
                    }
                }
                // Event frames are consumed by `subscribe`'s dedicated
                // connection — the main conn shouldn't see them.
            }
            // Connection died → fail all pending calls.
            let mut p = pending_reader.lock().await;
            for (_, tx) in p.drain() {
                let _ = tx.send(Response {
                    id: 0,
                    result: None,
                    error: Some("connection closed".into()),
                    streaming: false,
                });
            }
        });

        Ok(Self {
            writer: tx,
            pending,
            next_id: Arc::new(AtomicU64::new(1)),
            server: ack,
        })
    }

    /// One request → one response.
    pub async fn call(&self, method: &str, params: Value) -> IpcResult<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.writer
            .send(serde_json::to_value(Request {
                id,
                method: method.to_string(),
                params,
            })?)
            .map_err(|_| IpcError::Closed)?;
        let resp = rx.await.map_err(|_| IpcError::Closed)?;
        if let Some(err) = resp.error {
            return Err(IpcError::Daemon(err));
        }
        Ok(resp.result.unwrap_or(Value::Null))
    }

    /// A dedicated subscription connection — returns raw frames so the
    /// caller sees `resync_required` markers as well as `ServerEvent`s.
    pub async fn subscribe(
        runtime_dir: &Path,
        client_name: &str,
        since_seq: i64,
    ) -> IpcResult<mpsc::Receiver<Value>> {
        let token = auth::read_token(runtime_dir)?;
        let sock = auth::socket_path(runtime_dir);
        #[cfg(unix)]
        let name = sock
            .to_fs_name::<GenericFilePath>()
            .map_err(|e| IpcError::Handshake(e.to_string()))?;
        #[cfg(windows)]
        let name = {
            let _ = sock;
            "orc-daemon"
                .to_ns_name::<GenericNamespaced>()
                .map_err(|e| IpcError::Handshake(e.to_string()))?
        };
        let stream = Stream::connect(name).await?;
        let (read, mut write) = stream.split();
        let mut reader = BufReader::with_capacity(64 * 1024, read);

        write_line(&mut write, &Hello::new(client_name, token)).await?;
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let ack: Value = serde_json::from_str(line.trim())?;
        if ack.get("error").is_some() {
            return Err(IpcError::Handshake(ack.to_string()));
        }

        let req = Request {
            id: 1,
            method: methods::EVENTS_SUBSCRIBE.to_string(),
            params: serde_json::json!({ "since_seq": since_seq }),
        };
        write_line(&mut write, &req).await?;
        line.clear();
        reader.read_line(&mut line).await?;
        let resp: Response = serde_json::from_str(line.trim())?;
        if let Some(err) = resp.error {
            return Err(IpcError::Daemon(err));
        }

        let (tx, rx) = mpsc::channel(1024);
        tokio::spawn(async move {
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                match serde_json::from_str::<Value>(line.trim()) {
                    Ok(frame) => {
                        if tx.send(frame).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "bad event frame");
                    }
                }
            }
        });
        Ok(rx)
    }
}

async fn write_line(
    w: &mut (impl AsyncWriteExt + Unpin),
    v: &impl serde::Serialize,
) -> IpcResult<()> {
    let mut s = serde_json::to_string(v)?;
    s.push('\n');
    w.write_all(s.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}
