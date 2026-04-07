use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: String,
    pub state: State,
    pub workdir: String,
    pub started_unix: Option<i64>,
    pub sessions: Option<usize>,
    pub cpu_pct: Option<f64>,
    pub mem_used: Option<u64>,
    pub mem_total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Running,
    Stopped,
    Stale,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Running => "running",
            State::Stopped => "stopped",
            State::Stale => "stale",
        }
    }
}

/// Top-level entry point: gather rows, print fast pass, then live pass if TTY.
/// Stub — full implementation lands in Task 9.
pub fn run(_verbose: bool) -> Result<()> {
    Ok(())
}
