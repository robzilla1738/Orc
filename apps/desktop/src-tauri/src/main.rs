//! Orc desktop bridge.
//!
//! The webview never talks to the daemon directly — all traffic goes
//! through these commands, so the auth token stays in this process.
//! Closing the window hides to the tray; only "Stop all work & quit"
//! shuts the daemon down.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use orc_ipc::{proto::methods, IpcClient};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, State,
};
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

struct AppState {
    runtime_dir: PathBuf,
    client: Mutex<Option<IpcClient>>,
    sub: Mutex<Option<AbortHandle>>,
}

fn runtime_dir() -> PathBuf {
    if let Ok(d) = std::env::var("ORC_RUNTIME_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".orc")
}

async fn client(state: &State<'_, AppState>) -> Result<IpcClient, String> {
    let mut guard = state.client.lock().await;
    if let Some(c) = guard.as_ref() {
        if c.call(methods::PING, json!({})).await.is_ok() {
            return Ok(c.clone());
        }
        *guard = None;
    }
    let c = connect_or_spawn(&state.runtime_dir).await?;
    *guard = Some(c.clone());
    Ok(c)
}

async fn connect_or_spawn(dir: &Path) -> Result<IpcClient, String> {
    if let Ok(c) = IpcClient::connect(dir, "orc-desktop").await {
        return Ok(c);
    }
    spawn_daemon(dir);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        match IpcClient::connect(dir, "orc-desktop").await {
            Ok(c) => return Ok(c),
            Err(e) => {
                if std::time::Instant::now() > deadline {
                    return Err(format!("cannot reach orcd: {e}"));
                }
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
        }
    }
}

fn spawn_daemon(dir: &Path) {
    // Prefer a bundled orcd next to this executable, then PATH.
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let bundled = exe_dir.map(|d| d.join(if cfg!(windows) { "orcd.exe" } else { "orcd" }));
    let bin = bundled
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "orcd".to_string());
    let log = dir.join("orcd.log");
    let _ = std::fs::create_dir_all(dir);
    let file = std::fs::File::create(&log).ok();
    let out = file
        .as_ref()
        .and_then(|f| f.try_clone().ok())
        .map(Stdio::from)
        .unwrap_or_else(Stdio::null);
    let err = file.map(Stdio::from).unwrap_or_else(Stdio::null);
    match std::process::Command::new(&bin)
        .args(["--runtime-dir"])
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
    {
        Ok(_) => tracing::info!(bin, "spawned orcd"),
        Err(e) => tracing::warn!(bin, error = %e, "could not spawn orcd — start it manually"),
    }
}

#[tauri::command]
async fn orc_request(
    state: State<'_, AppState>,
    method: String,
    params: Option<Value>,
) -> Result<Value, String> {
    let c = client(&state).await?;
    let res = c
        .call(&method, params.unwrap_or(json!({})))
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "result": res }))
}

#[tauri::command]
async fn orc_events_subscribe(
    state: State<'_, AppState>,
    channel: tauri::ipc::Channel<Value>,
) -> Result<(), String> {
    let dir = state.runtime_dir.clone();
    let mut guard = state.sub.lock().await;
    if let Some(h) = guard.take() {
        h.abort();
    }
    let handle = tokio::spawn(async move {
        match IpcClient::subscribe(&dir, "orc-desktop", 0).await {
            Ok(mut rx) => {
                while let Some(frame) = rx.recv().await {
                    if channel.send(frame).is_err() {
                        return;
                    }
                }
                // Stream ended (daemon restart / dropped conn) — tell the
                // UI to reload a snapshot and resubscribe.
                let _ = channel.send(json!({ "type": "resync_required" }));
            }
            Err(e) => {
                let _ = channel.send(json!({
                    "type": "resync_required",
                    "reason": e.to_string(),
                }));
            }
        }
    });
    *guard = Some(handle.abort_handle());
    Ok(())
}

#[tauri::command]
async fn orc_events_unsubscribe(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(h) = state.sub.lock().await.take() {
        h.abort();
    }
    Ok(())
}

async fn stop_all_work(app: &AppHandle) {
    let state = app.state::<AppState>();
    if let Ok(c) = IpcClient::connect(&state.runtime_dir, "orc-desktop").await {
        let _ = c.call(methods::SHUTDOWN, json!({})).await;
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    tauri::Builder::default()
        .manage(AppState {
            runtime_dir: runtime_dir(),
            client: Mutex::new(None),
            sub: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            orc_request,
            orc_events_subscribe,
            orc_events_unsubscribe
        ])
        .setup(|app| {
            // Translucent window material — the desktop blurs through the
            // chrome, matching the design language. (NSGlassEffectView washes
            // out webview text on this stack; UnderWindowBackground keeps
            // the same look with readable content.)
            if let Some(w) = app.get_webview_window("main") {
                #[cfg(target_os = "macos")]
                {
                    use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial};
                    let _ = apply_vibrancy(
                        &w,
                        NSVisualEffectMaterial::UnderWindowBackground,
                        None,
                        None,
                    );
                }
            }
            let show = MenuItem::with_id(app, "show", "Show Orc", true, None::<&str>)?;
            let quit =
                MenuItem::with_id(app, "quit", "Quit (work keeps running)", true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop", "Stop all work & quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit, &stop])?;
            let mut tray = TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("Orc");
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    "stop" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            stop_all_work(&app).await;
                            app.exit(0);
                        });
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click { .. } = ev {
                        if let Some(w) = tray.app_handle().get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, ev| {
            // Close hides to tray — the daemon (and the work) survives.
            if let tauri::WindowEvent::CloseRequested { api, .. } = ev {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Orc")
        .run(|app, event| {
            // Dock click restores a hidden window — same as tray "Show Orc".
            if let tauri::RunEvent::Reopen { .. } = event {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
        });
}
