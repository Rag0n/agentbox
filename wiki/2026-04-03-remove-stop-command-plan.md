# Remove `stop` Command — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the `stop` subcommand from the CLI, since auto-stop makes it redundant.

**Architecture:** Delete the `Stop` variant from the `Commands` enum, its match arm, and its tests in `src/main.rs`. Update `README.md` to remove `stop` examples. No changes to `src/container.rs` — `stop()` is still used internally by auto-stop.

**Tech Stack:** Rust (clap CLI), Markdown

---

### Task 1: Remove `Stop` from `Commands` enum

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Remove the `Stop` variant from the `Commands` enum**

Delete these lines from the `Commands` enum:

```rust
    /// Stop containers (by name, current project, or --all)
    Stop {
        /// Container names to stop
        names: Vec<String>,
        /// Stop all agentbox containers
        #[arg(long)]
        all: bool,
    },
```

The enum should go from `Rm`, `Stop`, `Ls`, `Build`, `Config` to `Rm`, `Ls`, `Build`, `Config`.

- [ ] **Step 2: Remove the `Stop` match arm from `main()`**

Delete the entire `Some(Commands::Stop { names, all })` arm:

```rust
        Some(Commands::Stop { names, all }) => {
            let targets = if all {
                let all_names = container::list_names(cli.verbose)?;
                if all_names.is_empty() {
                    println!("No agentbox containers found.");
                    return Ok(());
                }
                all_names
            } else if names.is_empty() {
                let cwd = std::env::current_dir()?;
                vec![container::container_name(&cwd.to_string_lossy())]
            } else {
                names
            };
            for name in &targets {
                container::stop(name, cli.verbose)?;
                println!("Stopped {}", name);
            }
            Ok(())
        }
```

- [ ] **Step 3: Remove the 3 stop tests**

Delete these test functions from `mod tests`:
- `test_stop_subcommand_no_args`
- `test_stop_subcommand_with_names`
- `test_stop_subcommand_all`

- [ ] **Step 4: Verify it compiles and tests pass**

Run: `cargo test`
Expected: all tests pass (100 tests, down from 103)

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "Remove stop subcommand from CLI

Auto-stop (added 2026-03-22) makes manual stop redundant.
container::stop() remains for internal use by auto-stop."
```

---

### Task 2: Update README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Remove stop examples from Quick Start**

Delete these lines from the Quick Start section:

```markdown
# Stop current project's container
agentbox stop

# Stop specific containers
agentbox stop agentbox-myapp-abc123 agentbox-other-def456

# Stop all agentbox containers
agentbox stop --all
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "README: remove stop command examples"
```
