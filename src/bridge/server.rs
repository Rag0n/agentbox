use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::http;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::bridge::process::{is_command_allowed, parse_signal, spawn_and_stream};
use crate::bridge::protocol::{ClientMessage, ServerMessage};

/// Configuration for the WebSocket bridge server.
pub struct ServerConfig {
    pub token: String,
    pub allowed_commands: HashSet<String>,
    pub default_cwd: String,
    pub max_concurrent: usize,
}

/// Accept loop: listens for incoming TCP connections until shutdown is signalled.
pub async fn run_server(
    listener: TcpListener,
    config: Arc<ServerConfig>,
    shutdown: oneshot::Receiver<()>,
) {
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, addr)) => {
                        let cfg = Arc::clone(&config);
                        tokio::spawn(async move {
                            handle_connection(stream, addr, cfg).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("[bridge] accept error: {}", e);
                    }
                }
            }
            _ = &mut shutdown => {
                break;
            }
        }
    }
}

/// Handle a single WebSocket connection: auth, message dispatch, cleanup.
#[allow(clippy::result_large_err)]
async fn handle_connection(stream: TcpStream, addr: SocketAddr, config: Arc<ServerConfig>) {
    let expected_token = config.token.clone();

    // Perform WebSocket handshake with auth validation
    let ws_stream = match tokio_tungstenite::accept_hdr_async(
        stream,
        |request: &http::Request<()>, response: http::Response<()>| {
            let auth_header = request
                .headers()
                .get("Authorization")
                .and_then(|v| v.to_str().ok());

            let expected = format!("Bearer {}", expected_token);

            match auth_header {
                Some(val) if val == expected => Ok(response),
                _ => {
                    let err_response = http::Response::builder()
                        .status(http::StatusCode::UNAUTHORIZED)
                        .body(Some("Unauthorized".to_string()))
                        .unwrap();
                    Err(err_response)
                }
            }
        },
    )
    .await
    {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("[bridge] handshake failed for {}: {}", addr, e);
            return;
        }
    };

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Channel for sending ServerMessages back to the client
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<ServerMessage>();

    // Track active processes: id -> (pid, stdin)
    let mut processes: HashMap<String, (u32, tokio::process::ChildStdin)> = HashMap::new();
    let mut active_ids: HashSet<String> = HashSet::new();

    // Channel for cleanup notifications from PID reaper tasks
    let (cleanup_tx, mut cleanup_rx) = mpsc::unbounded_channel::<String>();

    // Spawn a task that forwards ServerMessages to the WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(msg) = msg_rx.recv().await {
            let json = match serde_json::to_string(&msg) {
                Ok(j) => j,
                Err(_) => continue,
            };
            if ws_tx.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    });

    // Message handling loop
    while let Some(result) = ws_rx.next().await {
        // Drain completed process notifications
        while let Ok(id) = cleanup_rx.try_recv() {
            processes.remove(&id);
            active_ids.remove(&id);
        }

        let ws_msg = match result {
            Ok(m) => m,
            Err(_) => break,
        };

        let text = match ws_msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };

        let client_msg: ClientMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                let _ = msg_tx.send(ServerMessage::Error {
                    id: String::new(),
                    message: format!("invalid message: {}", e),
                });
                continue;
            }
        };

        match client_msg {
            ClientMessage::Run { id, cmd, cwd } => {
                // Check duplicate ID
                if active_ids.contains(&id) {
                    let _ = msg_tx.send(ServerMessage::Error {
                        id,
                        message: "duplicate command id".to_string(),
                    });
                    continue;
                }

                // Check concurrent limit
                if active_ids.len() >= config.max_concurrent {
                    let _ = msg_tx.send(ServerMessage::Error {
                        id,
                        message: format!("concurrent limit reached ({})", config.max_concurrent),
                    });
                    continue;
                }

                // Check allowlist
                if !is_command_allowed(&cmd, &config.allowed_commands) {
                    let _ = msg_tx.send(ServerMessage::Error {
                        id,
                        message: format!(
                            "command not in allowlist: {}",
                            cmd.first().unwrap_or(&String::new())
                        ),
                    });
                    continue;
                }

                // Spawn the process
                let cwd_ref = cwd.as_deref();
                match spawn_and_stream(
                    id.clone(),
                    &cmd,
                    cwd_ref,
                    &config.default_cwd,
                    msg_tx.clone(),
                )
                .await
                {
                    Ok((pid, stdin)) => {
                        let _ = msg_tx.send(ServerMessage::Started {
                            id: id.clone(),
                            pid,
                        });
                        processes.insert(id.clone(), (pid, stdin));
                        active_ids.insert(id.clone());

                        // Spawn cleanup task: poll PID until process exits, then notify
                        let cleanup = cleanup_tx.clone();
                        let cleanup_id = id.clone();
                        tokio::spawn(async move {
                            loop {
                                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                                let alive = unsafe { libc::kill(pid as i32, 0) };
                                if alive != 0 {
                                    break;
                                }
                            }
                            let _ = cleanup.send(cleanup_id);
                        });
                    }
                    Err(e) => {
                        let _ = msg_tx.send(ServerMessage::Error { id, message: e });
                    }
                }
            }

            ClientMessage::Signal { id, signal } => {
                if let Some((pid, _)) = processes.get(&id) {
                    if let Some(sig) = parse_signal(&signal) {
                        unsafe {
                            libc::kill(*pid as i32, sig);
                        }
                    } else {
                        let _ = msg_tx.send(ServerMessage::Error {
                            id,
                            message: format!("unknown signal: {}", signal),
                        });
                    }
                } else {
                    let _ = msg_tx.send(ServerMessage::Error {
                        id,
                        message: "unknown process id".to_string(),
                    });
                }
            }

            ClientMessage::Stdin { id, data } => {
                if let Some((_, stdin)) = processes.get_mut(&id) {
                    if let Err(e) = stdin.write_all(data.as_bytes()).await {
                        let _ = msg_tx.send(ServerMessage::Error {
                            id,
                            message: format!("stdin write error: {}", e),
                        });
                    }
                } else {
                    let _ = msg_tx.send(ServerMessage::Error {
                        id,
                        message: "unknown process id".to_string(),
                    });
                }
            }
        }
    }

    // Connection closed: SIGTERM all children
    for (pid, _) in processes.values() {
        unsafe {
            libc::kill(*pid as i32, libc::SIGTERM);
        }
    }

    // Wait 5 seconds, then SIGKILL any survivors
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    for (pid, _) in processes.values() {
        let alive = unsafe { libc::kill(*pid as i32, 0) };
        if alive == 0 {
            unsafe {
                libc::kill(*pid as i32, libc::SIGKILL);
            }
        }
    }

    // Abort the send task
    send_task.abort();
}
