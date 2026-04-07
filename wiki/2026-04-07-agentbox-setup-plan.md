# agentbox setup Implementation Plan

> **For agentic workers:** REQUIRED: Use workflow:subagent-driven-development to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement a guided `agentbox setup` command that checks four prerequisites (Apple Container CLI, container system running, config file, authentication) and interactively fixes issues.

**Architecture:** New `src/setup.rs` module with pure check functions, a TOML-edit helper for idempotent config mutations, and a thin orchestrator that renders output and prompts for auth choices. Wired into `main.rs` via a new `Commands::Setup` variant.

**Tech Stack:** `toml_edit` crate for comment-preserving config edits; `std::io::stdin()` for prompts; existing `Config::load()` for parsing.

---

## Task 1: Add toml_edit dependency

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add toml_edit to dependencies**

Open `Cargo.toml` and find the `[dependencies]` section (around line 25). Add:

```toml
toml_edit = "0.22"
```

Place it alphabetically between `toml` and `uuid`.

- [ ] **Step 2: Run `cargo check` to verify the dependency compiles**

```bash
cargo check
```

Expected: No errors. The crate should download and compile cleanly.

---

## Task 2: Create src/setup.rs skeleton with enums and function stubs

**Files:**
- Create: `src/setup.rs`

- [ ] **Step 1: Create the file with module-level doc and imports**

```rust
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
```

- [ ] **Step 2: Define the Status enum**

```rust
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
```

- [ ] **Step 3: Add function stubs for the four checks**

```rust
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
```

- [ ] **Step 4: Add the module to src/main.rs**

Open `src/main.rs` and add near the top (after the other `mod` declarations, around line 8):

```rust
mod setup;
```

- [ ] **Step 5: Run `cargo check` to verify the skeleton compiles**

```bash
cargo check
```

Expected: Should have unimplemented! errors, but the module structure itself should be valid.

---

## Task 3: Implement check_container_cli()

**Files:**
- Modify: `src/setup.rs`

- [ ] **Step 1: Write the check_container_cli function**

Replace the stub with:

```rust
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
```

- [ ] **Step 2: Run `cargo check`**

```bash
cargo check
```

Expected: No errors in the setup module.

---

## Task 4: Implement check_container_system()

**Files:**
- Modify: `src/setup.rs`

- [ ] **Step 1: Add the parse_system_status helper function**

Add this before `check_container_system()`:

```rust
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
```

- [ ] **Step 2: Write check_container_system function**

Replace the stub with:

```rust
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
```

- [ ] **Step 3: Run `cargo check`**

```bash
cargo check
```

Expected: No errors.

---

## Task 5: Implement check_config_file()

**Files:**
- Modify: `src/setup.rs`

- [ ] **Step 1: Write check_config_file function**

Replace the stub with:

```rust
fn check_config_file() -> Status {
    let config_path = Config::config_path();
    if config_path.exists() {
        Status::Ok
    } else {
        Status::AutoFix {
            explanation: "Config file does not exist. Creating it from the default template...".to_string(),
            fix: Box::new(|| {
                let parent = config_path.parent().context("config path has no parent")?;
                std::fs::create_dir_all(parent)?;
                std::fs::write(&config_path, Config::init_template())?;
                Ok(())
            }),
        }
    }
}
```

- [ ] **Step 2: Run `cargo check`**

```bash
cargo check
```

Expected: No errors.

---

## Task 6: Implement authentication decision logic (pure function for testing)

**Files:**
- Modify: `src/setup.rs`

- [ ] **Step 1: Add the decide_auth helper function**

Add this before `check_authentication()`:

```rust
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
```

- [ ] **Step 2: Run `cargo check`**

```bash
cargo check
```

Expected: No errors.

---

## Task 7: Implement ensure_env_var_in_config() TOML helper and unit tests

**Files:**
- Modify: `src/setup.rs`
- Create: `tests/setup_tests.rs`

- [ ] **Step 1: Add ensure_env_var_in_config function**

Add this helper function to `src/setup.rs`:

```rust
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
```

- [ ] **Step 2: Create the test file**

Create `tests/setup_tests.rs`:

```rust
use std::path::PathBuf;
use tempfile::tempdir;
use std::fs;

// This test file will be populated in the next steps.
// For now, just ensure it compiles.

#[test]
fn placeholder() {
    // Placeholder to keep the test file valid.
}
```

- [ ] **Step 3: Add tempfile to dev-dependencies in Cargo.toml**

Open `Cargo.toml` and find the `[dev-dependencies]` section. Add:

```toml
tempfile = "3.8"
```

- [ ] **Step 4: Write unit test for ensure_env_var_in_config**

Replace the placeholder test in `tests/setup_tests.rs` with:

```rust
use agentbox::config::Config;
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn test_ensure_env_var_in_config_creates_env_section() {
    let temp = tempdir().unwrap();
    let config_path = temp.path().join("config.toml");

    // Empty config
    fs::write(&config_path, "").unwrap();

    // Simulate the function (we can't call it directly since it reads from a fixed path,
    // so we'll inline the logic for testing)
    let content = fs::read_to_string(&config_path).unwrap();
    let mut doc: toml_edit::DocumentMut = content.parse().unwrap();
    let env_tbl = match doc.entry("env").or_insert(toml_edit::table()).as_table_mut() {
        Some(t) => t,
        None => panic!("'env' is not a table"),
    };
    if !env_tbl.contains_key("MY_KEY") {
        env_tbl.insert("MY_KEY", toml_edit::value(""));
    }
    fs::write(&config_path, doc.to_string()).unwrap();

    // Verify the key was added
    let result: toml::Value = toml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(result["env"]["MY_KEY"].as_str(), Some(""));
}

#[test]
fn test_ensure_env_var_in_config_idempotent() {
    let temp = tempdir().unwrap();
    let config_path = temp.path().join("config.toml");

    // Config with env section already containing the key
    fs::write(&config_path, r#"[env]
MY_KEY = """#).unwrap();

    let content = fs::read_to_string(&config_path).unwrap();
    let mut doc: toml_edit::DocumentMut = content.parse().unwrap();
    let env_tbl = match doc.entry("env").or_insert(toml_edit::table()).as_table_mut() {
        Some(t) => t,
        None => panic!("'env' is not a table"),
    };
    if !env_tbl.contains_key("MY_KEY") {
        env_tbl.insert("MY_KEY", toml_edit::value(""));
    }
    let result_1 = doc.to_string();
    fs::write(&config_path, result_1.clone()).unwrap();

    // Run again
    let content = fs::read_to_string(&config_path).unwrap();
    let mut doc: toml_edit::DocumentMut = content.parse().unwrap();
    let env_tbl = match doc.entry("env").or_insert(toml_edit::table()).as_table_mut() {
        Some(t) => t,
        None => panic!("'env' is not a table"),
    };
    if !env_tbl.contains_key("MY_KEY") {
        env_tbl.insert("MY_KEY", toml_edit::value(""));
    }
    let result_2 = doc.to_string();

    // Should be identical
    assert_eq!(result_1, result_2);
}

#[test]
fn test_ensure_env_var_preserves_comments() {
    let temp = tempdir().unwrap();
    let config_path = temp.path().join("config.toml");

    let config_with_comment = r#"# This is my config
[env]
# My API key
EXISTING_KEY = "value"
"#;

    fs::write(&config_path, config_with_comment).unwrap();

    let content = fs::read_to_string(&config_path).unwrap();
    let mut doc: toml_edit::DocumentMut = content.parse().unwrap();
    let env_tbl = match doc.entry("env").or_insert(toml_edit::table()).as_table_mut() {
        Some(t) => t,
        None => panic!("'env' is not a table"),
    };
    if !env_tbl.contains_key("NEW_KEY") {
        env_tbl.insert("NEW_KEY", toml_edit::value(""));
    }
    fs::write(&config_path, doc.to_string()).unwrap();

    let result = fs::read_to_string(&config_path).unwrap();
    assert!(result.contains("# This is my config"));
    assert!(result.contains("# My API key"));
    assert!(result.contains("EXISTING_KEY"));
    assert!(result.contains("NEW_KEY"));
}
```

- [ ] **Step 5: Run the tests**

```bash
cargo test --test setup_tests
```

Expected: All three tests pass.

---

## Task 8: Implement check_authentication() menu options

**Files:**
- Modify: `src/setup.rs`

- [ ] **Step 1: Add the auth explanation string constant**

Add before `check_authentication()`:

```rust
const AUTH_EXPLANATION: &str = r#"macOS Keychain isn't reachable from the Linux container, so
Claude Code needs credentials via env var, or a one-time login
from inside the container (the token persists under ~/.claude)."#;
```

- [ ] **Step 2: Add helper to build menu options**

Add this function before `check_authentication()`:

```rust
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
```

- [ ] **Step 3: Run `cargo check`**

```bash
cargo check
```

Expected: No errors.

---

## Task 9: Implement check_authentication()

**Files:**
- Modify: `src/setup.rs`

- [ ] **Step 1: Write check_authentication function**

Replace the stub with:

```rust
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
```

- [ ] **Step 2: Run `cargo check`**

```bash
cargo check
```

Expected: No errors.

---

## Task 10: Implement run_setup() orchestrator and output helpers

**Files:**
- Modify: `src/setup.rs`

- [ ] **Step 1: Add print_indented helper**

Add before `run_setup()`:

```rust
fn print_indented(text: &str, indent: usize) {
    for line in text.lines() {
        println!("{}{}", " ".repeat(indent), line);
    }
}
```

- [ ] **Step 2: Write run_setup orchestrator**

Replace the stub with:

```rust
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
                prompt_menu(&menu)?;
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
```

- [ ] **Step 3: Run `cargo check`**

```bash
cargo check
```

Expected: No errors.

---

## Task 11: Implement prompt_menu()

**Files:**
- Modify: `src/setup.rs`

- [ ] **Step 1: Add prompt_menu function**

Add before `run_setup()`:

```rust
fn prompt_menu(menu: &[MenuOption]) -> Result<()> {
    for (i, option) in menu.iter().enumerate() {
        println!("          {}) {}", i + 1, option.label);
    }
    print!("        > ");
    std::io::Write::flush(&mut std::io::stdout())?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let choice = input.trim().parse::<usize>().unwrap_or(0);

    if choice > 0 && choice <= menu.len() {
        let option = &menu[choice - 1];
        (option.action)()?;
    } else {
        println!("        Invalid choice.");
    }

    Ok(())
}
```

Make sure to add `use std::io::Write;` at the top of the file if not already present.

- [ ] **Step 2: Run `cargo check`**

```bash
cargo check
```

Expected: No errors.

---

## Task 12: Modify src/main.rs to add Setup command routing

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add Setup variant to Commands enum**

Find the `Commands` enum (around line 38-46). Add the new variant:

```rust
    /// Run the interactive setup wizard
    Setup,
```

The enum should now look like:

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
    /// List all agentbox containers
    Ls,
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
}
```

- [ ] **Step 2: Add Setup case to the main match statement**

Find the `match cli.command` statement (around line 313). Add a new arm before or after the existing `Config` case:

```rust
        Some(Commands::Setup) => {
            setup::run_setup()?;
            Ok(())
        }
```

- [ ] **Step 3: Run `cargo check`**

```bash
cargo check
```

Expected: No errors.

---

## Task 13: Update check_prerequisites() error message

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Find and update the error message**

Locate the `check_prerequisites()` function (around line 194). Find the error message that says:

```rust
        Err(_) => {
            anyhow::bail!(
                "Apple Container CLI is not installed.\n\n\
                 Install it from: https://github.com/apple/container"
            );
        }
```

Replace it with:

```rust
        Err(_) => {
            anyhow::bail!(
                "Apple Container CLI is not installed.\n\n\
                 Run `agentbox setup` for guided setup, or install manually from:\n\
                 https://github.com/apple/container"
            );
        }
```

- [ ] **Step 2: Run `cargo check`**

```bash
cargo check
```

Expected: No errors.

---

## Task 14: Update README.md with setup command

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update Quick Start section**

Find the "## Quick Start" section (around line 25). Add this as the first item:

```markdown
# First time? Run setup to check prerequisites and configure authentication
agentbox setup

# Then use agentbox normally:
```

So the full section becomes:

```markdown
## Quick Start

```bash
# First time? Run setup to check prerequisites and configure authentication
agentbox setup

# Then use agentbox normally:

# Start interactive Claude session in current project
agentbox

# Run a task headlessly
agentbox "fix the failing tests"

# List all containers
agentbox ls

# Remove current project's container
agentbox rm

# Remove specific containers
agentbox rm agentbox-myapp-abc123 agentbox-other-def456

# Remove all agentbox containers
agentbox rm --all

# Force rebuild the image
agentbox build
```
```

- [ ] **Step 2: Update Authentication section**

Find the "## Authentication" section (around line 130). Replace the introductory paragraph with:

```markdown
## Authentication

macOS Keychain isn't accessible from inside the Linux container, so Claude Code needs credentials passed via environment variables, or a one-time login from inside the container (which persists under `~/.claude/`).

**Easiest approach: Run `agentbox setup`** — it will guide you through the options.

Alternatively, here are the three methods:
```

Then keep the rest of the section (Option A, Option B, etc.) as-is but update the numbering if needed.

- [ ] **Step 3: Run a quick visual check**

No test needed; just verify the README renders correctly:

```bash
head -50 README.md
```

Expected: Should see the "First time? Run setup..." in the Quick Start section.

---

## Task 15: Run full test suite to verify everything compiles and tests pass

**Files:**
- None (verification step)

- [ ] **Step 1: Run cargo test**

```bash
cargo test --all
```

Expected: All tests pass, including the new `setup_tests.rs` tests and any existing tests.

- [ ] **Step 2: Run cargo build**

```bash
cargo build --release
```

Expected: Clean build with no warnings or errors.

- [ ] **Step 3: Manual smoke test — happy path (all green)**

```bash
# Verify the command exists and all checks pass if already set up
./target/release/agentbox setup
```

Expected output (if everything is already configured):

```
  [1/4] Apple Container CLI ......... ✓
  [2/4] Container system running .... ✓
  [3/4] Config file ................. ✓
  [4/4] Authentication .............. ✓

Ready. Run `agentbox` to start coding.
```

- [ ] **Step 4: Verify the new command shows in help**

```bash
./target/release/agentbox --help | grep -i setup
```

Expected: Should see "setup" mentioned in the help output.

---

## Spec Coverage Checklist

✓ Check 1 (Container CLI): `check_container_cli()` prints install URL on failure  
✓ Check 2 (Container system running): `check_container_system()` auto-fixes with user consent  
✓ Check 3 (Config file exists): `check_config_file()` auto-creates from template  
✓ Check 4 (Authentication): `check_authentication()` interactive menu with 4 branches:
  - ✓ Branch 1: in-container login (print next steps)
  - ✓ Branch 2: API key (print export, prompt for config add, call `ensure_env_var_in_config`)
  - ✓ Branch 3: OAuth token (same as branch 2)
  - ✓ Branch 4: skip (print acknowledgement)
✓ TOML editing: `ensure_env_var_in_config()` is idempotent and preserves comments  
✓ Auth decision: `decide_auth()` checks env vars (literal + inherit) and credentials file  
✓ Orchestrator: `run_setup()` renders ✓/✗, counts passes, prints final summary  
✓ Module: `src/setup.rs` created with all functions  
✓ Main.rs: `Setup` variant added, routed to `setup::run_setup()`  
✓ Cargo.toml: `toml_edit` dependency added  
✓ README.md: Quick Start and Authentication sections updated  
✓ Testing: Unit tests for TOML helper and auth logic  

---

## Notes

- **No breaking changes** to existing commands or config format.
- **Idempotent** — running setup multiple times is safe.
- **Comment-preserving** — existing config comments survive TOML edits via `toml_edit`.
- **Auth always out-of-band** — no secrets are read or written by setup; user must export env vars or run `/login` themselves.
- **Re-run to confirm** — after picking an auth branch, user re-runs setup to verify it worked.
