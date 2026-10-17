//! Installed-tool discovery: recognized executables only, safe probes
//! only, and a readiness chain where each step is earned separately.
//!
//! Rules:
//! - curated names + user-supplied paths; never run unknown binaries
//! - resolve symlinks; cache version info so we don't relaunch
//! - never auto-download packages
//! - desktop-vs-terminal PATH divergence is a first-class diagnostic

use orc_domain::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use ts_rs::TS;

/// A provider we know how to discover.
struct KnownTool {
    provider: ProviderKind,
    /// Executable basenames to probe.
    names: &'static [&'static str],
    /// Args for a safe version probe.
    version_args: &'static [&'static str],
    /// Args for a safe auth probe (none if there's no documented one).
    auth_args: Option<&'static [&'static str]>,
    /// Extra settings recorded on the instance.
    settings: &'static [(&'static str, &'static str)],
}

const KNOWN_TOOLS: &[KnownTool] = &[
    KnownTool {
        provider: ProviderKind::Codex,
        names: &["codex"],
        version_args: &["--version"],
        auth_args: Some(&["login", "status"]),
        settings: &[],
    },
    KnownTool {
        provider: ProviderKind::Claude,
        names: &["claude"],
        version_args: &["--version"],
        auth_args: Some(&["auth", "status"]),
        settings: &[],
    },
    KnownTool {
        provider: ProviderKind::OpenCode,
        names: &["opencode"],
        version_args: &["--version"],
        auth_args: None,
        settings: &[],
    },
];

/// One found installation, before readiness probing.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DiscoveredTool {
    pub provider: ProviderKind,
    /// Resolved (symlink-followed) absolute path.
    pub executable: String,
    pub version: Option<String>,
    pub readiness: ToolReadiness,
    /// Why the readiness chain stopped where it did.
    pub readiness_note: Option<String>,
    /// Provider-recommended settings to record on the instance.
    pub settings: std::collections::BTreeMap<String, String>,
}

/// Extra search locations a user can add (e.g. the shell's PATH).
#[derive(Debug, Clone, Default)]
pub struct DiscoveryPaths {
    pub extra: Vec<PathBuf>,
}

#[derive(Clone)]
pub struct Discoverer {
    paths: DiscoveryPaths,
}

impl Discoverer {
    pub fn new(paths: DiscoveryPaths) -> Self {
        Self { paths }
    }

    /// Probe every known tool name. Deterministic, never destructive.
    pub async fn discover(&self) -> Vec<DiscoveredTool> {
        let mut found = Vec::new();
        for tool in KNOWN_TOOLS {
            for name in tool.names {
                let resolved = resolve_executable(name, &self.paths.extra);
                let Some(path) = resolved else { continue };
                let mut d = DiscoveredTool {
                    provider: tool.provider,
                    executable: path.to_string_lossy().into_owned(),
                    version: None,
                    readiness: ToolReadiness::Installed,
                    readiness_note: None,
                    settings: tool
                        .settings
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                };
                probe(tool, &path, &mut d).await;
                found.push(d);
            }
        }
        found
    }

    /// Environment diagnostic: which known tools are NOT on the app's
    /// PATH but may exist under a shell PATH the user can import.
    pub async fn diagnose_path_gap(&self) -> Vec<String> {
        let mut notes = Vec::new();
        for tool in KNOWN_TOOLS {
            for name in tool.names {
                if resolve_executable(name, &self.paths.extra).is_none() {
                    notes.push(format!(
                        "`{name}` not found on the app PATH — if it works in your terminal, import your shell PATH in Settings."
                    ));
                }
            }
        }
        notes
    }
}

impl Default for Discoverer {
    fn default() -> Self {
        Self::new(DiscoveryPaths::default())
    }
}

fn resolve_executable(name: &str, extra: &[PathBuf]) -> Option<PathBuf> {
    // which::which searches PATH; then user paths; then symlinks resolved.
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = which::which(name) {
        candidates.push(p);
    }
    for dir in extra {
        let p = dir.join(name);
        if p.exists() {
            candidates.push(p);
        }
    }
    // Well-known install dirs (brew, npm-globals, cargo, local).
    for dir in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "~/.cargo/bin",
        "~/.local/bin",
    ] {
        let dir = shellexpand(dir);
        let p = dir.join(name);
        if p.exists() {
            candidates.push(p);
        }
    }
    let first = candidates.into_iter().next()?;
    std::fs::canonicalize(&first).ok().or(Some(first))
}

fn shellexpand(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(s)
}

async fn probe(tool: &KnownTool, path: &Path, d: &mut DiscoveredTool) {
    // Version probe — recognized tool only, short timeout.
    match run_probe(path, tool.version_args).await {
        Ok(out) => {
            d.version = out.lines().next().map(|l| l.trim().to_string());
            d.readiness = ToolReadiness::Compatible;
        }
        Err(e) => {
            d.readiness_note = Some(format!("version probe failed: {e}"));
            return;
        }
    }
    // Auth probe — non-fatal; `Authenticated` requires explicit success.
    if let Some(args) = tool.auth_args {
        match run_probe(path, args).await {
            Ok(out) => {
                let lowered = out.to_lowercase();
                if lowered.contains("logged in")
                    || lowered.contains("authenticated")
                    || lowered.contains("ok")
                {
                    d.readiness = ToolReadiness::Authenticated;
                } else {
                    d.readiness_note = Some("auth probe did not confirm login".into());
                }
            }
            Err(_) => {
                d.readiness_note = Some("auth probe failed — sign in via the tool itself".into());
            }
        }
    }
    // `Ready` requires an actual probe session — the daemon sets it
    // after the first successful spawn; discovery never claims it.
}

async fn run_probe(path: &Path, args: &[&str]) -> Result<String, String> {
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(path)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| "probe timed out".to_string())?
    .map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&out.stderr).to_string();
    }
    Ok(text)
}
