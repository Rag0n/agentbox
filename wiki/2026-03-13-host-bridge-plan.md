# Host Bridge Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development to implement this plan. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Add a WebSocket bridge that lets containers execute allowlisted commands on the macOS host, with transparent symlink-based forwarding so agents don't need to know about the bridge.

**Architecture:** A bridge server runs as a background thread inside the `agentbox` process, listening on a random port. Inside the container, a `hostexec` binary (same `agentbox` crate, detected via `argv[0]`) connects to the bridge over WebSocket. Symlinks in `/usr/local/bin/` make host commands transparent to agents.

**Tech Stack:** Rust, tokio (async runtime), tokio-tungstenite (WebSocket), serde_json (protocol messages), rand (token generation)

**Design doc:** `wiki/2026-03-13-host-bridge-design.md`

---

## File Structure

```
src/
├── bridge/
│   ├── mod.rs          — Public API: start_bridge() -> BridgeHandle, BridgeConfig
│   ├── server.rs       — WebSocket accept loop, auth validation, per-connection handler
│   ├── process.rs      — Spawn child process, async stdout/stderr readers, signal forwarding
│   └── protocol.rs     — ClientMessage/ServerMessage enums, serde Serialize/Deserialize
├── hostexec.rs         — Client binary: argv[0] detection, WebSocket client, I/O relay
├── main.rs             — (modify) Add argv[0] dispatch, bridge startup, env var injection
├── config.rs           — (modify) Add BridgeConfig struct with allowed_commands, forward_not_found
├── container.rs        — (existing, unchanged)
├── image.rs            — (existing, unchanged)
├── git.rs              — (existing, unchanged)
resources/
├── entrypoint.sh       — (modify) Add symlink generation + command_not_found_handle setup
├── Dockerfile.default  — (existing, unchanged)
Cargo.toml              — (modify) Add tokio, tokio-tungstenite, rand, futures-util dependencies
```

---

## Chunk 1: Foundation

### Task 1: Add dependencies and protocol types

**Files:**
- Modify: `Cargo.toml`
- Create: `src/bridge/mod.rs`
- Create: `src/bridge/protocol.rs`

- [x] **Step 1: Add dependencies to Cargo.toml**

Add after existing `[dependencies]` entries in `Cargo.toml`:

```toml
tokio = { version = "1", features = ["rt-multi-thread", "net", "io-util", "process", "signal", "sync", "macros"] }
tokio-tungstenite = "0.24"
futures-util = { version = "0.3", default-features = false, features = ["sink"] }
rand = "0.8"
libc = "0.2"
```

Note: `serde_json` is already a dependency in the existing Cargo.toml.

- [x] **Step 2: Create protocol types**

Create `src/bridge/protocol.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Messages sent from the client (container) to the server (host).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Run {
        id: String,
        cmd: Vec<String>,
        cwd: Option<String>,
    },
    Signal {
        id: String,
        signal: String,
    },
    Stdin {
        id: String,
        data: String,
    },
}

/// Messages sent from the server (host) to the client (container).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Started {
        id: String,
        pid: u32,
    },
    Stdout {
        id: String,
        data: String,
    },
    Stderr {
        id: String,
        data: String,
    },
    Exit {
        id: String,
        code: i32,
    },
    Error {
        id: String,
        message: String,
    },
}
```

- [x] **Step 3: Create bridge module root**

Create `src/bridge/mod.rs`:

```rust
pub mod protocol;
pub mod process;
pub mod server;
```

- [x] **Step 4: Write tests for protocol serialization**

Add to `src/bridge/protocol.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_run_message() {
        let json = r#"{"type":"run","id":"1","cmd":["xcodebuild","-project","Foo.xcodeproj"],"cwd":"/tmp"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Run { id, cmd, cwd } => {
                assert_eq!(id, "1");
                assert_eq!(cmd, vec!["xcodebuild", "-project", "Foo.xcodeproj"]);
                assert_eq!(cwd, Some("/tmp".to_string()));
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn test_deserialize_run_no_cwd() {
        let json = r#"{"type":"run","id":"2","cmd":["adb","devices"]}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Run { id, cwd, .. } => {
                assert_eq!(id, "2");
                assert!(cwd.is_none());
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn test_deserialize_signal_message() {
        let json = r#"{"type":"signal","id":"1","signal":"SIGINT"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, ClientMessage::Signal { .. }));
    }

    #[test]
    fn test_deserialize_stdin_message() {
        let json = r#"{"type":"stdin","id":"1","data":"yes\n"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, ClientMessage::Stdin { .. }));
    }

    #[test]
    fn test_serialize_started_message() {
        let msg = ServerMessage::Started { id: "1".into(), pid: 12345 };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"started""#));
        assert!(json.contains(r#""pid":12345"#));
    }

    #[test]
    fn test_serialize_stdout_message() {
        let msg = ServerMessage::Stdout { id: "1".into(), data: "hello\n".into() };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"stdout""#));
        assert!(json.contains(r#""data":"hello\n""#));
    }

    #[test]
    fn test_serialize_exit_message() {
        let msg = ServerMessage::Exit { id: "1".into(), code: 0 };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"exit""#));
        assert!(json.contains(r#""code":0"#));
    }

    #[test]
    fn test_serialize_error_message() {
        let msg = ServerMessage::Error { id: "1".into(), message: "not allowed".into() };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"error""#));
    }
}
```

- [x] **Step 5: Run tests to verify**

Run: `cargo test bridge::protocol`
Expected: All 8 tests pass.

- [x] **Step 6: Commit**

```bash
git add Cargo.toml src/bridge/
git commit -m "feat(bridge): add WebSocket protocol types and bridge module skeleton"
```

---

### Task 2: Add bridge config to config.rs

**Files:**
- Modify: `src/config.rs`

- [x] **Step 1: Write test for bridge config parsing**

Add to `src/config.rs` tests module:

```rust
#[test]
fn test_parse_bridge_config() {
    let toml_str = r#"
        [bridge]
        allowed_commands = ["xcodebuild", "xcrun", "adb"]
        forward_not_found = true
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.bridge.allowed_commands, vec!["xcodebuild", "xcrun", "adb"]);
    assert!(config.bridge.forward_not_found);
}

#[test]
fn test_default_bridge_config() {
    let config = Config::default();
    assert!(config.bridge.allowed_commands.is_empty());
    assert!(!config.bridge.forward_not_found);
}

#[test]
fn test_bridge_config_omitted() {
    let config: Config = toml::from_str("").unwrap();
    assert!(config.bridge.allowed_commands.is_empty());
    assert!(!config.bridge.forward_not_found);
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test config::tests::test_parse_bridge_config`
Expected: FAIL — no `bridge` field on Config.

- [x] **Step 3: Add BridgeConfig struct and field to Config**

In `src/config.rs`, add the struct before the `Config` struct:

```rust
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct BridgeConfig {
    pub allowed_commands: Vec<String>,
    pub forward_not_found: bool,
}
```

Add field to `Config` struct:

```rust
#[serde(default)]
pub bridge: BridgeConfig,
```

Add to `Config::default()`:

```rust
bridge: BridgeConfig::default(),
```

- [x] **Step 4: Add bridge section to init_template()**

Add to the init template string in `Config::init_template()`, before the closing `"#`:

```toml
# Host bridge: execute commands on macOS host from container
# [bridge]
# allowed_commands = ["xcodebuild", "xcrun", "adb", "emulator"]
# forward_not_found = false
```

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test config::tests`
Expected: All tests pass (including the 3 new ones).

- [x] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add [bridge] config section for host command allowlist"
```

---

### Task 3: Process spawning and I/O streaming

**Files:**
- Create: `src/bridge/process.rs`

- [x] **Step 1: Write tests for allowlist checking**

Create `src/bridge/process.rs` with tests:

```rust
use std::collections::HashSet;

/// Check if a command is allowed by the allowlist.
/// Matches cmd[0] exactly against the allowed set.
pub fn is_command_allowed(cmd: &[String], allowed: &HashSet<String>) -> bool {
    cmd.first().map_or(false, |c| allowed.contains(c.as_str()))
}

/// Parse a signal name (e.g., "SIGINT") to a nix signal number.
pub fn parse_signal(name: &str) -> Option<i32> {
    match name {
        "SIGINT" => Some(2),
        "SIGHUP" => Some(1),
        "SIGTERM" => Some(15),
        "SIGQUIT" => Some(3),
        "SIGKILL" => Some(9),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_command() {
        let allowed: HashSet<String> = ["xcodebuild", "adb"].iter().map(|s| s.to_string()).collect();
        let cmd = vec!["xcodebuild".to_string(), "-project".to_string(), "Foo.xcodeproj".to_string()];
        assert!(is_command_allowed(&cmd, &allowed));
    }

    #[test]
    fn test_disallowed_command() {
        let allowed: HashSet<String> = ["xcodebuild".to_string()].into_iter().collect();
        let cmd = vec!["rm".to_string(), "-rf".to_string(), "/".to_string()];
        assert!(!is_command_allowed(&cmd, &allowed));
    }

    #[test]
    fn test_empty_cmd() {
        let allowed: HashSet<String> = ["xcodebuild".to_string()].into_iter().collect();
        let cmd: Vec<String> = vec![];
        assert!(!is_command_allowed(&cmd, &allowed));
    }

    #[test]
    fn test_exact_match_not_prefix() {
        let allowed: HashSet<String> = ["xc".to_string()].into_iter().collect();
        let cmd = vec!["xcodebuild".to_string()];
        assert!(!is_command_allowed(&cmd, &allowed));
    }

    #[test]
    fn test_parse_known_signals() {
        assert_eq!(parse_signal("SIGINT"), Some(2));
        assert_eq!(parse_signal("SIGTERM"), Some(15));
        assert_eq!(parse_signal("SIGKILL"), Some(9));
        assert_eq!(parse_signal("SIGHUP"), Some(1));
        assert_eq!(parse_signal("SIGQUIT"), Some(3));
    }

    #[test]
    fn test_parse_unknown_signal() {
        assert_eq!(parse_signal("SIGFOO"), None);
    }
}
```

- [x] **Step 2: Run tests to verify they pass**

Run: `cargo test bridge::process`
Expected: All 6 tests pass.

- [x] **Step 3: Add async process spawning function**

Add to `src/bridge/process.rs` (above the tests module):

```rust
use tokio::process::Command as TokioCommand;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::bridge::protocol::ServerMessage;

/// Spawn a command and stream its I/O over the provided channel.
/// Returns the child's PID and a stdin writer handle.
/// The child is waited on internally; an Exit message is sent when it completes.
pub async fn spawn_and_stream(
    id: String,
    cmd: &[String],
    cwd: Option<&str>,
    default_cwd: &str,
    tx: mpsc::UnboundedSender<ServerMessage>,
) -> Result<(u32, tokio::process::ChildStdin), String> {
    let program = &cmd[0];
    let args = &cmd[1..];

    let work_dir = cwd.unwrap_or(default_cwd);
    let work_dir = if std::path::Path::new(work_dir).exists() {
        work_dir
    } else {
        default_cwd
    };

    let mut child = TokioCommand::new(program)
        .args(args)
        .current_dir(work_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {}", program, e))?;

    let pid = child.id().unwrap_or(0);
    let stdin = child.stdin.take().unwrap();

    // Spawn stdout reader — chunk-based, not line-based, to handle
    // partial lines (progress bars, prompts) and non-UTF-8 bytes.
    if let Some(stdout) = child.stdout.take() {
        let tx = tx.clone();
        let id = id.clone();
        tokio::spawn(async move {
            let mut reader = stdout;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&buf[..n]).to_string();
                        let _ = tx.send(ServerMessage::Stdout {
                            id: id.clone(),
                            data,
                        });
                    }
                    Err(_) => break,
                }
            }
        });
    }

    // Spawn stderr reader — same chunk-based approach
    if let Some(stderr) = child.stderr.take() {
        let tx = tx.clone();
        let id = id.clone();
        tokio::spawn(async move {
            let mut reader = stderr;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&buf[..n]).to_string();
                        let _ = tx.send(ServerMessage::Stderr {
                            id: id.clone(),
                            data,
                        });
                    }
                    Err(_) => break,
                }
            }
        });
    }

    // Spawn a task to wait on the child and send the Exit message
    {
        let tx = tx.clone();
        let id = id.clone();
        tokio::spawn(async move {
            let exit_status = child.wait().await;
            let code = exit_status
                .map(|s| s.code().unwrap_or(-1))
                .unwrap_or(-1);
            let _ = tx.send(ServerMessage::Exit { id, code });
        });
    }

    Ok((pid, stdin))
}
```

- [x] **Step 4: Commit**

```bash
git add src/bridge/process.rs
git commit -m "feat(bridge): add process spawning with async I/O streaming and allowlist checking"
```

---

### Task 4: WebSocket server

**Files:**
- Create: `src/bridge/server.rs`
- Modify: `src/bridge/mod.rs`

- [x] **Step 1: Write the server module**

Create `src/bridge/server.rs`:

```rust
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::bridge::process;
use crate::bridge::protocol::{ClientMessage, ServerMessage};

/// Configuration for the bridge server.
pub struct ServerConfig {
    pub token: String,
    pub allowed_commands: HashSet<String>,
    pub default_cwd: String,
    pub max_concurrent: usize,
}

/// Start the bridge server. Returns the bound address.
pub async fn run_server(
    listener: TcpListener,
    config: Arc<ServerConfig>,
    mut shutdown: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            accept = listener.accept() => {
                match accept {
                    Ok((stream, addr)) => {
                        let config = config.clone();
                        tokio::spawn(handle_connection(stream, addr, config));
                    }
                    Err(e) => {
                        eprintln!("[agentbox bridge] accept error: {}", e);
                    }
                }
            }
            _ = &mut shutdown => {
                break;
            }
        }
    }
}

async fn handle_connection(stream: TcpStream, addr: SocketAddr, config: Arc<ServerConfig>) {
    // Perform WebSocket handshake with auth validation
    let ws_stream = match tokio_tungstenite::accept_hdr_async(
        stream,
        |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
         resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
            // Check Authorization header
            let auth = req
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let expected = format!("Bearer {}", config.token);
            if auth != expected {
                let resp = tokio_tungstenite::tungstenite::handshake::server::Response::builder()
                    .status(401)
                    .body(None)
                    .unwrap();
                return Err(resp);
            }
            Ok(resp)
        },
    )
    .await
    {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("[agentbox bridge] handshake failed from {}: {}", addr, e);
            return;
        }
    };

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Channel for server messages to send back over WebSocket
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<ServerMessage>();

    // Track running processes: id -> (PID, stdin writer)
    // PID is used for signal forwarding, stdin for writing to the process.
    // The child itself is waited on by a spawned task in process::spawn_and_stream.
    let processes: Arc<tokio::sync::Mutex<HashMap<String, (u32, tokio::process::ChildStdin)>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let active_ids: Arc<tokio::sync::Mutex<HashSet<String>>> =
        Arc::new(tokio::sync::Mutex::new(HashSet::new()));

    // Task: forward ServerMessages to WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(msg) = msg_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                if ws_tx.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }
        }
    });

    // Process incoming messages
    while let Some(Ok(msg)) = ws_rx.next().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };

        // Enforce max message size (1MB)
        if text.len() > 1_048_576 {
            continue;
        }

        let client_msg: ClientMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(_) => continue,
        };

        match client_msg {
            ClientMessage::Run { id, cmd, cwd } => {
                // Check duplicate ID and concurrent limit in a single lock
                {
                    let ids = active_ids.lock().await;
                    if ids.contains(&id) {
                        let _ = msg_tx.send(ServerMessage::Error {
                            id,
                            message: "duplicate command id".into(),
                        });
                        continue;
                    }
                    if ids.len() >= config.max_concurrent {
                        let _ = msg_tx.send(ServerMessage::Error {
                            id,
                            message: "too many concurrent commands".into(),
                        });
                        continue;
                    }
                }

                // Check allowlist
                if !process::is_command_allowed(&cmd, &config.allowed_commands) {
                    let cmd_name = cmd.first().map(|s| s.as_str()).unwrap_or("(empty)");
                    let _ = msg_tx.send(ServerMessage::Error {
                        id,
                        message: format!("command not in allowlist: {}", cmd_name),
                    });
                    continue;
                }

                active_ids.lock().await.insert(id.clone());

                // spawn_and_stream returns (PID, stdin) and internally spawns
                // a task that waits on the child and sends Exit when done.
                let tx = msg_tx.clone();
                let processes = processes.clone();
                let active_ids = active_ids.clone();
                let default_cwd = config.default_cwd.clone();

                match process::spawn_and_stream(
                    id.clone(),
                    &cmd,
                    cwd.as_deref(),
                    &default_cwd,
                    tx.clone(),
                )
                .await
                {
                    Ok((pid, stdin)) => {
                        let _ = tx.send(ServerMessage::Started {
                            id: id.clone(),
                            pid,
                        });
                        // Store PID + stdin for signal forwarding and stdin writes.
                        // The child process is waited on by spawn_and_stream internally.
                        // When Exit is received by the send_task, the client knows it's done.
                        processes.lock().await.insert(id.clone(), (pid, stdin));

                        // Spawn cleanup task: poll PID until process exits,
                        // then remove from tracking maps.
                        let processes_cleanup = processes.clone();
                        let active_ids_cleanup = active_ids.clone();
                        let id_cleanup = id.clone();
                        if let Ok(pid_i32) = i32::try_from(pid) {
                            tokio::spawn(async move {
                                loop {
                                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                                    let alive = unsafe { libc::kill(pid_i32, 0) == 0 };
                                    if !alive { break; }
                                }
                                processes_cleanup.lock().await.remove(&id_cleanup);
                                active_ids_cleanup.lock().await.remove(&id_cleanup);
                            });
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(ServerMessage::Error {
                            id: id.clone(),
                            message: e,
                        });
                        active_ids.lock().await.remove(&id);
                    }
                }
            }

            ClientMessage::Signal { id, signal } => {
                if let Some(sig) = process::parse_signal(&signal) {
                    let procs = processes.lock().await;
                    if let Some((pid, _)) = procs.get(&id) {
                        if let Ok(pid_i32) = i32::try_from(*pid) {
                            unsafe {
                                libc::kill(pid_i32, sig);
                            }
                        }
                    }
                }
            }

            ClientMessage::Stdin { id, data } => {
                let mut procs = processes.lock().await;
                if let Some((_, stdin)) = procs.get_mut(&id) {
                    let _ = stdin.write_all(data.as_bytes()).await;
                }
            }
        }
    }

    // Connection closed — kill all running children (SIGTERM first, SIGKILL after 5s)
    {
        let procs = processes.lock().await;
        for (_, (pid, _)) in procs.iter() {
            if let Ok(pid_i32) = i32::try_from(*pid) {
                unsafe { libc::kill(pid_i32, libc::SIGTERM); }
            }
        }
        // Give processes 5 seconds to exit gracefully, then SIGKILL
        drop(procs);
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let procs = processes.lock().await;
        for (_, (pid, _)) in procs.iter() {
            if let Ok(pid_i32) = i32::try_from(*pid) {
                unsafe { libc::kill(pid_i32, libc::SIGKILL); }
            }
        }
    }

    send_task.abort();
}
```

Note: `libc` dependency was already added to Cargo.toml in Task 1.

**Implementation notes for the implementer:**
- The `accept_hdr_async` callback signature must match tokio-tungstenite 0.24's `Callback` trait exactly. The error response type is `http::Response<Option<String>>`. Verify against the crate's docs if compilation fails.
- The orphan reaper (processes whose connections have died) is handled by the connection-close cleanup block above. For a more robust implementation, a periodic sweep could be added later.

- [x] **Step 2: Update bridge/mod.rs with public start API**

Replace `src/bridge/mod.rs` with:

```rust
pub mod process;
pub mod protocol;
pub mod server;

use std::collections::HashSet;
use std::net::SocketAddr;
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
    /// Returns the list of allowed commands for passing to the container.
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

/// Generate a random alphanumeric token.
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

/// Start the bridge server on a background thread.
/// Returns a handle with the port and token.
pub fn start_bridge(config: &BridgeConfig, default_cwd: &str) -> anyhow::Result<BridgeHandle> {
    if config.allowed_commands.is_empty() {
        // No bridge needed if no commands configured
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
            // Bind to 0.0.0.0 so the container VM can reach the bridge
            // via the host gateway IP (e.g., 192.168.64.1).
            // Security relies on token auth, not network binding.
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

    let port = port_rx.recv().map_err(|_| anyhow::anyhow!("bridge thread failed to start"))?;

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
}
```

- [x] **Step 3: Run all tests**

Run: `cargo test`
Expected: All tests pass.

- [x] **Step 4: Commit**

```bash
git add Cargo.toml src/bridge/
git commit -m "feat(bridge): add WebSocket server with auth, allowlist, and process management"
```

---

## Chunk 2: Client & Integration

### Task 5: Hostexec client

**Files:**
- Create: `src/hostexec.rs`

- [x] **Step 1: Write the hostexec client module**

Create `src/hostexec.rs`:

```rust
use std::process::ExitCode;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::http::Request;

use crate::bridge::protocol::{ClientMessage, ServerMessage};

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
        .header("sec-websocket-key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
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
        .send(Message::Text(run_msg.to_string().into()))
        .await
        .is_err()
    {
        eprintln!("hostexec: failed to send command");
        return 127;
    }

    // Set up signal forwarding
    let ws_tx = std::sync::Arc::new(tokio::sync::Mutex::new(ws_tx));
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
            let _ = tx.send(Message::Text(msg.to_string().into())).await;
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
                    if tx.send(Message::Text(msg.to_string().into())).await.is_err() {
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
```

- [x] **Step 2: Run `cargo check` to verify compilation**

Run: `cargo check`
Expected: No errors.

- [x] **Step 3: Commit**

```bash
git add src/hostexec.rs
git commit -m "feat(hostexec): add WebSocket client for transparent host command execution"
```

---

### Task 6: Wire everything into main.rs

**Files:**
- Modify: `src/main.rs`

- [x] **Step 1: Add module declarations**

Add to the top of `src/main.rs`, after existing `mod` declarations:

```rust
mod bridge;
mod hostexec;
```

- [x] **Step 2: Add argv[0] detection at the very start of main()**

Replace the current `fn main() -> Result<()> {` block opening with:

```rust
fn main() -> Result<()> {
    // Check if we're invoked as hostexec (symlink mode)
    let binary_name = std::env::args()
        .next()
        .and_then(|a| {
            std::path::Path::new(&a)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
        })
        .unwrap_or_default();

    if binary_name == "hostexec" {
        std::process::exit(hostexec::run(None));
    } else if binary_name != "agentbox" && !binary_name.is_empty() {
        // Invoked via a symlink like "xcodebuild" -> hostexec
        // But only if HOSTEXEC_HOST is set (we're in a container)
        if std::env::var("HOSTEXEC_HOST").is_ok() {
            std::process::exit(hostexec::run(Some(binary_name)));
        }
    }
```

- [x] **Step 3: Add bridge startup to the container run flow**

In `main.rs`, in the `None =>` arm where containers are started, add bridge startup **before** the container interaction. Find the line `let config = config::Config::load()?;` in the `None` arm and add the bridge logic after config is loaded but before container status check:

```rust
// Start bridge if configured
let bridge_handle = if !config.bridge.allowed_commands.is_empty() {
    match bridge::start_bridge(&config.bridge, &cwd_str) {
        Ok(handle) => {
            eprintln!(
                "[agentbox] bridge started on port {} ({} commands allowed)",
                handle.port,
                config.bridge.allowed_commands.len()
            );
            Some(handle)
        }
        Err(e) => {
            eprintln!("[agentbox] warning: bridge failed to start: {}", e);
            None
        }
    }
} else {
    None
};
```

- [x] **Step 4: Inject bridge env vars into env_vars**

In `build_env_vars` or at the call sites, add bridge env vars when the handle exists. In the `None =>` arm, after `build_env_vars` calls, add:

```rust
if let Some(ref handle) = bridge_handle {
    env_vars.push(("HOSTEXEC_HOST".into(), /* host gateway IP */ "192.168.64.1".into()));
    env_vars.push(("HOSTEXEC_PORT".into(), handle.port.to_string()));
    env_vars.push(("HOSTEXEC_TOKEN".into(), handle.token.clone()));
    env_vars.push(("HOSTEXEC_COMMANDS".into(), handle.commands_env(&config.bridge)));
    if config.bridge.forward_not_found {
        env_vars.push(("HOSTEXEC_FORWARD_NOT_FOUND".into(), "true".into()));
    }
}
```

Note: The host gateway IP (`192.168.64.1`) is the default for Apple Containers' virtual network. This should be auto-detected at runtime. A simple approach: try to resolve the gateway from the container networking config or use a well-known default. For the initial implementation, use the Apple Container default gateway. A follow-up task can make this configurable or auto-detected.

Add a helper function to detect the host IP:

```rust
/// Detect the host IP that containers can reach.
/// Apple Containers use 192.168.64.1 as the default gateway.
fn detect_host_ip() -> String {
    // TODO: auto-detect from container networking; for now use Apple Container default
    "192.168.64.1".to_string()
}
```

- [x] **Step 5: Pass env vars through all container paths**

The bridge env vars need to be injected in three places in `main.rs`:
1. `create_and_run` — already uses `build_env_vars` + git vars, add bridge vars to the env_vars before passing to `create_and_run`. Modify `create_and_run` to accept extra env vars, or inject them before the call.
2. `ContainerStatus::Running` → `container::exec` — add bridge vars to env_vars.
3. `ContainerStatus::Stopped` → `container::start` + `container::exec` — add bridge vars to env_vars.

The simplest approach: create a helper that builds the complete env vars including bridge:

```rust
fn build_all_env_vars(
    config: &config::Config,
    bridge_handle: Option<&bridge::BridgeHandle>,
) -> Vec<(String, String)> {
    let mut env_vars = build_env_vars(&config.env);
    env_vars.extend(git::git_env_vars());
    if let Some(handle) = bridge_handle {
        env_vars.push(("HOSTEXEC_HOST".into(), detect_host_ip()));
        env_vars.push(("HOSTEXEC_PORT".into(), handle.port.to_string()));
        env_vars.push(("HOSTEXEC_TOKEN".into(), handle.token.clone()));
        env_vars.push(("HOSTEXEC_COMMANDS".into(), handle.commands_env(&config.bridge)));
        if config.bridge.forward_not_found {
            env_vars.push(("HOSTEXEC_FORWARD_NOT_FOUND".into(), "true".into()));
        }
    }
    env_vars
}
```

Then use `build_all_env_vars` in all three container paths.

- [x] **Step 6: Run `cargo check`**

Run: `cargo check`
Expected: No errors.

- [x] **Step 7: Run all tests**

Run: `cargo test`
Expected: All existing tests + new tests pass.

- [x] **Step 8: Commit**

```bash
git add src/main.rs src/hostexec.rs
git commit -m "feat: integrate bridge server and hostexec into main binary"
```

---

### Task 7: Update entrypoint.sh for symlink generation

**Files:**
- Modify: `resources/entrypoint.sh`

- [x] **Step 1: Add symlink and command_not_found setup to entrypoint**

Update `resources/entrypoint.sh` to add symlink generation after the claude.json setup but before `exec claude`:

```bash
#!/bin/bash
set -e

DEFAULTS='{"hasCompletedOnboarding":true}'
CF="$HOME/.claude.json"
SEED="/tmp/claude-seed.json"

if [ -f "$SEED" ]; then
    jq -s '.[0] * .[1]' <(echo "$DEFAULTS") "$SEED" > "$CF"
else
    echo "$DEFAULTS" > "$CF"
fi

# Set up host bridge symlinks if configured
if [ -n "$HOSTEXEC_COMMANDS" ]; then
    for cmd in $HOSTEXEC_COMMANDS; do
        ln -sf /usr/local/bin/hostexec "/usr/local/bin/$cmd" 2>/dev/null || true
    done
fi

# Set up command_not_found fallback if enabled
if [ "$HOSTEXEC_FORWARD_NOT_FOUND" = "true" ]; then
    echo 'command_not_found_handle() { hostexec "$@"; }' >> /etc/bash.bashrc
fi

exec claude --dangerously-skip-permissions "$@"
```

- [x] **Step 2: Commit**

```bash
git add resources/entrypoint.sh
git commit -m "feat(entrypoint): add host bridge symlink generation and command_not_found fallback"
```

---

### Task 8: Integration test

**Files:**
- No new files — test via cargo test

- [x] **Step 1: Write an integration test for bridge start/connect/execute cycle**

Add to `src/bridge/mod.rs` tests:

```rust
#[tokio::test]
async fn test_bridge_start_and_connect() {
    let config = crate::config::BridgeConfig {
        allowed_commands: vec!["echo".to_string()],
        forward_not_found: false,
    };
    let handle = start_bridge(&config, "/tmp").unwrap();

    // Connect via WebSocket
    let url = format!("ws://127.0.0.1:{}/exec", handle.port);
    let request = tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(&url)
        .header("authorization", format!("Bearer {}", handle.token))
        .header("sec-websocket-key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
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
    tx.send(Message::Text(msg.to_string().into()))
        .await
        .unwrap();

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
        forward_not_found: false,
    };
    let handle = start_bridge(&config, "/tmp").unwrap();

    let url = format!("ws://127.0.0.1:{}/exec", handle.port);
    let request = tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(&url)
        .header("authorization", "Bearer wrong-token")
        .header("sec-websocket-key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
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
        forward_not_found: false,
    };
    let handle = start_bridge(&config, "/tmp").unwrap();

    let url = format!("ws://127.0.0.1:{}/exec", handle.port);
    let request = tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(&url)
        .header("authorization", format!("Bearer {}", handle.token))
        .header("sec-websocket-key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
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
    tx.send(Message::Text(msg.to_string().into()))
        .await
        .unwrap();

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
```

- [x] **Step 2: Run all tests**

Run: `cargo test`
Expected: All tests pass including the 3 new integration tests.

- [x] **Step 3: Run clippy and fmt**

Run: `cargo fmt && cargo clippy`
Expected: No warnings or errors.

- [x] **Step 4: Commit**

```bash
git add src/bridge/
git commit -m "test(bridge): add integration tests for bridge start, auth, and allowlist"
```

---

## Chunk 3: Remaining Tasks

### Task 9: Update config init template

**Files:**
- Modify: `src/config.rs`

Already covered in Task 2 Step 4. No additional work needed.

### Task 10: Host IP auto-detection (follow-up)

**Files:**
- Modify: `src/main.rs`

- [x] **Step 1: Research Apple Container gateway IP**

The default Apple Container virtual network uses `192.168.64.0/24` with the host at `192.168.64.1`. This is the gateway visible from inside the VM. For now, hardcode this default but make it overridable:

```toml
[bridge]
host_ip = "192.168.64.1"  # optional override
```

- [x] **Step 2: Add host_ip to BridgeConfig**

In `src/config.rs`, add to `BridgeConfig`:

```rust
pub host_ip: Option<String>,
```

- [x] **Step 3: Update detect_host_ip to use config**

In `src/main.rs`:

```rust
fn detect_host_ip(config: &config::BridgeConfig) -> String {
    config
        .host_ip
        .clone()
        .unwrap_or_else(|| "192.168.64.1".to_string())
}
```

- [x] **Step 4: Run tests**

Run: `cargo test`
Expected: All pass.

- [x] **Step 5: Commit**

```bash
git add src/config.rs src/main.rs
git commit -m "feat(config): add optional host_ip override for bridge connectivity"
```

---

### Task 11: Final verification and cleanup

- [x] **Step 1: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

- [x] **Step 2: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: No warnings.

- [x] **Step 3: Run fmt check**

Run: `cargo fmt -- --check`
Expected: No formatting issues.

- [x] **Step 4: Verify the complete file structure**

```bash
ls -la src/bridge/
# Should show: mod.rs, protocol.rs, process.rs, server.rs
```

- [x] **Step 5: Final commit if any remaining changes**

```bash
git status
# Commit any remaining changes
```
