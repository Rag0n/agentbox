# Codex integration implementation plan

> **For agentic workers:** REQUIRED: Use workflow:subagent-driven-development to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add OpenAI Codex CLI as a peer to Claude Code in agentbox, with parallel subcommands (`agentbox claude` / `agentbox codex`), a combined container image, config-driven default flags, and setup wizard extensions.

**Architecture:** A new `CodingAgent` enum (Claude | Codex) owns all agent-specific knowledge (binary name, entrypoint dispatch token, invocation token sequence). `RunMode::Claude` becomes `RunMode::Agent { agent, task, cli_flags }`. The hardcoded `--dangerously-skip-permissions` flag moves out of Rust/entrypoint into the `[cli.claude]` config defaults. `entrypoint.sh` switches from one ad-hoc `--shell` check to a proper first-arg dispatch, and exports `PATH` so both binaries resolve from `~/.local/bin`. Setup grows two new steps (default-agent selection and codex credential-store check) and gains a codex-first short-circuit so picking Codex as default doesn't block on missing Claude auth.

**Tech Stack:** Rust 2021 (clap, serde, toml, toml_edit, anyhow), bash entrypoint, Apple Container CLI, Debian bookworm-slim base image.

**Reference spec:** `wiki/2026-04-15-codex-integration-design.md`

---

## File structure

**Created:**
- `src/agent.rs` — `CodingAgent` enum, `FromStr`, `binary()`, `entrypoint_arg()`, `config_key()`, `invocation(flags, task)`.

**Modified:**
- `src/main.rs` — new `Claude` / `Codex` clap subcommands, `~/.codex` auto-mount, bare-command arm uses `resolve_default_agent()`, `RunMode::Agent` construction sites, `mod agent;` declaration.
- `src/container.rs` — `RunMode::Claude` → `RunMode::Agent`, `to_run_args` prepends `agent.entrypoint_arg()`, `build_exec_args` uses `agent.invocation()`. Test suite updated.
- `src/config.rs` — `default_agent: Option<String>` field, `resolve_default_agent() -> Result<CodingAgent>`, expanded `init_template()`.
- `src/setup.rs` — new `check_default_agent`, `prompt_default_agent`, `ensure_default_agent_in_config`, `check_codex_authentication`; `check_authentication` short-circuit when `default_agent = codex`; pipeline grows to 6 checks.
- `resources/Dockerfile.default` — codex install step.
- `resources/entrypoint.sh` — `export PATH`, first-arg dispatch (`--claude` | `--codex` | `--shell`).
- `README.md` — Codex parity docs, breaking-change callout, mount table row, config example updates.

**Not modified:** `src/bridge.rs`, `src/git.rs`, `src/hostexec.rs`, `src/image.rs`, `src/status.rs`, `Cargo.toml`. (Entrypoint hashing in `image.rs` already handles the entrypoint change via the shell-command work at `image.rs:101`.)

---

## Task 1: `CodingAgent` enum in `src/agent.rs`

**Files:**
- Create: `src/agent.rs`
- Modify: `src/main.rs:4` (add `mod agent;`)
- Test: tests live inside `src/agent.rs` behind `#[cfg(test)]`

- [ ] **Step 1: Write the failing tests**

Create `src/agent.rs` with tests and a stub enum so the file compiles:

```rust
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
        unimplemented!()
    }
    pub fn entrypoint_arg(&self) -> &'static str {
        unimplemented!()
    }
    pub fn config_key(&self) -> &'static str {
        unimplemented!()
    }
    pub fn invocation(&self, _flags: &[String], _task: Option<&str>) -> Vec<String> {
        unimplemented!()
    }
}

impl FromStr for CodingAgent {
    type Err = anyhow::Error;
    fn from_str(_s: &str) -> Result<Self> {
        unimplemented!()
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
```

Declare the module from `src/main.rs` so tests get compiled. Add this line after the existing `mod bridge;` block at `src/main.rs:4`:

```rust
mod agent;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib agent::tests -- --test-threads=1 2>&1 | tail -40`

Expected: All new tests fail (panicked at "not implemented") because the methods use `unimplemented!()`.

- [ ] **Step 3: Implement the methods**

Replace the stub bodies in `src/agent.rs`:

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib agent:: -- --test-threads=1 2>&1 | tail -20`

Expected: `test result: ok. 9 passed; 0 failed`.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test 2>&1 | tail -5`

Expected: All tests pass (existing 201 + 9 new = 210).

---

## Task 2: `default_agent` field + `resolve_default_agent()` in `src/config.rs`

**Files:**
- Modify: `src/config.rs` (add field, add method, update tests)

- [ ] **Step 1: Write the failing tests**

Append to the `mod tests` block in `src/config.rs` (after the existing `test_cli_config_omitted` test):

```rust
    #[test]
    fn test_parse_default_agent_claude() {
        let toml_str = r#"default_agent = "claude""#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_agent.as_deref(), Some("claude"));
    }

    #[test]
    fn test_parse_default_agent_codex() {
        let toml_str = r#"default_agent = "codex""#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_agent.as_deref(), Some("codex"));
    }

    #[test]
    fn test_parse_default_agent_invalid_still_parses_as_string() {
        // Invalid values must survive TOML parsing so setup can repair them
        // interactively; validation happens in resolve_default_agent().
        let toml_str = r#"default_agent = "gemini""#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_agent.as_deref(), Some("gemini"));
    }

    #[test]
    fn test_default_agent_omitted_is_none() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.default_agent.is_none());
    }

    #[test]
    fn test_resolve_default_agent_none_falls_back_to_claude() {
        use crate::agent::CodingAgent;
        let config = Config::default();
        assert_eq!(
            config.resolve_default_agent().unwrap(),
            CodingAgent::Claude
        );
    }

    #[test]
    fn test_resolve_default_agent_claude_and_codex() {
        use crate::agent::CodingAgent;
        let mut config = Config::default();
        config.default_agent = Some("claude".into());
        assert_eq!(
            config.resolve_default_agent().unwrap(),
            CodingAgent::Claude
        );
        config.default_agent = Some("codex".into());
        assert_eq!(
            config.resolve_default_agent().unwrap(),
            CodingAgent::Codex
        );
    }

    #[test]
    fn test_resolve_default_agent_invalid_errors_with_useful_message() {
        let mut config = Config::default();
        config.default_agent = Some("gemini".into());
        let err = config.resolve_default_agent().unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("gemini"), "error message should mention the bad value; got: {msg}");
        assert!(
            msg.contains("claude") && msg.contains("codex"),
            "error message should list valid options; got: {msg}"
        );
    }
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo test --lib config::tests::test_parse_default_agent_claude 2>&1 | tail -30`

Expected: compile errors ("no field `default_agent`", "no method `resolve_default_agent`").

- [ ] **Step 3: Implement the field and method**

In `src/config.rs`, add the field to the `Config` struct and initialize it in the `Default` impl:

```rust
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub cpus: Option<usize>,
    pub memory: String,
    pub dockerfile: Option<PathBuf>,
    pub default_agent: Option<String>,        // NEW
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub profiles: HashMap<String, Profile>,
    #[serde(default)]
    pub volumes: Vec<String>,
    #[serde(default)]
    pub bridge: BridgeConfig,
    #[serde(default)]
    pub cli: HashMap<String, CliConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cpus: None,
            memory: "8G".to_string(),
            dockerfile: None,
            default_agent: None,              // NEW
            env: HashMap::new(),
            profiles: HashMap::new(),
            volumes: Vec::new(),
            bridge: BridgeConfig::default(),
            cli: HashMap::new(),
        }
    }
}
```

Add the helper method inside `impl Config`:

```rust
    /// Resolve `default_agent` into a `CodingAgent`. Missing value falls
    /// back to `Claude`. Unknown strings produce an error with a useful
    /// message; the caller (runtime or setup) decides how to surface it.
    pub fn resolve_default_agent(&self) -> anyhow::Result<crate::agent::CodingAgent> {
        use anyhow::Context;
        use std::str::FromStr;

        match self.default_agent.as_deref() {
            None => Ok(crate::agent::CodingAgent::Claude),
            Some(s) => crate::agent::CodingAgent::from_str(s).with_context(|| {
                format!(
                    "invalid default_agent = {:?}; expected \"claude\" or \"codex\"",
                    s
                )
            }),
        }
    }
```

- [ ] **Step 4: Run the new tests to verify they pass**

Run: `cargo test --lib config::tests -- --test-threads=1 2>&1 | tail -20`

Expected: all config tests pass including the 7 new ones.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test 2>&1 | tail -5`

Expected: 217 passed, 0 failed.

---

## Task 3: Refactor `RunMode::Claude` → `RunMode::Agent` in `src/container.rs`

**Files:**
- Modify: `src/container.rs` (enum, `to_run_args`, `build_exec_args`, `is_interactive`, all tests)
- Modify: `src/main.rs` (all construction sites for `RunMode::Claude`)

This refactor touches many call sites at once because the type change forces all callers to update. Break it into stages: (1) add the new variant alongside the old, (2) migrate all sites, (3) remove the old variant.

- [ ] **Step 1: Write the failing codex tests**

Add these tests to the `mod tests` block in `src/container.rs` (near the existing `test_build_run_args`):

```rust
    #[test]
    fn test_run_args_agent_codex_interactive_no_task() {
        use crate::agent::CodingAgent;
        let opts = RunOpts {
            name: "agentbox-app-abc123".into(),
            image: "agentbox:default".into(),
            workdir: "/Users/alex/Dev/app".into(),
            cpus: 4,
            memory: "8G".into(),
            env_vars: vec![],
            volumes: vec![],
            mode: RunMode::Agent {
                agent: CodingAgent::Codex,
                task: None,
                cli_flags: vec!["--dangerously-bypass-approvals-and-sandbox".into()],
            },
        };
        let args = opts.to_run_args();
        let image_idx = args.iter().position(|a| a == "agentbox:default").unwrap();
        // After the image: --codex, then flags. No "exec".
        assert_eq!(args[image_idx + 1], "--codex");
        assert_eq!(args[image_idx + 2], "--dangerously-bypass-approvals-and-sandbox");
        assert!(!args[image_idx + 1..].contains(&"exec".to_string()));
        assert!(args.contains(&"--interactive".to_string()));
        assert!(args.contains(&"--tty".to_string()));
    }

    #[test]
    fn test_run_args_agent_codex_headless_puts_exec_before_flags() {
        use crate::agent::CodingAgent;
        let opts = RunOpts {
            name: "agentbox-app-abc123".into(),
            image: "agentbox:default".into(),
            workdir: "/Users/alex/Dev/app".into(),
            cpus: 4,
            memory: "8G".into(),
            env_vars: vec![],
            volumes: vec![],
            mode: RunMode::Agent {
                agent: CodingAgent::Codex,
                task: Some("fix tests".into()),
                cli_flags: vec!["--dangerously-bypass-approvals-and-sandbox".into()],
            },
        };
        let args = opts.to_run_args();
        let image_idx = args.iter().position(|a| a == "agentbox:default").unwrap();
        assert_eq!(args[image_idx + 1], "--codex");
        assert_eq!(args[image_idx + 2], "exec");
        assert_eq!(
            args[image_idx + 3],
            "--dangerously-bypass-approvals-and-sandbox"
        );
        assert_eq!(args[image_idx + 4], "fix tests");
        // Headless: no TTY
        assert!(!args.contains(&"--tty".to_string()));
    }

    #[test]
    fn test_run_args_agent_claude_headless_preserves_legacy_ordering() {
        use crate::agent::CodingAgent;
        let opts = RunOpts {
            name: "agentbox-app-abc123".into(),
            image: "agentbox:default".into(),
            workdir: "/Users/alex/Dev/app".into(),
            cpus: 4,
            memory: "8G".into(),
            env_vars: vec![],
            volumes: vec![],
            mode: RunMode::Agent {
                agent: CodingAgent::Claude,
                task: Some("fix tests".into()),
                cli_flags: vec!["--dangerously-skip-permissions".into()],
            },
        };
        let args = opts.to_run_args();
        let image_idx = args.iter().position(|a| a == "agentbox:default").unwrap();
        assert_eq!(args[image_idx + 1], "--claude");
        assert_eq!(args[image_idx + 2], "--dangerously-skip-permissions");
        assert_eq!(args[image_idx + 3], "-p");
        assert_eq!(args[image_idx + 4], "fix tests");
    }

    #[test]
    fn test_exec_args_agent_codex_headless() {
        use crate::agent::CodingAgent;
        let env_vars: Vec<(String, String)> = vec![];
        let mode = RunMode::Agent {
            agent: CodingAgent::Codex,
            task: Some("fix tests".into()),
            cli_flags: vec!["--dangerously-bypass-approvals-and-sandbox".into()],
        };
        let args = build_exec_args("mycontainer", &mode, &env_vars);
        let cmd = args.last().unwrap();
        assert!(cmd.contains("codex"));
        // `exec` subcommand must come BEFORE flags
        let exec_pos = cmd.find("'exec'").expect("expected 'exec' token in bash cmd");
        let flag_pos = cmd
            .find("'--dangerously-bypass-approvals-and-sandbox'")
            .expect("expected bypass flag");
        assert!(
            exec_pos < flag_pos,
            "exec should precede flags in codex headless; got: {cmd}"
        );
        assert!(cmd.contains("'fix tests'"));
        // Must NOT contain "-p" (that's claude's headless syntax)
        assert!(!cmd.contains("-p"));
    }

    #[test]
    fn test_exec_args_agent_codex_interactive() {
        use crate::agent::CodingAgent;
        let env_vars: Vec<(String, String)> = vec![];
        let mode = RunMode::Agent {
            agent: CodingAgent::Codex,
            task: None,
            cli_flags: vec![],
        };
        let args = build_exec_args("mycontainer", &mode, &env_vars);
        let cmd = args.last().unwrap();
        assert!(cmd.contains("codex"));
        assert!(!cmd.contains("'exec'"));
        assert!(args.contains(&"--interactive".to_string()));
        assert!(args.contains(&"--tty".to_string()));
    }
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo test --lib container::tests::test_run_args_agent_codex_interactive_no_task 2>&1 | tail -20`

Expected: compile errors (no `RunMode::Agent` variant).

- [ ] **Step 3: Update the `RunMode` enum and all call sites in `src/container.rs`**

Replace the `RunMode` enum at `src/container.rs:26-35`:

```rust
#[derive(Debug, Clone)]
pub enum RunMode {
    Agent {
        agent: crate::agent::CodingAgent,
        task: Option<String>,
        cli_flags: Vec<String>,
    },
    Shell {
        cmd: Vec<String>,
    },
}

impl RunMode {
    pub fn is_interactive(&self) -> bool {
        match self {
            RunMode::Agent { task, .. } => task.is_none(),
            RunMode::Shell { cmd } => cmd.is_empty(),
        }
    }
}
```

Update the `Agent`-arm of `RunOpts::to_run_args` at `src/container.rs:84-99`:

```rust
        match &self.mode {
            RunMode::Agent { agent, task, cli_flags } => {
                args.push(agent.entrypoint_arg().to_string());
                args.extend(agent.invocation(cli_flags, task.as_deref()));
            }
            RunMode::Shell { cmd } => {
                args.push("--shell".into());
                for token in cmd {
                    args.push(token.clone());
                }
            }
        }
```

Update the `Agent`-arm of `build_exec_args` at `src/container.rs:289-300`. Replace the existing `RunMode::Claude` match arm with:

```rust
        RunMode::Agent { agent, task, cli_flags } => {
            let mut cmd = setup;
            cmd.push_str("exec ");
            cmd.push_str(agent.binary());
            for tok in agent.invocation(cli_flags, task.as_deref()) {
                cmd.push_str(&format!(" '{}'", tok.replace('\'', "'\\''")));
            }
            args.push(cmd);
        }
```

(The change: we now quote every token uniformly via `invocation()`, instead of the old behavior that special-cased `--dangerously-skip-permissions` unquoted and `-p 'task'` separately. The extra quoting around `--flag` tokens is harmless — bash unwraps single-quoted literals the same way.)

- [ ] **Step 4: Update every existing `RunMode::Claude { ... }` site**

Migrate all call sites (compile errors will guide you; this is the exhaustive list):

`src/container.rs`:
- `test_build_run_args` at `src/container.rs:442` → `RunMode::Agent { agent: CodingAgent::Claude, task: None, cli_flags: vec![] }`
- `test_exec_args_with_env_vars` at `src/container.rs:524` → same pattern, `task: Some("fix tests".into())`
- Every other test using `RunMode::Claude { ... }` (there are ~15–20 such sites — search `RunMode::Claude` in the file and replace each with the `Agent` form using `CodingAgent::Claude`).

Add `use crate::agent::CodingAgent;` at the top of the `#[cfg(test)] mod tests` block in `src/container.rs`.

`src/main.rs`:
- At `src/main.rs:442` (in `test_build_env_vars_defaults`-adjacent build-opts test): same migration.
- `src/main.rs:490`: `RunMode::Claude { task: task_str, cli_flags }` → `RunMode::Agent { agent: CodingAgent::Claude, task: task_str, cli_flags }`. Add `use crate::agent::CodingAgent;` at the top of `main.rs` if not already there from task 1.
- Every other `RunMode::Claude { ... }` in `main.rs` tests — search and replace.

Fix the existing claude-specific test assertion in `src/container.rs:537`:

```rust
        // Old assertion checked unquoted "claude --dangerously-skip-permissions"
        // and "-p 'fix tests'". With the uniform-quoting change, update:
        assert!(cmd.contains("exec claude"));
        assert!(cmd.contains("'--dangerously-skip-permissions'"));
        assert!(cmd.contains("'-p'"));
        assert!(cmd.contains("'fix tests'"));
```

- [ ] **Step 5: Run the full test suite**

Run: `cargo test 2>&1 | tail -10`

Expected: 222 passed (210 from before + 5 new codex tests, minus any that needed assertion updates but pass after those updates). If any claude-specific assertion fails, it's a leftover from the uniform-quoting change — update the string expectation and re-run.

---

## Task 4: Update `entrypoint.sh` for first-arg dispatch and `PATH` export

**Files:**
- Modify: `resources/entrypoint.sh`

Note: `image.rs:101` already hashes `entrypoint.sh` into the rebuild cache, so users' images will rebuild on next invocation automatically.

- [ ] **Step 1: Rewrite the entrypoint**

Replace the entire contents of `resources/entrypoint.sh`:

```bash
#!/bin/bash
set -e

export PATH="/home/user/.local/bin:$PATH"

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
    mkdir -p /home/user/.local/bin
    for cmd in $HOSTEXEC_COMMANDS; do
        ln -sf /usr/local/bin/hostexec "/home/user/.local/bin/$cmd" 2>/dev/null || true
    done
fi

# Set up command_not_found fallback if enabled
if [ "$HOSTEXEC_FORWARD_NOT_FOUND" = "true" ]; then
    echo 'command_not_found_handle() { /usr/local/bin/hostexec "$@"; }' | sudo tee -a /etc/bash.bashrc > /dev/null
fi

AGENT="$1"; shift || true
case "$AGENT" in
  --claude) exec claude "$@" ;;
  --codex)  exec codex  "$@" ;;
  --shell)
    if [ $# -eq 0 ]; then
        exec bash -l
    else
        # Pass tokens as positional args so bash receives distinct words.
        exec bash -lc 'exec "$@"' bash "$@"
    fi
    ;;
  *)
    echo "agentbox entrypoint: unknown agent '$AGENT'" >&2
    exit 2
    ;;
esac
```

Two key changes vs the pre-existing script: (1) `export PATH=...` at the top makes `~/.local/bin` globally discoverable (no `bash -l` needed). (2) The ad-hoc `[ "$1" = "--shell" ]` branch becomes a proper `case` that also dispatches `--claude` and `--codex`.

The trailing hardcoded `exec claude --dangerously-skip-permissions "$@"` line is gone; the caller is now responsible for passing that flag via `cli_flags`.

- [ ] **Step 2: Run shellcheck on the entrypoint**

Run: `shellcheck resources/entrypoint.sh || true`

Expected: no new warnings beyond any that the prior version had. (`shellcheck` may warn about `shift || true`; that's intentional since `set -e` would otherwise exit when no args are present.)

- [ ] **Step 3: Run the full test suite (cache-invalidation sanity check)**

Run: `cargo test --lib image::tests 2>&1 | tail -10`

Expected: image tests pass; `test_needs_build_uses_cache_input_for_default_dockerfile` passes because the entrypoint change naturally invalidates the cache.

---

## Task 5: Add codex install to `resources/Dockerfile.default`

**Files:**
- Modify: `resources/Dockerfile.default`

- [ ] **Step 1: Insert the codex install step**

Find this line near the bottom of `resources/Dockerfile.default`:

```dockerfile
RUN curl -fsSL https://claude.ai/install.sh | bash
```

Add a codex install step immediately after it, before the `ENTRYPOINT` line:

```dockerfile
RUN curl -fsSL https://claude.ai/install.sh | bash

# Codex CLI (OpenAI). GitHub release tarball contains a single binary
# named codex-<triple>; rename to plain codex under ~/.local/bin to match
# the claude install location.
RUN ARCH=$(dpkg --print-architecture) \
 && case "$ARCH" in \
      amd64) TRIPLE="x86_64-unknown-linux-musl"  ;; \
      arm64) TRIPLE="aarch64-unknown-linux-musl" ;; \
      *) echo "unsupported arch: $ARCH" >&2; exit 1 ;; \
    esac \
 && mkdir -p /home/user/.local/bin \
 && curl -fsSL "https://github.com/openai/codex/releases/latest/download/codex-${TRIPLE}.tar.gz" \
  | tar xz -C /tmp \
 && mv "/tmp/codex-${TRIPLE}" /home/user/.local/bin/codex \
 && chmod +x /home/user/.local/bin/codex
```

- [ ] **Step 2: Run the full test suite**

Run: `cargo test 2>&1 | tail -5`

Expected: all tests still pass. The only file change is the Dockerfile; image building is not exercised in unit tests (manual smoke test covers it in Task 16).

---

## Task 6: Add `Claude` and `Codex` clap subcommands in `src/main.rs`

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Write the failing tests**

Append to the existing `mod tests` block in `src/main.rs`:

```rust
    #[test]
    fn test_claude_subcommand_no_task() {
        let cli = Cli::try_parse_from(["agentbox", "claude"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Claude { ref task }) if task.is_empty()
        ));
    }

    #[test]
    fn test_claude_subcommand_with_task() {
        let cli =
            Cli::try_parse_from(["agentbox", "claude", "fix", "the", "tests"]).unwrap();
        match cli.command {
            Some(Commands::Claude { task }) => {
                assert_eq!(task, vec!["fix", "the", "tests"]);
            }
            _ => panic!("expected Claude subcommand"),
        }
    }

    #[test]
    fn test_codex_subcommand_no_task() {
        let cli = Cli::try_parse_from(["agentbox", "codex"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Codex { ref task }) if task.is_empty()
        ));
    }

    #[test]
    fn test_codex_subcommand_with_task() {
        let cli = Cli::try_parse_from(["agentbox", "codex", "fix tests"]).unwrap();
        match cli.command {
            Some(Commands::Codex { task }) => {
                assert_eq!(task, vec!["fix tests"]);
            }
            _ => panic!("expected Codex subcommand"),
        }
    }

    #[test]
    fn test_codex_subcommand_with_passthrough_flags() {
        let raw_args: Vec<String> = vec![
            "agentbox".into(),
            "codex".into(),
            "fix".into(),
            "--".into(),
            "-c".into(),
            "model_reasoning_effort=high".into(),
        ];
        let (agentbox_args, passthrough) = split_at_double_dash(raw_args);
        let cli = Cli::try_parse_from(agentbox_args).unwrap();
        assert!(matches!(cli.command, Some(Commands::Codex { ref task }) if task == &vec!["fix"]));
        assert_eq!(
            passthrough,
            vec!["-c", "model_reasoning_effort=high"]
        );
    }
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo test --lib tests::test_codex_subcommand_no_task 2>&1 | tail -15`

Expected: compile error — no `Commands::Claude` / `Commands::Codex` variants.

- [ ] **Step 3: Add the subcommands to the `Commands` enum**

Update the `Commands` enum at `src/main.rs:40-68`:

```rust
#[derive(Subcommand)]
enum Commands {
    /// Remove containers (by name, current project, or --all)
    Rm {
        /// Container names to remove
        names: Vec<String>,
        /// Remove all agentbox containers
        #[arg(long)]
        all: bool,
    },
    /// Show rich container status (CPU, memory, project, sessions)
    #[command(alias = "ls")]
    Status,
    /// Force rebuild the container image (--no-cache for clean build)
    Build {
        /// Do not use cache when building the image
        #[arg(long)]
        no_cache: bool,
    },
    /// Configuration management
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Run the interactive setup wizard
    Setup,
    /// Open a bash shell in the container (no Claude)
    Shell,
    /// Run Claude Code (explicit; default when no subcommand is used)
    Claude {
        /// Task to run in headless mode
        #[arg(trailing_var_arg = true)]
        task: Vec<String>,
    },
    /// Run OpenAI Codex CLI
    Codex {
        /// Task to run in headless mode
        #[arg(trailing_var_arg = true)]
        task: Vec<String>,
    },
}
```

Add the corresponding match arms in the `main()` match block — insert after `Some(Commands::Shell) => { ... }` and before the `None => { ... }` arm at `src/main.rs:479`:

```rust
        Some(Commands::Claude { task }) => {
            let config = config::Config::load()?;
            run_agent(
                &cli,
                &config,
                agent::CodingAgent::Claude,
                task,
                passthrough_flags,
            )
        }
        Some(Commands::Codex { task }) => {
            let config = config::Config::load()?;
            run_agent(
                &cli,
                &config,
                agent::CodingAgent::Codex,
                task,
                passthrough_flags,
            )
        }
```

Extract a `run_agent` helper near `run_session` in `src/main.rs`:

```rust
fn run_agent(
    cli: &Cli,
    config: &config::Config,
    agent: agent::CodingAgent,
    task_tokens: Vec<String>,
    passthrough_flags: Vec<String>,
) -> Result<()> {
    let task_str = if task_tokens.is_empty() {
        None
    } else {
        Some(task_tokens.join(" "))
    };

    let mut cli_flags: Vec<String> = config.cli_flags(agent.config_key()).to_vec();
    cli_flags.extend(passthrough_flags);

    let mode = container::RunMode::Agent {
        agent,
        task: task_str,
        cli_flags,
    };

    let code = run_session(cli, config, mode)?;
    if code != 0 {
        bail!("container exited with status {}", code);
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib _subcommand 2>&1 | tail -25`

Expected: all 5 new subcommand tests pass (output includes existing `rm`/`status`/`shell` subcommand tests, which also pass).

- [ ] **Step 5: Run the full test suite**

Run: `cargo test 2>&1 | tail -5`

Expected: 227 passed.

---

## Task 7: Wire the bare-`agentbox` arm to `resolve_default_agent()`

**Files:**
- Modify: `src/main.rs` (the `None => { ... }` match arm)

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `src/main.rs`:

```rust
    #[test]
    fn test_bare_agentbox_uses_config_default_agent() {
        use crate::agent::CodingAgent;
        // default_agent omitted → Claude
        let c = config::Config::default();
        assert_eq!(c.resolve_default_agent().unwrap(), CodingAgent::Claude);

        // default_agent = codex → Codex
        let mut c = config::Config::default();
        c.default_agent = Some("codex".into());
        assert_eq!(c.resolve_default_agent().unwrap(), CodingAgent::Codex);
    }
```

- [ ] **Step 2: Run it to verify it passes already** (task 2 already implemented this)

Run: `cargo test --lib tests::test_bare_agentbox_uses_config_default_agent 2>&1 | tail -5`

Expected: PASS (this test is confirming behavior we wired in task 2; it documents the contract the bare-arm relies on).

- [ ] **Step 3: Update the `None => { ... }` arm**

Replace the bare-command match arm at `src/main.rs:479-501` with:

```rust
        None => {
            let config = config::Config::load()?;
            let agent = config.resolve_default_agent()?;
            run_agent(&cli, &config, agent, cli.task.clone(), passthrough_flags)
        }
```

The helper `run_agent` (added in task 6) centralizes all the flag assembly and RunMode construction. The old inline logic is deleted.

- [ ] **Step 4: Run the full test suite**

Run: `cargo test 2>&1 | tail -5`

Expected: 228 passed.

---

## Task 8: Mount `~/.codex` in `src/main.rs`

**Files:**
- Modify: `src/main.rs` (`create_and_run`)

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/main.rs`:

```rust
    #[test]
    fn test_codex_mount_added_by_create_and_run_pipeline() {
        // We verify the mount path assembly, not the full `container run` call.
        // A pure helper keeps this testable without I/O.
        let home = dirs::home_dir().unwrap();
        let codex_mount = build_codex_mount(&home);
        let expected = format!(
            "{}:/home/user/.codex",
            home.join(".codex").display()
        );
        assert_eq!(codex_mount, expected);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib tests::test_codex_mount_added_by_create_and_run_pipeline 2>&1 | tail -10`

Expected: compile error — `build_codex_mount` not found.

- [ ] **Step 3: Add the helper and invoke it**

Add above `create_and_run` in `src/main.rs`:

```rust
fn build_codex_mount(home: &std::path::Path) -> String {
    format!("{}:/home/user/.codex", home.join(".codex").display())
}
```

Update the mount-assembly block in `create_and_run` (around `src/main.rs:138-156`). Add these lines after the existing `claude_dir` mount logic and before the "host-path mount" block:

```rust
    // Ensure ~/.codex exists on host before mounting
    let codex_dir = home.join(".codex");
    if !codex_dir.exists() {
        std::fs::create_dir_all(&codex_dir)?;
    }
    volumes.push(build_codex_mount(&home));
```

Place this right below the existing claude-dir creation/mount block at `src/main.rs:133-141`. The existing dedup logic at `src/main.rs:159-166` already handles duplicate dest paths; `~/.codex` is a new path so there's no conflict.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib tests::test_codex_mount_added_by_create_and_run_pipeline 2>&1 | tail -5`

Expected: PASS.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test 2>&1 | tail -5`

Expected: 229 passed.

---

## Task 9: Update `Config::init_template()` with the codex-aware template

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Update the failing tests**

Find the existing `test_config_init_content` test in `src/config.rs` and replace it with:

```rust
    #[test]
    fn test_config_init_content() {
        let content = Config::init_template();
        assert!(content.contains("# cpus"));
        assert!(content.contains("# memory"));
        assert!(content.contains("# [env]"));
        assert!(content.contains("# [profiles."));
        assert!(content.contains("# volumes"));
        assert!(content.contains("# default_agent"));
        assert!(content.contains("[cli.claude]"));
        assert!(content.contains("--dangerously-skip-permissions"));
        assert!(content.contains("[cli.codex]"));
        assert!(content.contains("--dangerously-bypass-approvals-and-sandbox"));
        // default_agent stays commented out so setup prompts on fresh install
        assert!(content.contains("# default_agent ="));
        assert!(!content.lines().any(|l| l.trim_start().starts_with("default_agent =")));
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib config::tests::test_config_init_content 2>&1 | tail -10`

Expected: FAIL (old template has neither `default_agent` nor `[cli.codex]` uncommented).

- [ ] **Step 3: Replace the template**

Replace the `init_template()` body in `src/config.rs`:

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
"#
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib config::tests::test_config_init_content 2>&1 | tail -5`

Expected: PASS.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test 2>&1 | tail -5`

Expected: 229 passed.

---

## Task 10: `ensure_default_agent_in_config` helper in `src/setup.rs`

**Files:**
- Modify: `src/setup.rs`

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/setup.rs`:

```rust
    #[test]
    fn test_ensure_default_agent_writes_new_key() {
        use crate::agent::CodingAgent;
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "# existing comment\nmemory = \"4G\"\n").unwrap();

        ensure_default_agent_in_config(&path, CodingAgent::Codex).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("default_agent = \"codex\""));
        assert!(content.contains("memory = \"4G\""));
        assert!(content.contains("# existing comment"));
    }

    #[test]
    fn test_ensure_default_agent_uncomments_existing_key() {
        use crate::agent::CodingAgent;
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "# header\n# default_agent = \"claude\"\n[cli.claude]\nflags = []\n",
        )
        .unwrap();

        ensure_default_agent_in_config(&path, CodingAgent::Codex).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("default_agent = \"codex\""));
        // Commented-out version no longer present (replaced, not duplicated)
        let live_lines = content
            .lines()
            .filter(|l| l.trim_start().starts_with("default_agent"))
            .count();
        assert_eq!(live_lines, 1);
        // Other content preserved
        assert!(content.contains("[cli.claude]"));
    }

    #[test]
    fn test_ensure_default_agent_overwrites_existing_uncommented_value() {
        use crate::agent::CodingAgent;
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "default_agent = \"claude\"\n").unwrap();

        ensure_default_agent_in_config(&path, CodingAgent::Codex).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("default_agent = \"codex\""));
        assert!(!content.contains("default_agent = \"claude\""));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib setup::tests::test_ensure_default_agent 2>&1 | tail -15`

Expected: compile error — `ensure_default_agent_in_config` not found.

- [ ] **Step 3: Implement the helper**

Add to `src/setup.rs` near the existing `ensure_env_var_in_config` at line 136:

```rust
/// Idempotently set `default_agent` in the config file. If a commented
/// `# default_agent = ...` line exists, it is replaced. If an uncommented
/// line exists, its value is overwritten. Otherwise the key is inserted at
/// the top.
fn ensure_default_agent_in_config(path: &Path, agent: crate::agent::CodingAgent) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    let value = agent.config_key();
    let new_line = format!("default_agent = \"{}\"", value);

    // Prefer surgical line-level edits because the key may live outside any
    // table, which toml_edit handles awkwardly at the document root.
    let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    let mut replaced = false;

    for line in &mut lines {
        let trimmed = line.trim_start();
        let commented =
            trimmed.starts_with("#") && trimmed.trim_start_matches('#').trim_start().starts_with("default_agent");
        let live = trimmed.starts_with("default_agent");
        if commented || live {
            *line = new_line.clone();
            replaced = true;
            break;
        }
    }

    if !replaced {
        // Insert at the top after any leading comment block but before tables.
        let insert_at = lines
            .iter()
            .position(|l| l.trim_start().starts_with('['))
            .unwrap_or(lines.len());
        lines.insert(insert_at, new_line);
        if insert_at == 0 || lines.get(insert_at - 1).map_or(false, |l| !l.is_empty()) {
            lines.insert(insert_at + 1, String::new());
        }
    }

    let mut output = lines.join("\n");
    if !output.ends_with('\n') {
        output.push('\n');
    }
    std::fs::write(path, output)?;
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib setup::tests::test_ensure_default_agent 2>&1 | tail -10`

Expected: all 3 tests pass.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test 2>&1 | tail -5`

Expected: 232 passed.

---

## Task 11: `prompt_default_agent` helper + testable stdin

**Files:**
- Modify: `src/setup.rs`

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/setup.rs`:

```rust
    #[test]
    fn test_prompt_default_agent_accepts_1_as_claude() {
        use crate::agent::CodingAgent;
        let choice = prompt_default_agent_from(|| Ok("1".to_string())).unwrap();
        assert_eq!(choice, CodingAgent::Claude);
    }

    #[test]
    fn test_prompt_default_agent_accepts_2_as_codex() {
        use crate::agent::CodingAgent;
        let choice = prompt_default_agent_from(|| Ok("2".to_string())).unwrap();
        assert_eq!(choice, CodingAgent::Codex);
    }

    #[test]
    fn test_prompt_default_agent_loops_on_invalid_input() {
        use crate::agent::CodingAgent;
        let inputs = std::cell::RefCell::new(vec!["", "3", "foo", "2"].into_iter());
        let choice = prompt_default_agent_from(|| {
            let mut it = inputs.borrow_mut();
            Ok(it.next().unwrap().to_string())
        })
        .unwrap();
        assert_eq!(choice, CodingAgent::Codex);
        // All inputs consumed
        assert!(inputs.borrow_mut().next().is_none());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib setup::tests::test_prompt_default_agent 2>&1 | tail -10`

Expected: compile error — `prompt_default_agent_from` not found.

- [ ] **Step 3: Implement the prompt (with an injectable reader for tests)**

Add to `src/setup.rs` near the existing `prompt_menu` at line 250:

```rust
/// Testable core: reads one line at a time from the supplied callback, parses
/// it, loops until it gets "1" or "2".
fn prompt_default_agent_from<F>(mut read_line: F) -> Result<crate::agent::CodingAgent>
where
    F: FnMut() -> Result<String>,
{
    loop {
        println!("\n        Which agent should be the default?");
        println!("          1) Claude");
        println!("          2) Codex");
        print!("        > ");
        std::io::stdout().flush()?;

        let input = read_line()?;
        match input.trim() {
            "1" => return Ok(crate::agent::CodingAgent::Claude),
            "2" => return Ok(crate::agent::CodingAgent::Codex),
            _ => println!("        Invalid choice. Please enter 1 or 2."),
        }
    }
}

/// Production wrapper that reads from stdin.
fn prompt_default_agent() -> Result<crate::agent::CodingAgent> {
    prompt_default_agent_from(|| {
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        Ok(input)
    })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib setup::tests::test_prompt_default_agent 2>&1 | tail -10`

Expected: 3 tests pass.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test 2>&1 | tail -5`

Expected: 235 passed.

---

## Task 12: `check_default_agent` + wire into the setup pipeline

**Files:**
- Modify: `src/setup.rs`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/setup.rs`:

```rust
    #[test]
    fn test_decide_default_agent_status_ok_when_valid() {
        use crate::agent::CodingAgent;
        let mut c = Config::default();
        c.default_agent = Some("codex".into());
        assert!(matches!(
            decide_default_agent_status(&c),
            DefaultAgentStatus::Ok(CodingAgent::Codex)
        ));
        c.default_agent = Some("claude".into());
        assert!(matches!(
            decide_default_agent_status(&c),
            DefaultAgentStatus::Ok(CodingAgent::Claude)
        ));
    }

    #[test]
    fn test_decide_default_agent_status_needs_prompt_when_missing() {
        let c = Config::default();
        assert!(matches!(
            decide_default_agent_status(&c),
            DefaultAgentStatus::NeedsPrompt
        ));
    }

    #[test]
    fn test_decide_default_agent_status_needs_prompt_when_invalid() {
        let mut c = Config::default();
        c.default_agent = Some("gemini".into());
        assert!(matches!(
            decide_default_agent_status(&c),
            DefaultAgentStatus::NeedsPrompt
        ));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib setup::tests::test_decide_default_agent_status 2>&1 | tail -10`

Expected: compile error — `decide_default_agent_status` / `DefaultAgentStatus` not found.

- [ ] **Step 3: Implement the decision function and the check**

Add to `src/setup.rs`:

```rust
/// Pure decision for step 4. Separated from `check_default_agent` so tests
/// don't have to touch the filesystem.
enum DefaultAgentStatus {
    Ok(crate::agent::CodingAgent),
    NeedsPrompt,
}

fn decide_default_agent_status(config: &Config) -> DefaultAgentStatus {
    use std::str::FromStr;
    match config.default_agent.as_deref() {
        Some(s) => match crate::agent::CodingAgent::from_str(s) {
            Ok(agent) => DefaultAgentStatus::Ok(agent),
            Err(_) => DefaultAgentStatus::NeedsPrompt,
        },
        None => DefaultAgentStatus::NeedsPrompt,
    }
}

fn check_default_agent() -> Status {
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => return Status::Errored(e),
    };
    match decide_default_agent_status(&config) {
        DefaultAgentStatus::Ok(_) => Status::Ok,
        DefaultAgentStatus::NeedsPrompt => Status::AutoFix {
            explanation:
                "Default agent not set. Pick which agent runs when you type bare `agentbox`."
                    .to_string(),
            fix: Box::new(|| {
                let choice = prompt_default_agent()?;
                ensure_default_agent_in_config(&Config::config_path(), choice)?;
                println!(
                    "        ✓ Set default_agent = \"{}\"",
                    choice.config_key()
                );
                Ok(())
            }),
        },
    }
}
```

Wire it into the pipeline in `run_setup` at `src/setup.rs:272`:

```rust
    let checks: &[(&str, fn() -> Status)] = &[
        ("Apple Container CLI", check_container_cli),
        ("Container system running", check_container_system),
        ("Config file", check_config_file),
        ("Default agent", check_default_agent),           // NEW
        ("Claude authentication", check_authentication),  // (renamed from "Authentication")
    ];
```

(Step 6 — `check_codex_authentication` — is added in task 14.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib setup::tests::test_decide_default_agent_status 2>&1 | tail -10`

Expected: 3 tests pass.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test 2>&1 | tail -5`

Expected: 238 passed.

---

## Task 13: `Status::OkWithInfo` + Codex short-circuit for `check_authentication`

**Files:**
- Modify: `src/setup.rs`

Two changes here because the second depends on the first: (a) add a `Status::OkWithInfo(String)` variant so a check can count as passed while emitting an advisory note, (b) use it for the codex-default short-circuit in `check_authentication`. Task 14 also consumes this variant for codex advisory output.

- [ ] **Step 1: Add the `Status::OkWithInfo` variant and handle it in the orchestrator**

In `src/setup.rs`, extend the existing `Status` enum (currently at `setup.rs:21-40`):

```rust
pub enum Status {
    Ok,
    /// Non-blocking pass with an advisory note printed under the step label.
    /// Increments `passed`, so the overall setup can still complete cleanly.
    OkWithInfo(String),
    AutoFix { ... },        // unchanged
    Manual { ... },         // unchanged
    Interactive { ... },    // unchanged
    Errored(anyhow::Error), // unchanged
}
```

Add a match arm in `run_setup` (at `setup.rs:283`), immediately after the `Status::Ok` arm:

```rust
            Status::Ok => {
                println!("✓");
                passed += 1;
            }
            Status::OkWithInfo(info) => {
                println!("✓");
                print_indented(&info, 8);
                passed += 1;
            }
```

- [ ] **Step 2: Write the failing tests**

Add to `mod tests` in `src/setup.rs`:

```rust
    #[test]
    fn test_decide_auth_short_circuit_codex_default() {
        let mut c = Config::default();
        c.default_agent = Some("codex".into());
        assert!(decide_auth_with_codex_short_circuit(
            &c,
            &|_k| None,
            false
        ));
    }

    #[test]
    fn test_decide_auth_no_short_circuit_when_claude_default() {
        let mut c = Config::default();
        c.default_agent = Some("claude".into());
        // No credentials, no env vars, not short-circuited → false (prompt needed)
        assert!(!decide_auth_with_codex_short_circuit(
            &c,
            &|_k| None,
            false
        ));
    }

    #[test]
    fn test_decide_auth_no_short_circuit_when_default_agent_missing() {
        // Defensive: no default_agent set → treated as claude → normal flow
        let c = Config::default();
        assert!(!decide_auth_with_codex_short_circuit(
            &c,
            &|_k| None,
            false
        ));
    }

    #[test]
    fn test_check_authentication_returns_ok_with_info_for_codex_default() {
        let mut c = Config::default();
        c.default_agent = Some("codex".into());
        let status = check_authentication_with_config(&c);
        match status {
            Status::OkWithInfo(info) => {
                assert!(info.contains("Skipped"));
                assert!(info.contains("codex"));
            }
            _ => panic!("expected Status::OkWithInfo"),
        }
    }
```

(The test calls a new `check_authentication_with_config(&Config) -> Status` helper because the production `check_authentication()` loads config from disk. We extract the pure core.)

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --lib setup::tests::test_decide_auth_short_circuit_codex_default 2>&1 | tail -15`

Expected: compile errors — `decide_auth_with_codex_short_circuit` and `check_authentication_with_config` not found. (Compile failure is module-wide; running one test surfaces both.)

- [ ] **Step 4: Implement the helper and refactor `check_authentication`**

Replace the existing `decide_auth` block at `src/setup.rs:117-132` with:

```rust
/// Pure decision function: is authentication reachable?
fn decide_auth(
    config: &Config,
    host_env: &dyn Fn(&str) -> Option<String>,
    credentials_exists: bool,
) -> bool {
    for key in AUTH_KEYS {
        let Some(val) = config.env.get(*key) else { continue };
        if !val.is_empty() {
            return true;
        }
        if host_env(key).map_or(false, |v| !v.is_empty()) {
            return true;
        }
    }
    credentials_exists
}

/// Extension used by step 5: a codex-default user does not need claude auth
/// for setup to pass.
fn decide_auth_with_codex_short_circuit(
    config: &Config,
    host_env: &dyn Fn(&str) -> Option<String>,
    credentials_exists: bool,
) -> bool {
    if matches!(
        config.resolve_default_agent(),
        Ok(crate::agent::CodingAgent::Codex)
    ) {
        return true;
    }
    decide_auth(config, host_env, credentials_exists)
}
```

Replace `check_authentication` at `src/setup.rs:221-241` with a thin wrapper around a testable core:

```rust
fn check_authentication_with_config(config: &Config) -> Status {
    // Codex-default users: skip blocking Claude auth. Pass with advisory.
    if matches!(
        config.resolve_default_agent(),
        Ok(crate::agent::CodingAgent::Codex)
    ) {
        return Status::OkWithInfo(
            "Skipped — default_agent = codex.\n\
             Re-run `agentbox setup` after changing default_agent to \"claude\"\n\
             if you want Claude auth configured.".to_string(),
        );
    }

    let credentials_exists = credentials_file_path()
        .and_then(|p| std::fs::metadata(p).ok())
        .map_or(false, |m| m.len() > 0);

    let host_env = |key: &str| std::env::var(key).ok();

    if decide_auth(config, &host_env, credentials_exists) {
        Status::Ok
    } else {
        Status::Interactive {
            explanation: AUTH_EXPLANATION.to_string(),
            menu: build_auth_menu(),
        }
    }
}

fn check_authentication() -> Status {
    match Config::load() {
        Ok(c) => check_authentication_with_config(&c),
        Err(e) => Status::Errored(e),
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib setup::tests::test_decide_auth 2>&1 | tail -20`

Expected: new short-circuit tests pass; existing `decide_auth` tests continue to pass. Then confirm the `OkWithInfo` test separately:

Run: `cargo test --lib setup::tests::test_check_authentication_returns_ok_with_info 2>&1 | tail -5`

Expected: PASS.

- [ ] **Step 6: Run the full test suite**

Run: `cargo test 2>&1 | tail -5`

Expected: 242 passed (+4 new tests vs 238 previous).

---

## Task 14: `check_codex_authentication` + wire into pipeline

**Files:**
- Modify: `src/setup.rs`

Per codex's source (`AuthCredentialsStoreMode`'s `#[default]` is `File`) and OpenAI's public config reference, the effective default for `cli_auth_credentials_store` is `"file"`. Missing key therefore means "file" — no warning needed. Only explicit non-file values (`"keyring"`, `"auto"`, `"ephemeral"`) need a heads-up.

The check uses `Status::OkWithInfo` (introduced in Task 13) to emit advisory text after the ✓ without the step-label line-gluing issue that plain `print_indented` would cause.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/setup.rs`:

```rust
    #[test]
    fn test_codex_store_no_warning_when_config_missing() {
        // Missing file means codex will use its documented default ("file"),
        // which is what we want. No warning.
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("nonexistent.toml");
        assert!(!codex_store_warning_needed(&missing));
    }

    #[test]
    fn test_codex_store_no_warning_when_key_missing() {
        // Same rationale: missing key resolves to "file" default.
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "# comment only\n").unwrap();
        assert!(!codex_store_warning_needed(&path));
    }

    #[test]
    fn test_codex_store_warning_when_non_file_backend() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        for bad in ["keyring", "auto", "ephemeral"] {
            std::fs::write(
                &path,
                format!("cli_auth_credentials_store = \"{}\"\n", bad),
            )
            .unwrap();
            assert!(
                codex_store_warning_needed(&path),
                "expected warning for backend {bad}"
            );
        }
    }

    #[test]
    fn test_codex_store_no_warning_when_file_backend() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "cli_auth_credentials_store = \"file\"\n").unwrap();
        assert!(!codex_store_warning_needed(&path));
    }

    #[test]
    fn test_codex_store_warning_when_malformed_toml() {
        // Unparseable config — err on the side of warning so the user sees
        // their config is busted.
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "this = is = not toml").unwrap();
        assert!(codex_store_warning_needed(&path));
    }

    #[test]
    fn test_check_codex_authentication_returns_ok_with_info_always() {
        // The check never blocks. It returns OkWithInfo carrying at minimum
        // the device-code sign-in hint.
        let status = check_codex_authentication_with_path(None);
        match status {
            Status::OkWithInfo(info) => {
                // Asserting on "device code" (lowercased) is resilient to
                // exact menu-label drift in codex's TUI.
                assert!(info.to_lowercase().contains("device code"));
            }
            _ => panic!("expected Status::OkWithInfo"),
        }
    }

    #[test]
    fn test_check_codex_authentication_includes_store_warning_when_needed() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "cli_auth_credentials_store = \"keyring\"\n").unwrap();
        let status = check_codex_authentication_with_path(Some(&path));
        match status {
            Status::OkWithInfo(info) => {
                assert!(info.contains("cli_auth_credentials_store"));
                assert!(info.to_lowercase().contains("device code"));
            }
            _ => panic!("expected Status::OkWithInfo"),
        }
    }

    #[test]
    fn test_check_codex_authentication_omits_store_warning_when_file_default() {
        // Missing config (user has the documented default) → no warning.
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("nonexistent.toml");
        let status = check_codex_authentication_with_path(Some(&missing));
        match status {
            Status::OkWithInfo(info) => {
                assert!(!info.contains("cli_auth_credentials_store"));
                assert!(info.to_lowercase().contains("device code"));
            }
            _ => panic!("expected Status::OkWithInfo"),
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib setup::tests::test_codex_store_no_warning_when_config_missing 2>&1 | tail -15`

Expected: compile errors — `codex_store_warning_needed` and `check_codex_authentication_with_path` not found. (Compile failure is module-wide.)

- [ ] **Step 3: Implement the helpers and the check**

Add to `src/setup.rs`:

```rust
/// Inspect `cli_auth_credentials_store` in a codex config.toml file.
/// Returns `true` when the user should be warned: the value is explicitly
/// `"keyring"`, `"auto"`, `"ephemeral"`, or the TOML is malformed. Missing
/// file and missing key both mean "use the documented default (`"file"`)",
/// so they return `false`.
fn codex_store_warning_needed(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false; // no config file → default is "file"
    };
    match content.parse::<toml::Table>() {
        Err(_) => true, // malformed — point it out
        Ok(doc) => match doc.get("cli_auth_credentials_store").and_then(|v| v.as_str()) {
            // Missing key or explicit "file" → happy path
            None | Some("file") => false,
            // Any other value → warn
            _ => true,
        },
    }
}

fn codex_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex/config.toml"))
}

const CODEX_SIGNIN_HINT: &str =
    "First time using codex? Run `agentbox codex`.\n\
     On an unauthenticated container, codex's onboarding menu appears.\n\
     Pick the device-code sign-in flow (for remote/headless machines),\n\
     then open the URL shown on your Mac and enter the code. The option\n\
     may also be reachable by picking ChatGPT sign-in first and pressing\n\
     Esc when the browser step appears.";

const CODEX_STORE_WARNING: &str =
    "Heads-up: codex in the container cannot reach the macOS Keychain.\n\
     For auth to persist, add this line to ~/.codex/config.toml:\n\n    \
     cli_auth_credentials_store = \"file\"\n\n\
     If you already signed in with a non-file backend, sign in again from\n\
     within `agentbox codex` (or run `codex login` on the Mac) after\n\
     changing the setting.";

/// Testable core: takes an explicit path (or `None` to skip the store check).
/// The production wrapper reads from `~/.codex/config.toml`.
fn check_codex_authentication_with_path(codex_config: Option<&Path>) -> Status {
    let mut info = String::new();
    if let Some(path) = codex_config {
        if codex_store_warning_needed(path) {
            info.push_str(CODEX_STORE_WARNING);
            info.push_str("\n\n");
        }
    }
    info.push_str(CODEX_SIGNIN_HINT);
    Status::OkWithInfo(info)
}

fn check_codex_authentication() -> Status {
    let path = codex_config_path();
    check_codex_authentication_with_path(path.as_deref())
}
```

Wire into the pipeline in `run_setup`:

```rust
    let checks: &[(&str, fn() -> Status)] = &[
        ("Apple Container CLI", check_container_cli),
        ("Container system running", check_container_system),
        ("Config file", check_config_file),
        ("Default agent", check_default_agent),
        ("Claude authentication", check_authentication),
        ("Codex authentication", check_codex_authentication),  // NEW
    ];
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib setup::tests::test_codex_store 2>&1 | tail -15`

Expected: 5 store-helper tests pass. Then confirm the check-function tests:

Run: `cargo test --lib setup::tests::test_check_codex_authentication 2>&1 | tail -15`

Expected: 3 check-function tests pass.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test 2>&1 | tail -5`

Expected: 250 passed.

---

## Task 15: README updates

**Files:**
- Modify: `README.md`

No unit tests — doc changes only.

- [ ] **Step 1: Update the intro and quick-start sections**

In `README.md`, at the top of the file (around line 3), replace:

```markdown
Currently supports Claude Code. More agents planned.
```

with:

```markdown
Supported agents: Claude Code, OpenAI Codex.
```

In the Quick Start section (around line 25), add codex examples next to the existing ones:

```markdown
# Start interactive Claude session (default, unless default_agent is set in config)
agentbox

# Explicit agent subcommands
agentbox claude
agentbox codex

# Headless tasks
agentbox "fix the failing tests"
agentbox codex "fix the failing tests"
```

- [ ] **Step 2: Update the passing-flags section**

Add a codex example to the "Passing Flags to the Coding Agent" section:

```markdown
# Pass a codex config override (reasoning effort)
agentbox codex -- -c model_reasoning_effort=high

# Pass flags to codex via config
# [cli.codex]
# flags = ["--dangerously-bypass-approvals-and-sandbox", "-c", "model_reasoning_effort=medium"]
```

- [ ] **Step 3: Update the Configuration example block**

Replace the example `config.toml` block with:

```toml
# ~/.config/agentbox/config.toml

# Default agent for bare `agentbox`. Omit for "claude".
default_agent = "claude"   # or "codex"

# Resources
cpus = 4          # default: half of host cores
memory = "8G"     # default: 8G

# Override default Dockerfile
dockerfile = "/path/to/my.Dockerfile"

# Additional volumes to mount into containers
volumes = [
  "~/.config/worktrunk",
  "/opt/shared-libs",
  "/source/path:/dest/path",
]

# Environment variables passed into container
[env]
CLAUDE_CODE_OAUTH_TOKEN = ""
GH_TOKEN = ""
MY_API_KEY = "abc123"

# Default flags for each agent. Replace to override.
[cli.claude]
flags = ["--dangerously-skip-permissions"]

[cli.codex]
flags = ["--dangerously-bypass-approvals-and-sandbox"]

# Named profiles
[profiles.mystack]
dockerfile = "/path/to/mystack.Dockerfile"
```

- [ ] **Step 4: Rewrite the Authentication section**

Replace the existing Authentication section with parallel subsections:

```markdown
## Authentication

### Claude Code

macOS Keychain isn't accessible from inside the Linux container, so Claude Code needs credentials passed via environment variables, or a one-time login from inside the container (which persists under `~/.claude/`).

**Easiest approach: Run `agentbox setup`** — it will guide you through the options.

(Existing Claude sub-options A/B here, unchanged.)

### OpenAI Codex

Codex stores auth under `~/.codex/auth.json`, which agentbox mounts into the container read/write. Sign in once (host or container); the credentials persist for both.

**First sign-in.** Run `agentbox codex`. On an unauthenticated container, codex's onboarding menu appears; pick the device-code sign-in flow (intended for remote/headless machines), then open the URL on your Mac and enter the code shown in the terminal. Auth persists automatically via the `~/.codex` mount. If the onboarding menu only shows ChatGPT / API-key options up front, pick ChatGPT and press Esc on the browser screen — codex falls back to device code for headless environments.

**Credential storage — default case.** Codex stores credentials in `~/.codex/auth.json` by default (file-based). You don't need to configure anything; the mount makes the file reachable from both host and container.

**Credential storage — if you've customized it.** If you previously set `cli_auth_credentials_store = "keyring"` (or `"auto"`, `"ephemeral"`) in `~/.codex/config.toml`, auth won't propagate into the container — the Linux container can't reach the macOS Keychain. Switch the setting back to `"file"` and re-login:

```toml
# ~/.codex/config.toml
cli_auth_credentials_store = "file"
```

`agentbox setup` detects this case and prints the same hint.
```

- [ ] **Step 5: Update the "What's Mounted" table**

Replace the existing mount table with:

```markdown
| Host                | Container                 | Access     | Notes                                                         |
|---------------------|---------------------------|------------|---------------------------------------------------------------|
| Current directory   | Same path                 | read/write |                                                               |
| `~/.claude`         | `/home/user/.claude`      | read/write |                                                               |
| `~/.claude.json`    | `/tmp/claude-seed.json`   | read-only  | Seed only; `entrypoint.sh` `jq`-merges into `~/.claude.json` |
| `~/.codex`          | `/home/user/.codex`       | read/write |                                                               |
| Additional volumes  | Configured path           | read/write |                                                               |
```

- [ ] **Step 6: Add the breaking-change callout**

Immediately after the Install section, add:

```markdown
### Breaking change (pre-1.0)

agentbox no longer hardcodes `--dangerously-skip-permissions` into the claude invocation. The flag now lives in `[cli.claude] flags` in the config template.

- If your `~/.config/agentbox/config.toml` has a `[cli.claude] flags = [...]` entry, add `--dangerously-skip-permissions` to the list.
- If your config has no `[cli.claude]` section, add one with `flags = ["--dangerously-skip-permissions"]`.
- If you have no `~/.config/agentbox/config.toml`, run `agentbox setup` — it will create the file with correct defaults.
```

- [ ] **Step 7: Run the full test suite**

Run: `cargo test 2>&1 | tail -5`

Expected: 250 passed (no change; README doesn't affect tests).

---

## Task 16: Manual smoke tests

No code changes. Run each scenario by hand on the implementer's Mac and check the observable behavior against expectations.

Each smoke test below is a single step in the plan-task sense:

- [ ] **Smoke 1: Fresh install walkthrough.** Remove any existing config
      (`rm -f ~/.config/agentbox/config.toml`), run `agentbox setup`, observe
      all 6 steps complete, verify the written config matches the template
      from Task 9.
- [ ] **Smoke 2: `agentbox codex` cold-start.** `agentbox rm --all`, then
      `agentbox codex`. Image rebuilds with codex installed, entrypoint
      dispatches `--codex`, codex TUI launches.
- [ ] **Smoke 3: `agentbox codex` warm-start.** With the container from
      Smoke 2 already running, open a new terminal and run `agentbox codex`
      again. Exec path dispatches to codex, TUI launches.
- [ ] **Smoke 4: `agentbox codex "fix the tests"` headless.** Runs
      `codex exec "fix the tests"` inside, prints codex output, exits
      with codex's exit code.
- [ ] **Smoke 5: `agentbox claude` regression.** `agentbox claude` (no
      task) launches Claude TUI, same behavior as before the refactor.
      `agentbox claude "fix tests"` runs headlessly via `-p`.
- [ ] **Smoke 6: `default_agent = "codex"`.** Edit config to set
      `default_agent = "codex"`. Bare `agentbox` launches codex.
- [ ] **Smoke 7: Codex passthrough.** With `default_agent = "codex"`, run
      `agentbox -- -c model_reasoning_effort=high`. Flag reaches codex.
- [ ] **Smoke 8: `agentbox shell` unchanged.** Shell still opens bash; one-shot
      `agentbox shell -- ls /workspace` runs and exits.
- [ ] **Smoke 9: Device-code sign-in round-trip.** Delete `~/.codex/auth.json`
      if present (leave the default credential store alone; file is the
      default). Run `agentbox codex`, pick the device-code sign-in flow
      from codex's onboarding menu (label may vary by version; pick the
      option marked for headless/remote machines), visit URL on Mac,
      enter code. Exit, run `agentbox codex` again — no re-auth needed.
- [ ] **Smoke 10: Upgrade path.** With an existing user config that has
      `[cli.claude] flags = ["--model", "sonnet"]` (no bypass flag), run
      `agentbox`. Claude launches without the bypass flag and hits a
      permission prompt. Add `--dangerously-skip-permissions` to the flags
      per the README callout, re-run, prompt gone.
- [ ] **Smoke 11: Codex-first setup completes cleanly.** `rm -f
      ~/.config/agentbox/config.toml && agentbox setup`, pick Codex at
      step 4. Observe step 5 shows ✓ and prints the "Skipped —
      default_agent = codex" note (OkWithInfo). Step 6 shows ✓ and
      prints the device-code sign-in hint (and the credential-store
      warning only if `~/.codex/config.toml` sets a non-file backend).
      Final line reads "Ready. Run `agentbox` to start coding."
- [ ] **Smoke 12: Invalid menu input in step 4.** During step 4 of setup,
      type `3` then Enter. "Invalid choice. Please enter 1 or 2." appears
      and step 4 re-prompts. Step 4 does NOT count as passed until `1` or
      `2` is entered.
