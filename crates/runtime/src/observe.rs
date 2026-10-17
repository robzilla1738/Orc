//! Observed-process reconciliation: diff the sysinfo scan against the
//! last persisted set, emit Seen/Gone events. Observed is an ownership
//! category — these are never controllable.

use crate::bus::EventBus;
use orc_adapters::ObservedScanner;
use orc_domain::*;
use orc_storage::Store;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// A row from the observed_processes table for diffing.
#[derive(Debug, Clone)]
pub struct ObservedRow {
    pub pid: i64,
    pub exe: String,
    pub started_at: Option<String>,
}

pub struct Observer {
    scanner: Mutex<ObservedScanner>,
    store: Store,
    bus: EventBus,
    /// pid+startkey → seen marker, kept in memory between scans.
    last: Mutex<BTreeMap<String, orc_adapters::ObservedProcess>>,
}

impl Observer {
    pub fn new(store: Store, bus: EventBus) -> Arc<Self> {
        Arc::new(Self {
            scanner: Mutex::new(ObservedScanner::new()),
            store,
            bus,
            last: Mutex::new(BTreeMap::new()),
        })
    }

    /// One scan pass. Called on demand and by the daemon's timer.
    pub async fn scan_once(&self) -> usize {
        let now = {
            let mut s = self.scanner.lock().await;
            s.scan()
        };
        let mut last = self.last.lock().await;
        let mut next = BTreeMap::new();
        for proc in &now {
            let key = format!(
                "{}@{}",
                proc.pid,
                proc.started_at.map(|d| d.to_rfc3339()).unwrap_or_default()
            );
            next.insert(key.clone(), proc.clone());
            if !last.contains_key(&key) {
                self.emit(NewEvent::new(
                    EventSource::Runtime,
                    EventPayload::ObservedProcessSeen {
                        pid: proc.pid,
                        exe: proc.exe.clone(),
                        provider: proc.provider,
                        started_at: proc.started_at,
                        cwd: proc.cwd.clone(),
                    },
                ))
                .await;
            }
        }
        for key in last.keys() {
            if !next.contains_key(key) {
                let proc = &last[key];
                self.emit(NewEvent::new(
                    EventSource::Runtime,
                    EventPayload::ObservedProcessGone {
                        pid: proc.pid,
                        exe: proc.exe.clone(),
                    },
                ))
                .await;
            }
        }
        let n = next.len();
        *last = next;
        n
    }

    async fn emit(&self, ev: NewEvent) {
        match self.store.append(ev).await {
            Ok(persisted) => self.bus.publish(persisted),
            Err(e) => tracing::warn!(error = %e, "observed event append failed"),
        }
    }
}
