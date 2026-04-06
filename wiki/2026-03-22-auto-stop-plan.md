# Auto-stop Container on Last Session Exit — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Automatically stop a container when the last `agentbox` process attached to it exits.

**Architecture:** Register signal handlers (SIGHUP, SIGTERM) so the process survives long enough to clean up. After the blocking `container exec`/`run` call returns, check if other `agentbox` sessions are using the same container (via `ps`). If none, run `container stop`. Extract parsing logic into a testable pure function.

**Tech Stack:** Rust, libc (already a dependency)

---

### Task 1: Add `has_other_sessions` parser to `container.rs`

A pure function that parses `ps -eo pid,args` output and determines whether another process is using the same container.

**Files:**
- Modify: `src/container.rs`

- [ ] **Step 1: Write failing tests for `has_other_sessions`**

Add to the `#[cfg(test)] mod tests` block in `src/container.rs`:

```rust
#[test]
fn test_has_other_sessions_no_matches() {
    let ps_output = "  PID ARGS\n  100 /bin/bash\n  200 vim main.rs\n";
    assert!(!has_other_sessions(ps_output, "agentbox-myapp-abc123", 999));
}

#[test]
fn test_has_other_sessions_own_pid_excluded() {
    let ps_output = "  PID ARGS\n  100 container exec --tty agentbox-myapp-abc123 bash\n";
    assert!(!has_other_sessions(ps_output, "agentbox-myapp-abc123", 100));
}

#[test]
fn test_has_other_sessions_other_pid_found() {
    let ps_output = "  PID ARGS\n  100 container exec --tty agentbox-myapp-abc123 bash\n  200 vim\n";
    assert!(has_other_sessions(ps_output, "agentbox-myapp-abc123", 999));
}

#[test]
fn test_has_other_sessions_different_container() {
    let ps_output = "  PID ARGS\n  100 container exec --tty agentbox-other-def456 bash\n";
    assert!(!has_other_sessions(ps_output, "agentbox-myapp-abc123", 999));
}

#[test]
fn test_has_other_sessions_multiple_sessions() {
    let ps_output = "  PID ARGS\n  100 container exec --tty agentbox-myapp-abc123 bash\n  200 container exec agentbox-myapp-abc123 bash -lc claude\n";
    assert!(has_other_sessions(ps_output, "agentbox-myapp-abc123", 999));
}

#[test]
fn test_has_other_sessions_run_command() {
    let ps_output = "  PID ARGS\n  100 container run --name agentbox-myapp-abc123 --cpus 4 agentbox:default\n";
    assert!(has_other_sessions(ps_output, "agentbox-myapp-abc123", 999));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib container::tests::test_has_other_sessions -- --nocapture`
Expected: FAIL — `has_other_sessions` not found

- [ ] **Step 3: Implement `has_other_sessions`**

Add above the `#[cfg(test)]` block in `src/container.rs`:

```rust
/// Check if other processes are using the same container.
/// Parses `ps -eo pid,args` output, looking for `container` processes
/// that reference the given container name, excluding our own PID.
pub fn has_other_sessions(ps_output: &str, container_name: &str, our_pid: u32) -> bool {
    ps_output.lines().any(|line| {
        let trimmed = line.trim();
        let (pid_str, args) = match trimmed.split_once(char::is_whitespace) {
            Some((p, a)) => (p.trim(), a),
            None => return false,
        };
        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => return false,
        };
        if pid == our_pid {
            return false;
        }
        args.contains("container") && args.contains(container_name)
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib container::tests::test_has_other_sessions -- --nocapture`
Expected: all 6 tests PASS

- [ ] **Step 5: Commit**

```bash
git add src/container.rs
git commit -m "Add has_other_sessions parser for auto-stop detection"
```

---

### Task 2: Add `maybe_stop_container` to `container.rs`

Uses `has_other_sessions` to decide whether to stop the container.

**Files:**
- Modify: `src/container.rs`

- [ ] **Step 1: Implement `maybe_stop_container`**

Add below `has_other_sessions` in `src/container.rs`:

```rust
/// Stop the container if no other agentbox sessions are attached to it.
/// Called after the blocking exec/run call returns, and from signal handlers.
/// Errors are intentionally ignored — this is best-effort cleanup.
pub fn maybe_stop_container(name: &str) {
    let our_pid = std::process::id();

    let output = match Command::new("ps")
        .args(["-eo", "pid,args"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return, // Can't check — don't stop
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    if has_other_sessions(&stdout, name, our_pid) {
        return;
    }

    eprintln!("[agentbox] no other sessions, stopping container {}...", name);
    let _ = stop(name, false);
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build`
Expected: success, no errors

- [ ] **Step 3: Commit**

```bash
git add src/container.rs
git commit -m "Add maybe_stop_container for auto-stop cleanup"
```

---

### Task 3: Install signal handlers and call cleanup in `main.rs`

Wire up signal suppression and call `maybe_stop_container` after every exec/run exit path.

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add `install_signal_handlers` function**

Add above `fn main()` in `src/main.rs`:

```rust
/// Suppress SIGHUP and SIGTERM so the process survives long enough
/// to run cleanup after the child container process exits.
/// The child process (container exec/run) still receives these signals
/// via the process group and exits normally.
fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
}
```

- [ ] **Step 2: Add `use libc;` import if not already present**

At the top of `src/main.rs`, the `libc` crate is already in Cargo.toml but may not be imported. Check and add if needed.

- [ ] **Step 3: Refactor the `None` branch to capture exec/run result and run cleanup**

Replace the `None =>` branch body (lines 372–457 of `main.rs`) so that:
1. Signal handlers are installed before the blocking call
2. All three status branches (`Running`, `Stopped`, `NotFound`) return their `Result<()>` into a `let result = ...`
3. `container::maybe_stop_container(&name)` is called after
4. `result` is returned

The refactored `None` branch:

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

    // Start bridge if configured
    let _bridge_handle = if !config.bridge.allowed_commands.is_empty() {
        match bridge::start_bridge(&config.bridge, &cwd_str) {
            Ok(handle) => {
                eprintln!(
                    "[agentbox] bridge started on port {} ({} commands allowed)",
                    handle.port,
                    config.bridge.allowed_commands.len()
                );
                Some(handle)
            }
            Err(e) => {
                eprintln!("[agentbox] warning: bridge failed to start: {}", e);
                None
            }
        }
    } else {
        None
    };

    // Suppress SIGHUP/SIGTERM so we survive long enough for cleanup
    install_signal_handlers();

    let result = match container::status(&name)? {
        container::ContainerStatus::Running => {
            let env_vars = build_all_env_vars(&config, _bridge_handle.as_ref());
            container::exec(&name, task_str.as_deref(), &env_vars, cli.verbose)
        }
        container::ContainerStatus::Stopped => {
            let (dockerfile_content, image_tag) =
                image::resolve_dockerfile(&cwd, cli.profile.as_deref(), &config)?;
            let cache_key = image_tag.replace(':', "-");
            if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
                eprintln!("Image changed, recreating container...");
                container::rm(&name, cli.verbose)?;
                image::ensure_base_image(&dockerfile_content, false, cli.verbose)?;
                image::build(&image_tag, &dockerfile_content, false, false, cli.verbose)?;
                image::save_cache(&dockerfile_content, &cache_key, &image::cache_dir())?;
                create_and_run(
                    &name,
                    &image_tag,
                    &cwd_str,
                    &config,
                    task_str.as_deref(),
                    cli.verbose,
                    &cli.mount,
                    _bridge_handle.as_ref(),
                )
            } else {
                container::start(&name, cli.verbose)?;
                let env_vars = build_all_env_vars(&config, _bridge_handle.as_ref());
                container::exec(&name, task_str.as_deref(), &env_vars, cli.verbose)
            }
        }
        container::ContainerStatus::NotFound => {
            let (dockerfile_content, image_tag) =
                image::resolve_dockerfile(&cwd, cli.profile.as_deref(), &config)?;
            let cache_key = image_tag.replace(':', "-");
            if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
                eprintln!("Building image...");
                image::ensure_base_image(&dockerfile_content, false, cli.verbose)?;
                image::build(&image_tag, &dockerfile_content, false, false, cli.verbose)?;
                image::save_cache(&dockerfile_content, &cache_key, &image::cache_dir())?;
            }
            create_and_run(
                &name,
                &image_tag,
                &cwd_str,
                &config,
                task_str.as_deref(),
                cli.verbose,
                &cli.mount,
                _bridge_handle.as_ref(),
            )
        }
    };

    // Auto-stop container if we're the last session
    container::maybe_stop_container(&name);

    result
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build`
Expected: success

- [ ] **Step 5: Run all tests**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "Wire up auto-stop: signal handlers and cleanup after exec/run"
```

---

### Task 4: Manual integration test

Verify the feature works end-to-end on a real container.

**Files:** none (manual testing)

- [ ] **Step 1: Single session — verify container stops on exit**

```bash
# Terminal 1:
agentbox
# Inside container, exit Claude (Ctrl+D or /exit)
# Should see: [agentbox] no other sessions, stopping container ...

# Verify:
agentbox ls
# Container should show "stopped" status
```

- [ ] **Step 2: Two sessions — verify container stays running**

```bash
# Terminal 1:
agentbox

# Terminal 2 (same project directory):
agentbox

# Exit Claude in Terminal 1
# Should NOT see the "stopping container" message

# Exit Claude in Terminal 2
# Should see: [agentbox] no other sessions, stopping container ...

# Verify:
agentbox ls
# Container should show "stopped"
```

- [ ] **Step 3: Terminal close — verify SIGHUP is handled**

```bash
# Open a terminal, run:
agentbox

# Close the terminal window (not Ctrl+D, close the window/tab)

# In another terminal:
agentbox ls
# Container should show "stopped" (may take a moment)
```
