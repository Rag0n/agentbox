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
use std::process::Command;
use crate::config::Config;

pub enum Status {
    /// Check passed.
    Ok,
    /// Auto-fixable: orchestrator will run `fix()` with no prompt.
    AutoFix {
        explanation: String,
        fix: Box<dyn FnOnce() -> Result<()>>,
    },
    /// User must act out of band; we print instructions.
    Manual {
        explanation: String,
        next_steps: String,
    },
    /// Interactive menu (used only by auth check).
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
}

fn check_container_cli() -> Status {
    match Command::new("container")
        .arg("--version")
        .output()
    {
        Ok(_) => Status::Ok,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Status::Manual {
                explanation: "Apple Container CLI is not installed or not on PATH.".to_string(),
                next_steps: "Download and install from: https://github.com/apple/container/releases".to_string(),
            }
        }
        Err(e) => Status::Errored(anyhow::anyhow!("Failed to check container CLI: {}", e)),
    }
}

/// Parse `container system status` output and check if the system is running.
/// Looking for a line that matches: "status running"
fn parse_system_status(stdout: &str) -> bool {
    stdout
        .lines()
        .any(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            parts.len() == 2 && parts[0] == "status" && parts[1] == "running"
        })
}

fn check_container_system() -> Status {
    match Command::new("container")
        .args(["system", "status"])
        .output()
    {
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
            explanation: "Config file does not exist. Creating it from the default template...".to_string(),
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

/// Pure function that decides whether authentication is reachable.
/// Separated for testing purposes.
fn decide_auth(
    config: &Config,
    host_env: &dyn Fn(&str) -> Option<String>,
    credentials_exists: bool,
) -> bool {
    // Check for literal non-empty values in config
    if let Some(val) = config.env.get("ANTHROPIC_API_KEY") {
        if !val.is_empty() {
            return true;
        }
    }
    if let Some(val) = config.env.get("CLAUDE_CODE_OAUTH_TOKEN") {
        if !val.is_empty() {
            return true;
        }
    }

    // Check for empty (inherit) in config + host env
    if let Some(val) = config.env.get("ANTHROPIC_API_KEY") {
        if val.is_empty() && host_env("ANTHROPIC_API_KEY").is_some() {
            return true;
        }
    }
    if let Some(val) = config.env.get("CLAUDE_CODE_OAUTH_TOKEN") {
        if val.is_empty() && host_env("CLAUDE_CODE_OAUTH_TOKEN").is_some() {
            return true;
        }
    }

    // Check for on-disk credentials
    credentials_exists
}

/// Idempotently add a key to the [env] section of the config file.
/// Uses toml_edit to preserve comments and formatting.
/// If the key already exists, this is a no-op.
fn ensure_env_var_in_config(key: &str) -> Result<()> {
    use toml_edit::{value, table, DocumentMut};

    let path = Config::config_path();
    let content = std::fs::read_to_string(&path)?;
    let mut doc: DocumentMut = content.parse()?;

    let env_tbl = match doc.entry("env").or_insert(table()).as_table_mut() {
        Some(t) => t,
        None => anyhow::bail!("'env' in config is not a table"),
    };

    // Idempotent: if key already exists, do nothing
    if env_tbl.contains_key(key) {
        return Ok(());
    }

    env_tbl.insert(key, value(""));
    std::fs::write(&path, doc.to_string())?;
    Ok(())
}

const AUTH_EXPLANATION: &str = r#"macOS Keychain isn't reachable from the Linux container, so
Claude Code needs credentials via env var, or a one-time login
from inside the container (the token persists under ~/.claude)."#;

fn build_auth_menu() -> Vec<MenuOption> {
    vec![
        MenuOption {
            label: "Log in interactively inside the container (recommended for Pro/Max)",
            action: Box::new(|| {
                println!("\n        Next step: run `agentbox`, then inside Claude type `/login`.");
                println!("        Your token will be saved under ~/.claude and persist across sessions.");
                Ok(())
            }),
        },
        MenuOption {
            label: "Use an API key (ANTHROPIC_API_KEY)",
            action: Box::new(|| {
                println!("\n        Run this in your shell (and add it to ~/.zshrc / ~/.bashrc for next time):");
                println!("\n            export ANTHROPIC_API_KEY=\"sk-...\"");
                println!("\n        Add `ANTHROPIC_API_KEY = \"\"` under [env] in your config automatically? [Y/n]");
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if input.trim().is_empty() || input.trim().eq_ignore_ascii_case("y") {
                    ensure_env_var_in_config("ANTHROPIC_API_KEY")?;
                    println!("        ✓ Updated ~/.config/agentbox/config.toml");
                    println!("        Then re-run `agentbox setup` in a new shell to confirm.");
                }
                Ok(())
            }),
        },
        MenuOption {
            label: "Use a long-lived OAuth token (CLAUDE_CODE_OAUTH_TOKEN)",
            action: Box::new(|| {
                println!("\n        Requires the host `claude` CLI. Run this on your Mac first:");
                println!("\n            claude setup-token");
                println!("\n        Copy the token, then run in your shell (and add it to ~/.zshrc / ~/.bashrc):");
                println!("\n            export CLAUDE_CODE_OAUTH_TOKEN=\"your-token-here\"");
                println!("\n        Add `CLAUDE_CODE_OAUTH_TOKEN = \"\"` under [env] in your config automatically? [Y/n]");
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if input.trim().is_empty() || input.trim().eq_ignore_ascii_case("y") {
                    ensure_env_var_in_config("CLAUDE_CODE_OAUTH_TOKEN")?;
                    println!("        ✓ Updated ~/.config/agentbox/config.toml");
                    println!("        Then re-run `agentbox setup` in a new shell to confirm.");
                }
                Ok(())
            }),
        },
        MenuOption {
            label: "Skip for now",
            action: Box::new(|| {
                println!("\n        You can re-run `agentbox setup` at any time to set up authentication.");
                Ok(())
            }),
        },
    ]
}

fn check_authentication() -> Status {
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => return Status::Errored(e),
    };

    let host_env = |key: &str| std::env::var(key).ok();
    let credentials_path = dirs::home_dir()
        .map(|h| h.join(".claude/.credentials.json"))
        .and_then(|p| {
            std::fs::metadata(&p).ok().and_then(|m| {
                if m.len() > 0 {
                    Some(p)
                } else {
                    None
                }
            })
        });

    let credentials_exists = credentials_path.is_some();

    if decide_auth(&config, &host_env, credentials_exists) {
        Status::Ok
    } else {
        Status::Interactive {
            explanation: AUTH_EXPLANATION.to_string(),
            menu: build_auth_menu(),
        }
    }
}

fn print_indented(text: &str, indent: usize) {
    for line in text.lines() {
        println!("{}{}", " ".repeat(indent), line);
    }
}

fn prompt_menu(menu: Vec<MenuOption>) -> Result<()> {
    for (i, option) in menu.iter().enumerate() {
        println!("          {}) {}", i + 1, option.label);
    }
    print!("        > ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let choice = input.trim().parse::<usize>().unwrap_or(0);

    if choice > 0 && choice <= menu.len() {
        let mut menu_vec = menu;
        let option = menu_vec.remove(choice - 1);
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
        ("Authentication", check_authentication),
    ];

    let mut passed = 0;

    for (i, (name, check_fn)) in checks.iter().enumerate() {
        print!("  [{}/{}] {:<30} ", i + 1, checks.len(), name);
        match check_fn() {
            Status::Ok => {
                println!("✓");
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
            Status::Manual {
                explanation,
                next_steps,
            } => {
                println!("✗");
                print_indented(&explanation, 8);
                print_indented(&next_steps, 8);
            }
            Status::Interactive {
                explanation,
                menu,
            } => {
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
