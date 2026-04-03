# Remove `stop` command

## Problem

The `stop` subcommand is redundant now that auto-stop lands. Auto-stop (merged in the `2026-03-22` work) stops containers automatically when the last attached `agentbox` session exits. Users no longer need to manage container lifecycle manually.

Keeping `stop` adds surface area, documentation, and tests for a command that auto-stop handles transparently.

## Solution

Remove the `stop` subcommand entirely. The CLI simplifies to:

```
agentbox              # start/attach (handles stopped + not-found)
agentbox "task"       # headless mode
agentbox ls           # list containers
agentbox rm [--all]   # remove containers
agentbox build        # rebuild image
agentbox config init  # generate config
```

## Why this is safe

- Auto-stop fires on every normal exit (Ctrl+C, Ctrl+D, `/exit`), SIGHUP, and SIGTERM. Containers stop automatically when the last session detaches.
- `rm` covers explicit cleanup. Since `agentbox` transparently handles both `Stopped` and `NotFound` states, removing a container has the same user experience as stopping one — next launch recreates it.
- The SIGKILL edge case (auto-stop can't fire) is harmless: the container stays running and next `agentbox` launch reattaches to it.
- `container::stop()` stays in the codebase — it's used internally by `maybe_stop_container()` for auto-stop.

## Changes

### `src/main.rs`

- Remove `Stop` variant from `Commands` enum (lines 49-55)
- Remove `Some(Commands::Stop { .. })` match arm (lines 329-348)
- Remove 3 tests: `test_stop_subcommand_no_args`, `test_stop_subcommand_with_names`, `test_stop_subcommand_all`

### `src/container.rs`

No changes. `stop()` and `maybe_stop_container()` stay — they serve auto-stop.

### `README.md`

Remove the `stop` examples from Quick Start:

```bash
# These lines are removed:
# Stop current project's container
agentbox stop

# Stop specific containers
agentbox stop agentbox-myapp-abc123 agentbox-other-def456

# Stop all agentbox containers
agentbox stop --all
```

## Not in scope

- No changes to `rm` behavior
- No changes to auto-stop logic
- No new commands or flags
