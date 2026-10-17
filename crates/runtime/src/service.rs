//! The command service — the single entry point every surface calls.
//! Chat, buttons, the CLI, and MCP all produce `Command`s; this is where
//! they're validated, policy-checked, recorded, and dispatched.

use crate::bus::EventBus;
use crate::controller::{spawn_controller, ControllerCmd};
use crate::policy::{Decision, PolicyEngine};
use orc_adapters::traits::{AdapterAnswer, SpawnSpec};
use orc_adapters::{AdapterRegistry, Discoverer, DiscoveryPaths};
use orc_domain::*;
use orc_storage::{Snapshot, Store, TranscriptStore};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{info, instrument, warn};

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Storage(#[from] orc_storage::StorageError),
    #[error(transparent)]
    Adapter(#[from] orc_adapters::AdapterError),
}

type RResult<T> = Result<T, RuntimeError>;

#[derive(Clone)]
pub struct Runtime {
    store: Store,
    transcripts: TranscriptStore,
    bus: EventBus,
    adapters: AdapterRegistry,
    controllers: Arc<Mutex<HashMap<SessionId, mpsc::Sender<ControllerCmd>>>>,
    exit_tx: mpsc::UnboundedSender<SessionId>,
    exit_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<SessionId>>>,
    discoverer: Discoverer,
}

impl Runtime {
    pub fn new(
        store: Store,
        transcripts: TranscriptStore,
        adapters: AdapterRegistry,
        extra_paths: DiscoveryPaths,
    ) -> Self {
        let bus = EventBus::new();
        let (exit_tx, exit_rx) = mpsc::unbounded_channel();
        Self {
            store,
            transcripts,
            bus,
            adapters,
            controllers: Arc::new(Mutex::new(HashMap::new())),
            exit_tx,
            exit_rx: Arc::new(tokio::sync::Mutex::new(exit_rx)),
            discoverer: Discoverer::new(extra_paths),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn transcripts(&self) -> &TranscriptStore {
        &self.transcripts
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn adapter_registry(&self) -> &AdapterRegistry {
        &self.adapters
    }

    /// Drain exited controllers. Called by the daemon's housekeeping task.
    pub async fn reap_controllers(&self) {
        let mut rx = self.exit_rx.lock().await;
        while let Ok(id) = rx.try_recv() {
            self.controllers.lock().remove(&id);
        }
    }

    /// Live controllers for shutdown.
    pub async fn shutdown_all(&self) {
        let senders: Vec<mpsc::Sender<ControllerCmd>> =
            self.controllers.lock().values().cloned().collect();
        for tx in senders {
            let _ = tx.send(ControllerCmd::Shutdown).await;
        }
    }

    // ------------------------------------------------------------------
    // Queries
    // ------------------------------------------------------------------

    pub async fn snapshot(&self) -> RResult<Snapshot> {
        Ok(self.store.snapshot().await?)
    }

    pub async fn discover_tools(&self) -> RResult<Vec<DiscoveredToolView>> {
        let found = self.discoverer.discover().await;
        let path_notes = self.discoverer.diagnose_path_gap().await;
        let mut out = Vec::with_capacity(found.len());
        for d in found {
            // Persist what we learned — dedupe by executable path.
            let existing = self
                .store
                .find_tool_by_executable(&d.executable)
                .await?
                .map(|t| t.id);
            let tool = ToolInstance {
                id: existing.unwrap_or_default(),
                provider: d.provider,
                label: match &d.version {
                    Some(v) => format!("{} {v}", d.provider.label()),
                    None => d.provider.label().to_string(),
                },
                executable: d.executable.clone(),
                version: d.version.clone(),
                readiness: d.readiness,
                settings: d.settings.clone(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            self.store.upsert_tool(&tool).await?;
            self.emit(NewEvent::new(
                EventSource::Runtime,
                EventPayload::ToolChanged {
                    tool_instance_id: tool.id.clone(),
                },
            ))
            .await;
            out.push(DiscoveredToolView {
                tool,
                readiness_note: d.readiness_note,
            });
        }
        for note in path_notes {
            info!(note, "PATH gap");
        }
        Ok(out)
    }

    // ------------------------------------------------------------------
    // Command execution
    // ------------------------------------------------------------------

    /// Validate → record intent → dispatch → resolve. The one path for
    /// every operation in the product.
    #[instrument(skip(self, cmd), fields(op = %cmd.id, actor = ?cmd.actor))]
    pub async fn execute(&self, cmd: Command) -> RResult<CommandOutcome> {
        // Idempotency: seen-before keys replay the stored outcome, never
        // re-dispatch.
        if self.store.intent_seen(&cmd.idempotency_key).await? {
            warn!(key = %cmd.idempotency_key, "duplicate idempotency key");
            return Ok(CommandOutcome::Rejected {
                reason: "duplicate idempotency key".into(),
            });
        }
        self.store.record_intent(&cmd).await?;
        self.emit(NewEvent::new(
            EventSource::Runtime,
            EventPayload::CommandIntent {
                operation_id: cmd.id.clone(),
                idempotency_key: cmd.idempotency_key.clone(),
                actor: cmd.actor,
                kind: cmd.kind.clone(),
            },
        ))
        .await;

        let outcome = match self.dispatch(&cmd).await {
            Ok(outcome) => outcome,
            Err(e) => CommandOutcome::Rejected {
                reason: e.to_string(),
            },
        };

        self.store
            .resolve_intent(&cmd.idempotency_key, &outcome)
            .await?;
        self.emit(NewEvent::new(
            EventSource::Runtime,
            EventPayload::CommandResolved {
                operation_id: cmd.id,
                idempotency_key: cmd.idempotency_key,
                outcome: outcome.clone(),
            },
        ))
        .await;
        Ok(outcome)
    }

    async fn dispatch(&self, cmd: &Command) -> RResult<CommandOutcome> {
        match &cmd.kind {
            CommandKind::SessionStart {
                tool_instance_id,
                workspace_id,
                task_id,
                title,
                prompt,
                launch,
            } => {
                self.session_start(
                    cmd.actor,
                    tool_instance_id,
                    workspace_id,
                    task_id.as_ref(),
                    title,
                    prompt,
                    launch,
                    None,
                )
                .await
            }
            CommandKind::SessionImport {
                tool_instance_id,
                workspace_id,
                native_id,
                title,
            } => {
                self.session_import(cmd.actor, tool_instance_id, workspace_id, native_id, title)
                    .await
            }
            CommandKind::SessionSend { session_id, text } => {
                self.session_send(session_id, text).await
            }
            CommandKind::SessionSteer { session_id, text } => {
                self.session_steer(session_id, text).await
            }
            CommandKind::SessionCancel { session_id } => self.session_cancel(session_id).await,
            CommandKind::SessionResume { session_id } => {
                self.session_resume(cmd.actor, session_id).await
            }
            CommandKind::SessionFork { session_id } => {
                self.session_fork(cmd.actor, session_id).await
            }
            CommandKind::SessionDetach { session_id } => self.session_detach(session_id).await,
            CommandKind::AttentionAnswer {
                attention_id,
                answer,
            } => self.attention_answer(cmd.actor, attention_id, answer).await,
            CommandKind::AttentionDismiss { attention_id } => {
                self.attention_dismiss(attention_id).await
            }
            CommandKind::TaskCreate {
                workspace_id,
                title,
                description,
                steps,
            } => {
                self.task_create(workspace_id, title, description, steps)
                    .await
            }
            CommandKind::TaskUpdate { task_id, state } => self.task_update(task_id, *state).await,
            CommandKind::WorkspaceCreate { name, root } => self.workspace_create(name, root).await,
            CommandKind::WorkspaceAddAllowRule {
                workspace_id,
                action_kind,
                pattern,
            } => {
                self.workspace_add_rule(workspace_id, *action_kind, pattern)
                    .await
            }
            CommandKind::ArtifactRegister {
                session_id,
                run_id,
                kind,
                label,
                path,
            } => {
                self.artifact_register(session_id.as_ref(), run_id.as_ref(), *kind, label, path)
                    .await
            }
            CommandKind::ObserveScan => self.observe_scan().await,
            CommandKind::TranscriptAppend { .. } => {
                Err(CommandError::Internal("TranscriptAppend is internal".into()).into())
            }
        }
    }

    // ------------------------------------------------------------------
    // sessions
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    async fn session_start(
        &self,
        actor: Actor,
        tool_instance_id: &ToolInstanceId,
        workspace_id: &WorkspaceId,
        task_id: Option<&TaskId>,
        title: &str,
        prompt: &str,
        launch: &serde_json::Value,
        resume_session: Option<SessionId>,
    ) -> RResult<CommandOutcome> {
        let tool = self.store.get_tool(tool_instance_id).await?;
        let workspace = self.store.get_workspace(workspace_id).await?;
        if let Some(tid) = task_id {
            self.store.get_task(tid).await?;
        }

        match PolicyEngine::check_session_start(&workspace, &tool, actor) {
            Decision::Allow => {}
            Decision::Deny(reason) => {
                return Ok(CommandOutcome::Denied { reason });
            }
            Decision::RequireApproval => {
                return Ok(CommandOutcome::Denied {
                    reason: "session start requires human approval (agent-originated)".into(),
                });
            }
        }

        let adapter = self
            .adapters
            .get(tool.provider)
            .ok_or_else(|| CommandError::Provider(format!("no adapter for {}", tool.label)))?;

        let session_id = resume_session.unwrap_or_default();
        let mut env = child_env(&workspace);
        // Tool settings may inject env via `env.NAME` keys.
        for (k, v) in &tool.settings {
            if let Some(name) = k.strip_prefix("env.") {
                env.push((name.to_string(), v.clone()));
            }
        }
        let spec = SpawnSpec {
            session_id: session_id.clone(),
            executable: tool.executable.clone(),
            args: parse_args_setting(&tool),
            cwd: workspace.root.clone(),
            env,
            prompt: prompt.to_string(),
            launch: launch.clone(),
        };

        let adapter_session = adapter.spawn(spec).await?;
        let capabilities = adapter_session.capabilities.clone();
        let pid = adapter_session.pid;
        let proc_started = adapter_session.process_started_at;

        let now = chrono::Utc::now();
        let session = Session {
            id: session_id.clone(),
            workspace_id: workspace_id.clone(),
            task_id: task_id.cloned(),
            tool_instance_id: tool_instance_id.clone(),
            provider: tool.provider,
            ownership: Ownership::Managed,
            native_id: adapter_session.native_id.clone(),
            title: title.to_string(),
            state: SessionState::Starting,
            status_detail: Some(StatusDetail::Fact {
                message: "starting".into(),
            }),
            capabilities,
            pid,
            process_started_at: proc_started,
            last_meaningful_update: None,
            usage: None,
            last_event_seq: 0,
            created_at: now,
            updated_at: now,
            activity_started_at: Some(now),
        };
        self.store.upsert_session(&session).await?;
        self.emit(
            NewEvent::new(
                EventSource::Runtime,
                EventPayload::SessionCreated {
                    session_id: session_id.clone(),
                    ownership: Ownership::Managed,
                    title: title.to_string(),
                },
            )
            .for_session(session_id.clone()),
        )
        .await;

        let ctrl = spawn_controller(
            session,
            adapter_session,
            workspace,
            self.store.clone(),
            self.transcripts.clone(),
            self.bus.clone(),
            self.exit_tx.clone(),
        );
        self.controllers.lock().insert(session_id.clone(), ctrl);

        Ok(CommandOutcome::Applied {
            result: serde_json::json!({ "session_id": session_id }),
        })
    }

    async fn session_import(
        &self,
        actor: Actor,
        tool_instance_id: &ToolInstanceId,
        workspace_id: &WorkspaceId,
        native_id: &str,
        title: &str,
    ) -> RResult<CommandOutcome> {
        let tool = self.store.get_tool(tool_instance_id).await?;
        let workspace = self.store.get_workspace(workspace_id).await?;
        match PolicyEngine::check_session_start(&workspace, &tool, actor) {
            Decision::Allow => {}
            Decision::Deny(reason) => return Ok(CommandOutcome::Denied { reason }),
            Decision::RequireApproval => {
                return Ok(CommandOutcome::Denied {
                    reason: "import requires human approval (agent-originated)".into(),
                });
            }
        }
        let adapter = self
            .adapters
            .get(tool.provider)
            .ok_or_else(|| CommandError::Provider(format!("no adapter for {}", tool.label)))?;

        let spec = orc_adapters::traits::ImportSpec {
            session_id: SessionId::new(),
            executable: tool.executable.clone(),
            cwd: workspace.root.clone(),
            native_id: native_id.to_string(),
            launch: serde_json::json!({}),
        };
        let adapter_session = adapter.import(spec).await?;
        let capabilities = adapter_session.capabilities.clone();
        let pid = adapter_session.pid;
        let proc_started = adapter_session.process_started_at;

        let session_id = SessionId::new();
        let now = chrono::Utc::now();
        let session = Session {
            id: session_id.clone(),
            workspace_id: workspace_id.clone(),
            task_id: None,
            tool_instance_id: tool_instance_id.clone(),
            provider: tool.provider,
            ownership: Ownership::Imported,
            native_id: Some(native_id.to_string()),
            title: title.to_string(),
            state: SessionState::Starting,
            status_detail: Some(StatusDetail::Fact {
                message: "importing".into(),
            }),
            capabilities,
            pid,
            process_started_at: proc_started,
            last_meaningful_update: None,
            usage: None,
            last_event_seq: 0,
            created_at: now,
            updated_at: now,
            activity_started_at: Some(now),
        };
        self.store.upsert_session(&session).await?;
        self.emit(
            NewEvent::new(
                EventSource::Runtime,
                EventPayload::SessionCreated {
                    session_id: session_id.clone(),
                    ownership: Ownership::Imported,
                    title: title.to_string(),
                },
            )
            .for_session(session_id.clone()),
        )
        .await;
        let ctrl = spawn_controller(
            session,
            adapter_session,
            workspace,
            self.store.clone(),
            self.transcripts.clone(),
            self.bus.clone(),
            self.exit_tx.clone(),
        );
        self.controllers.lock().insert(session_id.clone(), ctrl);
        Ok(CommandOutcome::Applied {
            result: serde_json::json!({ "session_id": session_id }),
        })
    }

    async fn session_send(&self, session_id: &SessionId, text: &str) -> RResult<CommandOutcome> {
        let session = self.store.get_session(session_id).await?;
        if !session.state.is_active() && session.state != SessionState::Idle {
            return Ok(CommandOutcome::Rejected {
                reason: format!("session is {}", session.state.label()),
            });
        }
        if session.ownership == Ownership::Observed {
            return Ok(CommandOutcome::Denied {
                reason: "observed processes are not controllable — import it first".into(),
            });
        }
        let ctrl = self.controller_for(session_id)?;
        let run_id = RunId::new();
        self.emit(
            NewEvent::new(
                EventSource::Runtime,
                EventPayload::RunStarted {
                    run_id: run_id.clone(),
                    session_id: session_id.clone(),
                    instruction: text.to_string(),
                    attempt: 1,
                },
            )
            .for_session(session_id.clone()),
        )
        .await;
        ctrl.send(ControllerCmd::Send {
            run_id: run_id.clone(),
            text: text.to_string(),
        })
        .await
        .map_err(|_| CommandError::Provider("session controller gone".into()))?;
        Ok(CommandOutcome::Applied {
            result: serde_json::json!({ "run_id": run_id }),
        })
    }

    async fn session_steer(&self, session_id: &SessionId, text: &str) -> RResult<CommandOutcome> {
        let session = self.store.get_session(session_id).await?;
        if let Decision::Deny(reason) = PolicyEngine::check_capability(
            &session,
            "mid_turn_steer",
            &session.capabilities.mid_turn_steer,
        ) {
            return Ok(CommandOutcome::Denied { reason });
        }
        let ctrl = self.controller_for(session_id)?;
        ctrl.send(ControllerCmd::Steer {
            text: text.to_string(),
        })
        .await
        .map_err(|_| CommandError::Provider("session controller gone".into()))?;
        Ok(CommandOutcome::Applied {
            result: serde_json::json!({}),
        })
    }

    async fn session_cancel(&self, session_id: &SessionId) -> RResult<CommandOutcome> {
        let session = self.store.get_session(session_id).await?;
        if let Decision::Deny(reason) =
            PolicyEngine::check_capability(&session, "cancel", &session.capabilities.cancel)
        {
            return Ok(CommandOutcome::Denied { reason });
        }
        let ctrl = self.controller_for(session_id)?;
        ctrl.send(ControllerCmd::Cancel)
            .await
            .map_err(|_| CommandError::Provider("session controller gone".into()))?;
        Ok(CommandOutcome::Applied {
            result: serde_json::json!({}),
        })
    }

    async fn session_resume(
        &self,
        actor: Actor,
        session_id: &SessionId,
    ) -> RResult<CommandOutcome> {
        let session = self.store.get_session(session_id).await?;
        if let Decision::Deny(reason) =
            PolicyEngine::check_capability(&session, "resume", &session.capabilities.resume)
        {
            return Ok(CommandOutcome::Denied { reason });
        }
        // Re-launch through the adapter against the stored native id.
        let native = session.native_id.clone().ok_or_else(|| {
            CommandError::InvalidInput("session has no provider-native id to resume".into())
        })?;
        let tool = self.store.get_tool(&session.tool_instance_id).await?;
        let mut launch = serde_json::json!({});
        if let Some(o) = launch.as_object_mut() {
            o.insert("resume".into(), serde_json::Value::String(native));
        }
        // Route through session_start semantics on a fresh session id —
        // the resumed conversation continues under a new Orc-managed
        // handle but keeps native lineage via the launch arg.
        self.session_start(
            actor,
            &session.tool_instance_id,
            &session.workspace_id,
            session.task_id.as_ref(),
            &format!("{} (resumed)", session.title),
            "",
            &launch,
            Some(session_id.clone()),
        )
        .await
        .inspect_err(|_| {
            let _ = &tool;
        })
    }

    async fn session_fork(&self, actor: Actor, session_id: &SessionId) -> RResult<CommandOutcome> {
        let session = self.store.get_session(session_id).await?;
        if let Decision::Deny(reason) =
            PolicyEngine::check_capability(&session, "fork", &session.capabilities.fork)
        {
            return Ok(CommandOutcome::Denied { reason });
        }
        let native = session.native_id.clone().ok_or_else(|| {
            CommandError::InvalidInput("session has no provider-native id to fork".into())
        })?;
        let launch = serde_json::json!({ "fork": true, "resume": native });
        self.session_start(
            actor,
            &session.tool_instance_id,
            &session.workspace_id,
            session.task_id.as_ref(),
            &format!("{} (fork)", session.title),
            "",
            &launch,
            None,
        )
        .await
    }

    async fn session_detach(&self, session_id: &SessionId) -> RResult<CommandOutcome> {
        let ctrl = self.controllers.lock().remove(session_id);
        if let Some(ctrl) = ctrl {
            let _ = ctrl.send(ControllerCmd::Shutdown).await;
        }
        let mut session = self.store.get_session(session_id).await?;
        session.state = SessionState::Completed;
        session.status_detail = Some(StatusDetail::Fact {
            message: "detached by user".into(),
        });
        self.store.upsert_session(&session).await?;
        self.emit(
            NewEvent::new(
                EventSource::Runtime,
                EventPayload::SessionStateChanged {
                    session_id: session_id.clone(),
                    from: session.state,
                    to: SessionState::Completed,
                    reason: "detached by user".into(),
                    inferred: false,
                },
            )
            .for_session(session_id.clone()),
        )
        .await;
        Ok(CommandOutcome::Applied {
            result: serde_json::json!({}),
        })
    }

    // ------------------------------------------------------------------
    // attention
    // ------------------------------------------------------------------

    async fn attention_answer(
        &self,
        actor: Actor,
        attention_id: &AttentionId,
        answer: &Answer,
    ) -> RResult<CommandOutcome> {
        let item = self.store.get_attention(attention_id).await?;
        match PolicyEngine::check_attention_answer(&item, actor) {
            Decision::Allow => {}
            Decision::Deny(reason) => return Ok(CommandOutcome::Denied { reason }),
            Decision::RequireApproval => {}
        }

        // Answer must fit the item's kind.
        let fits = matches!(
            (&item.kind, answer),
            (
                AttentionKind::Question,
                Answer::Text { .. } | Answer::Choice { .. }
            ) | (
                AttentionKind::InferredPrompt,
                Answer::Text { .. } | Answer::Choice { .. }
            ) | (
                AttentionKind::Approval | AttentionKind::UncertainOutcome,
                Answer::Approve { .. } | Answer::Deny { .. },
            ) | (
                AttentionKind::AuthFailed | AttentionKind::ReviewRequest,
                Answer::Text { .. }
            )
        );
        if !fits {
            return Ok(CommandOutcome::Rejected {
                reason: format!("answer type does not fit a {} item", item.kind.label()),
            });
        }

        // Approval binding: the stored PendingAction's hash is the truth;
        // if it was tampered with between raise and answer, refuse.
        if let (AttentionKind::Approval, Answer::Approve { .. }) = (&item.kind, answer) {
            if let Some(pa) = &item.pending_action {
                let expected = PendingAction::compute_hash(
                    pa.kind,
                    &pa.args,
                    &item.workspace_id,
                    item.run_id.as_ref(),
                    pa.provider_ref.as_deref(),
                );
                if expected != pa.args_hash {
                    return Ok(CommandOutcome::Denied {
                        reason: "pending action hash mismatch — refusing to approve".into(),
                    });
                }
            }
        }

        // Route the answer to the provider when the session is live.
        if let Some(session_id) = &item.session_id {
            let ctrl = self.controllers.lock().get(session_id).cloned();
            if let Some(ctrl) = ctrl {
                let adapter_answer = match answer {
                    Answer::Text { text } => AdapterAnswer::Text(text.clone()),
                    Answer::Choice { index } => {
                        let option = item
                            .choices
                            .get(*index as usize)
                            .cloned()
                            .unwrap_or_default();
                        AdapterAnswer::Text(option)
                    }
                    Answer::Approve { .. } => AdapterAnswer::Approve { option_id: None },
                    Answer::Deny { reason } => AdapterAnswer::Deny {
                        reason: reason.clone(),
                    },
                };
                if let Some(req) = &item.provider_request_ref {
                    let _ = ctrl
                        .send(ControllerCmd::Answer {
                            request_ref: req.clone(),
                            answer: adapter_answer,
                        })
                        .await;
                }
            }
        }

        // "Allow for this workspace" installs an explicit allow rule.
        if let (AttentionKind::Approval, Answer::Approve { scope }) = (&item.kind, answer) {
            if *scope == ApprovalScope::WorkspaceRule {
                if let Some(pa) = &item.pending_action {
                    let _ = self
                        .workspace_add_rule(&item.workspace_id, pa.kind, &pa.display)
                        .await;
                }
            }
        }

        self.emit(NewEvent::new(
            EventSource::Interface,
            EventPayload::AttentionStatusChanged {
                attention_id: attention_id.clone(),
                from: AttentionStatus::Open,
                to: AttentionStatus::Answered,
                resolution: Some(answer.clone()),
            },
        ))
        .await;
        Ok(CommandOutcome::Applied {
            result: serde_json::json!({}),
        })
    }

    async fn attention_dismiss(&self, attention_id: &AttentionId) -> RResult<CommandOutcome> {
        let item = self.store.get_attention(attention_id).await?;
        if item.status != AttentionStatus::Open {
            return Ok(CommandOutcome::Denied {
                reason: "attention item is not open".into(),
            });
        }
        // Approvals may only be dismissed by answering them — never
        // silently discarded.
        if item.kind == AttentionKind::Approval {
            return Ok(CommandOutcome::Denied {
                reason: "approvals must be approved or denied, not dismissed".into(),
            });
        }
        self.emit(NewEvent::new(
            EventSource::Interface,
            EventPayload::AttentionStatusChanged {
                attention_id: attention_id.clone(),
                from: AttentionStatus::Open,
                to: AttentionStatus::Superseded,
                resolution: None,
            },
        ))
        .await;
        Ok(CommandOutcome::Applied {
            result: serde_json::json!({}),
        })
    }

    // ------------------------------------------------------------------
    // tasks / workspaces / artifacts
    // ------------------------------------------------------------------

    async fn task_create(
        &self,
        workspace_id: &WorkspaceId,
        title: &str,
        description: &Option<String>,
        steps: &[WorkflowStep],
    ) -> RResult<CommandOutcome> {
        self.store.get_workspace(workspace_id).await?;
        let now = chrono::Utc::now();
        let task = Task {
            id: TaskId::new(),
            workspace_id: workspace_id.clone(),
            title: title.to_string(),
            description: description.clone(),
            state: TaskState::Open,
            steps: steps.to_vec(),
            created_at: now,
            updated_at: now,
        };
        self.store.insert_task(&task).await?;
        self.emit(NewEvent::new(
            EventSource::Runtime,
            EventPayload::TaskChanged {
                task_id: task.id.clone(),
            },
        ))
        .await;
        Ok(CommandOutcome::Applied {
            result: serde_json::json!({ "task_id": task.id }),
        })
    }

    async fn task_update(&self, task_id: &TaskId, state: TaskState) -> RResult<CommandOutcome> {
        let mut task = self.store.get_task(task_id).await?;
        task.state = state;
        task.updated_at = chrono::Utc::now();
        self.store.insert_task(&task).await?;
        self.emit(NewEvent::new(
            EventSource::Runtime,
            EventPayload::TaskChanged {
                task_id: task.id.clone(),
            },
        ))
        .await;
        Ok(CommandOutcome::Applied {
            result: serde_json::json!({}),
        })
    }

    async fn workspace_create(&self, name: &str, root: &str) -> RResult<CommandOutcome> {
        let ws = Workspace {
            id: WorkspaceId::new(),
            name: name.to_string(),
            root: root.to_string(),
            allow_rules: vec![],
            network_allowed: true,
            remote_content_allowed: true,
            created_at: chrono::Utc::now(),
        };
        self.store.insert_workspace(&ws).await?;
        self.emit(NewEvent::new(
            EventSource::Runtime,
            EventPayload::WorkspaceChanged {
                workspace_id: ws.id.clone(),
            },
        ))
        .await;
        Ok(CommandOutcome::Applied {
            result: serde_json::json!({ "workspace_id": ws.id }),
        })
    }

    async fn workspace_add_rule(
        &self,
        workspace_id: &WorkspaceId,
        action_kind: ActionKind,
        pattern: &str,
    ) -> RResult<CommandOutcome> {
        let mut ws = self.store.get_workspace(workspace_id).await?;
        ws.allow_rules.push(AllowRule {
            action_kind,
            pattern: pattern.to_string(),
            created_at: chrono::Utc::now(),
        });
        self.store.update_workspace_rules(&ws).await?;
        self.emit(NewEvent::new(
            EventSource::Runtime,
            EventPayload::WorkspaceChanged {
                workspace_id: ws.id.clone(),
            },
        ))
        .await;
        Ok(CommandOutcome::Applied {
            result: serde_json::json!({}),
        })
    }

    async fn artifact_register(
        &self,
        session_id: Option<&SessionId>,
        run_id: Option<&RunId>,
        kind: ArtifactKind,
        label: &str,
        path: &str,
    ) -> RResult<CommandOutcome> {
        let artifact_id = ArtifactId::new();
        self.emit(NewEvent::new(
            EventSource::Runtime,
            EventPayload::ArtifactRegistered {
                artifact_id: artifact_id.clone(),
                session_id: session_id.cloned(),
                run_id: run_id.cloned(),
                label: label.to_string(),
                path: path.to_string(),
            },
        ))
        .await;
        let _ = kind;
        Ok(CommandOutcome::Applied {
            result: serde_json::json!({ "artifact_id": artifact_id }),
        })
    }

    async fn observe_scan(&self) -> RResult<CommandOutcome> {
        let observer = crate::observe::Observer::new(self.store.clone(), self.bus.clone());
        let n = observer.scan_once().await;
        Ok(CommandOutcome::Applied {
            result: serde_json::json!({ "observed": n }),
        })
    }

    // ------------------------------------------------------------------
    // internals
    // ------------------------------------------------------------------

    fn controller_for(
        &self,
        session_id: &SessionId,
    ) -> Result<mpsc::Sender<ControllerCmd>, RuntimeError> {
        self.controllers
            .lock()
            .get(session_id)
            .cloned()
            .ok_or_else(|| {
                CommandError::NotFound(format!("no live controller for {session_id}")).into()
            })
    }

    async fn emit(&self, ev: NewEvent) {
        match self.store.append(ev).await {
            Ok(persisted) => self.bus.publish(persisted),
            Err(e) => warn!(error = %e, "event append failed"),
        }
    }
}

/// A discovered tool merged with its persisted instance.
#[derive(Debug, Clone)]
pub struct DiscoveredToolView {
    pub tool: ToolInstance,
    pub readiness_note: Option<String>,
}

/// Sanitized child environment — agents get what they need, not the
/// daemon's secrets.
fn child_env(workspace: &Workspace) -> Vec<(String, String)> {
    let mut env = Vec::new();
    for key in [
        "PATH",
        "HOME",
        "LANG",
        "LC_ALL",
        "TERM",
        "USER",
        "SHELL",
        "TMPDIR",
        "XDG_RUNTIME_DIR",
        "SSH_AUTH_SOCK",
    ] {
        if let Ok(v) = std::env::var(key) {
            env.push((key.to_string(), v));
        }
    }
    env.push(("ORC_MANAGED".to_string(), "1".to_string()));
    if !workspace.network_allowed {
        env.push(("ORC_NO_NETWORK".to_string(), "1".to_string()));
    }
    env
}

fn parse_args_setting(tool: &ToolInstance) -> Vec<String> {
    tool.settings
        .get("args")
        .and_then(|s| shlex::split(s))
        .unwrap_or_default()
}
