# Host Bridge Design

## Problem

Agentbox runs AI agents inside Apple Containers (Linux VMs on macOS). Agents cannot execute macOS-specific commands like `xcodebuild`, `xcrun simctl`, `adb`, or `emulator` from within the container. iOS/Android developers need these tools for building, testing, and running apps on simulators/emulators.

No off-the-shelf solution exists for executing host commands from within macOS containers. Docker-based approaches (named pipes, nsenter, socket mounting) either don't work on macOS due to the VM layer or don't support arbitrary macOS command execution.

## Solution

A WebSocket-based bridge that lets the container execute allowlisted commands on the macOS host.

## Architecture

```
┌─────────────────────────────────┐     ┌──────────────────────────┐
│  Apple Container (Linux VM)     │     │  macOS Host              │
│                                 │     │                          │
│  ┌───────────┐                  │     │  ┌────────────────────┐  │
│  │ Agent     │                  │     │  │ agentbox bridge    │  │
│  │ (Claude)  │                  │     │  │                    │  │
│  └─────┬─────┘                  │     │  │  - WebSocket server│  │
│        │ runs "xcodebuild ..."  │     │  │  - Token auth      │  │
│        ▼                        │     │  │  - Allowlist check  │  │
│  ┌───────────┐   WebSocket      │     │  │  - Spawn processes │  │
│  │ hostexec  │──────────────────┼────►│  │  - Stream I/O      │  │
│  │ (symlink) │   (multiplexed)  │     │  └────────────────────┘  │
│  └───────────┘                  │     │                          │
│                                 │     │  Runs: xcodebuild, adb, │
│  /usr/local/bin/xcodebuild ─┐   │     │  xcrun simctl, etc.     │
│  /usr/local/bin/xcrun ──────┤   │     │                          │
│  /usr/local/bin/adb ────────┼→ hostexec                          │
│  /usr/local/bin/emulator ───┘   │     │                          │
└─────────────────────────────────┘     └──────────────────────────┘
```

## Components

### 1. Bridge Server (host-side)

Lives inside the `agentbox` binary as an async background task. Started automatically when a container launches, stopped when it exits.

- **Subcommand:** `agentbox bridge` (internal, used for debugging; normally started automatically)
- **Binds to:** `0.0.0.0:{random_port}` (must be reachable from the container VM)
- **Endpoint:** `ws://127.0.0.1:{port}/exec`
- **Auth:** Random token generated at startup, validated in WebSocket handshake via `Authorization: Bearer {token}` header
- **Allowlist enforcement:** Checks `cmd[0]` as an **exact match** against the `allowed_commands` list. Rejects with error message if not matched.
- **Process management:** Spawns child processes via `tokio::process::Command` (async), captures stdout/stderr via async readers, tracks PIDs for signal forwarding.
- **Lifecycle:** Bridge runs on a dedicated thread with its own tokio runtime. The main thread continues to run the container synchronously (existing blocking `container::run()` / `container::exec()` calls). On container exit, the bridge thread is signaled to shut down and all running child processes are terminated.
- **Connection drops:** When a WebSocket connection is closed (cleanly or due to network failure), all child processes associated with that connection are killed (SIGTERM, then SIGKILL after 5s timeout). An orphan reaper periodically checks for processes whose connections have disappeared.
- **Limits:** Maximum 16 concurrent commands per connection. Maximum WebSocket message size: 1MB.

### 2. Hostexec Client (container-side)

The same `agentbox` binary, detected via `argv[0]`. When invoked as `hostexec` (directly or via symlink), it acts as a client.

**Behavior:**

1. Reads `HOSTEXEC_HOST`, `HOSTEXEC_PORT`, and `HOSTEXEC_TOKEN` from environment
2. Checks `argv[0]` — if not `hostexec` or `agentbox`, uses it as command name (symlink mode)
3. Opens WebSocket to `ws://{HOSTEXEC_HOST}:{port}/exec`
4. Sends `run` message with command + args
5. Streams `stdout`/`stderr` to local stdout/stderr
6. Forwards catchable signals (SIGINT, SIGTERM, SIGHUP, SIGQUIT) as `signal` messages. SIGKILL cannot be caught and is not forwarded.
7. Exits with the code from the `exit` message
8. On connection failure: prints error to stderr and exits with code 127

**Usage:**

```sh
# Via symlink (transparent to agent)
xcodebuild -project Foo.xcodeproj -scheme Debug

# Direct invocation
hostexec xcodebuild -project Foo.xcodeproj

# As agentbox subcommand
agentbox hostexec xcodebuild -project Foo.xcodeproj
```

### 3. Agent Integration (transparent command forwarding)

Two mechanisms ensure agents don't need to know about the bridge:

**Primary — Symlink shims:** The container entrypoint auto-generates symlinks from the allowlist:

```sh
for cmd in $HOSTEXEC_COMMANDS; do
    ln -sf /usr/local/bin/hostexec /usr/local/bin/$cmd
done
```

When `hostexec` is invoked via symlink, it inspects `argv[0]` to determine the command name. The agent runs `xcodebuild ...` and it transparently executes on the host.

**Fallback — `command_not_found_handle`:** Optionally catches commands not found in the container and forwards them to the host:

```sh
command_not_found_handle() { hostexec "$@"; }
```

This catches commands the user forgot to add to the allowlist. Controlled by `forward_not_found` config option. If the server rejects the command (not in allowlist), hostexec prints `"command not found: {cmd}"` to stderr (mimicking the standard shell error) and exits with code 127.

## WebSocket Protocol

Single WebSocket connection at `ws://{HOSTEXEC_HOST}:{port}/exec`, multiplexed by client-chosen command ID.

**Handshake auth:**

```
Authorization: Bearer {HOSTEXEC_TOKEN}
```

### Command IDs

IDs are chosen by the client and must be unique within a connection. The server rejects a `run` message with a duplicate `id` (returns an `error` message).

### Client → Server

```jsonc
// Start a command
{ "type": "run", "id": "1", "cmd": ["xcodebuild", "-project", "Foo.xcodeproj"], "cwd": "/Users/me/project" }

// Send signal to running command (supported: SIGINT, SIGTERM, SIGHUP, SIGQUIT, SIGKILL)
{ "type": "signal", "id": "1", "signal": "SIGINT" }

// Write to stdin of running command (ignored if command hasn't started or has exited)
{ "type": "stdin", "id": "1", "data": "yes\n" }
```

### Server → Client

```jsonc
// Command started
{ "type": "started", "id": "1", "pid": 54321 }

// Output streams (data is UTF-8; non-UTF-8 bytes are replaced with U+FFFD)
{ "type": "stdout", "id": "1", "data": "Build Succeeded\n" }
{ "type": "stderr", "id": "1", "data": "warning: unused variable\n" }

// Command finished
{ "type": "exit", "id": "1", "code": 0 }

// Errors (allowlist rejection, duplicate id, etc.)
{ "type": "error", "id": "1", "message": "command not in allowlist: rm" }
```

### Working Directory

The `cwd` field in `run` messages specifies the host-side working directory for the command. Since agentbox mounts the project directory at the same path in both the container and host, the container's working directory is typically valid on the host. If `cwd` is omitted or the directory does not exist on the host, the server defaults to the project root directory (the directory from which `agentbox` was invoked).

## Configuration

In `~/.config/agentbox/config.toml`:

```toml
[bridge]
allowed_commands = [
    "xcodebuild",
    "xcrun",
    "open",
    "adb",
    "emulator",
    "gradle",
]
forward_not_found = true
```

## Container Binary Distribution

Multi-stage Dockerfile build. The agentbox binary is built from source in a Rust build stage and copied to the final image as `hostexec`:

```dockerfile
FROM rust:1.85 AS builder
RUN cargo install --git https://github.com/user/agentbox.git --root /usr/local

FROM debian:bookworm-slim
COPY --from=builder /usr/local/bin/agentbox /usr/local/bin/hostexec
```

When GitHub releases are available, this simplifies to a `curl` download.

Requires Apple `container` CLI v0.3.0+ (for multi-stage build support).

## Environment Variables

Passed from host to container at startup:

| Variable | Purpose |
|---|---|
| `HOSTEXEC_HOST` | Host IP/hostname for WebSocket connection. Agentbox auto-detects the host gateway IP (e.g., via container networking config). Unlike Docker's `host.docker.internal`, Apple Containers do not provide a built-in hostname for the host — this must be determined at runtime. |
| `HOSTEXEC_PORT` | Bridge server port on the host |
| `HOSTEXEC_TOKEN` | Auth token for WebSocket handshake. Visible to all processes in the container (via `/proc/*/environ`). This is acceptable because the agent is trusted within the container; the token exists to prevent other machines on the network from connecting. |
| `HOSTEXEC_COMMANDS` | Space-separated list of allowed commands (for symlink generation) |
| `HOSTEXEC_FORWARD_NOT_FOUND` | `true`/`false` — enable command_not_found fallback |

## Startup Lifecycle

The bridge starts on **every** `agentbox` invocation that interacts with a container (run, start+exec), not just on initial creation. This handles the case where `agentbox` was previously exited and re-invoked against a stopped container.

```
agentbox run / agentbox (reuse path)
  ├── Load [bridge] config
  ├── Start bridge server (dedicated thread with tokio runtime)
  │   ├── Bind to 127.0.0.1:{random_port}
  │   └── Generate random auth token
  ├── Detect host gateway IP for HOSTEXEC_HOST
  ├── Set HOSTEXEC_HOST, HOSTEXEC_PORT, HOSTEXEC_TOKEN, HOSTEXEC_COMMANDS env vars
  ├── Start/exec container (existing flow, env vars passed through)
  │   └── Container entrypoint:
  │       ├── Create symlinks for each HOSTEXEC_COMMANDS entry
  │       └── Optionally install command_not_found_handle
  └── On container exit: signal bridge thread to shut down, terminate child processes
```

## Rust Module Structure

```
src/
├── bridge/
│   ├── mod.rs        — Bridge server startup, shutdown, config
│   ├── server.rs     — WebSocket handler, auth, message dispatch
│   ├── process.rs    — Child process spawning, I/O streaming, signal forwarding
│   └── protocol.rs   — Message types (serde Serialize/Deserialize)
├── hostexec.rs       — Client: connect, send command, relay I/O
├── main.rs           — Add bridge startup + hostexec argv[0] detection
├── container.rs      — (existing, pass bridge env vars)
├── config.rs         — (existing, add [bridge] section)
├── image.rs          — (existing, unchanged)
└── git.rs            — (existing, unchanged)
```

**New dependencies:**

- `tokio` — async runtime for concurrent command streaming
- `tokio-tungstenite` — WebSocket server and client
- `rand` — auth token generation

## Security Model

- **Network isolation:** Bridge binds to `0.0.0.0` (required so the container VM can reach it via the host gateway IP). Security relies on token authentication, not network-level binding.
- **Token auth:** Random token per session, passed via env var. Required in WebSocket handshake. Token is visible to all processes inside the container — this is acceptable because the container is the trust boundary.
- **Command allowlist:** Only commands whose `cmd[0]` exactly matches an entry in `allowed_commands` are executed. All others rejected.
- **No shell interpretation:** Commands are executed directly via `std::process::Command` (array form), not passed through a shell. Prevents injection.
- **Process lifecycle:** All child processes terminated when bridge shuts down or when their WebSocket connection drops.
- **Platform note:** The host binary (macOS/aarch64) and container binary (Linux/aarch64) are different platform builds. They cannot be interchanged.

## Research Context

This design was informed by research into existing approaches:

- **distrobox/host-spawn:** Uses D-Bus + Flatpak for container→host command execution on Linux. The symlink/argv[0] pattern for transparent command forwarding was adopted directly from this project. D-Bus is Linux-only, so the transport was replaced with WebSocket over TCP.
- **OpenClaw/Docker Sandboxes:** Uses a host-side Gateway process that decides whether to run commands in a container (`docker exec`) or on the host. All orchestration is host-initiated — no container→host bridge exists.
- **SSH-based CI systems:** Jenkins, Buildkite, and GitHub Actions use SSH to connect to Mac build agents. Works but requires SSH server setup and keychain management.
- **Apple Containerization framework:** Uses vsock + gRPC for host→guest communication, but only in the host-to-guest direction. No reverse channel exists.

No existing tool was found that solves the container→host command execution problem on macOS.
