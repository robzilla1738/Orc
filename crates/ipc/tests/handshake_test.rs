//! IPC: handshake auth, version negotiation, request/response,
//! event subscription with replay + lag handling.

use async_trait::async_trait;
use orc_domain::Event;
use orc_ipc::{
    auth,
    proto::methods,
    server::{serve, RequestHandler},
    IpcClient,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{broadcast, watch};

struct Handler {
    token: String,
    tx: broadcast::Sender<Event>,
}

#[async_trait]
impl RequestHandler for Handler {
    async fn handle(&self, method: &str, _params: Value) -> Result<Value, String> {
        match method {
            methods::PING => Ok(json!({"pong": true})),
            methods::DISCOVER => Ok(json!([{"tool": "fake"}])),
            other => Err(format!("unknown {other}")),
        }
    }
    fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
    async fn events_since(&self, _since: i64, _limit: u32) -> Result<Vec<Event>, String> {
        Ok(vec![])
    }
    fn expected_token(&self) -> String {
        self.token.clone()
    }
}

#[tokio::test]
async fn handshake_and_ping() {
    let dir = tempfile::tempdir().unwrap();
    let token = auth::load_or_create_token(dir.path()).unwrap();
    // 256 bits, base64url-no-pad encoded.
    assert_eq!(token.len(), 43);

    // token file must be 0600
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("auth.token"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let (tx, _rx) = broadcast::channel(16);
    let handler = Arc::new(Handler {
        token: token.clone(),
        tx,
    });
    let sock = auth::socket_path(dir.path());
    let serve_sock = sock.clone();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        let _ = serve(&serve_sock, handler, shutdown_rx).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = IpcClient::connect(dir.path(), "test").await.unwrap();
    assert_eq!(client.server.protocol_version, orc_domain::PROTOCOL_VERSION);

    let pong = client.call(methods::PING, json!({})).await.unwrap();
    assert_eq!(pong["pong"], true);

    let disc = client.call(methods::DISCOVER, json!({})).await.unwrap();
    assert_eq!(disc[0]["tool"], "fake");

    let err = client.call("nope", json!({})).await;
    assert!(err.is_err());

    // A wrong token must be rejected.
    let bad = IpcClient::connect_with(&sock, "wrong-token", "bad").await;
    assert!(bad.is_err());

    let _ = shutdown_tx.send(true);
}
