//! Terminal OSC notifications after image rebuilds.
//!
//! See `wiki/2026-04-15-build-notifications-design.md` for rationale.

use anyhow::Result;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_map<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        let map: HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k: &str| map.get(k).cloned()
    }

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
}
