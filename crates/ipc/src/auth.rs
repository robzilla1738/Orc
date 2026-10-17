//! The auth token: a random 256-bit secret the daemon writes with
//! owner-only permissions. The directory is 0700 and the socket itself
//! lives inside it — same-user processes are the trust boundary.

use crate::error::{IpcError, IpcResult};
use rand::RngCore;
use std::path::{Path, PathBuf};

/// Files/dirs the daemon manages under a runtime dir.
pub const RUNTIME_DIR_NAME: &str = "orc";
pub const SOCKET_NAME: &str = "orc.sock";
pub const TOKEN_NAME: &str = "auth.token";
pub const DB_NAME: &str = "orc.db";
pub const PID_NAME: &str = "orc.pid";

/// Resolve the runtime dir: `$XDG_RUNTIME_DIR/orc` or
/// `~/.local/share/orc/run` (macOS fallback) — owner-only.
pub fn runtime_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        if !xdg.is_empty() {
            return Path::new(&xdg).join(RUNTIME_DIR_NAME);
        }
    }
    if let Some(proj) = directories::ProjectDirs::from("dev", "orc", "orc") {
        return proj.data_local_dir().join("run");
    }
    std::env::temp_dir().join(format!("orc-{}", std::process::id()))
}

pub fn socket_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(SOCKET_NAME)
}

/// Load the token, creating it with owner-only permissions if missing.
pub fn load_or_create_token(runtime_dir: &Path) -> IpcResult<String> {
    std::fs::create_dir_all(runtime_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(runtime_dir, std::fs::Permissions::from_mode(0o700));
    }
    let token_path = runtime_dir.join(TOKEN_NAME);
    if token_path.exists() {
        return Ok(std::fs::read_to_string(token_path)?.trim().to_string());
    }
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let token = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes);
    std::fs::write(&token_path, &token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

/// Read the token — the client side. Errors when the file is absent
/// (daemon not started).
pub fn read_token(runtime_dir: &Path) -> IpcResult<String> {
    let p = runtime_dir.join(TOKEN_NAME);
    if !p.exists() {
        return Err(IpcError::Handshake(
            "no auth token — is the daemon running?".into(),
        ));
    }
    Ok(std::fs::read_to_string(p)?.trim().to_string())
}

/// Constant-time-ish compare — the secret is local, but don't hand
/// timing away anyway.
pub fn check_token(expected: &str, presented: &str) -> bool {
    if expected.len() != presented.len() {
        return false;
    }
    expected
        .as_bytes()
        .iter()
        .zip(presented.as_bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}
