//! The task scheduler: a small persisted dependency graph, not an agent
//! framework. Steps become ready when prerequisites finish; dispatch
//! happens through the same CommandService as everything else.

use crate::service::Runtime;
use orc_domain::*;
use serde_json::json;
use std::sync::Arc;
use tracing::{info, warn};

/// Max step sessions dispatched concurrently per scheduler tick.
const MAX_CONCURRENT_STEPS: usize = 4;

/// The pieces of a task a step dispatch needs — cloned so the borrow
/// checker doesn't see `task.steps` mutating while `&Task` is alive.
struct StepCtx {
    task_id: TaskId,
    task_title: String,
    task_description: Option<String>,
    workspace_id: WorkspaceId,
}

pub struct Scheduler {
    runtime: Runtime,
}

impl Scheduler {
    pub fn new(runtime: Runtime) -> Arc<Self> {
        Arc::new(Self { runtime })
    }

    /// Advance every task: mark ready steps, dispatch them, bound
    /// retries. Called after task/session state changes.
    pub async fn tick(&self) {
        let tasks = match self.runtime.store().list_tasks().await {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "scheduler task list failed");
                return;
            }
        };
        for task in tasks {
            if !matches!(task.state, TaskState::Open | TaskState::InProgress) {
                continue;
            }
            self.advance(task).await;
        }
    }

    async fn advance(&self, mut task: Task) {
        let mut changed = false;
        let mut running = 0usize;

        for step in &task.steps {
            if step.state == StepState::Running {
                running += 1;
            }
        }

        // Snapshot states so prereq checks don't fight the mutation.
        let states: std::collections::HashMap<StepId, StepState> =
            task.steps.iter().map(|s| (s.id.clone(), s.state)).collect();
        for step in &mut task.steps {
            if step.state == StepState::Pending {
                let ready = step
                    .prerequisites
                    .iter()
                    .all(|pre| states.get(pre) == Some(&StepState::Done));
                if ready {
                    step.state = StepState::Ready;
                    changed = true;
                }
            }
        }

        // Dispatch Ready steps up to the concurrency bound. Clone the
        // bits `dispatch_step` needs so the borrow of `task` is free.
        let ctx = StepCtx {
            task_id: task.id.clone(),
            task_title: task.title.clone(),
            task_description: task.description.clone(),
            workspace_id: task.workspace_id.clone(),
        };
        for i in 0..task.steps.len() {
            if running >= MAX_CONCURRENT_STEPS {
                break;
            }
            if task.steps[i].state != StepState::Ready {
                continue;
            }
            let step = task.steps[i].clone();
            match self.dispatch_step(&ctx, &step).await {
                Ok(session_id) => {
                    let step = &mut task.steps[i];
                    step.session_id = Some(session_id);
                    step.state = StepState::Running;
                    step.attempts += 1;
                    running += 1;
                    changed = true;
                    info!(task = %task.id, step = %step.id, "step dispatched");
                }
                Err(e) => {
                    warn!(task = %task.id, step = %step.id, error = %e, "step dispatch failed");
                    let step = &mut task.steps[i];
                    step.attempts += 1;
                    if step.attempts >= step.max_attempts {
                        step.state = StepState::Failed;
                    }
                    changed = true;
                }
            }
        }

        // Reconcile step outcomes against session/run states.
        let sessions = self
            .runtime
            .store()
            .list_sessions()
            .await
            .unwrap_or_default();
        for step in task.steps.iter_mut() {
            if step.state != StepState::Running {
                continue;
            }
            let Some(sid) = &step.session_id else {
                continue;
            };
            let Some(sess) = sessions.iter().find(|s| &s.id == sid) else {
                step.state = StepState::Failed;
                changed = true;
                continue;
            };
            match sess.state {
                SessionState::Completed | SessionState::Idle => {
                    // A finished turn is not a verified outcome — the
                    // step is Done when the session produced output and
                    // ended; verify steps live downstream in the graph.
                    step.state = StepState::Done;
                    changed = true;
                }
                SessionState::Failed | SessionState::Cancelled | SessionState::Unknown => {
                    if step.attempts < step.max_attempts {
                        step.state = StepState::Ready;
                    } else {
                        step.state = StepState::Failed;
                    }
                    changed = true;
                }
                _ => {}
            }
        }

        // Task rollup: all steps terminal → done/failed/blocked.
        let all_terminal = task.steps.iter().all(|s| {
            matches!(
                s.state,
                StepState::Done | StepState::Failed | StepState::Skipped
            )
        });
        if all_terminal && !task.steps.is_empty() {
            let any_failed = task.steps.iter().any(|s| s.state == StepState::Failed);
            task.state = if any_failed {
                TaskState::Blocked
            } else {
                TaskState::Done
            };
            changed = true;
        }

        if changed {
            task.updated_at = chrono::Utc::now();
            if let Err(e) = self.runtime.store().insert_task(&task).await {
                warn!(error = %e, "task persist failed");
            }
            let _ = self
                .runtime
                .store()
                .append(NewEvent::new(
                    EventSource::Scheduler,
                    EventPayload::TaskChanged {
                        task_id: task.id.clone(),
                    },
                ))
                .await
                .map(|ev| self.runtime.bus().publish(ev));
        }
    }

    async fn dispatch_step(
        &self,
        task: &StepCtx,
        step: &WorkflowStep,
    ) -> Result<SessionId, String> {
        // Pick any ready tool; workspaces choose later per-tool routing.
        let tools = self
            .runtime
            .store()
            .list_tools()
            .await
            .map_err(|e| e.to_string())?;
        let tool = tools
            .into_iter()
            .find(|t| t.readiness.rank() >= ToolReadiness::Compatible.rank())
            .ok_or_else(|| "no compatible tool installed for scheduling".to_string())?;

        let instruction = step.instruction.clone().unwrap_or_else(|| {
            let mut s = format!("Step: {}\n\nTask: {}", step.title, task.task_title);
            if let Some(desc) = &task.task_description {
                s.push_str(&format!("\n\n{desc}"));
            }
            if let Some(out) = &step.expected_output {
                s.push_str(&format!("\n\nExpected output: {out}"));
            }
            s
        });

        let cmd = Command {
            id: OperationId::new(),
            idempotency_key: IdempotencyKey::new(),
            actor: Actor::Scheduler,
            kind: CommandKind::SessionStart {
                tool_instance_id: tool.id.clone(),
                workspace_id: task.workspace_id.clone(),
                task_id: Some(task.task_id.clone()),
                title: step.title.clone(),
                prompt: instruction,
                launch: json!({}),
            },
        };
        match self.runtime.execute(cmd).await {
            Ok(CommandOutcome::Applied { result }) => result
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(|s| SessionId::from_raw(s.to_string()))
                .ok_or_else(|| "dispatch returned no session id".to_string()),
            Ok(other) => Err(format!("dispatch rejected: {other:?}")),
            Err(e) => Err(e.to_string()),
        }
    }
}
