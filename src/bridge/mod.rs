pub mod process;
pub mod protocol;
pub mod server;

use std::collections::HashSet;
use std::sync::Arc;
use std::thread;

use rand::Rng;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::config::BridgeConfig;

/// Handle to a running bridge server. Drop to shut down.
pub struct BridgeHandle {
    pub port: u16,
    pub token: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl BridgeHandle {
    pub fn commands_env(&self, config: &BridgeConfig) -> String {
        config.allowed_commands.join(" ")
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn generate_token() -> String {
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| {
            let idx = rng.gen_range(0..36);
            if idx < 10 {
                (b'0' + idx) as char
            } else {
                (b'a' + idx - 10) as char
            }
        })
        .collect()
}

pub fn start_bridge(config: &BridgeConfig, default_cwd: &str) -> anyhow::Result<BridgeHandle> {
    if config.allowed_commands.is_empty() {
        anyhow::bail!("no allowed_commands configured in [bridge]");
    }

    let token = generate_token();
    let allowed: HashSet<String> = config.allowed_commands.iter().cloned().collect();
    let default_cwd = default_cwd.to_string();
    let token_clone = token.clone();

    let (port_tx, port_rx) = std::sync::mpsc::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let thread = thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to create tokio runtime");

        rt.block_on(async {
            let listener = TcpListener::bind("0.0.0.0:0")
                .await
                .expect("failed to bind bridge server");
            let addr = listener.local_addr().unwrap();
            port_tx.send(addr.port()).unwrap();

            let server_config = Arc::new(server::ServerConfig {
                token: token_clone,
                allowed_commands: allowed,
                default_cwd,
                max_concurrent: 16,
            });

            server::run_server(listener, server_config, shutdown_rx).await;
        });
    });

    let port = port_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("bridge thread failed to start"))?;

    Ok(BridgeHandle {
        port,
        token,
        shutdown_tx: Some(shutdown_tx),
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::protocol::Message;

    #[test]
    fn test_generate_token_length() {
        let token = generate_token();
        assert_eq!(token.len(), 32);
    }

    #[test]
    fn test_generate_token_alphanumeric() {
        let token = generate_token();
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn test_generate_token_unique() {
        let t1 = generate_token();
        let t2 = generate_token();
        assert_ne!(t1, t2);
    }

    #[tokio::test]
    async fn test_bridge_start_and_connect() {
        let config = crate::config::BridgeConfig {
            allowed_commands: vec!["echo".to_string()],
            ..Default::default()
        };
        let handle = start_bridge(&config, "/tmp").unwrap();

        // Connect via WebSocket
        let url = format!("ws://127.0.0.1:{}/exec", handle.port);
        let request = tokio_tungstenite::tungstenite::http::Request::builder()
            .uri(&url)
            .header("authorization", format!("Bearer {}", handle.token))
            .header(
                "sec-websocket-key",
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .header("sec-websocket-version", "13")
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .header("host", format!("127.0.0.1:{}", handle.port))
            .body(())
            .unwrap();

        let (ws_stream, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("failed to connect");

        let (mut tx, mut rx) = ws_stream.split();

        // Send an echo command
        let msg = serde_json::json!({
            "type": "run",
            "id": "test1",
            "cmd": ["echo", "hello bridge"],
        });
        tx.send(Message::Text(msg.to_string())).await.unwrap();

        // Collect responses
        let mut got_started = false;
        let mut got_stdout = false;
        let mut got_exit = false;
        let mut stdout_data = String::new();
        let mut exit_code = -1;

        while let Some(Ok(msg)) = rx.next().await {
            if let Message::Text(text) = msg {
                let server_msg: crate::bridge::protocol::ServerMessage =
                    serde_json::from_str(&text).unwrap();
                match server_msg {
                    crate::bridge::protocol::ServerMessage::Started { id, .. } => {
                        assert_eq!(id, "test1");
                        got_started = true;
                    }
                    crate::bridge::protocol::ServerMessage::Stdout { id, data } => {
                        assert_eq!(id, "test1");
                        stdout_data.push_str(&data);
                        got_stdout = true;
                    }
                    crate::bridge::protocol::ServerMessage::Exit { id, code } => {
                        assert_eq!(id, "test1");
                        exit_code = code;
                        got_exit = true;
                        break;
                    }
                    _ => {}
                }
            }
        }

        assert!(got_started, "should receive Started message");
        assert!(got_stdout, "should receive Stdout message");
        assert!(got_exit, "should receive Exit message");
        assert_eq!(stdout_data.trim(), "hello bridge");
        assert_eq!(exit_code, 0);

        drop(handle);
    }

    #[tokio::test]
    async fn test_bridge_rejects_unauthorized() {
        let config = crate::config::BridgeConfig {
            allowed_commands: vec!["echo".to_string()],
            ..Default::default()
        };
        let handle = start_bridge(&config, "/tmp").unwrap();

        let url = format!("ws://127.0.0.1:{}/exec", handle.port);
        let request = tokio_tungstenite::tungstenite::http::Request::builder()
            .uri(&url)
            .header("authorization", "Bearer wrong-token")
            .header(
                "sec-websocket-key",
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .header("sec-websocket-version", "13")
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .header("host", format!("127.0.0.1:{}", handle.port))
            .body(())
            .unwrap();

        let result = tokio_tungstenite::connect_async(request).await;
        assert!(result.is_err(), "should reject wrong token");

        drop(handle);
    }

    #[tokio::test]
    async fn test_bridge_rejects_disallowed_command() {
        let config = crate::config::BridgeConfig {
            allowed_commands: vec!["echo".to_string()],
            ..Default::default()
        };
        let handle = start_bridge(&config, "/tmp").unwrap();

        let url = format!("ws://127.0.0.1:{}/exec", handle.port);
        let request = tokio_tungstenite::tungstenite::http::Request::builder()
            .uri(&url)
            .header("authorization", format!("Bearer {}", handle.token))
            .header(
                "sec-websocket-key",
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .header("sec-websocket-version", "13")
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .header("host", format!("127.0.0.1:{}", handle.port))
            .body(())
            .unwrap();

        let (ws_stream, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("failed to connect");

        let (mut tx, mut rx) = ws_stream.split();

        let msg = serde_json::json!({
            "type": "run",
            "id": "test1",
            "cmd": ["rm", "-rf", "/"],
        });
        tx.send(Message::Text(msg.to_string())).await.unwrap();

        if let Some(Ok(Message::Text(text))) = rx.next().await {
            let server_msg: crate::bridge::protocol::ServerMessage =
                serde_json::from_str(&text).unwrap();
            match server_msg {
                crate::bridge::protocol::ServerMessage::Error { message, .. } => {
                    assert!(message.contains("not in allowlist"));
                }
                other => panic!("expected Error, got {:?}", other),
            }
        }

        drop(handle);
    }
}
