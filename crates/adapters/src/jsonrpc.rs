//! Newline-delimited JSON-RPC over child stdio — the shared transport
//! for Codex app-server and ACP. Client calls resolve through oneshot
//! channels; server→client requests surface as `Incoming::Request` so
//! approvals/questions can be answered bidirectionally.

use crate::error::{AdapterError, AdapterResult};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

/// A message arriving from the child that isn't a response to one of
/// our calls.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum Incoming {
    /// Server→client request: must be answered via `respond`.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// Fire-and-forget notification.
    Notification { method: String, params: Value },
}

#[derive(Debug)]
struct Pending {
    tx: oneshot::Sender<Result<Value, String>>,
}

#[derive(Clone)]
pub struct StdioJsonRpc {
    outgoing: mpsc::UnboundedSender<Value>,
    pending: Arc<Mutex<HashMap<i64, Pending>>>,
    next_id: Arc<AtomicI64>,
}

impl StdioJsonRpc {
    /// Spawn read/write tasks over child stdio. `incoming` receives
    /// server-initiated requests and notifications. `closed` resolves
    /// when the read side ends (process exit / EOF).
    pub fn start(
        stdin: ChildStdin,
        stdout: ChildStdout,
        incoming: mpsc::Sender<Incoming>,
    ) -> (Self, JoinHandle<()>) {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
        let pending: Arc<Mutex<HashMap<i64, Pending>>> = Arc::new(Mutex::new(HashMap::new()));

        // Writer task.
        let mut stdin = stdin;
        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                let mut line = msg.to_string();
                line.push('\n');
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        // Reader task.
        let pending_reader = pending.clone();
        let (closed_tx, closed_rx) = oneshot::channel::<()>();
        let read_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut closed_tx = Some(closed_tx);
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        let msg: Value = match serde_json::from_str(line) {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::debug!(error = %e, "dropping non-JSON rpc line");
                                continue;
                            }
                        };
                        handle_message(&msg, &pending_reader, &incoming).await;
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(error = %e, "rpc reader error");
                        break;
                    }
                }
            }
            if let Some(tx) = closed_tx.take() {
                let _ = tx.send(());
            }
            // Fail every unresolved call so callers don't hang forever.
            let mut p = pending_reader.lock().await;
            for (_, pending) in p.drain() {
                let _ = pending.tx.send(Err("connection closed".into()));
            }
        });

        let rpc = Self {
            outgoing: out_tx,
            pending,
            next_id: Arc::new(AtomicI64::new(1)),
        };
        // The returned JoinHandle resolves when the read loop ends —
        // that IS the closed signal for callers.
        drop(closed_rx);
        (rpc, read_task)
    }

    /// A request expecting a response.
    pub async fn call(&self, method: &str, params: Value) -> AdapterResult<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, Pending { tx });
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.outgoing.send(msg).map_err(|_| AdapterError::Closed)?;
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(AdapterError::Protocol(e)),
            Err(_) => Err(AdapterError::Closed),
        }
    }

    /// A notification — no response expected.
    pub fn notify(&self, method: &str, params: Value) -> AdapterResult<()> {
        self.outgoing
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            }))
            .map_err(|_| AdapterError::Closed)
    }

    /// Respond to a server→client request (e.g. an approval answer).
    pub fn respond(&self, id: Value, result: Value) -> AdapterResult<()> {
        self.outgoing
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result,
            }))
            .map_err(|_| AdapterError::Closed)
    }

    /// Respond to a server→client request with an error.
    pub fn respond_error(&self, id: Value, code: i64, message: &str) -> AdapterResult<()> {
        self.outgoing
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": code, "message": message },
            }))
            .map_err(|_| AdapterError::Closed)
    }
}

/// A `request_ref` stores the JSON-serialized form of the original RPC
/// id — parse it back so the response id keeps its original type.
pub fn ref_to_value(request_ref: &str) -> Value {
    serde_json::from_str(request_ref).unwrap_or_else(|_| Value::from(request_ref))
}

async fn handle_message(
    msg: &Value,
    pending: &Arc<Mutex<HashMap<i64, Pending>>>,
    incoming: &mpsc::Sender<Incoming>,
) {
    let has_method = msg.get("method").is_some();
    let has_id = msg.get("id").is_some();
    let is_response = msg.get("result").is_some() || msg.get("error").is_some();

    match (has_method, has_id, is_response) {
        // Server→client request (approval, fs access, …).
        (true, true, false) => {
            let _ = incoming
                .send(Incoming::Request {
                    id: msg["id"].clone(),
                    method: msg["method"].as_str().unwrap_or_default().to_string(),
                    params: msg.get("params").cloned().unwrap_or(Value::Null),
                })
                .await;
        }
        // Notification.
        (true, false, false) => {
            let _ = incoming
                .send(Incoming::Notification {
                    method: msg["method"].as_str().unwrap_or_default().to_string(),
                    params: msg.get("params").cloned().unwrap_or(Value::Null),
                })
                .await;
        }
        // Response to one of our calls.
        (false, true, true) => {
            if let Some(id) = msg["id"].as_i64() {
                if let Some(p) = pending.lock().await.remove(&id) {
                    if let Some(err) = msg.get("error") {
                        let _ = p.tx.send(Err(err.to_string()));
                    } else {
                        let _ = p.tx.send(Ok(msg["result"].clone()));
                    }
                }
            }
        }
        _ => {
            tracing::debug!(%msg, "unclassified rpc frame");
        }
    }
}
