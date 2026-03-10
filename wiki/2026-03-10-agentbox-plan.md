# agentbox Implementation Plan

> REQUIRED SUB-SKILL: Use workflow:executing-plans to implement this plan task-by-task.

**Goal:** Build a Rust CLI tool that runs Claude Code inside isolated Apple Containers with project-scoped filesystem access.

**Architecture:** Rust binary using clap for CLI, toml/serde for config, shelling out to `container` CLI for all container operations. Default Dockerfile embedded in binary via `include_str!`. JSON parsing for structured container status queries.

**Tech Stack:** Rust, clap 4.5, serde, toml, serde_json, sha2, num_cpus, anyhow

---

### Task 0: Project Scaffold

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `resources/Dockerfile.default`

**Step 1: Initialize the Rust project**

Run: `cargo init agentbox` from the worktree root.

Then replace `Cargo.toml` with:

```toml
[package]
name = "agentbox"
version = "0.1.0"
edition = "2021"
description = "Run AI coding agents in isolated Apple Containers"
license = "MIT"

[dependencies]
clap = { version = "4.5", features = ["derive"] }
toml = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
sha2 = "0.10"
num_cpus = "1.17"
anyhow = "1.0"
```

**Step 2: Create the default Dockerfile**

Create `resources/Dockerfile.default`:

```dockerfile
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       git jq less procps curl sudo ca-certificates ripgrep \
    && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
       -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
       > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends gh \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash -G sudo user \
    && echo "user ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/user

USER user
WORKDIR /home/user

RUN curl -fsSL https://claude.ai/install.sh | bash

ENTRYPOINT ["claude", "--dangerously-skip-permissions"]
```

**Step 3: Write minimal main.rs that compiles**

```rust
fn main() {
    println!("agentbox v0.1.0");
}
```

**Step 4: Verify it compiles**

Run: `cd agentbox && cargo build`
Expected: Compiles with no errors.

**Step 5: Commit**

Use the `workflow:commit` skill to stage and commit: "Scaffold agentbox Rust project with dependencies and default Dockerfile"

---

### Task 1: CLI Argument Parsing

**Files:**
- Modify: `src/main.rs`

**Step 1: Write the failing test**

Add to `src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_no_args_parses_as_interactive() {
        let cli = Cli::try_parse_from(["agentbox"]).unwrap();
        assert!(cli.command.is_none());
        assert!(cli.task.is_empty());
    }

    #[test]
    fn test_task_arg_parses_as_headless() {
        let cli = Cli::try_parse_from(["agentbox", "fix the tests"]).unwrap();
        assert_eq!(cli.task, vec!["fix the tests"]);
    }

    #[test]
    fn test_rm_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "rm"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Rm)));
    }

    #[test]
    fn test_ls_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "ls"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Ls)));
    }

    #[test]
    fn test_stop_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "stop"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Stop)));
    }

    #[test]
    fn test_build_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "build"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Build)));
    }

    #[test]
    fn test_config_init_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "config", "init"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Config { command: ConfigCommands::Init })));
    }

    #[test]
    fn test_profile_flag() {
        let cli = Cli::try_parse_from(["agentbox", "--profile", "mystack"]).unwrap();
        assert_eq!(cli.profile, Some("mystack".to_string()));
    }

    #[test]
    fn test_verbose_flag() {
        let cli = Cli::try_parse_from(["agentbox", "--verbose"]).unwrap();
        assert!(cli.verbose);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test`
Expected: FAIL — `Cli`, `Commands`, etc. not defined.

**Step 3: Implement the CLI structs**

Replace `src/main.rs` with:

```rust
use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "agentbox",
    about = "Run AI coding agents in isolated Apple Containers",
    version
)]
struct Cli {
    /// Task to run in headless mode
    #[arg(trailing_var_arg = true, conflicts_with = "command")]
    task: Vec<String>,

    /// Use a named profile from config
    #[arg(long)]
    profile: Option<String>,

    /// Print container commands being executed
    #[arg(long)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Remove the container for current project
    Rm,
    /// Stop the container for current project
    Stop,
    /// List all agentbox containers
    Ls,
    /// Force rebuild the container image
    Build,
    /// Configuration management
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Generate config file with commented examples
    Init,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Rm) => todo!("rm"),
        Some(Commands::Stop) => todo!("stop"),
        Some(Commands::Ls) => todo!("ls"),
        Some(Commands::Build) => todo!("build"),
        Some(Commands::Config { command }) => match command {
            ConfigCommands::Init => todo!("config init"),
        },
        None => {
            if cli.task.is_empty() {
                todo!("interactive mode")
            } else {
                todo!("headless mode: {}", cli.task.join(" "))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_no_args_parses_as_interactive() {
        let cli = Cli::try_parse_from(["agentbox"]).unwrap();
        assert!(cli.command.is_none());
        assert!(cli.task.is_empty());
    }

    #[test]
    fn test_task_arg_parses_as_headless() {
        let cli = Cli::try_parse_from(["agentbox", "fix the tests"]).unwrap();
        assert_eq!(cli.task, vec!["fix the tests"]);
    }

    #[test]
    fn test_rm_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "rm"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Rm)));
    }

    #[test]
    fn test_ls_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "ls"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Ls)));
    }

    #[test]
    fn test_stop_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "stop"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Stop)));
    }

    #[test]
    fn test_build_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "build"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Build)));
    }

    #[test]
    fn test_config_init_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "config", "init"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Config { command: ConfigCommands::Init })
        ));
    }

    #[test]
    fn test_profile_flag() {
        let cli = Cli::try_parse_from(["agentbox", "--profile", "mystack"]).unwrap();
        assert_eq!(cli.profile, Some("mystack".to_string()));
    }

    #[test]
    fn test_verbose_flag() {
        let cli = Cli::try_parse_from(["agentbox", "--verbose"]).unwrap();
        assert!(cli.verbose);
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: All 9 tests pass.

**Step 5: Commit**

Use the `workflow:commit` skill: "Add CLI argument parsing with clap derive"

---

### Task 2: Configuration Module

**Files:**
- Create: `src/config.rs`
- Modify: `src/main.rs` (add `mod config;`)

**Step 1: Write the failing test**

Create `src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.memory, "8G");
        assert!(config.cpus.is_none());
        assert!(config.dockerfile.is_none());
        assert!(config.env.is_empty());
        assert!(config.profiles.is_empty());
    }

    #[test]
    fn test_parse_full_config() {
        let toml_str = r#"
            cpus = 4
            memory = "16G"
            dockerfile = "/path/to/Dockerfile"

            [env]
            GH_TOKEN = ""
            LINEAR_API_KEY = "abc123"

            [profiles.mystack]
            dockerfile = "/path/to/mystack.Dockerfile"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.cpus, Some(4));
        assert_eq!(config.memory, "16G");
        assert_eq!(config.dockerfile, Some("/path/to/Dockerfile".into()));
        assert_eq!(config.env.get("GH_TOKEN").unwrap(), "");
        assert_eq!(config.env.get("LINEAR_API_KEY").unwrap(), "abc123");
        assert_eq!(
            config.profiles.get("mystack").unwrap().dockerfile,
            "/path/to/mystack.Dockerfile"
        );
    }

    #[test]
    fn test_parse_empty_config() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.memory, "8G");
    }

    #[test]
    fn test_effective_cpus_from_config() {
        let mut config = Config::default();
        config.cpus = Some(2);
        assert_eq!(config.effective_cpus(), 2);
    }

    #[test]
    fn test_effective_cpus_default_half_host() {
        let config = Config::default();
        let cpus = config.effective_cpus();
        assert!(cpus >= 1);
        assert!(cpus <= num_cpus::get());
    }

    #[test]
    fn test_config_init_content() {
        let content = Config::init_template();
        assert!(content.contains("# cpus"));
        assert!(content.contains("# memory"));
        assert!(content.contains("# [env]"));
        assert!(content.contains("# [profiles."));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test`
Expected: FAIL — `Config` not defined.

**Step 3: Implement config module**

Write `src/config.rs`:

```rust
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub cpus: Option<usize>,
    pub memory: String,
    pub dockerfile: Option<PathBuf>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub profiles: HashMap<String, Profile>,
}

#[derive(Debug, Deserialize)]
pub struct Profile {
    pub dockerfile: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cpus: None,
            memory: "8G".to_string(),
            dockerfile: None,
            env: HashMap::new(),
            profiles: HashMap::new(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path();
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)
                .with_context(|| format!("failed to read {}", config_path.display()))?;
            toml::from_str(&content)
                .with_context(|| format!("failed to parse {}", config_path.display()))
        } else {
            Ok(Self::default())
        }
    }

    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("agentbox")
            .join("config.toml")
    }

    pub fn effective_cpus(&self) -> usize {
        self.cpus.unwrap_or_else(|| {
            let total = num_cpus::get();
            (total / 2).max(1)
        })
    }

    pub fn init_template() -> &'static str {
        r#"# agentbox configuration
# See: https://github.com/<user>/agentbox

# Resources (auto-detected from host if not set)
# cpus = 4          # default: half of host cores
# memory = "8G"     # default: 8G

# Override the default Dockerfile for all projects
# dockerfile = "/path/to/my-default.Dockerfile"

# Environment variables to pass into container
# [env]
# GH_TOKEN = ""           # empty = inherit from host env
# LINEAR_API_KEY = "abc"  # literal value

# Named profiles with custom Dockerfiles
# [profiles.mystack]
# dockerfile = "/path/to/mystack.Dockerfile"
"#
    }
}

// tests below...
```

Add `dirs = "6.0"` to `Cargo.toml` dependencies.

Add `mod config;` to `src/main.rs`.

**Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: All tests pass (previous + new config tests).

**Step 5: Commit**

Use the `workflow:commit` skill: "Add configuration module with TOML parsing and defaults"

---

### Task 3: Git Identity Detection

**Files:**
- Create: `src/git.rs`
- Modify: `src/main.rs` (add `mod git;`)

**Step 1: Write the failing test**

Create `src/git.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_env_vars_returns_four_vars() {
        // This test relies on git being configured on the host.
        // If git config is not set, it returns an empty vec.
        let vars = git_env_vars();
        // Either 0 (not configured) or 4 (all four vars)
        assert!(vars.len() == 0 || vars.len() == 4);
    }

    #[test]
    fn test_git_env_var_names() {
        let vars = git_env_vars();
        if !vars.is_empty() {
            let keys: Vec<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
            assert!(keys.contains(&"GIT_AUTHOR_NAME"));
            assert!(keys.contains(&"GIT_AUTHOR_EMAIL"));
            assert!(keys.contains(&"GIT_COMMITTER_NAME"));
            assert!(keys.contains(&"GIT_COMMITTER_EMAIL"));
        }
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test`
Expected: FAIL — `git_env_vars` not defined.

**Step 3: Implement git module**

```rust
use std::process::Command;

/// Read git user.name and user.email from host and return
/// as environment variable pairs for the container.
pub fn git_env_vars() -> Vec<(String, String)> {
    let name = git_config("user.name");
    let email = git_config("user.email");

    match (name, email) {
        (Some(name), Some(email)) => vec![
            ("GIT_AUTHOR_NAME".into(), name.clone()),
            ("GIT_AUTHOR_EMAIL".into(), email.clone()),
            ("GIT_COMMITTER_NAME".into(), name),
            ("GIT_COMMITTER_EMAIL".into(), email),
        ],
        _ => vec![],
    }
}

fn git_config(key: &str) -> Option<String> {
    Command::new("git")
        .args(["config", "--global", key])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

// tests below...
```

Add `mod git;` to `src/main.rs`.

**Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: All tests pass.

**Step 5: Commit**

Use the `workflow:commit` skill: "Add git identity detection from host config"

---

### Task 4: Container CLI Wrapper

**Files:**
- Create: `src/container.rs`
- Modify: `src/main.rs` (add `mod container;`)

This is the core module. It wraps all `container` CLI calls.

**Step 1: Write the failing test**

Create `src/container.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_container_name_from_path() {
        let name = container_name("/Users/alex/Dev/myapp");
        assert!(name.starts_with("agentbox-myapp-"));
        assert_eq!(name.len(), "agentbox-myapp-".len() + 6); // 6-char hash
    }

    #[test]
    fn test_container_name_uniqueness() {
        let a = container_name("/Users/alex/work/myapp");
        let b = container_name("/Users/alex/personal/myapp");
        assert_ne!(a, b); // different paths, different hashes
    }

    #[test]
    fn test_container_name_stability() {
        let a = container_name("/Users/alex/Dev/myapp");
        let b = container_name("/Users/alex/Dev/myapp");
        assert_eq!(a, b); // same path, same name
    }

    #[test]
    fn test_build_run_args() {
        let opts = RunOpts {
            name: "agentbox-myapp-abc123".into(),
            image: "agentbox:default".into(),
            workdir: "/Users/alex/Dev/myapp".into(),
            cpus: 4,
            memory: "8G".into(),
            env_vars: vec![
                ("GH_TOKEN".into(), "tok123".into()),
            ],
            volumes: vec![
                ("/Users/alex/Dev/myapp:/Users/alex/Dev/myapp".into()),
            ],
            interactive: true,
            task: None,
        };
        let args = opts.to_run_args();
        assert!(args.contains(&"--name".to_string()));
        assert!(args.contains(&"agentbox-myapp-abc123".to_string()));
        assert!(args.contains(&"--cpus".to_string()));
        assert!(args.contains(&"4".to_string()));
        assert!(args.contains(&"--memory".to_string()));
        assert!(args.contains(&"8G".to_string()));
        assert!(args.contains(&"--interactive".to_string()));
        assert!(args.contains(&"--tty".to_string()));
    }

    #[test]
    fn test_build_run_args_headless() {
        let opts = RunOpts {
            name: "agentbox-myapp-abc123".into(),
            image: "agentbox:default".into(),
            workdir: "/Users/alex/Dev/myapp".into(),
            cpus: 2,
            memory: "4G".into(),
            env_vars: vec![],
            volumes: vec![],
            interactive: false,
            task: Some("fix the tests".into()),
        };
        let args = opts.to_run_args();
        assert!(!args.contains(&"--interactive".to_string()));
        assert!(!args.contains(&"--tty".to_string()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"fix the tests".to_string()));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test`
Expected: FAIL — types not defined.

**Step 3: Implement container module**

```rust
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::process::Command;

/// Generate a deterministic container name from a project path.
pub fn container_name(path: &str) -> String {
    let dir_name = std::path::Path::new(path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let hash = format!("{:x}", Sha256::digest(path.as_bytes()));
    let short_hash = &hash[..6];
    format!("agentbox-{}-{}", dir_name, short_hash)
}

#[derive(Debug)]
pub struct RunOpts {
    pub name: String,
    pub image: String,
    pub workdir: String,
    pub cpus: usize,
    pub memory: String,
    pub env_vars: Vec<(String, String)>,
    pub volumes: Vec<String>,
    pub interactive: bool,
    pub task: Option<String>,
}

impl RunOpts {
    pub fn to_run_args(&self) -> Vec<String> {
        let mut args = vec!["run".to_string()];

        args.extend(["--name".into(), self.name.clone()]);
        args.extend(["--cpus".into(), self.cpus.to_string()]);
        args.extend(["--memory".into(), self.memory.clone()]);
        args.extend(["--workdir".into(), self.workdir.clone()]);

        if self.interactive {
            args.push("--interactive".into());
            args.push("--tty".into());
        }

        for (key, val) in &self.env_vars {
            args.extend(["--env".into(), format!("{}={}", key, val)]);
        }

        for vol in &self.volumes {
            args.extend(["--volume".into(), vol.clone()]);
        }

        args.push(self.image.clone());

        // Append task args after image (passed to entrypoint)
        if let Some(task) = &self.task {
            args.extend(["-p".into(), task.clone()]);
        }

        args
    }
}

#[derive(Debug, PartialEq)]
pub enum ContainerStatus {
    Running,
    Stopped,
    NotFound,
}

/// Check container status using `container inspect --format json`.
pub fn status(name: &str) -> Result<ContainerStatus> {
    let output = Command::new("container")
        .args(["inspect", "--format", "json", name])
        .output()
        .context("failed to run 'container inspect'")?;

    if !output.status.success() {
        return Ok(ContainerStatus::NotFound);
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("failed to parse container inspect output")?;

    let running = json
        .pointer("/State/Running")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if running {
        Ok(ContainerStatus::Running)
    } else {
        Ok(ContainerStatus::Stopped)
    }
}

/// Run a container with the given options.
pub fn run(opts: &RunOpts, verbose: bool) -> Result<()> {
    let args = opts.to_run_args();
    if verbose {
        eprintln!("[agentbox] container {}", args.join(" "));
    }
    let status = Command::new("container")
        .args(&args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .context("failed to run 'container run'")?;

    if !status.success() {
        bail!("container exited with status {}", status);
    }
    Ok(())
}

/// Exec into a running container.
pub fn exec(name: &str, task: Option<&str>, verbose: bool) -> Result<()> {
    let mut args = vec!["exec".to_string()];
    if task.is_none() {
        args.push("--interactive".into());
        args.push("--tty".into());
    }
    args.push(name.to_string());
    args.push("claude".into());
    args.push("--dangerously-skip-permissions".into());
    if let Some(t) = task {
        args.extend(["-p".into(), t.to_string()]);
    }

    if verbose {
        eprintln!("[agentbox] container {}", args.join(" "));
    }
    let status = Command::new("container")
        .args(&args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .context("failed to run 'container exec'")?;

    if !status.success() {
        bail!("container exec exited with status {}", status);
    }
    Ok(())
}

/// Start a stopped container.
pub fn start(name: &str, verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("[agentbox] container start {}", name);
    }
    let status = Command::new("container")
        .args(["start", name])
        .status()
        .context("failed to run 'container start'")?;

    if !status.success() {
        bail!("container start failed");
    }
    Ok(())
}

/// Stop a running container.
pub fn stop(name: &str, verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("[agentbox] container stop {}", name);
    }
    let status = Command::new("container")
        .args(["stop", name])
        .status()
        .context("failed to run 'container stop'")?;

    if !status.success() {
        bail!("container stop failed");
    }
    Ok(())
}

/// Remove a container.
pub fn rm(name: &str, verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("[agentbox] container rm -f {}", name);
    }
    let status = Command::new("container")
        .args(["rm", "-f", name])
        .status()
        .context("failed to run 'container rm'")?;

    if !status.success() {
        bail!("container rm failed");
    }
    Ok(())
}

/// List all agentbox containers.
pub fn list(verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("[agentbox] container ls --all --format json");
    }
    let output = Command::new("container")
        .args(["ls", "--all", "--format", "json"])
        .output()
        .context("failed to run 'container ls'")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Filter to only agentbox containers and display
    for line in stdout.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
            let name = json.pointer("/Names")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if name.starts_with("agentbox-") {
                let state = json.pointer("/State")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                println!("{}\t{}", name, state);
            }
        }
    }
    Ok(())
}

// tests below...
```

Add `mod container;` to `src/main.rs`.

**Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: All tests pass.

**Step 5: Commit**

Use the `workflow:commit` skill: "Add container CLI wrapper with run, exec, start, stop, rm, ls"

---

### Task 5: Image Build & Caching Module

**Files:**
- Create: `src/image.rs`
- Modify: `src/main.rs` (add `mod image;`)

**Step 1: Write the failing test**

Create `src/image.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_embedded_dockerfile_not_empty() {
        assert!(!DEFAULT_DOCKERFILE.is_empty());
        assert!(DEFAULT_DOCKERFILE.contains("debian:bookworm-slim"));
    }

    #[test]
    fn test_dockerfile_checksum_deterministic() {
        let a = checksum("hello world");
        let b = checksum("hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn test_dockerfile_checksum_changes() {
        let a = checksum("version 1");
        let b = checksum("version 2");
        assert_ne!(a, b);
    }

    #[test]
    fn test_resolve_dockerfile_project_first() {
        let tmp = tempfile::tempdir().unwrap();
        let project_df = tmp.path().join("agentbox.Dockerfile");
        fs::write(&project_df, "FROM test:project").unwrap();

        let (content, tag) = resolve_dockerfile(
            tmp.path(),
            None, // no profile
            &Config::default(),
        ).unwrap();

        assert!(content.contains("FROM test:project"));
        assert!(tag.starts_with("agentbox:project-"));
    }

    #[test]
    fn test_resolve_dockerfile_falls_through_to_default() {
        let tmp = tempfile::tempdir().unwrap();
        // No project Dockerfile, no profile, no config override

        let (content, tag) = resolve_dockerfile(
            tmp.path(),
            None,
            &Config::default(),
        ).unwrap();

        assert!(content.contains("debian:bookworm-slim"));
        assert_eq!(tag, "agentbox:default");
    }

    #[test]
    fn test_needs_build_no_cache() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(needs_build("test content", "default", tmp.path()));
    }

    #[test]
    fn test_needs_build_matching_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let content = "test dockerfile content";
        let hash = checksum(content);
        let cache_file = tmp.path().join("default.sha256");
        fs::write(&cache_file, &hash).unwrap();

        assert!(!needs_build(content, "default", tmp.path()));
    }

    #[test]
    fn test_needs_build_stale_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_file = tmp.path().join("default.sha256");
        fs::write(&cache_file, "old_hash").unwrap();

        assert!(needs_build("new content", "default", tmp.path()));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test`
Expected: FAIL — types not defined.

**Step 3: Implement image module**

Add `tempfile = "3.14"` to `[dev-dependencies]` in `Cargo.toml` and `dirs = "6.0"` to `[dependencies]`.

```rust
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::config::Config;

pub const DEFAULT_DOCKERFILE: &str = include_str!("../resources/Dockerfile.default");

pub fn checksum(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

/// Resolve which Dockerfile to use. Returns (content, image_tag).
pub fn resolve_dockerfile(
    project_dir: &Path,
    profile: Option<&str>,
    config: &Config,
) -> Result<(String, String)> {
    let dir_name = project_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();

    // 1. Per-project Dockerfile
    let project_df = project_dir.join("agentbox.Dockerfile");
    if project_df.exists() {
        let content = std::fs::read_to_string(&project_df)
            .with_context(|| format!("failed to read {}", project_df.display()))?;
        return Ok((content, format!("agentbox:project-{}", dir_name)));
    }

    // 2. Named profile
    if let Some(name) = profile {
        if let Some(p) = config.profiles.get(name) {
            let content = std::fs::read_to_string(&p.dockerfile)
                .with_context(|| format!("failed to read profile '{}' Dockerfile: {}", name, p.dockerfile.display()))?;
            return Ok((content, format!("agentbox:profile-{}", name)));
        } else {
            anyhow::bail!("profile '{}' not found in config", name);
        }
    }

    // 3. Global default override
    if let Some(ref df) = config.dockerfile {
        let content = std::fs::read_to_string(df)
            .with_context(|| format!("failed to read {}", df.display()))?;
        return Ok((content, "agentbox:default".into()));
    }

    // 4. Built-in default
    Ok((DEFAULT_DOCKERFILE.to_string(), "agentbox:default".into()))
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("~/.cache"))
        .join("agentbox")
}

pub fn needs_build(dockerfile_content: &str, cache_key: &str, cache_path: &Path) -> bool {
    let current_hash = checksum(dockerfile_content);
    let cache_file = cache_path.join(format!("{}.sha256", cache_key));

    match std::fs::read_to_string(&cache_file) {
        Ok(cached_hash) => cached_hash.trim() != current_hash,
        Err(_) => true,
    }
}

pub fn save_cache(dockerfile_content: &str, cache_key: &str, cache_path: &Path) -> Result<()> {
    std::fs::create_dir_all(cache_path)?;
    let hash = checksum(dockerfile_content);
    let cache_file = cache_path.join(format!("{}.sha256", cache_key));
    std::fs::write(&cache_file, &hash)?;
    Ok(())
}

/// Build an image using `container build`.
pub fn build(tag: &str, dockerfile_content: &str, verbose: bool) -> Result<()> {
    // Write Dockerfile to a temp dir for building
    let tmp = tempfile::tempdir().context("failed to create temp dir")?;
    let df_path = tmp.path().join("Dockerfile");
    std::fs::write(&df_path, dockerfile_content)?;

    if verbose {
        eprintln!("[agentbox] container build -t {} -f {} {}", tag, df_path.display(), tmp.path().display());
    }

    let status = std::process::Command::new("container")
        .args([
            "build",
            "-t", tag,
            "-f", &df_path.to_string_lossy(),
            &tmp.path().to_string_lossy(),
        ])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .context("failed to run 'container build'")?;

    if !status.success() {
        anyhow::bail!("container build failed");
    }
    Ok(())
}

// tests below...
```

Add `tempfile = "3.14"` to main `[dependencies]` too (used by `build` function).

**Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: All tests pass.

**Step 5: Commit**

Use the `workflow:commit` skill: "Add image build and caching module with Dockerfile resolution"

---

### Task 6: Wire Up Main — Config Init Command

**Files:**
- Modify: `src/main.rs`

**Step 1: Implement `config init`**

In `src/main.rs`, replace the `todo!("config init")` with:

```rust
Some(Commands::Config { command }) => match command {
    ConfigCommands::Init => {
        let path = config::Config::config_path();
        if path.exists() {
            eprintln!("Config already exists at {}", path.display());
            eprintln!("Edit it directly or remove it first.");
            std::process::exit(1);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, config::Config::init_template())?;
        println!("Created {}", path.display());
        Ok(())
    }
},
```

**Step 2: Verify it compiles**

Run: `cargo build`
Expected: Compiles.

**Step 3: Commit**

Use the `workflow:commit` skill: "Wire up config init command"

---

### Task 7: Wire Up Main — Core Lifecycle (run/attach/exec)

**Files:**
- Modify: `src/main.rs`

This is the main orchestration logic.

**Step 1: Implement the start/attach/create flow**

Replace the `None` match arm in main with:

```rust
None => {
    let config = config::Config::load()?;
    let cwd = std::env::current_dir()?;
    let cwd_str = cwd.to_string_lossy().to_string();
    let name = container::container_name(&cwd_str);
    let task_str = if cli.task.is_empty() {
        None
    } else {
        Some(cli.task.join(" "))
    };

    // Check container status
    match container::status(&name)? {
        container::ContainerStatus::Running => {
            // Attach to running container
            container::exec(&name, task_str.as_deref(), cli.verbose)?;
        }
        container::ContainerStatus::Stopped => {
            // Check if image needs rebuild
            let (dockerfile_content, image_tag) =
                image::resolve_dockerfile(&cwd, cli.profile.as_deref(), &config)?;
            let cache_key = image_tag.replace(':', "-");
            if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
                eprintln!("Image changed, recreating container...");
                container::rm(&name, cli.verbose)?;
                image::build(&image_tag, &dockerfile_content, cli.verbose)?;
                image::save_cache(&dockerfile_content, &cache_key, &image::cache_dir())?;
                create_and_run(&name, &image_tag, &cwd_str, &config, task_str.as_deref(), cli.verbose)?;
            } else {
                container::start(&name, cli.verbose)?;
                container::exec(&name, task_str.as_deref(), cli.verbose)?;
            }
        }
        container::ContainerStatus::NotFound => {
            // Build image if needed, then create container
            let (dockerfile_content, image_tag) =
                image::resolve_dockerfile(&cwd, cli.profile.as_deref(), &config)?;
            let cache_key = image_tag.replace(':', "-");
            if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
                eprintln!("Building image...");
                image::build(&image_tag, &dockerfile_content, cli.verbose)?;
                image::save_cache(&dockerfile_content, &cache_key, &image::cache_dir())?;
            }
            create_and_run(&name, &image_tag, &cwd_str, &config, task_str.as_deref(), cli.verbose)?;
        }
    }
    Ok(())
}
```

Add helper function:

```rust
fn create_and_run(
    name: &str,
    image: &str,
    workdir: &str,
    config: &config::Config,
    task: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let home = dirs::home_dir().context("cannot determine home directory")?;

    // Collect env vars: config env + git identity
    let mut env_vars: Vec<(String, String)> = Vec::new();

    // Config env vars (empty value = inherit from host)
    for (key, val) in &config.env {
        let value = if val.is_empty() {
            std::env::var(key).unwrap_or_default()
        } else {
            val.clone()
        };
        if !value.is_empty() {
            env_vars.push((key.clone(), value));
        }
    }

    // Git identity
    env_vars.extend(git::git_env_vars());

    // Ensure ~/.claude.json exists
    let claude_json = home.join(".claude.json");
    if !claude_json.exists() {
        std::fs::write(&claude_json, "{}")?;
    }

    let opts = container::RunOpts {
        name: name.into(),
        image: image.into(),
        workdir: workdir.into(),
        cpus: config.effective_cpus(),
        memory: config.memory.clone(),
        env_vars,
        volumes: vec![
            format!("{}:{}", workdir, workdir),
            format!("{}:/home/user/.claude", home.join(".claude").display()),
            format!("{}:/home/user/.claude.json", claude_json.display()),
        ],
        interactive: task.is_none(),
        task: task.map(String::from),
    };

    container::run(&opts, verbose)
}
```

**Step 2: Verify it compiles**

Run: `cargo build`
Expected: Compiles.

**Step 3: Commit**

Use the `workflow:commit` skill: "Wire up core lifecycle: run, attach, exec with image auto-build"

---

### Task 8: Wire Up Main — Remaining Commands (rm, stop, ls, build)

**Files:**
- Modify: `src/main.rs`

**Step 1: Implement remaining commands**

Replace the `todo!()` arms:

```rust
Some(Commands::Rm) => {
    let cwd = std::env::current_dir()?;
    let name = container::container_name(&cwd.to_string_lossy());
    container::rm(&name, cli.verbose)?;
    println!("Removed {}", name);
    Ok(())
}
Some(Commands::Stop) => {
    let cwd = std::env::current_dir()?;
    let name = container::container_name(&cwd.to_string_lossy());
    container::stop(&name, cli.verbose)?;
    println!("Stopped {}", name);
    Ok(())
}
Some(Commands::Ls) => {
    container::list(cli.verbose)?;
    Ok(())
}
Some(Commands::Build) => {
    let config = config::Config::load()?;
    let cwd = std::env::current_dir()?;
    let (dockerfile_content, image_tag) =
        image::resolve_dockerfile(&cwd, cli.profile.as_deref(), &config)?;
    let cache_key = image_tag.replace(':', "-");
    eprintln!("Building {}...", image_tag);
    image::build(&image_tag, &dockerfile_content, cli.verbose)?;
    image::save_cache(&dockerfile_content, &cache_key, &image::cache_dir())?;
    println!("Built {}", image_tag);
    Ok(())
}
```

**Step 2: Verify it compiles**

Run: `cargo build`
Expected: Compiles.

**Step 3: Run all tests**

Run: `cargo test`
Expected: All tests pass.

**Step 4: Commit**

Use the `workflow:commit` skill: "Wire up rm, stop, ls, and build commands"

---

### Task 9: Prerequisite Check (Apple Container CLI)

**Files:**
- Modify: `src/main.rs`

**Step 1: Add container CLI check at startup**

Add before CLI parsing in `main()`:

```rust
fn check_prerequisites() -> Result<()> {
    let output = std::process::Command::new("container")
        .arg("system")
        .arg("version")
        .output();

    match output {
        Ok(o) if o.status.success() => Ok(()),
        _ => {
            eprintln!("Error: Apple Container CLI is not installed or not running.");
            eprintln!("");
            eprintln!("Install it from: https://github.com/apple/container");
            eprintln!("Then run: container system start");
            std::process::exit(1);
        }
    }
}
```

Call `check_prerequisites()?;` at the top of `main()`.

**Step 2: Verify it compiles**

Run: `cargo build`
Expected: Compiles.

**Step 3: Commit**

Use the `workflow:commit` skill: "Add Apple Container CLI prerequisite check"

---

### Task 10: README

**Files:**
- Create: `README.md`

**Step 1: Write README**

```markdown
# agentbox

Run AI coding agents in isolated Apple Containers. Your project directory is mounted read/write — everything else on your filesystem is inaccessible.

Currently supports Claude Code. More agents planned.

## Requirements

- macOS 26+ on Apple Silicon
- [Apple Container CLI](https://github.com/apple/container)

## Install

### Cargo

```bash
cargo install agentbox
```

### Pre-built binary

```bash
curl -fsSL https://github.com/<user>/agentbox/releases/latest/download/agentbox-darwin-arm64.tar.gz | tar xz
mv agentbox ~/.local/bin/
```

### Homebrew (coming soon)

```bash
brew install agentbox
```

## Quick Start

```bash
# Start interactive Claude session in current project
agentbox

# Run a task headlessly
agentbox "fix the failing tests"

# List all containers
agentbox ls

# Stop the container
agentbox stop

# Remove the container
agentbox rm

# Force rebuild the image
agentbox build
```

## Configuration

Optional. Create with `agentbox config init`.

Located at `~/.config/agentbox/config.toml`:

```toml
# Resources
cpus = 4          # default: half of host cores
memory = "8G"     # default: 8G

# Override default Dockerfile
dockerfile = "/path/to/my.Dockerfile"

# Environment variables passed into container
[env]
GH_TOKEN = ""           # empty = inherit from host
LINEAR_API_KEY = "abc"  # literal value

# Named profiles
[profiles.mystack]
dockerfile = "/path/to/mystack.Dockerfile"
```

## Custom Dockerfiles

### Per-project

Place an `agentbox.Dockerfile` in your project root. It's detected automatically.

Can extend the default image:

```dockerfile
FROM agentbox:default

RUN sudo apt-get update && sudo apt-get install -y nodejs
```

### Profiles

Define in config, use with `--profile`:

```bash
agentbox --profile mystack
```

## What's Mounted

| Host | Container | Access |
|------|-----------|--------|
| Current directory | Same path | read/write |
| `~/.claude` | `/home/user/.claude` | read/write |
| `~/.claude.json` | `/home/user/.claude.json` | read/write |

## What's Isolated

Claude **cannot** access `~/.ssh`, `~/.aws`, `~/.gnupg`, or any other host directory.

## How It Works

agentbox uses Apple Containers to run a lightweight Linux VM with Claude Code. Containers are persistent (reused across sessions) and auto-named by project directory. Images auto-rebuild when the Dockerfile changes.
```

**Step 2: Commit**

Use the `workflow:commit` skill: "Add README with installation and usage docs"

---

### Task 11: End-to-End Smoke Test

**Files:** none (manual testing)

**Step 1: Build release binary**

Run: `cargo build --release`
Expected: Compiles.

**Step 2: Test help output**

Run: `./target/release/agentbox --help`
Expected: Shows usage with all commands.

**Step 3: Test config init**

Run: `./target/release/agentbox config init`
Expected: Creates `~/.config/agentbox/config.toml`.

**Step 4: Test in a project directory (requires Apple Container CLI)**

Run: `cd /tmp && mkdir test-project && cd test-project && /path/to/agentbox`
Expected: Builds default image, creates container, launches Claude.

**Step 5: Commit any fixes**

Use the `workflow:commit` skill if fixes were needed.
