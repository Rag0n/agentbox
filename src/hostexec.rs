use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::http::Request;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::bridge::protocol::ServerMessage;

/// Run the hostexec client. Called when binary is invoked as "hostexec" or via symlink.
/// Returns the exit code as i32.
pub fn run(command_name: Option<String>) -> i32 {
    let host = match std::env::var("HOSTEXEC_HOST") {
        Ok(h) => h,
        Err(_) => {
            eprintln!("hostexec: HOSTEXEC_HOST not set (not running inside agentbox?)");
            return 127;
        }
    };
    let port = match std::env::var("HOSTEXEC_PORT") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("hostexec: HOSTEXEC_PORT not set");
            return 127;
        }
    };
    let token = match std::env::var("HOSTEXEC_TOKEN") {
        Ok(t) => t,
        Err(_) => {
            eprintln!("hostexec: HOSTEXEC_TOKEN not set");
            return 127;
        }
    };

    // Build command from argv
    let args: Vec<String> = std::env::args().collect();
    let cmd = if let Some(name) = command_name {
        // Symlink mode: argv[0] is the command name, rest are args
        let mut cmd = vec![name];
        cmd.extend(args[1..].to_vec());
        cmd
    } else if args.len() > 1 {
        // Direct mode: hostexec <command> [args...]
        args[1..].to_vec()
    } else {
        eprintln!("usage: hostexec <command> [args...]");
        return 127;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");

    rt.block_on(run_client(&host, &port, &token, cmd))
}

async fn run_client(host: &str, port: &str, token: &str, cmd: Vec<String>) -> i32 {
    let url = format!("ws://{}:{}/exec", host, port);

    let request = match Request::builder()
        .uri(&url)
        .header("authorization", format!("Bearer {}", token))
        .header(
            "sec-websocket-key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .header("sec-websocket-version", "13")
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("host", format!("{}:{}", host, port))
        .body(())
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("hostexec: failed to build request: {}", e);
            return 127;
        }
    };

    let (ws_stream, _) = match tokio_tungstenite::connect_async(request).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hostexec: connection failed: {}", e);
            return 127;
        }
    };

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Get cwd for the run message
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().to_string());

    // Send run command
    let run_msg = serde_json::json!({
        "type": "run",
        "id": "1",
        "cmd": cmd,
        "cwd": cwd,
    });
    if ws_tx
        .send(Message::Text(run_msg.to_string()))
        .await
        .is_err()
    {
        eprintln!("hostexec: failed to send command");
        return 127;
    }

    // Set up signal forwarding
    let ws_tx = Arc::new(Mutex::new(ws_tx));
    let ws_tx_sig = ws_tx.clone();

    tokio::spawn(async move {
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .expect("failed to register SIGINT");
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM");
        let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
            .expect("failed to register SIGHUP");
        let mut sigquit = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::quit())
            .expect("failed to register SIGQUIT");

        loop {
            let signal_name = tokio::select! {
                _ = sigint.recv() => "SIGINT",
                _ = sigterm.recv() => "SIGTERM",
                _ = sighup.recv() => "SIGHUP",
                _ = sigquit.recv() => "SIGQUIT",
            };
            let msg = serde_json::json!({
                "type": "signal",
                "id": "1",
                "signal": signal_name,
            });
            let mut tx = ws_tx_sig.lock().await;
            let _ = tx.send(Message::Text(msg.to_string())).await;
        }
    });

    // Forward local stdin to the host process
    let ws_tx_stdin = ws_tx.clone();
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let data = String::from_utf8_lossy(&buf[..n]).to_string();
                    let msg = serde_json::json!({
                        "type": "stdin",
                        "id": "1",
                        "data": data,
                    });
                    let mut tx = ws_tx_stdin.lock().await;
                    if tx.send(Message::Text(msg.to_string())).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Read responses
    let mut exit_code = 127;
    while let Some(Ok(msg)) = ws_rx.next().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };

        let server_msg: ServerMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(_) => continue,
        };

        match server_msg {
            ServerMessage::Stdout { data, .. } => {
                use std::io::Write;
                let _ = std::io::stdout().write_all(data.as_bytes());
                let _ = std::io::stdout().flush();
            }
            ServerMessage::Stderr { data, .. } => {
                use std::io::Write;
                let _ = std::io::stderr().write_all(data.as_bytes());
                let _ = std::io::stderr().flush();
            }
            ServerMessage::Exit { code, .. } => {
                exit_code = code;
                break;
            }
            ServerMessage::Error { message, .. } => {
                // Check if this is an allowlist rejection via command_not_found fallback
                if message.starts_with("command not in allowlist:") {
                    let cmd_name = cmd.first().map(|s| s.as_str()).unwrap_or("(unknown)");
                    eprintln!("command not found: {}", cmd_name);
                } else {
                    eprintln!("hostexec: {}", message);
                }
                exit_code = 127;
                break;
            }
            ServerMessage::Started { .. } => {}
        }
    }

    exit_code
}
