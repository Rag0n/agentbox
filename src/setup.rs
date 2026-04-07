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
    unimplemented!()
}

fn check_container_system() -> Status {
    unimplemented!()
}

fn check_config_file() -> Status {
    unimplemented!()
}

fn check_authentication() -> Status {
    unimplemented!()
}

pub fn run_setup() -> Result<()> {
    unimplemented!()
}
