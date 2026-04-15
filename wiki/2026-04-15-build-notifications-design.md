# Terminal notifications after image rebuilds

## Problem

When `agentbox` rebuilds the container image, the build can take minutes. Users
routinely tab away during the wait — to Slack, to the browser, to another
terminal — and then miss the moment when the session is actually ready for
input. The symptom is "I started `agentbox` five minutes ago, forgot about it,
and now Claude has been sitting idle at a prompt."

A rebuild can also fail after the user has tabbed away. They learn about it
only when they come back, which may be much later. A failure notification lets
them act sooner.

## Solution

Send a terminal notification when a rebuild finishes — success *or* failure.
Use OSC escape sequences so the notification is raised by the terminal
emulator (Ghostty, WezTerm, iTerm2, Kitty), which surfaces it as a native
system notification regardless of window or tab focus.

Fires as soon as the rebuild sequence finishes successfully — for session
invocations (interactive, headless, or shell), that is right before the
`create_and_run` call that then does its own host-side setup (volume
resolution, env wiring) and finally invokes `container::run`; for the
explicit `agentbox build` command, it's just before the command exits.
That means in the session paths the notification lands slightly ahead of
the actual moment `container::run` takes over — by the small fixed cost
of `create_and_run`'s host-side setup, which is fast compared to the
build — rather than trying to fire from inside `container::run` itself.
The title speaks about the build completing rather than the session
being "ready", because agentbox has both interactive and non-interactive
invocations and "ready" would be misleading for the latter.

Notifications are on by default. Disable via config.

```
┌─────────────────────────────────────────────┐
│ ●  agentbox: build complete                 │
│    myapp                                    │
└─────────────────────────────────────────────┘
```

## Scope

Notifications fire **only for image rebuilds**. Specifically:

| Trigger site (in `src/main.rs`) | When |
|---|---|
| Stopped container + image changed (line 361) | Cache invalidated, image rebuilt, container recreated |
| No container exists + image needs build (line 387) | Cold start with outdated or missing cache |
| Explicit `agentbox build` (line 497) | User invoked the build subcommand (always rebuilds; no `needs_build` guard) |

(Line numbers as of this branch's state; approximate and may drift, use the
surrounding code — `image::needs_build` / `Commands::Build` — as the anchor.)

Cache hits do not fire a notification — the session starts in under a second
and there is nothing to wait for. Notifications for Claude's own prompts
(questions, plan-ready, permissions, task completion) are out of scope and
already covered by the separate `agent-notifications` plugin.

## Architecture

### New module: `src/notify.rs`

Public API:

```rust
/// Run the standard build sequence (ensure_base_image + build + save_cache).
/// Fires a failure notification if the sequence errors. Does not print any
/// user-facing message — the caller owns pre-build messaging (different sites
/// need different wording). Does not check `needs_build` — the caller has
/// already decided a build is required.
pub fn run_build(
    config: &Config,
    dockerfile: &str,
    image_tag: &str,
    cache_key: &str,
    no_cache: bool,
    pull: bool,
    verbose: bool,
) -> Result<()>;

/// Fire the "agentbox: build complete" notification. No-op if notifications
/// are disabled or no supported terminal is detected. The title is deliberately
/// about the build (not the session) because agentbox has invocations that
/// land at a user-ready prompt (bare `agentbox`, `agentbox shell` with no
/// command) and invocations that don't (`agentbox "headless task"`,
/// `agentbox shell -- cmd`, `agentbox build`). "Build complete" is the
/// strongest claim that holds across all of them.
pub fn send_success(config: &Config);

/// Fire the "agentbox: build failed" notification. No-op if disabled or
/// unsupported terminal. Called internally by `run_build`.
pub fn send_failure(config: &Config);
```

Internal helpers (not exposed):

- `detect_terminal(env: impl Fn(&str) -> Option<String>) -> Option<OscKind>`
- `write_osc(writer: &mut impl Write, kind: OscKind, title: &str, body: &str) -> io::Result<()>`
- `sanitize(s: &str) -> String` — strips bytes that could break the OSC envelope or render badly: `\x1b` (ESC), `\x07` (BEL), `;` (OSC 777 field separator), `\n`, `\r`
- `open_tty() -> Option<File>` — `OpenOptions::new().write(true).open("/dev/tty")`

Taking env lookup as a closure parameter (not calling `std::env::var` directly)
keeps `detect_terminal` pure and trivially testable — no mutation of
process-global env state.

### Call site patterns in `main.rs`

Each site peeks `needs_build` to decide whether to build, prints its own
context message, and calls `run_build`. The `did_build` flag is tracked by
the caller and used to fire `send_success` at the handoff.

**Stopped container + image changed** (`main.rs:361`):

```rust
let did_build = if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
    eprintln!("Image changed, recreating container...");
    container::rm(&name, cli.verbose)?;
    notify::run_build(&config, &dockerfile_content, &image_tag, &cache_key,
                      false, false, cli.verbose)?;
    true
} else {
    false
};

if did_build {
    notify::send_success(&config);
    create_and_run(...)
} else {
    container::start(&name, cli.verbose)?;
    container::exec(...)
}
```

**No container exists + image needs build** (`main.rs:387`):

```rust
let did_build = if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
    eprintln!("Building image...");
    notify::run_build(&config, &dockerfile_content, &image_tag, &cache_key,
                      false, false, cli.verbose)?;
    true
} else {
    false
};

if did_build {
    notify::send_success(&config);
}
create_and_run(...)
```

**Explicit `agentbox build`** (`main.rs:497`). This path always rebuilds (no
`needs_build` guard — the user asked for it explicitly) and preserves the
existing `"Building {image_tag}..."` / `"Built {image_tag}"` messages:

```rust
eprintln!("Building {}...", image_tag);
notify::run_build(&config, &dockerfile_content, &image_tag, &cache_key,
                  no_cache, true, cli.verbose)?;
println!("Built {}", image_tag);
notify::send_success(&config);
```

### Why the helper does not print

Each build site has its own distinct user-facing output:

- Stopped path: `"Image changed, recreating container..."` (the user cares about the container being destroyed; rebuild is implied).
- NotFound path: `"Building image..."`.
- Explicit `agentbox build`: `"Building {image_tag}..."` before the build *and* `"Built {image_tag}"` after (the only site that emits a post-build confirmation, because it's the only site that finishes at a normal shell — not in front of a Claude/bash prompt that would make the line redundant).

Pushing the print out of the helper keeps each site's messaging faithful to
its context rather than forcing a shared generic line.

### Why the caller fires success

Each caller's handoff moment is different and only visible at the caller
level: the Stopped path needs to call `create_and_run` next (the
`container::rm` is done before the build, so nothing remains between the
build returning and the handoff); the NotFound path also next calls
`create_and_run`; the Build subcommand has nothing else to do and exits.
Firing from the caller lets the notification land right before each path's
handoff moment, not from inside the helper where the right moment can't
be named.

### Why the helper fires failure

A build failure terminates the whole operation via `?` propagation. There is
no later handoff point at which to fire anything. The helper catches the
error locally, fires `send_failure`, and re-raises.

## Terminal detection and OSC sequences

Detection reads env vars in the following order; first match wins:

| Env condition | Terminal | OSC kind |
|---|---|---|
| `TERM_PROGRAM=ghostty` | Ghostty | OSC 777 |
| `WEZTERM_EXECUTABLE` set | WezTerm | OSC 777 |
| `TERM_PROGRAM=iTerm.app` or `ITERM_SESSION_ID` set | iTerm2 | OSC 9 |
| `KITTY_WINDOW_ID` set | Kitty | OSC 99 |
| none of the above | — | `None` (silent skip) |

OSC emission formats:

| Kind | Byte sequence |
|---|---|
| OSC 777 | `\x1b]777;notify;<title>;<body>\x07` |
| OSC 9 | `\x1b]9;<title> — <body>\x07` |
| OSC 99 | `\x1b]99;;<title> — <body>\x07` |

OSC 9 and OSC 99 display a single text field, so title and body are joined
with an em-dash separator. OSC 777 gives title and body as distinct fields.

**Kitty form choice.** Kitty's OSC 99 protocol supports either a "simple" form
(`OSC 99 ; ; payload ST`, no metadata, treated as a single complete
notification) or a richer chunked form with metadata like `i=<id>`, `d=0/1`
(done-flag), `p=title`/`p=body`. The richer form requires at minimum `d=1` on
the final chunk; `d=0` means "partial, more coming" and the terminal holds
the notification until a `d=1` chunk with the same `id` arrives. To deliver
a title + body in one message, we use the simple form and rely on the
em-dash separator. It renders reliably on current Kitty without any risk
of the terminal waiting for a never-arriving completion chunk.
([protocol reference](https://sw.kovidgoyal.net/kitty/desktop-notifications/))

Bytes are written to `/dev/tty` via `OpenOptions::new().write(true).open(...)`.
Going directly to the controlling terminal (not stdout/stderr) means the
notification fires even when agentbox's output is piped or redirected, and
does not corrupt any captured output.

Before emission, the body (which can contain a project name derived from a
directory basename) runs through `sanitize()` to strip bytes that would
break the OSC envelope or render badly: `\x1b` (ESC), `\x07` (BEL), `;`
(OSC 777 field separator — a legal directory name containing `;` would
otherwise inject extra fields), `\n`, and `\r`. Sanitization is applied
uniformly across OSC kinds rather than per-kind; directory names with `;`
are rare and stripping universally keeps the helper simple. Titles are
hardcoded and need no sanitization.

## Config schema

New section in `~/.config/agentbox/config.toml`:

```toml
# Terminal notifications after long-running image rebuilds.
#
# When agentbox rebuilds the container image (because the Dockerfile changed,
# or on first run), the build can take minutes. If you tab away during that
# wait, a completion notification lets you know when the build has finished
# — whether it succeeded (so you can come back and use the session, run the
# task, or see the build output) or failed (so you don't wait for nothing).
#
# Notifications ONLY fire for image rebuilds — not for normal session starts
# where the cached image is reused, and not for the coding agent's own
# prompts (those are covered by agent-specific plugins like agent-notifications).
#
# Sent via OSC escape sequences. Supported terminals: Ghostty, WezTerm,
# iTerm2, Kitty. Other terminals silently skip (no visible garbage).
[notifications]
enabled = true   # default: true. Set to false to disable.
```

Rust types added to `src/config.rs`:

```rust
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct NotificationsConfig {
    pub enabled: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}
```

And on the existing `Config` struct:

```rust
#[serde(default)]
pub notifications: NotificationsConfig,
```

`#[serde(default)]` on the field ensures existing config files without a
`[notifications]` section still load fine. Users upgrading don't need to edit
anything; the feature turns itself on automatically.

`agentbox config init` output includes the documented section as shown above,
so new users get the doc comments explaining scope, not a bare `enabled = true`.
This requires extending `Config::init_template()` in `src/config.rs` to append
the `[notifications]` block (with its doc comments) alongside the existing
entries, and extending the existing `test_config_init_content` test to assert
the new section's presence (see Testing).

## Notification content

**Success:**
- Title: `agentbox: build complete`
- Body: `<project-name>` — basename of `std::env::current_dir()`

**Failure:**
- Title: `agentbox: build failed`
- Body: `<project-name>`

Example rendering on Ghostty:

> **agentbox: build complete**
> myapp

iTerm2 / Kitty render with em-dash separator:

> agentbox: build complete — myapp

**Why "build complete", not "ready":** agentbox has several invocation shapes
that share the rebuild path, and they land in different end-states:

| Invocation | After rebuild, the user sees… |
|---|---|
| `agentbox` (interactive) | Claude prompt, waiting for input |
| `agentbox shell` (no command) | bash prompt, waiting for input |
| `agentbox "fix tests"` (headless) | headless task running, no prompt |
| `agentbox shell -- ls` (one-shot) | command output, then exit |
| `agentbox build` | `Built <tag>` line, then exit |

Some of these are "ready" in the usual sense (a prompt), others are not.
The title speaks about the build (which did finish) rather than the session
state (which varies), so one wording stays accurate across every path.

**Deliberate omissions:**

- No duration. The user was already waiting; they know roughly how long it took.
- No error message in the failure body. The failure is fully surfaced on the
  terminal regardless: `image::build` captures child stderr, and on a
  non-zero exit the current implementation raises
  `anyhow::bail!("container build failed:\n{}", captured_stderr)` which
  prints the captured stderr as part of the error. In `--verbose` mode the
  stderr also streams live during the build. Either way the user sees the
  full error when they tab back; a truncated error in a notification popup
  would add noise without adding information.
- No emoji in titles. Plain titles match the rest of agentbox's output style
  (log prefixes like `[agentbox]`, no decorations).
- Project name included because users commonly run agentbox in multiple tabs
  against different projects. A bare "build complete" is ambiguous otherwise.

## `run_build` behavior

Full logic:

```rust
pub fn run_build(
    config: &Config,
    dockerfile: &str,
    image_tag: &str,
    cache_key: &str,
    no_cache: bool,
    pull: bool,
    verbose: bool,
) -> Result<()> {
    let result: Result<()> = (|| {
        image::ensure_base_image(dockerfile, no_cache, verbose)?;
        image::build(image_tag, dockerfile, no_cache, pull, verbose)?;
        image::save_cache(dockerfile, cache_key, &image::cache_dir())?;
        Ok(())
    })();

    if result.is_err() {
        send_failure(config);
    }
    result
}
```

Behavior contract:

| Case | Return | Notification fired |
|---|---|---|
| Build succeeded | `Ok(())` | none (caller fires `send_success` at handoff) |
| Build failed (any of the three steps) | `Err(e)` | `send_failure` fired internally before return |

The helper does not check `needs_build` — callers do that so they can print
their own context-appropriate message and perform site-specific setup (like
`container::rm` in the Stopped path) before the build.

The `pull` and `no_cache` flags differ per site: stopped/not-found paths use
`pull=false, no_cache=false`; `agentbox build` uses `pull=true` and passes the
`--no-cache` flag through.

## Edge cases

| Case | Behavior |
|---|---|
| `/dev/tty` cannot be opened (non-TTY, CI) | Silent skip. Never propagates an error. |
| `enabled = false` in config | `send_success` and `send_failure` return immediately. Build still runs normally. |
| Unsupported terminal (`detect_terminal` returns `None`) | Silent skip. No warning — users on Terminal.app didn't opt into notifications and shouldn't be reminded of the fact every build. |
| SSH session to the macOS host | Typically falls into the "unsupported terminal" case above. Env vars like `TERM_PROGRAM`, `ITERM_SESSION_ID`, `WEZTERM_EXECUTABLE`, `KITTY_WINDOW_ID` are usually not forwarded over SSH, so detection returns `None` and the notification silently skips — even though the OSC bytes themselves *could* traverse the PTY to the local emulator. Smarter detection (e.g. via `SSH_CONNECTION`) is out of scope; listed in non-goals. |
| Two agentbox invocations racing (two tabs) | Two independent notifications. No suppression window needed; these are separate sessions. |
| Container creation fails after successful build | Success notification *does* fire (it precedes `create_and_run`), then `create_and_run` errors a moment later. The user may see a "build complete" notification briefly before the error surfaces on return. Acceptable: container creation is fast compared to the build, so the window is small, and firing before handoff is the only way to get the notification out before `container::run` takes over the terminal. |
| Ctrl-C during build | SIGINT propagates to the agentbox parent process. The existing signal handlers (`install_signal_handlers`) only ignore SIGHUP and SIGTERM, and the explicit `agentbox build` subcommand installs no handlers at all — so the parent dies before the build error can be observed. No notification fires. Acceptable: the user initiated the interruption and does not need to be told about it. |

## Testing

### Unit tests in `src/notify.rs`

**Terminal detection** (using a `Fn(&str) -> Option<String>` closure to inject env):

- `TERM_PROGRAM=ghostty` → `Some(Osc777)`
- `WEZTERM_EXECUTABLE=/path` → `Some(Osc777)`
- `TERM_PROGRAM=iTerm.app` → `Some(Osc9)`
- `ITERM_SESSION_ID=...` alone → `Some(Osc9)`
- `KITTY_WINDOW_ID=...` → `Some(Osc99)`
- Empty env → `None`
- `TERM_PROGRAM=Apple_Terminal` → `None`

**OSC emission format** (writing to a `Vec<u8>`):

- OSC 777: exact byte match `\x1b]777;notify;title;body\x07`
- OSC 9: exact byte match `\x1b]9;title — body\x07`
- OSC 99: exact byte match `\x1b]99;;title — body\x07` (simple form, no metadata — see "Kitty form choice" in OSC sequences section)

**Sanitization:**

- `\x1b` (ESC) stripped
- `\x07` (BEL) stripped
- `;` stripped (e.g. `"foo;bar"` → `"foobar"`) — prevents OSC 777 field injection
- `\n` stripped
- `\r` stripped
- Regular spaces preserved
- Non-ASCII / UTF-8 bytes preserved (modern terminals render them correctly)

**Config defaults (in `src/config.rs`):**

- `NotificationsConfig::default().enabled == true`
- TOML without `[notifications]` section loads with `enabled == true`
- TOML with `enabled = false` loads with `enabled == false`

**Config init template (extend the existing `test_config_init_content` in `src/config.rs`):**

- `Config::init_template()` contains the string `[notifications]`
- `Config::init_template()` contains `enabled = true`
- `Config::init_template()` contains the key phrase explaining scope
  (e.g. `"# Terminal notifications after long-running image rebuilds."`)
- The `enabled` line is active (not commented out) so disabling is an
  explicit edit rather than an uncomment-and-edit

### Not tested automatically

- `/dev/tty` open behavior — OS-dependent, not where bugs live
- `run_build` end-to-end — thin composition of already-tested `image::*` functions and the `send_failure` function tested above
- Actual visual appearance of notifications — manual verification

### Manual verification checklist

Covers each RunMode that triggers a rebuild, plus disable/fallback paths.

1. **Explicit build, success.** On Ghostty: `agentbox build` with a small Dockerfile change → notification appears, title "agentbox: build complete", body = project name.
2. **Explicit build, failure.** Introduce a bad `RUN` line, run `agentbox build` → "agentbox: build failed" notification with the same project-name body. Stdout progress appears live during the build; the captured stderr is then printed as part of the `container build failed:` error message after the non-zero exit. Run the same command with `--verbose` to confirm stderr also streams live in that mode.
3. **Interactive session rebuild.** `agentbox` (no task) with a Dockerfile change → notification fires right before Claude's prompt appears.
4. **Headless task rebuild.** `agentbox "echo hello"` with a Dockerfile change → notification fires right before the headless agent starts; the user then sees the agent run and eventually produce output. (If `agent-notifications` is installed, a second "task completed" notification arrives later — that is the expected overlap.)
5. **Shell rebuild, interactive shell.** `agentbox shell` with a Dockerfile change → notification fires right before the bash prompt is shown.
6. **Shell rebuild, one-shot command.** `agentbox shell -- ls` with a Dockerfile change → notification fires before `ls` runs; `ls` output follows.
7. **Config disabled.** Set `[notifications] enabled = false` → none of the above produce a notification; everything else still works.
8. **Unsupported terminal.** In Terminal.app (or simulate via `TERM_PROGRAM=Apple_Terminal` and unset `ITERM_SESSION_ID`): no notification, no garbage bytes in the terminal output.
9. **Output redirected.** `agentbox build > /dev/null 2>&1` on Ghostty → notification still fires (writes to `/dev/tty`, not stdout).
10. **Cache hit (no rebuild).** Run any of 1–6 a second time without changing the Dockerfile → no notification fires (nothing was waited on).
11. **`agentbox config init`.** Run on a fresh config path → emitted file contains the `[notifications]` section with its doc comments and `enabled = true`.

## Files touched

**New:**

- `src/notify.rs` — ~150 lines including tests. OSC logic, config type usage, detection, emission, `run_build`, `send_*` functions, unit tests.

**Modified:**

- `src/main.rs` — `mod notify;` declaration. Three build-site blocks restructured: each calls `notify::run_build(...)` inside the existing `needs_build` check (or unconditionally, for the explicit `agentbox build` path) and tracks a local `did_build` flag, then fires `notify::send_success(&config)` just before the handoff. The handoff is `create_and_run` for session invocations of all shapes (interactive, headless, shell-interactive, shell-one-shot — all pass through `run_session`), and command exit for `agentbox build`.
- `src/config.rs` — add `NotificationsConfig` struct and `notifications: NotificationsConfig` field on `Config`. Extend `init_template()` to include the `[notifications]` section with its doc comments. Extend the existing `test_config_init_content` test with assertions for the new section.
- `README.md` — new `Notifications` subsection under `Configuration`.

**Unchanged:**

- `src/image.rs`, `src/container.rs`, `src/status.rs`, all other modules
- `resources/Dockerfile.default`, `resources/entrypoint.sh`
- `Cargo.toml` — no new dependencies; uses only `std::fs`, `std::io::Write`, `std::env::var`

## Non-goals / future work

- **Notifications for non-OSC terminals** (Terminal.app, VS Code, Warp). Could be added later via an `osascript` or `terminal-notifier` fallback, behind a config opt-in. Out of scope for v1 because the zero-dependency story is a core agentbox value and most agentbox users are on modern terminal emulators that support OSC.
- **Smarter detection for SSH sessions.** When agentbox is run through SSH, the local terminal's env vars usually aren't forwarded, so detection returns `None`. A future iteration could check `SSH_CONNECTION` and attempt OSC regardless (most modern emulators interpret OSC through SSH correctly), but the risk of emitting OSC bytes into terminals that don't interpret them — where they'd become visible garbage — makes this worth deferring until we can ship a confident detection strategy.
- **Threshold-based firing** (only notify if build exceeded N seconds). Adds timing code for marginal benefit — short builds still get the notification, which is redundant but harmless.
- **Suppression when terminal is focused.** OSC-interpreting terminals typically have their own "show in background only" preference — we defer to that rather than adding our own frontmost detection.
- **Richer failure content** (error message, exit code in body). The terminal already surfaces the full build error (captured stderr included in the anyhow error on exit; also streamed live in `--verbose` mode); the notification just needs to wake the user up.
- **Per-mode success titles.** Could tailor the success title per RunMode (e.g. "Claude ready" for interactive, "Task started" for headless). One title across all paths is sufficient; if user feedback shows ambiguity, adding a mode-aware variant is a small additive change.
