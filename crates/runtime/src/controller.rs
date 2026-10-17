//! One session controller per session: a Tokio task with a bounded
//! command channel that serializes send/cancel/answer/reconnect and
//! owns state transitions. Protocol reading stays independent of
//! approval waiting — the reader task lives in the adapter, so a pending
//! answer never stalls the stream.

use crate::bus::EventBus;
use crate::policy::{Decision, PolicyEngine};
use orc_adapters::traits::{AdapterAnswer, AdapterControl, AdapterEvent, AdapterSession};
use orc_domain::*;
use orc_storage::{Store, TranscriptStore};
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{debug, instrument, warn};

/// Commands the service sends to a live controller.
#[derive(Debug)]
pub enum ControllerCmd {
    /// Forward user input to the adapter. `run_id` binds the turn for
    /// run lifecycle events.
    Send {
        run_id: RunId,
        text: String,
    },
    Steer {
        text: String,
    },
    /// Forward an answered structured request.
    Answer {
        request_ref: String,
        answer: AdapterAnswer,
    },
    Cancel,
    Shutdown,
}

/// How often pending output is flushed into an event batch.
const FLUSH_EVERY: Duration = Duration::from_millis(150);
/// Force a flush past this buffered size.
const FLUSH_AT: usize = 32 * 1024;
/// No output for this long in a Running session → labeled inference.
const STALL_AFTER: Duration = Duration::from_secs(300);
/// Preview text length carried on OutputAppended events.
const PREVIEW_LEN: usize = 240;

/// Spawn the controller task; returns its command sender.
pub fn spawn_controller(
    session: Session,
    adapter: AdapterSession,
    workspace: Workspace,
    store: Store,
    transcripts: TranscriptStore,
    bus: EventBus,
    on_exit: mpsc::UnboundedSender<SessionId>,
) -> mpsc::Sender<ControllerCmd> {
    let (tx, rx) = mpsc::channel::<ControllerCmd>(64);
    let ctx = Controller {
        session,
        workspace,
        adapter,
        store,
        transcripts,
        bus,
        rx,
        on_exit,
        pending_events: Vec::new(),
        pending_bytes: 0,
        active_run: None,
        last_output_at: std::time::Instant::now(),
        stall_reported: false,
    };
    tokio::spawn(ctx.run());
    tx
}

struct Controller {
    session: Session,
    workspace: Workspace,
    adapter: AdapterSession,
    store: Store,
    transcripts: TranscriptStore,
    bus: EventBus,
    rx: mpsc::Receiver<ControllerCmd>,
    on_exit: mpsc::UnboundedSender<SessionId>,
    pending_events: Vec<NewEvent>,
    pending_bytes: usize,
    active_run: Option<RunId>,
    last_output_at: std::time::Instant,
    stall_reported: bool,
}

impl Controller {
    #[instrument(skip_all, fields(session = %self.session.id))]
    async fn run(mut self) {
        let mut flush = interval(FLUSH_EVERY);
        let mut stall = interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                cmd = self.rx.recv() => {
                    match cmd {
                        Some(cmd) => {
                            if self.handle_cmd(cmd).await {
                                break; // shutdown requested
                            }
                        }
                        None => break, // all senders gone
                    }
                }
                ev = self.adapter.events.recv() => {
                    match ev {
                        Some(ev) => {
                            if self.handle_event(ev).await {
                                break; // exited
                            }
                        }
                        None => {
                            // Adapter channel closed — treat as exit.
                            self.emit_state(SessionState::Completed, "adapter closed", false).await;
                            break;
                        }
                    }
                }
                _ = flush.tick() => {
                    self.flush_events().await;
                }
                _ = stall.tick() => {
                    self.check_stall().await;
                }
            }
        }
        self.flush_events().await;
        let _ = self.on_exit.send(self.session.id.clone());
        debug!("controller stopped");
    }

    // ------------------------------------------------------------------
    // commands from the service
    // ------------------------------------------------------------------

    /// Returns true when the controller should stop.
    async fn handle_cmd(&mut self, cmd: ControllerCmd) -> bool {
        match cmd {
            ControllerCmd::Send { run_id, text } => {
                self.active_run = Some(run_id);
                self.emit_state(SessionState::Running, "turn dispatched", false)
                    .await;
                if self
                    .adapter
                    .control
                    .send(AdapterControl::Send { text })
                    .await
                    .is_err()
                {
                    self.emit_adapter_error("send failed: adapter channel closed")
                        .await;
                }
                false
            }
            ControllerCmd::Steer { text } => {
                let _ = self
                    .adapter
                    .control
                    .send(AdapterControl::Steer { text })
                    .await;
                false
            }
            ControllerCmd::Answer {
                request_ref,
                answer,
            } => {
                let _ = self
                    .adapter
                    .control
                    .send(AdapterControl::Answer {
                        request_ref,
                        answer,
                    })
                    .await;
                false
            }
            ControllerCmd::Cancel => {
                let _ = self.adapter.control.send(AdapterControl::Cancel).await;
                if let Some(run) = self.active_run.take() {
                    self.emit(NewEvent::new(
                        EventSource::Runtime,
                        EventPayload::RunStateChanged {
                            run_id: run,
                            session_id: self.session.id.clone(),
                            from: RunState::Running,
                            to: RunState::Cancelled,
                            reason: "cancelled by user".into(),
                        },
                    ))
                    .await;
                }
                self.emit_state(SessionState::Idle, "cancelled", false)
                    .await;
                false
            }
            ControllerCmd::Shutdown => {
                let _ = self.adapter.control.send(AdapterControl::Shutdown).await;
                true
            }
        }
    }

    // ------------------------------------------------------------------
    // events from the adapter
    // ------------------------------------------------------------------

    /// Returns true when the controller should stop.
    async fn handle_event(&mut self, ev: AdapterEvent) -> bool {
        match ev {
            AdapterEvent::Started { native_id } => {
                if let Some(native) = native_id {
                    self.emit(NewEvent::new(
                        EventSource::Adapter,
                        EventPayload::SessionNativeIdAssigned {
                            session_id: self.session.id.clone(),
                            native_id: native,
                        },
                    ))
                    .await;
                }
                if self.session.state == SessionState::Starting {
                    self.emit_state(SessionState::Idle, "adapter ready", false)
                        .await;
                }
            }
            AdapterEvent::Output { stream, text } => {
                self.last_output_at = std::time::Instant::now();
                self.stall_reported = false;
                let bytes = text.as_bytes();
                match self
                    .transcripts
                    .append(&self.session.id, stream, bytes)
                    .await
                {
                    Ok(span) => {
                        let preview = tail_preview(&text);
                        self.pending_events.push(
                            NewEvent::new(
                                EventSource::Adapter,
                                EventPayload::OutputAppended {
                                    session_id: self.session.id.clone(),
                                    run_id: self.active_run.clone(),
                                    stream,
                                    offset: span.offset,
                                    len: span.len,
                                    preview,
                                },
                            )
                            .for_session(self.session.id.clone()),
                        );
                        self.pending_bytes += span.len as usize;
                        if self.pending_bytes >= FLUSH_AT {
                            self.flush_events().await;
                        }
                    }
                    Err(e) => warn!(error = %e, "transcript append failed"),
                }
            }
            AdapterEvent::Meaningful { summary } => {
                self.emit(
                    NewEvent::new(
                        EventSource::Adapter,
                        EventPayload::SessionUpdate {
                            session_id: self.session.id.clone(),
                            summary,
                        },
                    )
                    .for_session(self.session.id.clone()),
                )
                .await;
            }
            AdapterEvent::ApprovalRequested {
                request_ref,
                action,
            } => {
                self.last_output_at = std::time::Instant::now();
                let args_hash = PendingAction::compute_hash(
                    action.kind,
                    &action.args,
                    &self.workspace.id,
                    self.active_run.as_ref(),
                    Some(&request_ref),
                );
                let pending = PendingAction {
                    kind: action.kind,
                    display: action.display.clone(),
                    args: action.args,
                    args_hash,
                    provider_ref: Some(request_ref.clone()),
                };
                match PolicyEngine::check_approval(&self.workspace, &pending) {
                    Decision::Allow => {
                        // Covered by an explicit allow rule — answer it
                        // without bothering the human.
                        let _ = self
                            .adapter
                            .control
                            .send(AdapterControl::Answer {
                                request_ref,
                                answer: AdapterAnswer::Approve { option_id: None },
                            })
                            .await;
                        self.emit(
                            NewEvent::new(
                                EventSource::Runtime,
                                EventPayload::SessionUpdate {
                                    session_id: self.session.id.clone(),
                                    summary: format!(
                                        "auto-approved by workspace rule: {}",
                                        pending.display
                                    ),
                                },
                            )
                            .for_session(self.session.id.clone()),
                        )
                        .await;
                    }
                    Decision::RequireApproval | Decision::Deny(_) => {
                        self.raise_attention(
                            AttentionKind::Approval,
                            format!("Approve {}?", pending.display),
                            pending.display.clone(),
                            Some(pending),
                            vec![],
                            Some(request_ref),
                        )
                        .await;
                        self.emit_state(
                            SessionState::WaitingForApproval,
                            "approval requested by provider",
                            false,
                        )
                        .await;
                    }
                }
            }
            AdapterEvent::Question {
                request_ref,
                prompt,
                choices,
            } => {
                self.raise_attention(
                    AttentionKind::Question,
                    truncate(&prompt, 120),
                    prompt,
                    None,
                    choices,
                    Some(request_ref),
                )
                .await;
                self.emit_state(
                    SessionState::WaitingForInput,
                    "question from provider",
                    false,
                )
                .await;
            }
            AdapterEvent::AuthFailed { message } => {
                self.raise_attention(
                    AttentionKind::AuthFailed,
                    "Authentication failed".to_string(),
                    message,
                    None,
                    vec![],
                    None,
                )
                .await;
            }
            AdapterEvent::TurnFinished {
                native_turn_id,
                reason,
                ok,
            } => {
                if let Some(run) = self.active_run.take() {
                    self.emit(
                        NewEvent::new(
                            EventSource::Adapter,
                            EventPayload::RunStateChanged {
                                run_id: run,
                                session_id: self.session.id.clone(),
                                from: RunState::Running,
                                to: if ok {
                                    RunState::Succeeded
                                } else {
                                    RunState::Failed
                                },
                                reason: reason.clone(),
                            },
                        )
                        .for_session(self.session.id.clone()),
                    )
                    .await;
                }
                let _ = native_turn_id;
                self.emit_state(
                    SessionState::Idle,
                    if ok { "turn finished" } else { "turn failed" },
                    false,
                )
                .await;
            }
            AdapterEvent::Exited { code, reason } => {
                let to = match code {
                    Some(0) | None => SessionState::Completed,
                    Some(_) => SessionState::Failed,
                };
                let why = reason.unwrap_or_else(|| match code {
                    Some(c) => format!("exited with code {c}"),
                    None => "process ended".into(),
                });
                if let Some(run) = self.active_run.take() {
                    self.emit(
                        NewEvent::new(
                            EventSource::Runtime,
                            EventPayload::RunStateChanged {
                                run_id: run,
                                session_id: self.session.id.clone(),
                                from: RunState::Running,
                                to: if code == Some(0) {
                                    RunState::Succeeded
                                } else {
                                    RunState::Failed
                                },
                                reason: why.clone(),
                            },
                        )
                        .for_session(self.session.id.clone()),
                    )
                    .await;
                }
                self.emit_state(to, &why, false).await;
                // Dead providers can't answer: expire their requests.
                let _ = self
                    .store
                    .expire_attention_for_session(&self.session.id)
                    .await;
                return true;
            }
            AdapterEvent::Usage { usage } => {
                self.emit(
                    NewEvent::new(
                        EventSource::Adapter,
                        EventPayload::SessionUsage {
                            session_id: self.session.id.clone(),
                            usage,
                        },
                    )
                    .for_session(self.session.id.clone()),
                )
                .await;
            }
            AdapterEvent::Artifact { label, path } => {
                self.emit(
                    NewEvent::new(
                        EventSource::Adapter,
                        EventPayload::ArtifactRegistered {
                            artifact_id: ArtifactId::new(),
                            session_id: Some(self.session.id.clone()),
                            run_id: self.active_run.clone(),
                            label,
                            path,
                        },
                    )
                    .for_session(self.session.id.clone()),
                )
                .await;
            }
            AdapterEvent::Error { message } => {
                self.emit_adapter_error(&message).await;
            }
        }
        false
    }

    // ------------------------------------------------------------------
    // helpers
    // ------------------------------------------------------------------

    async fn raise_attention(
        &mut self,
        kind: AttentionKind,
        title: String,
        body: String,
        pending_action: Option<PendingAction>,
        choices: Vec<String>,
        provider_request_ref: Option<String>,
    ) {
        self.emit(
            NewEvent::new(
                EventSource::Adapter,
                EventPayload::AttentionRaised {
                    attention_id: AttentionId::new(),
                    session_id: Some(self.session.id.clone()),
                    workspace_id: self.session.workspace_id.clone(),
                    run_id: self.active_run.clone(),
                    kind,
                    title,
                    body,
                    pending_action,
                    choices,
                    provider_request_ref,
                    expires_with_session: true,
                },
            )
            .for_session(self.session.id.clone()),
        )
        .await;
    }

    async fn emit_state(&mut self, to: SessionState, reason: &str, inferred: bool) {
        if self.session.state == to {
            return;
        }
        let from = self.session.state;
        self.session.state = to;
        self.emit(
            NewEvent::new(
                EventSource::Runtime,
                EventPayload::SessionStateChanged {
                    session_id: self.session.id.clone(),
                    from,
                    to,
                    reason: reason.to_string(),
                    inferred,
                },
            )
            .for_session(self.session.id.clone()),
        )
        .await;
    }

    async fn emit_adapter_error(&mut self, message: &str) {
        self.emit(
            NewEvent::new(
                EventSource::Adapter,
                EventPayload::SessionUpdate {
                    session_id: self.session.id.clone(),
                    summary: format!("adapter error: {message}"),
                },
            )
            .for_session(self.session.id.clone()),
        )
        .await;
    }

    async fn emit(&mut self, ev: NewEvent) {
        match self.store.append(ev).await {
            Ok(persisted) => self.bus.publish(persisted),
            Err(e) => warn!(error = %e, "event append failed"),
        }
    }

    async fn flush_events(&mut self) {
        if self.pending_events.is_empty() {
            return;
        }
        let batch = std::mem::take(&mut self.pending_events);
        self.pending_bytes = 0;
        match self.store.append_batch(batch).await {
            Ok(persisted) => {
                for ev in persisted {
                    self.bus.publish(ev);
                }
            }
            Err(e) => warn!(error = %e, "event batch append failed"),
        }
    }

    /// Stall detection — inference, never presented as fact.
    async fn check_stall(&mut self) {
        if self.session.state != SessionState::Running || self.stall_reported {
            return;
        }
        if self.last_output_at.elapsed() >= STALL_AFTER {
            self.stall_reported = true;
            self.emit_state(
                SessionState::Running,
                "no output for 5 minutes — may be stalled",
                true,
            )
            .await;
        }
    }
}

fn tail_preview(text: &str) -> String {
    let trimmed = text.trim_end();
    if trimmed.len() <= PREVIEW_LEN {
        trimmed.to_string()
    } else {
        trimmed[trimmed.len() - PREVIEW_LEN..].to_string()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
