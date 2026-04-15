//! Interactive setup command for agentbox.
//!
//! Guides first-time users through checking:
//! - Apple Container CLI is installed
//! - Container system is running
//! - Config file exists
//! - Authentication is configured
//!
//! See wiki/2026-04-07-agentbox-setup-design.md for full spec.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use crate::config::Config;

const ANTHROPIC_API_KEY: &str = "ANTHROPIC_API_KEY";
const CLAUDE_CODE_OAUTH_TOKEN: &str = "CLAUDE_CODE_OAUTH_TOKEN";
const AUTH_KEYS: &[&str] = &[ANTHROPIC_API_KEY, CLAUDE_CODE_OAUTH_TOKEN];

pub enum Status {
    Ok,
    /// Non-blocking pass with an advisory note printed under the step label.
    /// Increments `passed`, so the overall setup can still complete cleanly.
    OkWithInfo(String),
    /// Auto-fixable: orchestrator runs `fix()` directly, no consent prompt.
    AutoFix {
        explanation: String,
        fix: Box<dyn FnOnce() -> Result<()>>,
    },
    /// User must act out of band; we print instructions.
    Manual {
        explanation: String,
        next_steps: String,
    },
    /// Interactive menu (used only by the auth check).
    Interactive {
        explanation: String,
        menu: Vec<MenuOption>,
    },
    /// The check itself errored (e.g., parse failure).
    Errored(anyhow::Error),
}

pub struct MenuOption {
    pub label: &'static str,
    pub action: Box<dyn FnOnce() -> Result<()>>,
    pub header_before: Option<&'static str>,
}

fn check_container_cli() -> Status {
    match Command::new("container").arg("--version").output() {
        Ok(_) => Status::Ok,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Status::Manual {
            explanation: "Apple Container CLI is not installed or not on PATH.".to_string(),
            next_steps: "Download and install from: https://github.com/apple/container/releases"
                .to_string(),
        },
        Err(e) => Status::Errored(anyhow::anyhow!("Failed to check container CLI: {}", e)),
    }
}

/// Parse `container system status` output: looks for a `status running` line.
fn parse_system_status(stdout: &str) -> bool {
    stdout.lines().any(|line| {
        let parts: Vec<&str> = line.split_whitespace().collect();
        parts.len() == 2 && parts[0] == "status" && parts[1] == "running"
    })
}

fn check_container_system() -> Status {
    match Command::new("container").args(["system", "status"]).output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if parse_system_status(&stdout) {
                Status::Ok
            } else {
                Status::AutoFix {
                    explanation: "Container system is not running. Starting it...".to_string(),
                    fix: Box::new(|| {
                        let status = Command::new("container")
                            .args(["system", "start"])
                            .stdin(std::process::Stdio::inherit())
                            .stdout(std::process::Stdio::inherit())
                            .stderr(std::process::Stdio::inherit())
                            .status()
                            .context("failed to run 'container system start'")?;
                        if status.success() {
                            Ok(())
                        } else {
                            anyhow::bail!("container system start exited with non-zero status")
                        }
                    }),
                }
            }
        }
        Err(e) => Status::Errored(anyhow::anyhow!("Failed to check container system: {}", e)),
    }
}

fn check_config_file() -> Status {
    if Config::config_path().exists() {
        Status::Ok
    } else {
        Status::AutoFix {
            explanation: "Config file does not exist. Creating it from the default template..."
                .to_string(),
            fix: Box::new(|| {
                let config_path = Config::config_path();
                let parent = config_path.parent().context("config path has no parent")?;
                std::fs::create_dir_all(parent)?;
                std::fs::write(&config_path, Config::init_template())?;
                Ok(())
            }),
        }
    }
}

/// Pure decision function: is authentication reachable?
/// Separated from `check_authentication` so it can be unit-tested without I/O.
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
#[cfg(test)]
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

/// Idempotently add a key to the `[env]` section of the config file at `path`.
/// Preserves comments and formatting via `toml_edit`. No-op if the key exists.
fn ensure_env_var_in_config(path: &Path, key: &str) -> Result<()> {
    use toml_edit::{table, value, DocumentMut};

    let content = std::fs::read_to_string(path)?;
    let mut doc: DocumentMut = content.parse()?;

    let env_tbl = doc
        .entry("env")
        .or_insert(table())
        .as_table_mut()
        .context("'env' in config is not a table")?;

    if env_tbl.contains_key(key) {
        return Ok(());
    }

    env_tbl.insert(key, value(""));
    std::fs::write(path, doc.to_string())?;
    Ok(())
}

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
            trimmed.starts_with('#') && trimmed.trim_start_matches('#').trim_start().starts_with("default_agent");
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

/// Pure decision for step 4. Separated from `check_default_agent` so tests
/// don't have to touch the filesystem. The inner `CodingAgent` on `Ok` is
/// used by tests to verify the decision resolves to the right agent, even
/// though production only cares about Ok-vs-NeedsPrompt.
#[allow(dead_code)]
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

const AUTH_EXPLANATION: &str = "macOS Keychain isn't reachable from the Linux container.\n\
Claude Code needs either a one-time login from inside the container\n\
(Pro/Max subscribers; persists under ~/.claude) or credentials via env var.";

/// Prompt user for `[Y/n]` confirmation, then add `key = ""` under `[env]` in
/// the config file. Used by both the API-key and OAuth-token menu branches.
fn prompt_and_add_env_var(key: &str) -> Result<()> {
    println!(
        "\n        Add `{} = \"\"` under [env] in your config automatically? [Y/n]",
        key
    );
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let answer = input.trim();
    if answer.is_empty() || answer.eq_ignore_ascii_case("y") {
        ensure_env_var_in_config(&Config::config_path(), key)?;
        println!("        ✓ Updated ~/.config/agentbox/config.toml");
        println!("        Then re-run `agentbox setup` in a new shell to confirm.");
    }
    Ok(())
}

fn build_auth_menu() -> Vec<MenuOption> {
    vec![
        MenuOption {
            label: "Log in once inside the container (Pro/Max subscription)",
            action: Box::new(|| {
                println!("\n        Next step: run `agentbox`, then type `/login` inside Claude.");
                println!("        The credentials persist under ~/.claude — you only do this once.");
                Ok(())
            }),
            header_before: Some("Recommended:"),
        },
        MenuOption {
            label: "Use a long-lived OAuth token (CLAUDE_CODE_OAUTH_TOKEN)",
            action: Box::new(|| {
                println!("\n        Requires the host `claude` CLI. Run this on your Mac first:");
                println!("\n            claude setup-token");
                println!("\n        Copy the token, then run in your shell (and add it to ~/.zshrc / ~/.bashrc):");
                println!("\n            export {}=\"your-token-here\"", CLAUDE_CODE_OAUTH_TOKEN);
                prompt_and_add_env_var(CLAUDE_CODE_OAUTH_TOKEN)
            }),
            header_before: Some("Alternatives:"),
        },
        MenuOption {
            label: "Use an API key (ANTHROPIC_API_KEY)",
            action: Box::new(|| {
                println!("\n        Run this in your shell (and add it to ~/.zshrc / ~/.bashrc for next time):");
                println!("\n            export {}=\"sk-...\"", ANTHROPIC_API_KEY);
                prompt_and_add_env_var(ANTHROPIC_API_KEY)
            }),
            header_before: None,
        },
        MenuOption {
            label: "Skip for now",
            action: Box::new(|| {
                println!("\n        You can re-run `agentbox setup` at any time to set up authentication.");
                Ok(())
            }),
            header_before: None,
        },
    ]
}

fn credentials_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude/.credentials.json"))
}

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
     Pick the device code sign-in flow (for remote/headless machines),\n\
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

/// Build the "missing [cli.X]" hint for an agent. Both the default flag and
/// the rationale are agent-specific; `CodingAgent::config_key` supplies the
/// section name.
fn flags_hint(agent: crate::agent::CodingAgent) -> String {
    use crate::agent::CodingAgent;
    let (flag, rationale) = match agent {
        CodingAgent::Claude => (
            "--dangerously-skip-permissions",
            "claude prompts for permission on every tool use.",
        ),
        CodingAgent::Codex => (
            "--dangerously-bypass-approvals-and-sandbox",
            "codex tries to sandbox (bubblewrap) and prompts for approvals.",
        ),
    };
    let key = agent.config_key();
    format!(
        "Missing [cli.{key}] in ~/.config/agentbox/config.toml. Add:\n\n    \
         [cli.{key}]\n    flags = [\"{flag}\"]\n\n\
         Without it, {rationale}"
    )
}

/// Pure decision for the CLI-flags check. Inspects which `[cli.*]` sections
/// are present on `config`. Presence-only: a section with `flags = []` is
/// treated as an intentional user choice and does not produce a warning.
fn check_agent_flags_with_config(config: &Config) -> Status {
    use crate::agent::CodingAgent;
    let missing: Vec<CodingAgent> = [CodingAgent::Claude, CodingAgent::Codex]
        .into_iter()
        .filter(|a| !config.cli.contains_key(a.config_key()))
        .collect();
    if missing.is_empty() {
        Status::Ok
    } else {
        let info = missing
            .iter()
            .map(|a| flags_hint(*a))
            .collect::<Vec<_>>()
            .join("\n\n");
        Status::OkWithInfo(info)
    }
}

fn check_agent_flags() -> Status {
    match Config::load() {
        Ok(c) => check_agent_flags_with_config(&c),
        Err(e) => Status::Errored(e),
    }
}

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

fn print_indented(text: &str, indent: usize) {
    let pad = " ".repeat(indent);
    for line in text.lines() {
        println!("{}{}", pad, line);
    }
}

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

fn render_menu<W: Write>(menu: &[MenuOption], out: &mut W) -> std::io::Result<()> {
    for (i, option) in menu.iter().enumerate() {
        if let Some(header) = option.header_before {
            writeln!(out)?;
            writeln!(out, "          {}", header)?;
        }
        writeln!(out, "            {}) {}", i + 1, option.label)?;
    }
    Ok(())
}

fn prompt_menu(mut menu: Vec<MenuOption>) -> Result<()> {
    let mut stdout = std::io::stdout();
    render_menu(&menu, &mut stdout)?;
    print!("        > ");
    stdout.flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let choice = input.trim().parse::<usize>().unwrap_or(0);

    if choice > 0 && choice <= menu.len() {
        let option = menu.remove(choice - 1);
        (option.action)()?;
    } else {
        println!("        Invalid choice.");
    }

    Ok(())
}

pub fn run_setup() -> Result<()> {
    let checks: &[(&str, fn() -> Status)] = &[
        ("Apple Container CLI", check_container_cli),
        ("Container system running", check_container_system),
        ("Config file", check_config_file),
        ("Default agent", check_default_agent),
        ("Agent flags", check_agent_flags),
        ("Claude authentication", check_authentication),
        ("Codex authentication", check_codex_authentication),
    ];

    let mut passed = 0;

    for (i, (name, check_fn)) in checks.iter().enumerate() {
        print!("  [{}/{}] {:<30} ", i + 1, checks.len(), name);
        match check_fn() {
            Status::Ok => {
                println!("✓");
                passed += 1;
            }
            Status::OkWithInfo(info) => {
                println!("✓");
                print_indented(&info, 8);
                passed += 1;
            }
            Status::AutoFix { explanation, fix } => {
                println!("✗");
                if !explanation.is_empty() {
                    print_indented(&explanation, 8);
                }
                match fix() {
                    Ok(()) => {
                        println!("        ✓");
                        passed += 1;
                    }
                    Err(e) => {
                        println!("        failed: {:#}", e);
                    }
                }
            }
            Status::Manual { explanation, next_steps } => {
                println!("✗");
                print_indented(&explanation, 8);
                print_indented(&next_steps, 8);
            }
            Status::Interactive { explanation, menu } => {
                println!("✗");
                print_indented(&explanation, 8);
                prompt_menu(menu)?;
            }
            Status::Errored(e) => {
                println!("error: {:#}", e);
            }
        }
    }

    println!();
    if passed == checks.len() {
        println!("Ready. Run `agentbox` to start coding.");
    } else {
        println!(
            "{}/{} checks passed. Re-run `agentbox setup` after completing the steps above.",
            passed, checks.len()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn config_with_env(entries: &[(&str, &str)]) -> Config {
        let mut env = HashMap::new();
        for (k, v) in entries {
            env.insert(k.to_string(), v.to_string());
        }
        Config { env, ..Config::default() }
    }

    #[test]
    fn parse_system_status_running() {
        assert!(parse_system_status("status running\n"));
    }

    #[test]
    fn parse_system_status_stopped() {
        assert!(!parse_system_status("status stopped\n"));
    }

    #[test]
    fn parse_system_status_unrelated_output() {
        assert!(!parse_system_status("some random output\n"));
    }

    #[test]
    fn decide_auth_literal_api_key() {
        let config = config_with_env(&[(ANTHROPIC_API_KEY, "sk-test")]);
        assert!(decide_auth(&config, &|_| None, false));
    }

    #[test]
    fn decide_auth_inherited_from_host() {
        let config = config_with_env(&[(ANTHROPIC_API_KEY, "")]);
        let host = |k: &str| (k == ANTHROPIC_API_KEY).then(|| "sk-host".to_string());
        assert!(decide_auth(&config, &host, false));
    }

    #[test]
    fn decide_auth_inherited_but_host_unset() {
        let config = config_with_env(&[(ANTHROPIC_API_KEY, "")]);
        assert!(!decide_auth(&config, &|_| None, false));
    }

    #[test]
    fn decide_auth_credentials_file_only() {
        let config = Config::default();
        assert!(decide_auth(&config, &|_| None, true));
    }

    #[test]
    fn decide_auth_nothing_configured() {
        let config = Config::default();
        assert!(!decide_auth(&config, &|_| None, false));
    }

    #[test]
    fn ensure_env_var_creates_section_in_empty_config() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "").unwrap();

        ensure_env_var_in_config(&path, "MY_KEY").unwrap();

        let result: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(result["env"]["MY_KEY"].as_str(), Some(""));
    }

    #[test]
    fn ensure_env_var_is_idempotent() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "[env]\nMY_KEY = \"existing\"\n").unwrap();

        ensure_env_var_in_config(&path, "MY_KEY").unwrap();

        // Existing value preserved, not overwritten with "".
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("MY_KEY = \"existing\""));
    }

    #[test]
    fn ensure_env_var_preserves_comments_and_other_keys() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let original = "# This is my config\n[env]\n# My API key\nEXISTING_KEY = \"value\"\n";
        std::fs::write(&path, original).unwrap();

        ensure_env_var_in_config(&path, "NEW_KEY").unwrap();

        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.contains("# This is my config"));
        assert!(result.contains("# My API key"));
        assert!(result.contains("EXISTING_KEY = \"value\""));
        assert!(result.contains("NEW_KEY"));
    }

    #[test]
    fn ensure_env_var_errors_when_env_is_not_a_table() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "env = \"oops\"\n").unwrap();

        let err = ensure_env_var_in_config(&path, "MY_KEY").unwrap_err();
        assert!(err.to_string().contains("not a table"));
    }

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

    fn config_with_cli_sections(claude: bool, codex: bool) -> Config {
        use crate::config::CliConfig;
        let mut cli: HashMap<String, CliConfig> = HashMap::new();
        if claude {
            cli.insert(
                "claude".into(),
                CliConfig { flags: vec!["--dangerously-skip-permissions".into()] },
            );
        }
        if codex {
            cli.insert(
                "codex".into(),
                CliConfig { flags: vec!["--dangerously-bypass-approvals-and-sandbox".into()] },
            );
        }
        Config { cli, ..Config::default() }
    }

    #[test]
    fn test_check_agent_flags_ok_when_both_sections_present() {
        let c = config_with_cli_sections(true, true);
        assert!(matches!(check_agent_flags_with_config(&c), Status::Ok));
    }

    #[test]
    fn test_check_agent_flags_info_when_codex_missing() {
        // Typical upgrade path: user's pre-codex config has [cli.claude] only.
        let c = config_with_cli_sections(true, false);
        match check_agent_flags_with_config(&c) {
            Status::OkWithInfo(info) => {
                assert!(info.contains("[cli.codex]"));
                assert!(info.contains("--dangerously-bypass-approvals-and-sandbox"));
                assert!(!info.contains("[cli.claude]"));
            }
            _ => panic!("expected OkWithInfo"),
        }
    }

    #[test]
    fn test_check_agent_flags_info_when_claude_missing() {
        let c = config_with_cli_sections(false, true);
        match check_agent_flags_with_config(&c) {
            Status::OkWithInfo(info) => {
                assert!(info.contains("[cli.claude]"));
                assert!(info.contains("--dangerously-skip-permissions"));
                assert!(!info.contains("[cli.codex]"));
            }
            _ => panic!("expected OkWithInfo"),
        }
    }

    #[test]
    fn test_check_agent_flags_info_when_both_missing() {
        // Fresh Config::default() has no cli sections — matches what a user
        // hand-rolling a minimal config would look like.
        let c = Config::default();
        match check_agent_flags_with_config(&c) {
            Status::OkWithInfo(info) => {
                assert!(info.contains("[cli.claude]"));
                assert!(info.contains("[cli.codex]"));
            }
            _ => panic!("expected OkWithInfo"),
        }
    }

    #[test]
    fn test_check_agent_flags_ok_when_sections_present_but_flags_empty() {
        // Section present with `flags = []` is an intentional user choice.
        use crate::config::CliConfig;
        let mut cli: HashMap<String, CliConfig> = HashMap::new();
        cli.insert("claude".into(), CliConfig { flags: vec![] });
        cli.insert("codex".into(), CliConfig { flags: vec![] });
        let c = Config { cli, ..Config::default() };
        assert!(matches!(check_agent_flags_with_config(&c), Status::Ok));
    }

    #[test]
    fn test_render_menu_formats_headers_and_options() {
        let menu = vec![
            MenuOption {
                label: "First",
                action: Box::new(|| Ok(())),
                header_before: Some("Recommended:"),
            },
            MenuOption {
                label: "Second",
                action: Box::new(|| Ok(())),
                header_before: Some("Alternatives:"),
            },
            MenuOption {
                label: "Third",
                action: Box::new(|| Ok(())),
                header_before: None,
            },
        ];
        let mut buf = Vec::new();
        render_menu(&menu, &mut buf).unwrap();
        let rendered = String::from_utf8(buf).unwrap();

        let expected = "\n          Recommended:\n            1) First\n\n          Alternatives:\n            2) Second\n            3) Third\n";
        assert_eq!(rendered, expected);
    }

    #[test]
    fn test_build_auth_menu_structure() {
        let menu = build_auth_menu();
        assert_eq!(menu.len(), 4);
        assert_eq!(
            menu[0].label,
            "Log in once inside the container (Pro/Max subscription)"
        );
        assert_eq!(menu[0].header_before, Some("Recommended:"));
        assert_eq!(
            menu[1].label,
            "Use a long-lived OAuth token (CLAUDE_CODE_OAUTH_TOKEN)"
        );
        assert_eq!(menu[1].header_before, Some("Alternatives:"));
        assert_eq!(menu[2].label, "Use an API key (ANTHROPIC_API_KEY)");
        assert_eq!(menu[2].header_before, None);
        assert_eq!(menu[3].label, "Skip for now");
        assert_eq!(menu[3].header_before, None);
    }

    #[test]
    fn test_render_menu_matches_expected_layout() {
        let menu = build_auth_menu();
        let mut buf = Vec::new();
        render_menu(&menu, &mut buf).unwrap();
        let rendered = String::from_utf8(buf).unwrap();

        let expected = "\n          Recommended:\n            1) Log in once inside the container (Pro/Max subscription)\n\n          Alternatives:\n            2) Use a long-lived OAuth token (CLAUDE_CODE_OAUTH_TOKEN)\n            3) Use an API key (ANTHROPIC_API_KEY)\n            4) Skip for now\n";
        assert_eq!(rendered, expected);
    }

    #[test]
    fn test_auth_explanation_mentions_pro_max_and_in_container_login() {
        // Guards against accidentally dropping the Pro/Max qualifier
        // (which would re-mislead Console-only users) and the persistence hint.
        assert!(AUTH_EXPLANATION.contains("Pro/Max"));
        assert!(AUTH_EXPLANATION.contains("one-time login"));
        assert!(AUTH_EXPLANATION.contains("~/.claude"));
    }
}
