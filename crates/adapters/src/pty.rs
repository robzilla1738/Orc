//! Generic pseudo-terminal adapter — the honest fallback for tools
//! with no richer protocol. Interactive I/O only: output capture,
//! stdin writes, kill. A model may flag likely questions in output,
//! but those never become trusted approval requests.

use crate::error::{AdapterError, AdapterResult};
use crate::traits::*;
use async_trait::async_trait;
use orc_domain::*;
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct PtyAdapter;

impl PtyAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PtyAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Kill a process by pid without holding the `Child` handle — the wait
/// thread keeps that lock for the process's whole life.
fn kill_pid(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status();
    }
}

#[async_trait]
impl AgentAdapter for PtyAdapter {
    fn provider(&self) -> ProviderKind {
        ProviderKind::Pty
    }

    fn base_capabilities(&self) -> Capabilities {
        Capabilities::pty()
    }

    async fn spawn(&self, spec: SpawnSpec) -> AdapterResult<AdapterSession> {
        let (event_tx, event_rx) = mpsc::channel(256);
        let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<AdapterControl>(64);

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 32,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AdapterError::Spawn(e.to_string()))?;

        let mut cmd = CommandBuilder::new(&spec.executable);
        for arg in &spec.args {
            cmd.arg(arg);
        }
        cmd.cwd(&spec.cwd);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| AdapterError::Spawn(e.to_string()))?;
        let pid = child.process_id();
        let started_at = chrono::Utc::now();

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| AdapterError::Spawn(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| AdapterError::Spawn(e.to_string()))?;
        let writer = Arc::new(Mutex::new(writer));
        let child = Arc::new(Mutex::new(child));

        // Read loop — blocking reads on the PTY master run on a std
        // thread; chunks go to the runtime via the event channel.
        {
            let event_tx = event_tx.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 16 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => {
                            let _ = event_tx.blocking_send(AdapterEvent::Exited {
                                code: None,
                                reason: Some("pty closed".into()),
                            });
                            break;
                        }
                        Ok(n) => {
                            let text = String::from_utf8_lossy(&buf[..n]).to_string();
                            if event_tx
                                .blocking_send(AdapterEvent::Output {
                                    stream: Stream::Stdout,
                                    text,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            });
        }

        // Control loop — serialized owner operations.
        {
            let writer = writer.clone();
            tokio::spawn(async move {
                while let Some(cmd) = ctrl_rx.recv().await {
                    match cmd {
                        AdapterControl::Send { text } | AdapterControl::Steer { text } => {
                            let mut w = writer.lock();
                            let mut input = text;
                            if !input.ends_with('\n') {
                                input.push('\n');
                            }
                            if w.write_all(input.as_bytes()).is_err() {
                                break;
                            }
                            let _ = w.flush();
                        }
                        AdapterControl::Answer { .. } => {
                            // PTY has no structured answer channel; the
                            // runtime should never offer one.
                        }
                        AdapterControl::Cancel | AdapterControl::Shutdown => {
                            // NB: don't lock `child` — the wait thread holds
                            // it for the process's whole life. Kill by pid.
                            kill_pid(pid);
                        }
                    }
                }
            });
        }

        // Exit watcher — reports the real exit status.
        {
            let event_tx = event_tx.clone();
            std::thread::spawn(move || {
                let status = child.lock().wait().ok();
                let _ = event_tx.blocking_send(AdapterEvent::Exited {
                    code: status.map(|s| s.exit_code() as i32),
                    reason: None,
                });
            });
        }

        // Deliver the initial prompt the only way a terminal allows —
        // as an input line. Structured delivery is what protocols are
        // for; the PTY is the honest fallback.
        if !spec.prompt.is_empty() {
            let mut w = writer.lock();
            let mut input = spec.prompt.clone();
            input.push('\n');
            let _ = w.write_all(input.as_bytes());
            let _ = w.flush();
        }

        let _ = event_tx
            .send(AdapterEvent::Started { native_id: None })
            .await;

        Ok(AdapterSession {
            pid,
            process_started_at: Some(started_at),
            native_id: None,
            capabilities: Capabilities::pty(),
            events: event_rx,
            control: ctrl_tx,
        })
    }
}
