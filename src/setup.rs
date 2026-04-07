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

fn check_authentication() -> Status {
    unimplemented!()
}

pub fn run_setup() -> Result<()> {
    unimplemented!()
}
