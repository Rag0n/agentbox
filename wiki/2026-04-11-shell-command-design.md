# `agentbox shell` command

## Problem

agentbox always launches Claude inside the container. To poke around — install
something with `apt`, run a one-off `npm test`, inspect a file, check a tool
version, debug a host-bridge symlink — users either have to ask Claude to do it
on their behalf or shell out manually with `container exec ... bash`. Both are
clumsy. There's no first-class way to drop into the same container Claude uses,
with the same volumes, env, and host-bridge wiring, but without Claude itself.

## Solution

A new `agentbox shell` subcommand that opens a bash session inside the project
container. Same state machine as the default command (create-if-missing,
start-if-stopped), same flags (`--profile`, `--mount`, `--verbose`), same
volumes, env, and host bridge. The only difference is what runs at the end:
`bash -l` instead of `claude --dangerously-skip-permissions`.

```bash
# Interactive shell
agentbox shell

# One-shot command (anything after `--` becomes the bash command)
agentbox shell -- ls -la /workspace
agentbox shell -- npm test

# Standard flags work the same as the default command
agentbox shell --profile mystack
agentbox shell --mount ~/.config/foo
agentbox shell --verbose
```

`shell` does **not** honor `cli.claude.flags` from config — those are
claude-specific. Everything else (volumes, env, profile, mount, bridge) applies
identically.

## State machine

Same three states as `agentbox` default, with the same recovery logic — the
only thing that changes is what gets run at the end.

| Container state | Action                                                                  |
|-----------------|-------------------------------------------------------------------------|
| `Running`       | `container exec` → bash command (entrypoint not invoked)                |
| `Stopped`       | `container start`, then `container exec` → bash command                 |
| `NotFound`      | (rebuild image if needed) → `container run` with `--shell [cmd]` after the image; routes through the entrypoint |

After the bash session exits, the existing `container::maybe_stop_container`
auto-stop logic runs unchanged — if no other agentbox sessions are attached,
the container stops. The same `install_signal_handlers` (SIGHUP/SIGTERM ignore)
applies, so cleanup runs even on terminal close.

Critically, the `Running` and `Stopped` paths bypass `entrypoint.sh` entirely
because `container exec` doesn't invoke the entrypoint. The entrypoint changes
only matter for the `NotFound` cold-start case.

## `entrypoint.sh` change

Add a `--shell` switch right before the final `exec claude` line. All the
existing setup (`.claude.json` seed, HOSTEXEC symlinks,
`command_not_found_handle`) runs unchanged for both modes — useful in a shell
too.

```bash
# ... existing setup unchanged ...

if [ "$1" = "--shell" ]; then
    shift
    if [ $# -eq 0 ]; then
        exec bash -l
    else
        exec bash -lc "$*"
    fi
fi

exec claude --dangerously-skip-permissions "$@"
```

**Why `bash -l` (login shell):** matches the existing `bash -lc` in
`build_exec_args`, so PATH includes `~/.local/bin` (where the host bridge
symlinks live).

**Why route via the entrypoint and not bypass it:** keeps a single source of
truth for container setup. A custom Dockerfile that does `FROM agentbox:default`
automatically inherits both the setup and the shell switch.

### Cache invalidation

`image::needs_build` currently hashes only the Dockerfile content. Touching
`entrypoint.sh` would not auto-invalidate the cache, so existing users'
containers would still launch with the old entrypoint and `agentbox shell` would
fail in the cold-start case until they ran `agentbox build --no-cache` manually.

Fix: `needs_build` (or its callers) hashes
`dockerfile_content + ENTRYPOINT_SCRIPT` together for any image that bundles
the entrypoint. Side benefit: any future entrypoint tweak auto-invalidates the
cache for free.

## Module layout

The change is small enough that no new module is needed. All Rust changes live
in `main.rs`, `container.rs`, and `image.rs`.

### `src/main.rs`

Add a `Shell` variant to the `Commands` enum. The trailing bash command (if
any) is captured the same way the default command captures `task`: via
`split_at_double_dash` *before* clap parsing. Anything after `--` becomes the
shell command, separated from any clap-recognized flags.

```rust
/// Open a bash shell in the container (no Claude)
Shell,
```

The `Some(Commands::Shell)` match arm mirrors the default `None` arm — same
state machine, same env/volume building, same bridge setup, same auto-stop on
exit. The only differences:

- Pass `RunMode::Shell { cmd: passthrough_flags }` instead of
  `RunMode::Claude { task, cli_flags }`.
- Skip the `cli.claude.flags` config merge (those are claude flags; shell
  doesn't get them).

### `src/container.rs`

Replace the `task: Option<String>` field on `RunOpts` and the
`task: Option<&str>` parameter on `exec()` with a `RunMode` enum so invalid
states are unrepresentable:

```rust
pub enum RunMode {
    Claude {
        task: Option<String>,
        cli_flags: Vec<String>,
    },
    Shell {
        cmd: Vec<String>,
    },
}
```

`task` and `cli_flags` move into `RunMode::Claude`. The `interactive` boolean
on `RunOpts` is derived from the variant rather than passed separately:

- `Claude { task: None, .. }` → interactive (TTY)
- `Claude { task: Some(_), .. }` → headless (no TTY)
- `Shell { cmd }` where `cmd.is_empty()` → interactive (TTY)
- `Shell { cmd }` where `!cmd.is_empty()` → headless (no TTY, exit-code passthrough)

`RunOpts::to_run_args` branches on the variant:

- `Claude` → unchanged behavior: append `cli_flags` after the image, then
  `-p <task>` if a task is set.
- `Shell` → append `--shell` after the image, then each cmd token as a separate
  arg. The entrypoint receives `--shell ls -la` and assembles the bash command
  from `"$*"`.

`container::exec()` similarly takes a `&RunMode`. A new private helper
`build_shell_cmd_string(env_vars, cmd)` returns the shell-side bash payload.
The existing HOSTEXEC setup block from `build_exec_args` is extracted to a
shared helper used by both the claude and shell variants:

```rust
fn build_setup_prefix(env_vars: &[(String, String)]) -> String {
    // HOSTEXEC symlink setup + command_not_found_handle setup
    // (lifted from current build_exec_args)
}
```

Then the two `bash -lc` payloads become:

- Claude: `<setup_prefix>; claude --dangerously-skip-permissions <flags> [-p task]`
- Shell, interactive: `<setup_prefix>; exec bash -l`
- Shell, one-shot: `<setup_prefix>; exec bash -lc '<escaped cmd>'`

Single-quote escaping mirrors the existing claude-flag escaping
(`'\''` substitution).

### `src/image.rs`

Modify `needs_build` (or wrap its callers) to hash
`format!("{}\n{}", dockerfile_content, ENTRYPOINT_SCRIPT)` for any image whose
Dockerfile bundles the entrypoint script. The base-image `ensure_base_image`
path also incorporates the entrypoint hash, so default-base images rebuild on
entrypoint changes. Per-project Dockerfiles that don't COPY entrypoint.sh are
unaffected.

The simplest implementation: add `ENTRYPOINT_SCRIPT` to the hashed content
unconditionally for the default image and any image that contains the literal
string `entrypoint.sh` in its content (the marker that signals the script will
be COPY'd in). One regex check, two extra lines per call site.

## `README.md` updates

1. **Quick Start** — add the shell examples next to the existing ones:

   ```bash
   # Open an interactive bash shell in the container
   agentbox shell

   # Run a one-shot command in the container
   agentbox shell -- npm test
   ```

2. **Custom Dockerfiles** — append a short caveat:

   > **Note:** `agentbox shell` requires the agentbox entrypoint script for the
   > cold-start case (when the container doesn't yet exist). If your custom
   > Dockerfile uses `FROM agentbox:default`, it works automatically. If your
   > Dockerfile replaces the entrypoint or uses a fully different base image,
   > the cold-start case won't launch a shell — run `agentbox` first to create
   > the container, then `agentbox shell` works via the exec path.

## Test plan

### Unit tests

Added to existing test modules in `container.rs`, `main.rs`, and `image.rs`.

**`RunOpts::to_run_args` (container.rs):**

- `RunMode::Shell { cmd: empty }` → `--shell` is the only arg after the image,
  no `-p`, no `cli_flags`.
- `RunMode::Shell { cmd: vec!["ls", "-la"] }` → args after image are
  `--shell ls -la`, in that order.
- Existing `RunMode::Claude` tests stay green (regression coverage for the
  refactor).

**`build_shell_cmd_string` / `container::exec` shell path (container.rs):**

- Empty cmd → result ends with `exec bash -l`.
- One-shot cmd `vec!["ls", "-la"]` → result contains `exec bash -lc 'ls -la'`,
  no trailing `claude`.
- Single quotes in cmd values are escaped (`Don't break things` →
  `Don'\''t break things`).
- HOSTEXEC env vars present → setup prefix block precedes the bash invocation.
- `HOSTEXEC_FORWARD_NOT_FOUND=true` → `command_not_found_handle` block present.
- No HOSTEXEC env → no setup prefix.

**Interactive vs headless derivation (container.rs):**

- `RunMode::Shell { cmd: empty }` → exec args contain `--interactive --tty`.
- `RunMode::Shell { cmd: ["ls"] }` → exec args do *not* contain `--tty`.
- Same coverage for `RunMode::Claude` (regression).

**Clap parsing (main.rs):**

- `agentbox shell` → `Commands::Shell`, no payload.
- `agentbox shell -- ls -la` → after `split_at_double_dash`, `passthrough_flags`
  is `["ls", "-la"]`.
- `agentbox shell --profile foo -- ls` → profile flag captured, payload
  captured.
- `agentbox shell --verbose` → verbose flag captured, no payload.

**Cache invalidation (image.rs):**

- `needs_build` with same Dockerfile content but different `ENTRYPOINT_SCRIPT`
  inputs → returns `true` (cache invalidates on entrypoint change).
- `needs_build` with same Dockerfile and same entrypoint → returns `false`
  (no false positives).
- Per-project Dockerfile that doesn't reference entrypoint.sh →
  entrypoint changes do not invalidate (existing behavior preserved).

### Manual smoke test

Documented in the implementation plan, not automated (Apple Container CLI is
not available in CI):

1. `agentbox rm` then `agentbox shell` → cold-start path works, lands in bash,
   `whoami` returns `user`, `pwd` is the project workdir.
2. From an existing running container: open a second terminal, `agentbox shell`
   → exec path works, both sessions visible in `agentbox status`.
3. `agentbox shell -- ls /workspace` → one-shot exits cleanly with non-zero
   exit propagation if the command fails.
4. With `bridge.allowed_commands = ["xcodebuild"]` configured: `agentbox shell`
   → `which xcodebuild` shows `/home/user/.local/bin/xcodebuild` symlink to
   `hostexec`.
5. Auto-stop: `agentbox shell` then exit (Ctrl+D) → container stops if no other
   sessions, stays running if another `agentbox` session is attached.
6. Image upgrade: with an existing container built from the old entrypoint, run
   the new agentbox binary and verify `agentbox shell` triggers an image
   rebuild (the entrypoint hash changed), then works.

## Out of scope

- **Per-shell config** (`cli.shell.flags` etc.) — YAGNI. The default
  `agentbox shell` is parameterless; users who want common args can alias in
  their host shell.
- **Choosing a different shell binary** — `bash -l` only. Supporting zsh/fish
  in custom Dockerfiles is a separate ask.
- **`agentbox shell <name>` to attach to a specific named container** — like
  the default command, `shell` operates on the current project's container.
- **`--detach` / `-d`** — no background-mode shell. The whole point is
  foreground interaction.
- **Refactoring the exec/run code paths to share more** — only the targeted
  `RunMode` enum refactor. No broader cleanup.
