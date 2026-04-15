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
}
