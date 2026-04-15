# Build Notifications Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Send an OSC terminal notification when `agentbox` finishes an image rebuild (success or failure), so users who tab away during long builds know when to come back.

**Architecture:** New `src/notify.rs` module owns terminal detection (env-var based, returns an `OscKind` enum), OSC emission (to `/dev/tty` directly, bypassing stdout redirection), sanitization (strips envelope-breaking bytes from user-controlled body text), and a thin `run_build` helper that sequences `image::ensure_base_image` + `image::build` and — if those succeed — then calls `image::save_cache`. `run_build` fires `send_failure` *only* on build-phase errors (ensure_base_image or build itself); a `save_cache` error propagates but does NOT fire a failure notification, because the image has actually built correctly at that point — calling it a "build failure" would be a lie. The decision logic lives in `run_build_inner`, a pure function with injected closures that is unit-tested; `run_build` itself is a thin wrapper that passes the real `image::*` calls in. Three call sites in `main.rs` are restructured to call `run_build` and fire `send_success` at their handoff points. One new field on `Config` (`notifications.enabled`, default `true`) gates the feature.

**Tech Stack:** Rust std library only — `std::fs::OpenOptions`, `std::io::Write`, `std::env::var`, `serde` (already a dependency for config). No new Cargo dependencies.

**Reference:** Design doc at `wiki/2026-04-15-build-notifications-design.md`. Read it before starting — it contains the full rationale for design choices (OSC formats, Kitty simple form, per-site pre-build messaging, etc.).

---

## Task 1: Config — Add NotificationsConfig and wire into Config

**Files:**
- Modify: `src/config.rs` (add struct near top, add field on `Config`, update manual `impl Default for Config`)
- Tests: extend existing test module in `src/config.rs`

- [ ] **Step 1: Write a failing test for the default value**

Add this test to the existing `mod tests` block in `src/config.rs` (below `test_default_config`):

```rust
#[test]
fn test_default_notifications_enabled() {
    let config = Config::default();
    assert!(config.notifications.enabled);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --quiet test_default_notifications_enabled 2>&1 | tail -20`
Expected: compile error — `no field 'notifications' on type 'Config'`.

- [ ] **Step 3: Define NotificationsConfig and add to Config**

Add this near the top of `src/config.rs`, below the existing `CliConfig` struct (around line 20):

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

Add the field to the `Config` struct (before the closing brace of the struct, around line 37):

```rust
    #[serde(default)]
    pub notifications: NotificationsConfig,
```

Update the manual `impl Default for Config` block (around line 45) to include the new field:

```rust
impl Default for Config {
    fn default() -> Self {
        Self {
            cpus: None,
            memory: "8G".to_string(),
            dockerfile: None,
            default_agent: None,
            env: HashMap::new(),
            profiles: HashMap::new(),
            volumes: Vec::new(),
            bridge: BridgeConfig::default(),
            cli: HashMap::new(),
            notifications: NotificationsConfig::default(),
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --quiet test_default_notifications_enabled 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Write a failing test for TOML parsing (enabled = false)**

Add this test to the same `mod tests` block:

```rust
#[test]
fn test_parse_notifications_disabled() {
    let toml_str = r#"
        memory = "4G"

        [notifications]
        enabled = false
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert!(!config.notifications.enabled);
}
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test --quiet test_parse_notifications_disabled 2>&1 | tail -20`
Expected: PASS (no further code change needed — `#[serde(default)]` already handles the parsing path, the field's `Default` is `true`, and the explicit `false` overrides).

- [ ] **Step 7: Write a failing test for TOML without notifications section (should default to enabled)**

Add this test to the same `mod tests` block:

```rust
#[test]
fn test_parse_without_notifications_section_defaults_to_enabled() {
    let toml_str = r#"
        memory = "4G"
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert!(config.notifications.enabled);
}
```

- [ ] **Step 8: Run the test to verify it passes**

Run: `cargo test --quiet test_parse_without_notifications_section_defaults_to_enabled 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 9: Run the full test suite to confirm no regressions**

Run: `cargo test --quiet 2>&1 | tail -5`
Expected: all previously passing tests still pass (baseline was 294 passing; now 294 + 3 new = 297 passing, 0 failed).

---

## Task 2: Config — Extend init_template with [notifications] section

**Files:**
- Modify: `src/config.rs` — `init_template()` function (around line 116) and `test_config_init_content` test (around line 249)

- [ ] **Step 1: Add failing assertions to test_config_init_content**

Edit `src/config.rs` — locate the existing `test_config_init_content` test and add these assertions before its closing brace:

```rust
    // notifications section
    assert!(content.contains("[notifications]"));
    assert!(content.contains("enabled = true"));
    assert!(content.contains("Terminal notifications after long-running image rebuilds"));
    // enabled line must be active (not commented) so disabling requires an explicit edit
    assert!(content.lines().any(|l| l.trim_start().starts_with("enabled = true")));
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --quiet test_config_init_content 2>&1 | tail -20`
Expected: FAIL — one of the `assert!(content.contains(...))` calls fails.

- [ ] **Step 3: Extend init_template() with the [notifications] block**

In `src/config.rs`, find the `init_template()` function. Append a `[notifications]` block to the end of the template string (right before the closing `"#`), so the raw string ends like this:

```rust
    pub fn init_template() -> &'static str {
        r#"# agentbox configuration

# Default agent used by bare `agentbox`. `agentbox setup` will write this
# for you; uncomment and edit to change it manually.
# default_agent = "claude"   # or "codex"

# Resources (auto-detected from host if not set)
# cpus = 4          # default: half of host cores
# memory = "8G"     # default: 8G

# Additional volumes to mount into containers
# volumes = [
#   "~/.config/tool",            # tilde = home-relative mapping
#   "/opt/libs",                 # absolute = same path in container
#   "/src/path:/dest/path",     # explicit source:dest mapping
# ]

# Override the default Dockerfile for all projects
# dockerfile = "~/.config/agentbox/Dockerfile.custom"

# Environment variables to pass into container
# [env]
# KEY = ""        # empty = inherit from host env
# KEY = "value"   # literal value

# Named profiles with custom Dockerfiles
# [profiles.name]
# dockerfile = "/path/to/Dockerfile"

# Default flags for each coding agent.
# Replace to override. The "dangerously-*" flags bypass in-agent
# sandboxing because the container already isolates the agent.
[cli.claude]
flags = ["--dangerously-skip-permissions"]

[cli.codex]
flags = ["--dangerously-bypass-approvals-and-sandbox"]

# Host bridge: execute commands on macOS host from container
# [bridge]
# allowed_commands = ["xcodebuild", "xcrun", "adb", "emulator"]
# forward_not_found = false

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
enabled = true
"#
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --quiet test_config_init_content 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Run the full test suite to confirm no regressions**

Run: `cargo test --quiet 2>&1 | tail -5`
Expected: all tests pass.

---

## Task 3: Notify — OscKind enum and detect_terminal

**Files:**
- Create: `src/notify.rs`
- Modify: `src/main.rs` — add `mod notify;` declaration

- [ ] **Step 1: Create src/notify.rs with the OscKind enum skeleton**

Create `src/notify.rs` with initial content:

```rust
//! Terminal OSC notifications after image rebuilds.
//!
//! See `wiki/2026-04-15-build-notifications-design.md` for rationale.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscKind {
    /// OSC 777 — Ghostty, WezTerm. Supports separate title and body fields.
    Osc777,
    /// OSC 9 — iTerm2. Single text field (title and body joined with em-dash).
    Osc9,
    /// OSC 99 simple form — Kitty. Single text field, no metadata
    /// (avoids Kitty's d=0/d=1 chunked protocol).
    Osc99,
}

/// Detect the terminal emulator from a set of environment variable lookups.
/// Caller passes a closure so tests can inject env without mutating process state.
/// Returns `None` for unsupported terminals — callers silently skip notifications.
///
/// Treats empty-string values as "not set" (e.g. `VAR= ./agentbox ...`),
/// because `std::env::var` returns `Ok("")` in that case. Without this, a
/// user who explicitly un-set `KITTY_WINDOW_ID=` would still be detected
/// as Kitty.
pub fn detect_terminal<F: Fn(&str) -> Option<String>>(env: F) -> Option<OscKind> {
    // Helper: treat empty strings as absent.
    let get = |k: &str| env(k).filter(|s| !s.is_empty());

    match get("TERM_PROGRAM").as_deref() {
        Some("ghostty") => return Some(OscKind::Osc777),
        Some("iTerm.app") => return Some(OscKind::Osc9),
        _ => {}
    }
    if get("WEZTERM_EXECUTABLE").is_some() {
        return Some(OscKind::Osc777);
    }
    if get("ITERM_SESSION_ID").is_some() {
        return Some(OscKind::Osc9);
    }
    if get("KITTY_WINDOW_ID").is_some() {
        return Some(OscKind::Osc99);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_map(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + '_ {
        let map: HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k: &str| map.get(k).cloned()
    }
}
```

Note: `HashMap` is imported for use in tests. The `pub use` of it from tests is indirect (tests see the outer scope); keeping the import in the module body is fine.

- [ ] **Step 2: Declare the module in main.rs**

Edit `src/main.rs` top-of-file module declarations (around lines 4-11). Add `mod notify;` alphabetically with the others:

```rust
mod bridge;
mod config;
mod container;
mod git;
mod hostexec;
mod image;
mod notify;
mod setup;
mod status;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build 2>&1 | tail -20`
Expected: clean build (warnings about unused functions are OK since the module isn't used yet).

- [ ] **Step 4: Write failing tests for detect_terminal**

Append to the `mod tests` block in `src/notify.rs`:

```rust
    #[test]
    fn test_detect_ghostty_by_term_program() {
        let env = env_map(&[("TERM_PROGRAM", "ghostty")]);
        assert_eq!(detect_terminal(env), Some(OscKind::Osc777));
    }

    #[test]
    fn test_detect_wezterm_by_executable() {
        let env = env_map(&[("WEZTERM_EXECUTABLE", "/Applications/WezTerm.app/...")]);
        assert_eq!(detect_terminal(env), Some(OscKind::Osc777));
    }

    #[test]
    fn test_detect_iterm2_by_term_program() {
        let env = env_map(&[("TERM_PROGRAM", "iTerm.app")]);
        assert_eq!(detect_terminal(env), Some(OscKind::Osc9));
    }

    #[test]
    fn test_detect_iterm2_by_session_id() {
        let env = env_map(&[("ITERM_SESSION_ID", "w0t1p0:abc123")]);
        assert_eq!(detect_terminal(env), Some(OscKind::Osc9));
    }

    #[test]
    fn test_detect_kitty_by_window_id() {
        let env = env_map(&[("KITTY_WINDOW_ID", "1")]);
        assert_eq!(detect_terminal(env), Some(OscKind::Osc99));
    }

    #[test]
    fn test_detect_apple_terminal_unsupported() {
        let env = env_map(&[("TERM_PROGRAM", "Apple_Terminal")]);
        assert_eq!(detect_terminal(env), None);
    }

    #[test]
    fn test_detect_empty_env_unsupported() {
        let env = env_map(&[]);
        assert_eq!(detect_terminal(env), None);
    }

    #[test]
    fn test_detect_empty_string_values_treated_as_absent() {
        // Users may explicitly unset vars via `VAR= ./agentbox ...`. std::env::var
        // returns Ok("") in that case — we must treat it as "not set".
        let env = env_map(&[
            ("TERM_PROGRAM", ""),
            ("WEZTERM_EXECUTABLE", ""),
            ("ITERM_SESSION_ID", ""),
            ("KITTY_WINDOW_ID", ""),
        ]);
        assert_eq!(detect_terminal(env), None);
    }

    #[test]
    fn test_detect_term_program_takes_precedence_over_iterm_session() {
        // If both are set, TERM_PROGRAM wins because it's checked first.
        let env = env_map(&[
            ("TERM_PROGRAM", "ghostty"),
            ("ITERM_SESSION_ID", "should-be-ignored"),
        ]);
        assert_eq!(detect_terminal(env), Some(OscKind::Osc777));
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --quiet notify:: 2>&1 | tail -20`
Expected: all 9 `test_detect_*` tests PASS.

---

## Task 4: Notify — sanitize()

**Files:**
- Modify: `src/notify.rs`

- [ ] **Step 1: Write failing tests for sanitize**

Append to the `mod tests` block in `src/notify.rs`:

```rust
    #[test]
    fn test_sanitize_strips_esc() {
        assert_eq!(sanitize("foo\x1bbar"), "foobar");
    }

    #[test]
    fn test_sanitize_strips_bel() {
        assert_eq!(sanitize("foo\x07bar"), "foobar");
    }

    #[test]
    fn test_sanitize_strips_semicolon() {
        // Prevents OSC 777 field injection.
        assert_eq!(sanitize("my;project"), "myproject");
    }

    #[test]
    fn test_sanitize_strips_newline_and_cr() {
        assert_eq!(sanitize("foo\nbar"), "foobar");
        assert_eq!(sanitize("foo\rbar"), "foobar");
        assert_eq!(sanitize("foo\r\nbar"), "foobar");
    }

    #[test]
    fn test_sanitize_preserves_spaces() {
        assert_eq!(sanitize("my project"), "my project");
    }

    #[test]
    fn test_sanitize_preserves_utf8() {
        assert_eq!(sanitize("café 🚀"), "café 🚀");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --quiet notify::tests::test_sanitize 2>&1 | tail -20`
Expected: compile error — `sanitize` not defined.

- [ ] **Step 3: Implement sanitize**

Add this function to the body of `src/notify.rs` (below `detect_terminal`, above `#[cfg(test)]`):

```rust
/// Strip bytes that could break the OSC envelope or render badly.
///
/// Strips: `\x1b` (ESC — starts a new escape sequence), `\x07` (BEL — ends
/// OSC), `;` (OSC 777 field separator — a legal directory name containing
/// `;` would otherwise inject extra fields), `\n`, `\r`.
///
/// Applied uniformly across OSC kinds; `;` isn't strictly required for
/// OSC 9/99 but stripping universally keeps the helper simple.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(*c, '\x1b' | '\x07' | ';' | '\n' | '\r'))
        .collect()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --quiet notify::tests::test_sanitize 2>&1 | tail -20`
Expected: all 6 `test_sanitize_*` tests PASS.

---

## Task 5: Notify — write_osc()

**Files:**
- Modify: `src/notify.rs`

- [ ] **Step 1: Write failing tests for write_osc for each OSC kind**

Append to the `mod tests` block in `src/notify.rs`:

```rust
    #[test]
    fn test_write_osc_777_format() {
        let mut buf = Vec::<u8>::new();
        write_osc(&mut buf, OscKind::Osc777, "title", "body").unwrap();
        assert_eq!(buf, b"\x1b]777;notify;title;body\x07");
    }

    #[test]
    fn test_write_osc_9_format() {
        let mut buf = Vec::<u8>::new();
        write_osc(&mut buf, OscKind::Osc9, "title", "body").unwrap();
        assert_eq!(buf, "\x1b]9;title — body\x07".as_bytes());
    }

    #[test]
    fn test_write_osc_99_simple_form() {
        let mut buf = Vec::<u8>::new();
        write_osc(&mut buf, OscKind::Osc99, "title", "body").unwrap();
        // Simple form, no metadata. See Kitty form choice in design doc.
        assert_eq!(buf, "\x1b]99;;title — body\x07".as_bytes());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --quiet notify::tests::test_write_osc 2>&1 | tail -20`
Expected: compile error — `write_osc` not defined.

- [ ] **Step 3: Implement write_osc**

Add this function to the body of `src/notify.rs` (below `sanitize`, above `#[cfg(test)]`). Update the top-of-file imports to also pull in `io::Write`:

```rust
use std::io::{self, Write};
```

Then the function:

```rust
/// Write an OSC notification sequence to `writer`.
///
/// Callers are responsible for sanitizing user-controlled input (typically
/// the body) via `sanitize` before calling this.
pub fn write_osc<W: Write>(
    writer: &mut W,
    kind: OscKind,
    title: &str,
    body: &str,
) -> io::Result<()> {
    match kind {
        OscKind::Osc777 => write!(writer, "\x1b]777;notify;{};{}\x07", title, body)?,
        OscKind::Osc9 => write!(writer, "\x1b]9;{} — {}\x07", title, body)?,
        OscKind::Osc99 => write!(writer, "\x1b]99;;{} — {}\x07", title, body)?,
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --quiet notify::tests::test_write_osc 2>&1 | tail -20`
Expected: all 3 `test_write_osc_*` tests PASS.

---

## Task 6: Notify — send_success / send_failure / open_tty

**Files:**
- Modify: `src/notify.rs`

- [ ] **Step 1: Write failing tests for the public send helpers (using an injected Write)**

Before implementing the public helpers, add a testable inner function
`send_with` that takes a writer, a terminal kind, and a config. Write tests
against it. Append to `mod tests`:

```rust
    use crate::config::Config;

    fn enabled_config() -> Config {
        Config::default()
    }

    fn disabled_config() -> Config {
        let mut c = Config::default();
        c.notifications.enabled = false;
        c
    }

    #[test]
    fn test_send_with_writes_success_osc_777() {
        let mut buf = Vec::<u8>::new();
        send_with(&mut buf, OscKind::Osc777, &enabled_config(), Kind::Success, "myapp").unwrap();
        assert_eq!(buf, b"\x1b]777;notify;agentbox: build complete;myapp\x07");
    }

    #[test]
    fn test_send_with_writes_failure_osc_9() {
        let mut buf = Vec::<u8>::new();
        send_with(&mut buf, OscKind::Osc9, &enabled_config(), Kind::Failure, "myapp").unwrap();
        assert_eq!(buf, "\x1b]9;agentbox: build failed — myapp\x07".as_bytes());
    }

    #[test]
    fn test_send_with_noop_when_disabled() {
        let mut buf = Vec::<u8>::new();
        send_with(&mut buf, OscKind::Osc777, &disabled_config(), Kind::Success, "myapp").unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn test_send_with_sanitizes_project_name() {
        let mut buf = Vec::<u8>::new();
        send_with(&mut buf, OscKind::Osc777, &enabled_config(), Kind::Success, "my;project\nweird").unwrap();
        // Semicolon and newline stripped from body.
        assert_eq!(buf, b"\x1b]777;notify;agentbox: build complete;myprojectweird\x07");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --quiet notify::tests::test_send_with 2>&1 | tail -20`
Expected: compile error — `send_with`, `Kind` not defined.

- [ ] **Step 3: Implement the Kind enum and send_with**

Add to the body of `src/notify.rs`, below `write_osc` and above `#[cfg(test)]`:

```rust
/// Which kind of event the notification describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Success,
    Failure,
}

impl Kind {
    fn title(self) -> &'static str {
        match self {
            Kind::Success => "agentbox: build complete",
            Kind::Failure => "agentbox: build failed",
        }
    }
}

/// Testable core: write a notification to `writer`, given an already-detected
/// OSC kind and a project name. No-op when disabled in config. Callers that
/// reach the TTY use `send_success` / `send_failure`.
fn send_with<W: Write>(
    writer: &mut W,
    kind: OscKind,
    config: &crate::config::Config,
    event: Kind,
    project_name: &str,
) -> io::Result<()> {
    if !config.notifications.enabled {
        return Ok(());
    }
    let body = sanitize(project_name);
    write_osc(writer, kind, event.title(), &body)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --quiet notify::tests::test_send_with 2>&1 | tail -20`
Expected: all 4 `test_send_with_*` tests PASS.

- [ ] **Step 5: Implement open_tty and the public send_success / send_failure**

Add imports at top of `src/notify.rs`:

```rust
use std::fs::{File, OpenOptions};
use std::path::Path;
```

Add these functions below `send_with`, above `#[cfg(test)]`:

```rust
/// Open `/dev/tty` for writing. Returns `None` if the controlling terminal
/// is unavailable (non-TTY context, CI, etc.) — callers silently skip.
fn open_tty() -> Option<File> {
    OpenOptions::new().write(true).open(Path::new("/dev/tty")).ok()
}

/// Determine the project name to use as the notification body.
/// Falls back to "agentbox" if the cwd basename can't be resolved.
fn project_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "agentbox".to_string())
}

/// Fire the "agentbox: build complete" notification. No-op if disabled or
/// no supported terminal detected.
pub fn send_success(config: &crate::config::Config) {
    let _ = send_event(config, Kind::Success);
}

/// Fire the "agentbox: build failed" notification. No-op if disabled or
/// no supported terminal detected. Called internally from `run_build` on error.
pub fn send_failure(config: &crate::config::Config) {
    let _ = send_event(config, Kind::Failure);
}

fn send_event(config: &crate::config::Config, event: Kind) -> io::Result<()> {
    if !config.notifications.enabled {
        return Ok(());
    }
    let kind = match detect_terminal(|k| std::env::var(k).ok()) {
        Some(k) => k,
        None => return Ok(()),
    };
    let mut tty = match open_tty() {
        Some(f) => f,
        None => return Ok(()),
    };
    send_with(&mut tty, kind, config, event, &project_name())
}
```

- [ ] **Step 6: Verify build and full tests still pass**

Run: `cargo build 2>&1 | tail -5 && cargo test --quiet 2>&1 | tail -5`
Expected: clean build. Total tests pass count increased by roughly 17 (detect_terminal + sanitize + write_osc + send_with).

---

## Task 7: Notify — run_build() with testable decision core

**Files:**
- Modify: `src/notify.rs`

The core decision — "fire `send_failure` on build errors but NOT on `save_cache` errors" — is important enough to cover with tests. We factor the decision out into a pure `run_build_inner` that takes the three build steps and the failure-notify callback as closures, then write `run_build` as a thin wrapper that injects the real `image::*` calls and `send_failure`. Tests exercise `run_build_inner` with closures that return controlled `Ok`/`Err` without needing to actually invoke `container build`.

- [ ] **Step 1: Write failing tests for run_build_inner**

Append to the `mod tests` block in `src/notify.rs`:

```rust
    use std::cell::Cell;

    #[test]
    fn test_run_build_inner_fires_failure_on_ensure_base_error() {
        let fired = Cell::new(false);
        let result = run_build_inner(
            || Err(anyhow::anyhow!("ensure_base failed")),
            || unreachable!("build should not run if ensure_base fails"),
            || unreachable!("save_cache should not run if ensure_base fails"),
            || fired.set(true),
        );
        assert!(result.is_err());
        assert!(fired.get(), "ensure_base failure must fire send_failure");
    }

    #[test]
    fn test_run_build_inner_fires_failure_on_build_error() {
        let fired = Cell::new(false);
        let result = run_build_inner(
            || Ok(()),
            || Err(anyhow::anyhow!("build failed")),
            || unreachable!("save_cache should not run if build fails"),
            || fired.set(true),
        );
        assert!(result.is_err());
        assert!(fired.get(), "build failure must fire send_failure");
    }

    #[test]
    fn test_run_build_inner_does_not_fire_failure_on_save_cache_error() {
        // This is the important regression guard: save_cache runs after the
        // image has already built successfully. A cache-persistence failure
        // is NOT a "build failed" condition.
        let fired = Cell::new(false);
        let result = run_build_inner(
            || Ok(()),
            || Ok(()),
            || Err(anyhow::anyhow!("save_cache failed")),
            || fired.set(true),
        );
        assert!(result.is_err());
        assert!(!fired.get(), "save_cache failure must NOT fire send_failure");
    }

    #[test]
    fn test_run_build_inner_fires_nothing_on_full_success() {
        let fired = Cell::new(false);
        let result = run_build_inner(
            || Ok(()),
            || Ok(()),
            || Ok(()),
            || fired.set(true),
        );
        assert!(result.is_ok());
        assert!(!fired.get(), "success path must not fire send_failure");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --quiet notify::tests::test_run_build_inner 2>&1 | tail -20`
Expected: compile error — `run_build_inner` not defined.

- [ ] **Step 3: Add run_build_inner and run_build to src/notify.rs**

Add `use anyhow::Result;` to the top of the file (alongside other `use` statements).

Add to the body of `src/notify.rs` below `send_event`, above `#[cfg(test)]`:

```rust
/// Decision core for `run_build`. Split out of `run_build` so the
/// "fire failure on build error but NOT on save_cache error" rule has
/// automated coverage — production code supplies closures that do the
/// real work; tests supply closures that return controlled Ok/Err.
fn run_build_inner(
    ensure_base: impl FnOnce() -> Result<()>,
    build: impl FnOnce() -> Result<()>,
    save_cache: impl FnOnce() -> Result<()>,
    on_failure: impl FnOnce(),
) -> Result<()> {
    let build_result: Result<()> = (|| {
        ensure_base()?;
        build()?;
        Ok(())
    })();

    match build_result {
        Err(e) => {
            on_failure();
            Err(e)
        }
        Ok(()) => {
            // Build succeeded; save_cache failures are not "build failed".
            save_cache()
        }
    }
}

/// Run the standard rebuild sequence (`ensure_base_image` + `build`, then
/// `save_cache`). On a true build failure (either base-image prep or the
/// main image build), fires `send_failure` before propagating the error.
///
/// `save_cache` is treated specially: it's filesystem bookkeeping that runs
/// *after* the image is already built. If it fails (disk full, permissions,
/// etc.) the image actually exists and built correctly; we propagate the
/// error but do NOT fire `send_failure`, because "build failed" would be
/// incorrect — the build succeeded, only the cache metadata didn't persist.
/// The caller's `send_success` also won't fire, because the `?` propagation
/// aborts before reaching it. Net effect on save_cache failure: no
/// notification, error surfaces on stderr. (In practice this is rare; the
/// cache file is tiny.)
///
/// Does not print any user-facing message — callers own pre-build output
/// because different sites need different wording. Does not check
/// `image::needs_build` — callers have already decided a build is required.
pub fn run_build(
    config: &crate::config::Config,
    dockerfile: &str,
    image_tag: &str,
    cache_key: &str,
    no_cache: bool,
    pull: bool,
    verbose: bool,
) -> Result<()> {
    run_build_inner(
        || crate::image::ensure_base_image(dockerfile, no_cache, verbose),
        || crate::image::build(image_tag, dockerfile, no_cache, pull, verbose),
        || crate::image::save_cache(dockerfile, cache_key, &crate::image::cache_dir()),
        || send_failure(config),
    )
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --quiet notify::tests::test_run_build_inner 2>&1 | tail -20`
Expected: all 4 `test_run_build_inner_*` tests PASS.

- [ ] **Step 5: Verify full test suite still passes**

Run: `cargo test --quiet 2>&1 | tail -5`
Expected: all tests pass.

---

## Task 8: Main — wire notify into Stopped path

**Files:**
- Modify: `src/main.rs` around lines 357-381 (the `ContainerStatus::Stopped` branch of `run_session`)

- [ ] **Step 1: Replace the Stopped branch contents**

Edit `src/main.rs`. Locate the `container::ContainerStatus::Stopped =>` arm (at `run_session`). Replace the entire arm body with:

```rust
        container::ContainerStatus::Stopped => {
            let (dockerfile_content, image_tag) =
                image::resolve_dockerfile(&cwd, cli.profile.as_deref(), config)?;
            let cache_key = image_tag.replace(':', "-");
            let did_build = if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
                eprintln!("Image changed, recreating container...");
                container::rm(&name, cli.verbose)?;
                notify::run_build(
                    config,
                    &dockerfile_content,
                    &image_tag,
                    &cache_key,
                    false,
                    false,
                    cli.verbose,
                )?;
                true
            } else {
                false
            };

            if did_build {
                notify::send_success(config);
                create_and_run(
                    &name,
                    &image_tag,
                    &cwd_str,
                    config,
                    mode.clone(),
                    cli.verbose,
                    &cli.mount,
                    bridge_handle.as_ref(),
                )
            } else {
                container::start(&name, cli.verbose)?;
                let env_vars = build_all_env_vars(config, bridge_handle.as_ref());
                container::exec(&name, &mode, &env_vars, cli.verbose)
            }
        }
```

- [ ] **Step 2: Verify build passes**

Run: `cargo build 2>&1 | tail -10`
Expected: clean build.

- [ ] **Step 3: Verify full test suite passes**

Run: `cargo test --quiet 2>&1 | tail -5`
Expected: all previously passing tests still pass.

---

## Task 9: Main — wire notify into NotFound path

**Files:**
- Modify: `src/main.rs` around lines 383-403 (the `ContainerStatus::NotFound` branch of `run_session`)

- [ ] **Step 1: Replace the NotFound branch contents**

Edit `src/main.rs`. Locate the `container::ContainerStatus::NotFound =>` arm. Replace the entire arm body with:

```rust
        container::ContainerStatus::NotFound => {
            let (dockerfile_content, image_tag) =
                image::resolve_dockerfile(&cwd, cli.profile.as_deref(), config)?;
            let cache_key = image_tag.replace(':', "-");
            let did_build = if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
                eprintln!("Building image...");
                notify::run_build(
                    config,
                    &dockerfile_content,
                    &image_tag,
                    &cache_key,
                    false,
                    false,
                    cli.verbose,
                )?;
                true
            } else {
                false
            };

            if did_build {
                notify::send_success(config);
            }
            create_and_run(
                &name,
                &image_tag,
                &cwd_str,
                config,
                mode,
                cli.verbose,
                &cli.mount,
                bridge_handle.as_ref(),
            )
        }
```

- [ ] **Step 2: Verify build passes**

Run: `cargo build 2>&1 | tail -10`
Expected: clean build.

- [ ] **Step 3: Verify full test suite passes**

Run: `cargo test --quiet 2>&1 | tail -5`
Expected: all previously passing tests still pass.

---

## Task 10: Main — wire notify into explicit Build subcommand

**Files:**
- Modify: `src/main.rs` around lines 497-509 (the `Commands::Build` arm in `main`)

- [ ] **Step 1: Replace the Build arm body**

Edit `src/main.rs`. Locate the `Some(Commands::Build { no_cache }) =>` arm. Replace the entire arm body with:

```rust
        Some(Commands::Build { no_cache }) => {
            let config = config::Config::load()?;
            let cwd = std::env::current_dir()?;
            let (dockerfile_content, image_tag) =
                image::resolve_dockerfile(&cwd, cli.profile.as_deref(), &config)?;
            let cache_key = image_tag.replace(':', "-");
            eprintln!("Building {}...", image_tag);
            notify::run_build(
                &config,
                &dockerfile_content,
                &image_tag,
                &cache_key,
                no_cache,
                true,
                cli.verbose,
            )?;
            println!("Built {}", image_tag);
            notify::send_success(&config);
            Ok(())
        }
```

Note: the existing code called `image::ensure_base_image` *before* the `"Building {image_tag}..."` message; the new flow moves that call inside `run_build`, which runs `ensure_base_image` as its first step. Net user-facing order: we print `"Building {image_tag}..."`, then the base-image build (if needed) streams, then the main build streams, then `"Built {image_tag}"`, then the notification fires. Base-image prep is fast compared to the actual build, so this reordering has negligible effect.

- [ ] **Step 2: Verify build passes**

Run: `cargo build 2>&1 | tail -10`
Expected: clean build.

- [ ] **Step 3: Verify full test suite passes**

Run: `cargo test --quiet 2>&1 | tail -5`
Expected: all previously passing tests still pass.

---

## Task 11: README — add Notifications subsection

**Files:**
- Modify: `README.md` — add a new subsection under `## Configuration`, after the existing config example block

- [ ] **Step 1: Insert the Notifications subsection**

Find `## Configuration` in `README.md`. After the existing config TOML example block (ending with the `### Per-project` heading's preceding content), insert:

```markdown
### Terminal notifications

After a long image rebuild, agentbox sends a terminal notification so you can tab away without missing when it's done. Notifications fire only when a rebuild actually runs (not on cached session starts, not for the coding agent's own prompts — those are covered by agent-specific plugins like `agent-notifications`).

Supported terminals (native, no extra install): Ghostty, WezTerm, iTerm2, Kitty. Other terminals silently skip — no visible garbage.

On by default. Disable:

```toml
# ~/.config/agentbox/config.toml
[notifications]
enabled = false
```

Success fires with title `agentbox: build complete`; failure with `agentbox: build failed`. Body is the project directory name.
```

- [ ] **Step 2: Visually confirm the README renders correctly**

Run: `head -180 README.md | tail -60`
Expected: new subsection appears in a coherent place within the Configuration section, heading level `###` matches surrounding subsections.

---

## Task 12: Manual verification checklist

**Files:** none (manual testing only)

This task is a sanity check before declaring the feature done. Run through each scenario and note any unexpected behavior.

- [ ] **Setup:** ensure you are on a supported terminal (Ghostty, WezTerm, iTerm2, or Kitty). `echo $TERM_PROGRAM` should show one of `ghostty`, `iTerm.app`; or `$WEZTERM_EXECUTABLE` / `$KITTY_WINDOW_ID` should be set.

- [ ] **Build the agentbox binary from the worktree:**

Run: `cargo build --release 2>&1 | tail -3 && ls -l target/release/agentbox`
Expected: clean build, binary exists.

- [ ] **Scenario 1 — Explicit build, success:**
  1. Cd to any test project. Add a trivial change to a `agentbox.Dockerfile` (e.g., an extra `RUN echo test` line) OR force a rebuild via `agentbox build --no-cache`.
  2. Run `./target/release/agentbox build`.
  3. While the build runs, switch focus to another app.
  4. **Expected:** system notification appears when the build finishes. Title "agentbox: build complete", body = project name.
  5. Terminal output shows `Built <image_tag>`.

- [ ] **Scenario 2 — Explicit build, failure:**
  1. Introduce a broken `RUN` line in the Dockerfile (e.g., `RUN exit 1`).
  2. Run `./target/release/agentbox build`.
  3. **Expected:** "agentbox: build failed" notification with project-name body. Stdout progress is visible live during the build; the captured stderr is printed as part of the `container build failed:` anyhow error after the non-zero exit.
  4. Run the same with `--verbose` and confirm stderr also streams live during the build.

- [ ] **Scenario 3 — Interactive session rebuild:**
  1. Repair the Dockerfile. Run `./target/release/agentbox rm` to clear any container.
  2. Add a trivial Dockerfile change to force a rebuild.
  3. Run `./target/release/agentbox` (no task — interactive mode).
  4. **Expected:** notification fires right before Claude's prompt appears. User tabs back to see Claude waiting for input.

- [ ] **Scenario 4 — Headless task rebuild:**
  1. Add another trivial Dockerfile change.
  2. Run `./target/release/agentbox "echo hello"`.
  3. **Expected:** notification fires right before the headless task starts. Task then runs and produces output. If `agent-notifications` is installed, a second "task completed" notification arrives later (expected overlap).

- [ ] **Scenario 5 — Shell rebuild, interactive shell:**
  1. Add another Dockerfile change.
  2. Run `./target/release/agentbox shell`.
  3. **Expected:** notification fires right before the bash prompt is shown.

- [ ] **Scenario 6 — Shell rebuild, one-shot command:**
  1. Add another Dockerfile change.
  2. Run `./target/release/agentbox shell -- ls`.
  3. **Expected:** notification fires before `ls` runs; `ls` output follows.

- [ ] **Scenario 7 — Config disabled:**
  1. Edit `~/.config/agentbox/config.toml`: set `[notifications] enabled = false`.
  2. Repeat any of Scenarios 1–6. **Expected:** no notification fires. Everything else works as normal. Revert the config afterwards.

- [ ] **Scenario 8 — Unsupported terminal:**
  1. In a supported terminal, run: `TERM_PROGRAM=Apple_Terminal WEZTERM_EXECUTABLE= ITERM_SESSION_ID= KITTY_WINDOW_ID= ./target/release/agentbox build`.
  2. **Expected:** no notification; no garbage bytes (no `\x1b]...` visible) in terminal output. The build otherwise proceeds normally.

- [ ] **Scenario 9 — Output redirected:**
  1. Add a Dockerfile change. Run `./target/release/agentbox build > /tmp/build.log 2>&1`.
  2. **Expected:** notification still fires (it writes to `/dev/tty`, not stdout). `/tmp/build.log` contains the build output.

- [ ] **Scenario 10 — Cache hit, session commands (no rebuild):**
  1. Immediately after a successful session rebuild from Scenario 3, 4, 5, or 6 — with the same Dockerfile — rerun the same command.
  2. **Expected:** no notification fires (the `needs_build` check returned false; no build happened).
  3. *Not applicable to `agentbox build`:* the explicit build subcommand unconditionally rebuilds by design and will always fire a notification — there is no "cache hit" path for it.

- [ ] **Scenario 11 — agentbox config init:**
  1. Back up `~/.config/agentbox/config.toml`, then delete it.
  2. Run `./target/release/agentbox config init`.
  3. **Expected:** new config file exists with a `[notifications]` section, its doc comments, and `enabled = true`. Restore the backup afterwards.

- [ ] **Final sanity check:** run `cargo test --quiet 2>&1 | tail -5` one more time. Expected: all tests pass.

---

## Notes for the implementing engineer

- **TDD discipline:** for every code-producing step that has a matching test step, write the test *first* and confirm it fails before writing the implementation. This is cheap insurance against writing unused code or skipping behaviors.
- **No new dependencies:** everything is stdlib. If you find yourself wanting to add a crate, stop and re-read the design doc — the zero-deps property is intentional.
- **Don't test `/dev/tty` directly:** `open_tty()` is a thin wrapper over `OpenOptions::open` and has no logic worth unit-testing in isolation. Runtime behavior is covered by the manual verification in Task 12.
- **`run_build` has no unit test:** it composes already-tested functions (`image::*` and `send_failure`). Attempting to test it would require shimming out the `container` CLI, which isn't worth the complexity. Integration is verified by Scenarios 1 and 2.
- **Line numbers in this plan are approximate.** As you edit earlier tasks, later line numbers shift. Use the symbol references (`ContainerStatus::Stopped`, `Commands::Build`, etc.) as anchors.
