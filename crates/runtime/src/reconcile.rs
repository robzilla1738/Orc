//! Startup reconciliation. After a daemon crash, recover durable state:
//! unresolved command intents become outcome-unknown, sessions marked
//! active are demoted to `Unknown` (their controller handles died with
//! the process), and session-bound attention expires.

use crate::bus::EventBus;
use orc_domain::*;
use orc_storage::Store;
use tracing::{info, warn};

/// Run at daemon startup, before serving requests.
pub async fn reconcile(store: &Store, bus: &EventBus) -> usize {
    let mut recovered = 0;

    // 1. Unresolved command intents → OutcomeUnknown + attention items
    //    where a human should look.
    match store.pending_intents().await {
        Ok(pending) => {
            let n = pending.len();
            for intent in pending {
                warn!(
                    operation = %intent.operation_id,
                    "intent never resolved — marking outcome unknown"
                );
                let outcome = CommandOutcome::OutcomeUnknown {
                    reason: "daemon stopped between dispatch and acknowledgment".into(),
                };
                let _ = store
                    .resolve_intent(&intent.idempotency_key, &outcome)
                    .await;
                let _ = store
                    .append(NewEvent::new(
                        EventSource::Runtime,
                        EventPayload::CommandResolved {
                            operation_id: intent.operation_id.clone(),
                            idempotency_key: intent.idempotency_key.clone(),
                            outcome,
                        },
                    ))
                    .await
                    .map(|ev| bus.publish(ev));
            }
            recovered += n;
        }
        Err(e) => warn!(error = %e, "pending intent scan failed"),
    }

    // 2. Sessions still marked active — the controllers that owned them
    //    died with the daemon. Demote to Unknown with an honest note;
    //    providers that support resume can bring them back.
    match store.active_sessions().await {
        Ok(sessions) => {
            for mut session in sessions {
                let from = session.state;
                session.state = SessionState::Unknown;
                session.status_detail = Some(StatusDetail::Fact {
                    message: "daemon restarted; control channel lost. Resume to continue.".into(),
                });
                let _ = store.upsert_session(&session).await;
                let _ = store.expire_attention_for_session(&session.id).await;
                let _ = store
                    .append(
                        NewEvent::new(
                            EventSource::Runtime,
                            EventPayload::SessionStateChanged {
                                session_id: session.id.clone(),
                                from,
                                to: SessionState::Unknown,
                                reason: "daemon restarted; control channel lost".into(),
                                inferred: false,
                            },
                        )
                        .for_session(session.id.clone()),
                    )
                    .await
                    .map(|ev| bus.publish(ev));
                recovered += 1;
            }
        }
        Err(e) => warn!(error = %e, "active session scan failed"),
    }

    if recovered > 0 {
        info!(recovered, "startup reconciliation complete");
    }
    recovered
}
