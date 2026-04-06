# Auto-stop container on last session exit

## Problem

When a user exits Claude Code (Ctrl+C, Ctrl+D, or closes the terminal), the container keeps running indefinitely. Users who run multiple containers forget to stop them manually, wasting resources.

## Solution

Stop the container automatically when the last `agentbox` process attached to it exits. Since the existing lifecycle already handles `Stopped → start → exec`, this costs nothing on next launch.

## Design

### Signal handling

Register handlers for SIGHUP and SIGTERM in `main()` before the blocking `container::exec()` or `container::run()` call. Store the container name so the handler can access it.

SIGINT (Ctrl+C) propagates to the child `container exec` process via the process group. The blocking call returns, and cleanup runs in the normal exit path.

### Cleanup function

```
maybe_stop_container(container_name: &str)
```

Called from:
- After the blocking `exec()`/`run()` call returns (normal exit, including Ctrl+C)
- From SIGHUP/SIGTERM signal handlers

Logic:
1. Find other `agentbox` processes (excluding own PID) that reference the same container name in their args
2. If none found, run `container stop {container_name}`
3. If others exist, exit without stopping

### Process detection

Shell out to `pgrep -a agentbox` or equivalent to find other agentbox processes. Filter by container name in args. Exclude own PID via `std::process::id()`.

### Edge cases

- **Crash / SIGKILL**: Cannot be caught. Container stays running. Same as current behavior, acceptable.
- **Race — two sessions exit simultaneously, both see the other alive**: Neither stops the container. It persists until next session, which reattaches. Harmless.
- **Race — two sessions exit simultaneously, both see zero others**: Both call `container stop`. Double stop is a no-op. Harmless.
- **Multiple containers (different projects)**: Container names are per-project (`agentbox-{name}-{hash}`), so detection is scoped correctly.

### Files to modify

- `src/main.rs` — signal handler registration, cleanup call after blocking exec/run
- `src/container.rs` — add `maybe_stop_container()` function (or a new module)

### Not in scope

- No pidfiles or lock files
- No config option (auto-stop is always on)
- No changes to container image or entrypoint
- No changes to the bridge
