//! Observed external processes — detection, not control. Identity is
//! pid + executable + start time so a stale pid is never signaled or
//! mislabeled.

use orc_domain::ProviderKind;
use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use ts_rs::TS;

/// A likely agent process found on the machine.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ObservedProcess {
    pub pid: u32,
    pub exe: String,
    pub provider: ProviderKind,
    /// OS process start time — required to prove identity.
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub cwd: Option<String>,
    pub cmdline: Vec<String>,
}

/// Known agent executable basenames → provider.
const KNOWN: &[(&str, ProviderKind)] = &[
    ("codex", ProviderKind::Codex),
    ("claude", ProviderKind::Claude),
    ("opencode", ProviderKind::OpenCode),
    ("gemini", ProviderKind::Pty),
    ("aider", ProviderKind::Pty),
    ("cursor-agent", ProviderKind::Pty),
    ("goose", ProviderKind::Pty),
];

pub struct ObservedScanner {
    sys: System,
}

impl ObservedScanner {
    pub fn new() -> Self {
        Self { sys: System::new() }
    }

    /// Rescan and return currently-observed agent processes.
    pub fn scan(&mut self) -> Vec<ObservedProcess> {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_exe(UpdateKind::Always)
                .with_cmd(UpdateKind::Always)
                .with_cwd(UpdateKind::Always),
        );
        let mut out = Vec::new();
        for (pid, proc_) in self.sys.processes() {
            let exe = proc_
                .exe()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let base = exe.rsplit('/').next().unwrap_or(&exe).to_string();
            let provider = KNOWN
                .iter()
                .find(|(name, _)| base == *name || base.starts_with(&format!("{name}-")))
                .map(|(_, p)| *p);
            let Some(provider) = provider else { continue };
            let started = if proc_.start_time() > 0 {
                chrono::DateTime::from_timestamp(proc_.start_time() as i64, 0)
            } else {
                None
            };
            out.push(ObservedProcess {
                pid: pid.as_u32(),
                exe: proc_
                    .exe()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or(base),
                provider,
                started_at: started,
                cwd: proc_.cwd().map(|p| p.to_string_lossy().into_owned()),
                cmdline: proc_
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect(),
            });
        }
        out
    }
}

impl Default for ObservedScanner {
    fn default() -> Self {
        Self::new()
    }
}
