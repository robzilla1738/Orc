//! `orc-fake-agent` — a fake provider executable for tests.
//! Speaks newline-delimited JSON on stdio and supports failure modes
//! via `ORC_FAKE_MODE`:
//!   normal    — emits init, echoes input as assistant text, exits 0
//!   hang      — emits init then never finishes (cancellation tests)
//!   approval  — emits init then an approval request, then exits
//!   crash     — exits non-zero after init
//!   question  — emits a question request, then exits
//!   noisy     — floods output (batching/backpressure tests)

use std::io::{BufRead, Write};

fn emit(v: serde_json::Value) {
    let out = std::io::stdout();
    let mut h = out.lock();
    let _ = writeln!(h, "{}", serde_json::to_string(&v).unwrap());
    let _ = h.flush();
}

fn main() {
    let mode = std::env::var("ORC_FAKE_MODE").unwrap_or_else(|_| "normal".into());
    let session_id =
        std::env::var("ORC_FAKE_SESSION_ID").unwrap_or_else(|_| "fake-native-1".into());

    emit(serde_json::json!({
        "type": "system",
        "subtype": "init",
        "session_id": session_id,
    }));

    match mode.as_str() {
        "hang" => {
            // Never finish — waits to be killed.
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        }
        "crash" => {
            eprintln!("fake-agent crashed deliberately");
            std::process::exit(42);
        }
        "approval" => {
            emit(serde_json::json!({
                "type": "approval_request",
                "request_id": "req-1",
                "kind": "exec",
                "command": "rm -rf /tmp/whatever",
                "reason": "fake cleanup",
            }));
            std::process::exit(0);
        }
        "question" => {
            emit(serde_json::json!({
                "type": "question",
                "request_id": "req-1",
                "text": "Which database?",
                "options": ["postgres", "sqlite"],
            }));
            std::process::exit(0);
        }
        "noisy" => {
            for i in 0..50_000 {
                emit(serde_json::json!({
                    "type": "assistant",
                    "message": {"content": [{"type": "output_text", "text": format!("noise line {i}")}]},
                }));
            }
            std::process::exit(0);
        }
        _ => {
            // normal: one assistant line per stdin line, then exit on EOF.
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(l) = line else { break };
                if l.is_empty() {
                    continue;
                }
                let prompt: serde_json::Value =
                    serde_json::from_str(&l).unwrap_or_else(|_| serde_json::json!({}));
                let text = prompt
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first())
                    .and_then(|b| b.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or(&l)
                    .to_string();
                emit(serde_json::json!({
                    "type": "assistant",
                    "message": {"content": [{"type": "output_text", "text": format!("echo: {text}")}]},
                }));
                emit(serde_json::json!({
                    "type": "result",
                    "subtype": "success",
                    "total_cost_usd": 0.0,
                    "usage": {"input_tokens": 1, "output_tokens": 1},
                }));
            }
            std::process::exit(0);
        }
    }
}
