//! Coding agent abstraction. Owns per-agent CLI knowledge
//! (binary name, entrypoint dispatch token, argv ordering).

use anyhow::{bail, Result};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgent {
    Claude,
    Codex,
}

impl CodingAgent {
    pub fn binary(&self) -> &'static str {
        match self {
            CodingAgent::Claude => "claude",
            CodingAgent::Codex => "codex",
        }
    }

    pub fn entrypoint_arg(&self) -> &'static str {
        match self {
            CodingAgent::Claude => "--claude",
            CodingAgent::Codex => "--codex",
        }
    }

    pub fn config_key(&self) -> &'static str {
        match self {
            CodingAgent::Claude => "claude",
            CodingAgent::Codex => "codex",
        }
    }

    /// Build the argv tokens that follow the binary name.
    ///
    /// Per-agent CLI shape:
    /// - Claude interactive: `<flags>`
    /// - Claude headless:    `<flags> -p <task>`
    /// - Codex  interactive: `<flags>`
    /// - Codex  headless:    `exec <flags> <task>`  (subcommand-first,
    ///                       flags after the subcommand per codex's docs)
    pub fn invocation(&self, flags: &[String], task: Option<&str>) -> Vec<String> {
        match (self, task) {
            (CodingAgent::Claude, None) => flags.to_vec(),
            (CodingAgent::Claude, Some(t)) => {
                let mut v = flags.to_vec();
                v.push("-p".to_string());
                v.push(t.to_string());
                v
            }
            (CodingAgent::Codex, None) => flags.to_vec(),
            (CodingAgent::Codex, Some(t)) => {
                let mut v = vec!["exec".to_string()];
                v.extend(flags.iter().cloned());
                v.push(t.to_string());
                v
            }
        }
    }
}

impl FromStr for CodingAgent {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "claude" => Ok(CodingAgent::Claude),
            "codex" => Ok(CodingAgent::Codex),
            other => bail!(
                "unknown agent {:?}; expected \"claude\" or \"codex\"",
                other
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_roundtrips_claude_and_codex() {
        assert_eq!("claude".parse::<CodingAgent>().unwrap(), CodingAgent::Claude);
        assert_eq!("codex".parse::<CodingAgent>().unwrap(), CodingAgent::Codex);
    }

    #[test]
    fn from_str_rejects_unknown_strings() {
        assert!("gemini".parse::<CodingAgent>().is_err());
        assert!("".parse::<CodingAgent>().is_err());
        assert!("Claude".parse::<CodingAgent>().is_err()); // case-sensitive
    }

    #[test]
    fn binary_returns_expected_names() {
        assert_eq!(CodingAgent::Claude.binary(), "claude");
        assert_eq!(CodingAgent::Codex.binary(), "codex");
    }

    #[test]
    fn entrypoint_arg_returns_flag_form() {
        assert_eq!(CodingAgent::Claude.entrypoint_arg(), "--claude");
        assert_eq!(CodingAgent::Codex.entrypoint_arg(), "--codex");
    }

    #[test]
    fn config_key_matches_toml_section_name() {
        assert_eq!(CodingAgent::Claude.config_key(), "claude");
        assert_eq!(CodingAgent::Codex.config_key(), "codex");
    }

    #[test]
    fn invocation_claude_interactive_returns_flags_only() {
        let flags = vec!["--model".to_string(), "sonnet".to_string()];
        let got = CodingAgent::Claude.invocation(&flags, None);
        assert_eq!(got, vec!["--model", "sonnet"]);
    }

    #[test]
    fn invocation_claude_headless_appends_p_and_task() {
        let flags = vec!["--dangerously-skip-permissions".to_string()];
        let got = CodingAgent::Claude.invocation(&flags, Some("fix tests"));
        assert_eq!(
            got,
            vec!["--dangerously-skip-permissions", "-p", "fix tests"]
        );
    }

    #[test]
    fn invocation_codex_interactive_returns_flags_only() {
        let flags = vec!["--dangerously-bypass-approvals-and-sandbox".to_string()];
        let got = CodingAgent::Codex.invocation(&flags, None);
        assert_eq!(got, vec!["--dangerously-bypass-approvals-and-sandbox"]);
    }

    #[test]
    fn invocation_codex_headless_places_exec_before_flags_and_task_last() {
        let flags = vec!["--dangerously-bypass-approvals-and-sandbox".to_string()];
        let got = CodingAgent::Codex.invocation(&flags, Some("fix tests"));
        assert_eq!(
            got,
            vec![
                "exec",
                "--dangerously-bypass-approvals-and-sandbox",
                "fix tests"
            ]
        );
    }

    #[test]
    fn invocation_codex_headless_empty_flags() {
        let got = CodingAgent::Codex.invocation(&[], Some("do thing"));
        assert_eq!(got, vec!["exec", "do thing"]);
    }
}
