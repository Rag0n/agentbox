# `agentbox shell` Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an `agentbox shell` subcommand that opens a bash shell in the project's container without launching Claude. Supports both interactive sessions (`agentbox shell`) and one-shot commands (`agentbox shell -- npm test`).

**Architecture:** Reuse the existing default-command state machine (Running / Stopped / NotFound). Introduce a `RunMode` enum (`Claude` / `Shell`) so `RunOpts` and `container::exec` can branch on intent. Extend `entrypoint.sh` with a `--shell` switch for the cold-start path. Hash `entrypoint.sh` into the image cache key so users get an auto-rebuild on upgrade.

**Tech Stack:** Rust, clap, `container` CLI (Apple Containers), bash.

**Spec:** [`wiki/2026-04-11-shell-command-design.md`](2026-04-11-shell-command-design.md)

---

### Task 1: Hash `entrypoint.sh` into image cache key

Currently `image::needs_build` only hashes the Dockerfile content, so changing `entrypoint.sh` does not invalidate any cached image. After Task 5 modifies the entrypoint, existing users would silently keep the old entrypoint and `agentbox shell` cold-start would fail. Fix this first so Task 5 lands cleanly.

**Files:**
- Modify: `src/image.rs`

- [ ] **Step 1: Write the failing tests**

Add at the bottom of the `tests` module in `src/image.rs`:

```rust
#[test]
fn test_cache_input_includes_entrypoint_when_dockerfile_references_it() {
    let dockerfile = "FROM debian:bookworm-slim\nCOPY entrypoint.sh /usr/local/bin/\nENTRYPOINT [\"entrypoint.sh\"]";
    let result = cache_input(dockerfile);
    assert!(result.contains(ENTRYPOINT_SCRIPT));
    assert!(result.len() > dockerfile.len());
}

#[test]
fn test_cache_input_excludes_entrypoint_when_dockerfile_doesnt_reference_it() {
    let dockerfile = "FROM debian:bookworm-slim\nCMD [\"sleep\", \"infinity\"]";
    let result = cache_input(dockerfile);
    assert_eq!(result, dockerfile);
}

#[test]
fn test_needs_build_uses_cache_input_for_default_dockerfile() {
    let tmp = tempfile::tempdir().unwrap();
    // Pre-seed cache with the hash of dockerfile alone (the OLD format)
    let old_hash = checksum(DEFAULT_DOCKERFILE);
    let cache_file = tmp.path().join("default.sha256");
    std::fs::write(&cache_file, &old_hash).unwrap();
    // needs_build should now return true because cache_input incorporates entrypoint too
    assert!(needs_build(DEFAULT_DOCKERFILE, "default", tmp.path()));
}

#[test]
fn test_needs_build_stable_when_cache_matches_combined_hash() {
    let tmp = tempfile::tempdir().unwrap();
    save_cache(DEFAULT_DOCKERFILE, "default", tmp.path()).unwrap();
    assert!(!needs_build(DEFAULT_DOCKERFILE, "default", tmp.path()));
}
```

- [ ] **Step 2: Run tests and verify they fail**

Run from the worktree root:

```bash
cargo test --quiet -p agentbox cache_input 2>&1 | tail -20
```

Expected: compile errors (`cache_input` not defined). That's the failure mode for the first two tests.

- [ ] **Step 3: Implement `cache_input` and update `needs_build`/`save_cache`**

In `src/image.rs`, add a new private function (place above `needs_build`):

```rust
/// Build the cache-hash input for a dockerfile. If the Dockerfile bundles the
/// agentbox entrypoint script (detected by literal `entrypoint.sh` reference),
/// fold the script's bytes into the input so any future entrypoint change
/// auto-invalidates the cache.
fn cache_input(dockerfile_content: &str) -> String {
    if dockerfile_content.contains("entrypoint.sh") {
        format!("{}\n--ENTRYPOINT--\n{}", dockerfile_content, ENTRYPOINT_SCRIPT)
    } else {
        dockerfile_content.to_string()
    }
}
```

Modify `needs_build` (replace the body, keep the signature):

```rust
pub fn needs_build(dockerfile_content: &str, cache_key: &str, cache_path: &Path) -> bool {
    let current_hash = checksum(&cache_input(dockerfile_content));
    let cache_file = cache_path.join(format!("{}.sha256", cache_key));

    match std::fs::read_to_string(&cache_file) {
        Ok(cached_hash) => cached_hash.trim() != current_hash,
        Err(_) => true,
    }
}
```

Modify `save_cache` similarly:

```rust
pub fn save_cache(dockerfile_content: &str, cache_key: &str, cache_path: &Path) -> Result<()> {
    std::fs::create_dir_all(cache_path)?;
    let hash = checksum(&cache_input(dockerfile_content));
    let cache_file = cache_path.join(format!("{}.sha256", cache_key));
    std::fs::write(&cache_file, &hash)?;
    Ok(())
}
```

- [ ] **Step 4: Run tests and verify they pass**

```bash
cargo test --quiet -p agentbox image:: 2>&1 | tail -20
```

Expected: all `image::tests::*` tests pass, including the four new ones. The pre-existing `test_needs_build_matching_cache` test still passes because both `save_cache` and `needs_build` now use the same `cache_input` transformation.

- [ ] **Step 5: Run the full test suite**

```bash
cargo test --quiet 2>&1 | tail -10
```

Expected: `test result: ok. 187 passed; 0 failed` (183 baseline + 4 new tests).

---

### Task 2: Introduce `RunMode` enum and refactor `RunOpts` / `exec` (Claude variant only)

Pure refactor: no behavior change. Replace `RunOpts.task` / `RunOpts.cli_flags` / `RunOpts.interactive` (and the parallel `exec()` parameters) with a single `RunMode` enum holding either `Claude { task, cli_flags }` or `Shell { cmd }`. Existing tests must stay green after rewrites. Also extract a `run_session` helper in `main.rs` so the Shell variant in Task 6 can reuse it.

**Files:**
- Modify: `src/container.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add the `RunMode` enum to `container.rs`**

In `src/container.rs`, add this just above the existing `pub struct RunOpts`:

```rust
#[derive(Debug, Clone)]
pub enum RunMode {
    Claude {
        task: Option<String>,
        cli_flags: Vec<String>,
    },
    Shell {
        cmd: Vec<String>,
    },
}

impl RunMode {
    /// Whether this mode should attach a TTY (interactive) or run non-interactively.
    pub fn is_interactive(&self) -> bool {
        match self {
            RunMode::Claude { task, .. } => task.is_none(),
            RunMode::Shell { cmd } => cmd.is_empty(),
        }
    }
}
```

- [ ] **Step 2: Replace `RunOpts` fields with `mode: RunMode`**

In `src/container.rs`, replace the existing `RunOpts` struct:

```rust
#[derive(Debug)]
pub struct RunOpts {
    pub name: String,
    pub image: String,
    pub workdir: String,
    pub cpus: usize,
    pub memory: String,
    pub env_vars: Vec<(String, String)>,
    pub volumes: Vec<String>,
    pub mode: RunMode,
}
```

Note: `interactive`, `task`, and `cli_flags` are removed — all derived from or held inside `mode`.

- [ ] **Step 3: Update `RunOpts::to_run_args` to use `mode`**

Replace the body of `to_run_args` (the `Shell` arm is a stub here that just panics; Task 3 fills it in with tests):

```rust
impl RunOpts {
    pub fn to_run_args(&self) -> Vec<String> {
        let mut args = vec!["run".to_string()];

        args.extend(["--name".into(), self.name.clone()]);
        args.extend(["--cpus".into(), self.cpus.to_string()]);
        args.extend(["--memory".into(), self.memory.clone()]);
        args.extend(["--workdir".into(), self.workdir.clone()]);
        args.push("--init".into());

        if self.mode.is_interactive() {
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

        match &self.mode {
            RunMode::Claude { task, cli_flags } => {
                for flag in cli_flags {
                    args.push(flag.clone());
                }
                if let Some(t) = task {
                    args.extend(["-p".into(), t.clone()]);
                }
            }
            RunMode::Shell { cmd: _ } => {
                // Filled in by Task 3
                unimplemented!("RunMode::Shell args added in Task 3");
            }
        }

        args
    }
}
```

- [ ] **Step 4: Update existing `to_run_args` tests to construct `RunOpts` via `RunMode::Claude`**

Find the tests in `src/container.rs` that create `RunOpts` literals (search for `RunOpts {`). There are five of them (`test_build_run_args`, `test_run_args_with_cli_flags`, `test_run_args_cli_flags_empty`, `test_build_run_args_headless`, plus the test structure inside `test_volume_deduplication` which only uses helpers — verify by inspection). Rewrite each to set `mode` instead of `task`/`cli_flags`/`interactive`.

Example — `test_build_run_args`:

```rust
#[test]
fn test_build_run_args() {
    let opts = RunOpts {
        name: "agentbox-myapp-abc123".into(),
        image: "agentbox:default".into(),
        workdir: "/Users/alex/Dev/myapp".into(),
        cpus: 4,
        memory: "8G".into(),
        env_vars: vec![("GH_TOKEN".into(), "tok123".into())],
        volumes: vec!["/Users/alex/Dev/myapp:/Users/alex/Dev/myapp".into()],
        mode: RunMode::Claude { task: None, cli_flags: vec![] },
    };
    let args = opts.to_run_args();
    assert!(args.contains(&"--name".to_string()));
    assert!(args.contains(&"agentbox-myapp-abc123".to_string()));
    assert!(args.contains(&"--cpus".to_string()));
    assert!(args.contains(&"4".to_string()));
    assert!(args.contains(&"--memory".to_string()));
    assert!(args.contains(&"8G".to_string()));
    assert!(args.contains(&"--init".to_string()));
    assert!(args.contains(&"--interactive".to_string()));
    assert!(args.contains(&"--tty".to_string()));
}
```

Example — `test_build_run_args_headless`:

```rust
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
        mode: RunMode::Claude {
            task: Some("fix the tests".into()),
            cli_flags: vec![],
        },
    };
    let args = opts.to_run_args();
    assert!(!args.contains(&"--interactive".to_string()));
    assert!(!args.contains(&"--tty".to_string()));
    assert!(args.contains(&"-p".to_string()));
    assert!(args.contains(&"fix the tests".to_string()));
}
```

Example — `test_run_args_with_cli_flags`:

```rust
#[test]
fn test_run_args_with_cli_flags() {
    let opts = RunOpts {
        name: "agentbox-myapp-abc123".into(),
        image: "agentbox:default".into(),
        workdir: "/Users/alex/Dev/myapp".into(),
        cpus: 2,
        memory: "4G".into(),
        env_vars: vec![],
        volumes: vec![],
        mode: RunMode::Claude {
            task: Some("fix tests".into()),
            cli_flags: vec!["--model".into(), "sonnet".into()],
        },
    };
    let args = opts.to_run_args();
    let image_pos = args.iter().position(|a| a == "agentbox:default").unwrap();
    let model_pos = args.iter().position(|a| a == "--model").unwrap();
    let p_pos = args.iter().position(|a| a == "-p").unwrap();
    assert!(image_pos < model_pos);
    assert!(model_pos < p_pos);
}
```

Example — `test_run_args_cli_flags_empty`:

```rust
#[test]
fn test_run_args_cli_flags_empty() {
    let opts = RunOpts {
        name: "test".into(),
        image: "agentbox:default".into(),
        workdir: "/tmp".into(),
        cpus: 1,
        memory: "4G".into(),
        env_vars: vec![],
        volumes: vec![],
        mode: RunMode::Claude { task: None, cli_flags: vec![] },
    };
    let args = opts.to_run_args();
    assert_eq!(args.last().unwrap(), "agentbox:default");
}
```

- [ ] **Step 5: Update `container::exec` and `build_exec_args` to take `&RunMode`**

Replace the signatures and the bits inside `build_exec_args` that reference `task`/`cli_flags` directly:

```rust
/// Build the argument list for `container exec`.
fn build_exec_args(name: &str, mode: &RunMode, env_vars: &[(String, String)]) -> Vec<String> {
    let mut args = vec!["exec".to_string()];
    if mode.is_interactive() {
        args.push("--interactive".into());
        args.push("--tty".into());
    }
    for (key, val) in env_vars {
        args.extend(["--env".into(), format!("{}={}", key, val)]);
    }
    args.extend(["--user".into(), "user".to_string()]);
    args.push(name.to_string());
    args.push("bash".into());
    args.extend(["-lc".into()]);

    let mut cmd = String::new();
    if env_vars.iter().any(|(k, _)| k == "HOSTEXEC_COMMANDS") {
        cmd.push_str(
            "if [ -n \"$HOSTEXEC_COMMANDS\" ]; then \
             mkdir -p ~/.local/bin; \
             for c in $HOSTEXEC_COMMANDS; do \
             ln -sf /usr/local/bin/hostexec ~/.local/bin/$c; \
             done; fi; ",
        );
    }
    if env_vars
        .iter()
        .any(|(k, v)| k == "HOSTEXEC_FORWARD_NOT_FOUND" && v == "true")
    {
        cmd.push_str(
            "if ! grep -q command_not_found_handle /etc/bash.bashrc 2>/dev/null; then \
             echo 'command_not_found_handle() { /usr/local/bin/hostexec \"$@\"; }' \
             | sudo tee -a /etc/bash.bashrc > /dev/null; fi; ",
        );
    }

    match mode {
        RunMode::Claude { task, cli_flags } => {
            cmd.push_str("claude --dangerously-skip-permissions");
            for flag in cli_flags {
                cmd.push_str(&format!(" '{}'", flag.replace('\'', "'\\''")));
            }
            if let Some(t) = task {
                cmd.push_str(&format!(" -p '{}'", t.replace('\'', "'\\''")));
            }
        }
        RunMode::Shell { cmd: _ } => {
            // Filled in by Task 4
            unimplemented!("RunMode::Shell exec args added in Task 4");
        }
    }
    args.push(cmd);
    args
}
```

```rust
/// Exec into a running container.
pub fn exec(
    name: &str,
    mode: &RunMode,
    env_vars: &[(String, String)],
    verbose: bool,
) -> Result<()> {
    let args = build_exec_args(name, mode, env_vars);

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
```

- [ ] **Step 6: Update existing `build_exec_args` tests**

Search for `build_exec_args(` in `src/container.rs` tests. There are seven call sites (`test_exec_args_with_env_vars`, `test_exec_args_interactive_no_task`, `test_exec_args_with_hostexec_env`, `test_exec_args_with_forward_not_found`, `test_exec_args_no_hostexec_without_env`, `test_exec_args_with_cli_flags`, `test_exec_args_cli_flags_before_task`, `test_exec_args_cli_flags_empty`, `test_exec_args_cli_flags_with_single_quotes` — verify by grep). Each one passes a `task` and `cli_flags`. Rewrite each to construct a `RunMode::Claude` and pass it instead.

Example — `test_exec_args_with_env_vars`:

```rust
#[test]
fn test_exec_args_with_env_vars() {
    let env_vars = vec![
        ("GH_TOKEN".to_string(), "tok123".to_string()),
        ("TERM".to_string(), "xterm".to_string()),
    ];
    let mode = RunMode::Claude {
        task: Some("fix tests".into()),
        cli_flags: vec![],
    };
    let args = build_exec_args("mycontainer", &mode, &env_vars);
    assert!(args.contains(&"--env".to_string()));
    assert!(args.contains(&"GH_TOKEN=tok123".to_string()));
    assert!(args.contains(&"TERM=xterm".to_string()));
    assert!(!args.contains(&"--interactive".to_string()));
    assert!(args.contains(&"bash".to_string()));
    let cmd = args.last().unwrap();
    assert!(cmd.contains("claude --dangerously-skip-permissions"));
    assert!(cmd.contains("-p 'fix tests'"));
}
```

Example — `test_exec_args_interactive_no_task`:

```rust
#[test]
fn test_exec_args_interactive_no_task() {
    let mode = RunMode::Claude { task: None, cli_flags: vec![] };
    let args = build_exec_args("mycontainer", &mode, &[]);
    assert!(args.contains(&"--interactive".to_string()));
    assert!(args.contains(&"--tty".to_string()));
    let cmd = args.last().unwrap();
    assert_eq!(cmd, "claude --dangerously-skip-permissions");
}
```

Example — `test_exec_args_with_cli_flags`:

```rust
#[test]
fn test_exec_args_with_cli_flags() {
    let cli_flags = vec![
        "--append-system-prompt".to_string(),
        "Be careful.".to_string(),
        "--model".to_string(),
        "sonnet".to_string(),
    ];
    let mode = RunMode::Claude { task: None, cli_flags };
    let args = build_exec_args("mycontainer", &mode, &[]);
    let cmd = args.last().unwrap();
    assert!(cmd.contains("claude --dangerously-skip-permissions"));
    assert!(cmd.contains("'--append-system-prompt' 'Be careful.'"));
    assert!(cmd.contains("'--model' 'sonnet'"));
}
```

Apply the same transformation pattern to the remaining `build_exec_args` tests: build `RunMode::Claude { task, cli_flags }`, then call `build_exec_args(name, &mode, &env_vars)`.

- [ ] **Step 7: Update `main.rs` call sites**

Find all callers of `container::exec` and `RunOpts {` in `src/main.rs`. There are three exec call sites (in the `Running`, `Stopped`, and possibly the `create_and_run` flow) and one `RunOpts` literal (in `create_and_run`).

Update `create_and_run` signature to take a `RunMode` instead of `task`, `cli_flags`:

```rust
#[allow(clippy::too_many_arguments)]
fn create_and_run(
    name: &str,
    image_tag: &str,
    workdir: &str,
    config: &config::Config,
    mode: container::RunMode,
    verbose: bool,
    extra_volumes: &[String],
    bridge_handle: Option<&bridge::BridgeHandle>,
) -> Result<()> {
    let home = dirs::home_dir().context("cannot determine home directory")?;

    let env_vars = build_all_env_vars(config, bridge_handle);

    let claude_dir = home.join(".claude");
    if !claude_dir.exists() {
        std::fs::create_dir_all(&claude_dir)?;
    }

    let mut volumes = vec![
        format!("{}:{}", workdir, workdir),
        format!("{}:/home/user/.claude", claude_dir.display()),
    ];

    let home_str = home.to_string_lossy();
    if home_str != "/home/user" {
        volumes.push(format!("{}:{}", claude_dir.display(), claude_dir.display()));
    }

    let claude_json = home.join(".claude.json");
    if claude_json.exists() {
        volumes.push(format!(
            "{}:/tmp/claude-seed.json:ro",
            claude_json.display()
        ));
    }

    let mut seen_dests: std::collections::HashSet<String> = volumes
        .iter()
        .filter_map(|v| {
            let parts: Vec<&str> = v.splitn(3, ':').collect();
            parts.get(1).map(|s| s.to_string())
        })
        .collect();

    for spec in config.volumes.iter().chain(extra_volumes.iter()) {
        let resolved = resolve_volume(spec)?;
        let parts: Vec<&str> = resolved.splitn(3, ':').collect();
        let source = parts.first().unwrap_or(&"");
        let dest = parts.get(1).unwrap_or(&"");
        if !std::path::Path::new(source).exists() {
            eprintln!(
                "[agentbox] warning: mount source does not exist: {}",
                source
            );
        }
        if seen_dests.insert(dest.to_string()) {
            volumes.push(resolved);
        }
    }

    let opts = container::RunOpts {
        name: name.into(),
        image: image_tag.into(),
        workdir: workdir.into(),
        cpus: config.effective_cpus(),
        memory: config.memory.clone(),
        env_vars,
        volumes,
        mode,
    };

    container::run(&opts, verbose)
}
```

Update the `None` match arm in `main()` to construct `RunMode::Claude` and update all call sites:

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

    let mut cli_flags: Vec<String> = config.cli_flags("claude").to_vec();
    cli_flags.extend(passthrough_flags.clone());

    let mode = container::RunMode::Claude {
        task: task_str,
        cli_flags,
    };

    let bridge_handle = if !config.bridge.allowed_commands.is_empty() {
        match bridge::start_bridge(&config.bridge, &cwd_str) {
            Ok(handle) => {
                if cli.verbose {
                    eprintln!(
                        "[agentbox] bridge started on port {} ({} commands allowed)",
                        handle.port,
                        config.bridge.allowed_commands.len()
                    );
                }
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

    install_signal_handlers();

    let result = match container::status(&name)? {
        container::ContainerStatus::Running => {
            let env_vars = build_all_env_vars(&config, bridge_handle.as_ref());
            container::exec(&name, &mode, &env_vars, cli.verbose)
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
                    mode,
                    cli.verbose,
                    &cli.mount,
                    bridge_handle.as_ref(),
                )
            } else {
                container::start(&name, cli.verbose)?;
                let env_vars = build_all_env_vars(&config, bridge_handle.as_ref());
                container::exec(&name, &mode, &env_vars, cli.verbose)
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
                mode,
                cli.verbose,
                &cli.mount,
                bridge_handle.as_ref(),
            )
        }
    };

    container::maybe_stop_container(&name, cli.verbose);

    result
}
```

Note the mode is moved (consumed) into `create_and_run` in the cold-start branches. The `Stopped` branch's stale-image case also moves `mode` into `create_and_run`. Since both moves are mutually exclusive (only one match arm runs), this compiles. The exec calls take `&mode` (borrow), which works in the other arms.

Wait — there's a borrow-checker subtlety here. The `Stopped` arm borrows `mode` for `exec` in its else branch but moves it in its if branch. That's OK because they're mutually exclusive within the match. But the outer match has three arms — `Running` borrows, `Stopped` borrows or moves, `NotFound` moves. Rust allows this because the match arms are mutually exclusive at runtime, and the borrow checker is per-arm. If the compiler complains, the fix is to clone `mode` (it's a small enum holding `Vec<String>` — a clone is cheap and clear).

If borrow checker rejects: change `mode, ` to `mode.clone(), ` in the two `create_and_run` calls.

- [ ] **Step 8: Run all tests, expect passing**

```bash
cargo test --quiet 2>&1 | tail -10
```

Expected: `test result: ok. 187 passed; 0 failed`. No new tests added in this task — purely a refactor — so the count is the same as Task 1's final count.

If there are compilation errors, the most likely culprit is a missed `RunOpts` or `exec` call site. Search:

```bash
```

Then re-grep with the Grep tool for `RunOpts {` and `container::exec(` and `build_exec_args(` to confirm all sites use the new shape.

---

### Task 3: Add Shell variant in `RunOpts::to_run_args`

Replace the `unimplemented!` stub from Task 2 step 3 with the real Shell-variant arg generation. The container CLI receives `--shell` followed by each cmd token as a separate arg; the entrypoint reassembles them via `"$*"`.

**Files:**
- Modify: `src/container.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/container.rs`:

```rust
#[test]
fn test_run_args_shell_interactive_no_cmd() {
    let opts = RunOpts {
        name: "agentbox-myapp-abc123".into(),
        image: "agentbox:default".into(),
        workdir: "/Users/alex/Dev/myapp".into(),
        cpus: 2,
        memory: "4G".into(),
        env_vars: vec![],
        volumes: vec![],
        mode: RunMode::Shell { cmd: vec![] },
    };
    let args = opts.to_run_args();
    let image_pos = args.iter().position(|a| a == "agentbox:default").unwrap();
    let after_image: Vec<&String> = args.iter().skip(image_pos + 1).collect();
    assert_eq!(after_image, vec![&"--shell".to_string()]);
    // Interactive: TTY flags present
    assert!(args.contains(&"--interactive".to_string()));
    assert!(args.contains(&"--tty".to_string()));
}

#[test]
fn test_run_args_shell_with_cmd() {
    let opts = RunOpts {
        name: "agentbox-myapp-abc123".into(),
        image: "agentbox:default".into(),
        workdir: "/Users/alex/Dev/myapp".into(),
        cpus: 2,
        memory: "4G".into(),
        env_vars: vec![],
        volumes: vec![],
        mode: RunMode::Shell {
            cmd: vec!["ls".into(), "-la".into(), "/workspace".into()],
        },
    };
    let args = opts.to_run_args();
    let image_pos = args.iter().position(|a| a == "agentbox:default").unwrap();
    let after_image: Vec<&String> = args.iter().skip(image_pos + 1).collect();
    assert_eq!(
        after_image,
        vec![
            &"--shell".to_string(),
            &"ls".to_string(),
            &"-la".to_string(),
            &"/workspace".to_string(),
        ]
    );
    // Headless: no TTY flags
    assert!(!args.contains(&"--tty".to_string()));
}

#[test]
fn test_run_mode_is_interactive() {
    assert!(RunMode::Claude { task: None, cli_flags: vec![] }.is_interactive());
    assert!(!RunMode::Claude { task: Some("t".into()), cli_flags: vec![] }.is_interactive());
    assert!(RunMode::Shell { cmd: vec![] }.is_interactive());
    assert!(!RunMode::Shell { cmd: vec!["ls".into()] }.is_interactive());
}
```

- [ ] **Step 2: Run tests and verify they fail**

```bash
cargo test --quiet -p agentbox test_run_args_shell 2>&1 | tail -20
```

Expected: panic at `unimplemented!("RunMode::Shell args added in Task 3")` for the first two tests. The third test (`test_run_mode_is_interactive`) should already pass since `is_interactive` was added in Task 2.

- [ ] **Step 3: Replace the `unimplemented!` stub**

In `src/container.rs`, replace the `RunMode::Shell` arm in `to_run_args`:

```rust
RunMode::Shell { cmd } => {
    args.push("--shell".into());
    for token in cmd {
        args.push(token.clone());
    }
}
```

- [ ] **Step 4: Run tests and verify they pass**

```bash
cargo test --quiet -p agentbox test_run_args_shell 2>&1 | tail -10
```

Expected: both new shell tests pass.

- [ ] **Step 5: Run the full test suite**

```bash
cargo test --quiet 2>&1 | tail -10
```

Expected: `test result: ok. 190 passed; 0 failed` (187 + 3 new tests).

---

### Task 4: Add Shell variant in `build_exec_args` / `exec`

Replace the `unimplemented!` stub from Task 2 step 5 with the real Shell-variant bash command generation. Extract the HOSTEXEC setup prefix into a shared helper so both Claude and Shell paths use it.

**Implementation note:** For the one-shot case, we use bash positional args (`exec "$@"` script + tokens passed as separate process args) rather than string interpolation. Reason: string interpolation requires shell-quoting each user token, which produces fragile escape sequences that break on unbalanced quotes (e.g., `agentbox shell -- echo "Don't"` would produce a bash syntax error). Positional args sidestep all of that — bash receives the user tokens as distinct process args, just like `docker exec mycontainer cmd arg1 arg2`. This means the structure of `args` for shell-with-cmd differs from the claude path: more args after the script, not just one cmd string. Tests assert on the structured args, not on `args.last()`.

**Files:**
- Modify: `src/container.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/container.rs`:

```rust
#[test]
fn test_exec_args_shell_interactive_no_cmd() {
    let mode = RunMode::Shell { cmd: vec![] };
    let args = build_exec_args("mycontainer", &mode, &[]);
    assert!(args.contains(&"--interactive".to_string()));
    assert!(args.contains(&"--tty".to_string()));
    let script = args.last().unwrap();
    assert!(script.ends_with("exec bash -l"), "got: {}", script);
    assert!(!script.contains("claude"));
}

#[test]
fn test_exec_args_shell_with_cmd() {
    let mode = RunMode::Shell {
        cmd: vec!["ls".into(), "-la".into(), "/workspace".into()],
    };
    let args = build_exec_args("mycontainer", &mode, &[]);
    // Headless: no --tty
    assert!(!args.contains(&"--tty".to_string()));
    // Find -lc; the next arg is the script.
    let lc_pos = args.iter().position(|a| a == "-lc").unwrap();
    let script = &args[lc_pos + 1];
    assert!(script.ends_with("exec \"$@\""), "got: {}", script);
    assert!(!script.contains("claude"));
    // After the script: $0 placeholder, then user tokens verbatim.
    assert_eq!(&args[lc_pos + 2], "bash");
    assert_eq!(&args[lc_pos + 3], "ls");
    assert_eq!(&args[lc_pos + 4], "-la");
    assert_eq!(&args[lc_pos + 5], "/workspace");
}

#[test]
fn test_exec_args_shell_cmd_preserves_quotes_via_positional_args() {
    // Tokens with quotes/spaces need no shell escaping because they go
    // through process args (docker exec semantics), not string interpolation.
    let mode = RunMode::Shell {
        cmd: vec!["echo".into(), "Don't".into()],
    };
    let args = build_exec_args("mycontainer", &mode, &[]);
    let lc_pos = args.iter().position(|a| a == "-lc").unwrap();
    // User tokens appear verbatim — no quoting, no escape sequences.
    assert_eq!(&args[lc_pos + 3], "echo");
    assert_eq!(&args[lc_pos + 4], "Don't");
}

#[test]
fn test_exec_args_shell_with_hostexec_env() {
    let env_vars = vec![
        ("HOSTEXEC_HOST".to_string(), "192.168.64.1".to_string()),
        ("HOSTEXEC_PORT".to_string(), "12345".to_string()),
        ("HOSTEXEC_TOKEN".to_string(), "tok".to_string()),
        ("HOSTEXEC_COMMANDS".to_string(), "xcodebuild xcrun".to_string()),
    ];
    let mode = RunMode::Shell { cmd: vec![] };
    let args = build_exec_args("mycontainer", &mode, &env_vars);
    let script = args.last().unwrap();
    // HOSTEXEC setup must run before the bash exec
    assert!(script.contains("HOSTEXEC_COMMANDS"));
    assert!(script.contains("ln -sf /usr/local/bin/hostexec"));
    assert!(script.contains("exec bash -l"));
    // Setup precedes the bash exec
    let setup_pos = script.find("ln -sf").unwrap();
    let bash_pos = script.find("exec bash").unwrap();
    assert!(setup_pos < bash_pos);
}

#[test]
fn test_exec_args_shell_with_forward_not_found() {
    let env_vars = vec![
        ("HOSTEXEC_COMMANDS".to_string(), "xcodebuild".to_string()),
        ("HOSTEXEC_FORWARD_NOT_FOUND".to_string(), "true".to_string()),
    ];
    let mode = RunMode::Shell { cmd: vec![] };
    let args = build_exec_args("mycontainer", &mode, &env_vars);
    let script = args.last().unwrap();
    assert!(script.contains("command_not_found_handle"));
    assert!(script.contains("exec bash -l"));
}

#[test]
fn test_exec_args_shell_with_cmd_includes_setup_prefix() {
    // With env vars + a one-shot cmd, the script (not args.last()) carries
    // the HOSTEXEC setup prefix.
    let env_vars = vec![
        ("HOSTEXEC_COMMANDS".to_string(), "xcodebuild".to_string()),
    ];
    let mode = RunMode::Shell {
        cmd: vec!["xcodebuild".into(), "-version".into()],
    };
    let args = build_exec_args("mycontainer", &mode, &env_vars);
    let lc_pos = args.iter().position(|a| a == "-lc").unwrap();
    let script = &args[lc_pos + 1];
    assert!(script.contains("ln -sf /usr/local/bin/hostexec"));
    assert!(script.ends_with("exec \"$@\""));
}
```

- [ ] **Step 2: Run tests and verify they fail**

```bash
cargo test --quiet -p agentbox test_exec_args_shell 2>&1 | tail -20
```

Expected: panic at `unimplemented!("RunMode::Shell exec args added in Task 4")` for all six new tests.

- [ ] **Step 3: Extract the HOSTEXEC setup prefix to a helper**

In `src/container.rs`, add this private helper above `build_exec_args` (it lifts the existing two `if env_vars.iter()...` blocks verbatim):

```rust
/// Returns the HOSTEXEC bash setup prefix that runs before the main command.
/// Mirrors the logic that the entrypoint runs at cold-start, applied here for
/// the exec path which bypasses the entrypoint.
fn build_setup_prefix(env_vars: &[(String, String)]) -> String {
    let mut prefix = String::new();
    if env_vars.iter().any(|(k, _)| k == "HOSTEXEC_COMMANDS") {
        prefix.push_str(
            "if [ -n \"$HOSTEXEC_COMMANDS\" ]; then \
             mkdir -p ~/.local/bin; \
             for c in $HOSTEXEC_COMMANDS; do \
             ln -sf /usr/local/bin/hostexec ~/.local/bin/$c; \
             done; fi; ",
        );
    }
    if env_vars
        .iter()
        .any(|(k, v)| k == "HOSTEXEC_FORWARD_NOT_FOUND" && v == "true")
    {
        prefix.push_str(
            "if ! grep -q command_not_found_handle /etc/bash.bashrc 2>/dev/null; then \
             echo 'command_not_found_handle() { /usr/local/bin/hostexec \"$@\"; }' \
             | sudo tee -a /etc/bash.bashrc > /dev/null; fi; ",
        );
    }
    prefix
}
```

Update `build_exec_args` to call the helper instead of inlining the two blocks, and replace the `unimplemented!` with the real Shell payload. Note that the Shell-with-cmd branch pushes additional args after the script (the `bash` $0 placeholder and the user tokens), so the function structure changes from "always push one cmd string at the end" to "match-arm-specific tail":

```rust
fn build_exec_args(name: &str, mode: &RunMode, env_vars: &[(String, String)]) -> Vec<String> {
    let mut args = vec!["exec".to_string()];
    if mode.is_interactive() {
        args.push("--interactive".into());
        args.push("--tty".into());
    }
    for (key, val) in env_vars {
        args.extend(["--env".into(), format!("{}={}", key, val)]);
    }
    args.extend(["--user".into(), "user".to_string()]);
    args.push(name.to_string());
    args.push("bash".into());
    args.extend(["-lc".into()]);

    let setup = build_setup_prefix(env_vars);

    match mode {
        RunMode::Claude { task, cli_flags } => {
            let mut cmd = setup;
            cmd.push_str("claude --dangerously-skip-permissions");
            for flag in cli_flags {
                cmd.push_str(&format!(" '{}'", flag.replace('\'', "'\\''")));
            }
            if let Some(t) = task {
                cmd.push_str(&format!(" -p '{}'", t.replace('\'', "'\\''")));
            }
            args.push(cmd);
        }
        RunMode::Shell { cmd: shell_cmd } if shell_cmd.is_empty() => {
            let mut cmd = setup;
            cmd.push_str("exec bash -l");
            args.push(cmd);
        }
        RunMode::Shell { cmd: shell_cmd } => {
            // Use bash positional args. The script execs "$@", and the user
            // tokens are passed as separate process args after a $0 placeholder.
            // This preserves arg boundaries without any string-level escaping.
            let script = format!("{}exec \"$@\"", setup);
            args.push(script);
            args.push("bash".into()); // $0 placeholder for the inner bash
            for token in shell_cmd {
                args.push(token.clone());
            }
        }
    }
    args
}
```

The `cmd: shell_cmd` pattern destructures the `Shell` variant's field; the binding name `shell_cmd` avoids shadowing the outer scope.

- [ ] **Step 4: Run tests and verify they pass**

```bash
cargo test --quiet -p agentbox test_exec_args 2>&1 | tail -20
```

Expected: all new shell tests pass AND all existing claude tests still pass (the helper extraction is behavior-preserving for the Claude path).

- [ ] **Step 5: Run the full test suite**

```bash
cargo test --quiet 2>&1 | tail -10
```

Expected: `test result: ok. 196 passed; 0 failed` (190 + 6 new tests).

---

### Task 5: Modify `entrypoint.sh` to support `--shell` switch

Add the bash branch right before the existing `exec claude` line. All the existing setup (`.claude.json` seed, HOSTEXEC symlinks, `command_not_found_handle`) runs unchanged for both modes.

**Implementation note:** For the one-shot case, mirror the exec path's positional-args strategy. The user's tokens arrive as positional params (`$1` onwards after `shift`), and `bash -lc 'exec "$@"' bash "$@"` invokes a login shell that loads PATH/env from `~/.bashrc` and `.profile`, then execs the user command directly with arg boundaries preserved. This sidesteps the bash quoting trap that would break `agentbox shell -- echo "Don't"` if we used `"$*"`.

**Files:**
- Modify: `resources/entrypoint.sh`

- [ ] **Step 1: Apply the entrypoint change**

Replace the contents of `resources/entrypoint.sh` with:

```bash
#!/bin/bash
set -e

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

# Shell mode: open bash instead of claude (used by `agentbox shell`)
if [ "$1" = "--shell" ]; then
    shift
    if [ $# -eq 0 ]; then
        exec bash -l
    else
        # Pass user tokens as positional args to a login shell, which execs
        # them directly. Preserves arg boundaries (no shell-quoting bugs)
        # and still loads PATH from .bashrc/.profile.
        exec bash -lc 'exec "$@"' bash "$@"
    fi
fi

exec claude --dangerously-skip-permissions "$@"
```

- [ ] **Step 2: Run cache-invalidation test to verify the entrypoint change is detected**

```bash
cargo test --quiet -p agentbox test_needs_build_uses_cache_input_for_default_dockerfile 2>&1 | tail -10
```

Expected: pass. (The test only checks that needs_build returns true for an old hash; it doesn't depend on entrypoint contents specifically. But it confirms the path is wired.)

Also run the embedded-content test to verify Cargo picks up the new entrypoint bytes:

```bash
cargo test --quiet -p agentbox test_cache_input_includes_entrypoint_when_dockerfile_references_it 2>&1 | tail -10
```

Expected: pass (the test asserts `result.contains(ENTRYPOINT_SCRIPT)`, which now includes the shell branch).

- [ ] **Step 3: Run the full test suite**

```bash
cargo test --quiet 2>&1 | tail -10
```

Expected: `test result: ok. 196 passed; 0 failed`. No test count change (script change, not Rust code change).

- [ ] **Step 4: Document manual smoke tests for after Task 6**

Just a note for the implementer — the bash entrypoint can't be unit tested. After Task 6 wires up the CLI, run these manually from the worktree:

1. `cargo run --quiet -- shell` (cold start) → lands in `bash -l`, `whoami` shows `user`, `pwd` shows the worktree path.
2. `cargo run --quiet -- shell -- ls /workspace` → runs `ls`, exits with status from `ls`.
3. From a second terminal while the first interactive session is running: `cargo run --quiet -- shell` → exec path, both sessions visible in `agentbox status`.
4. With `bridge.allowed_commands = ["xcodebuild"]` in `~/.config/agentbox/config.toml`: inside `cargo run -- shell`, run `which xcodebuild` → shows symlink under `~/.local/bin/`.

If any of these fail, debug before proceeding to Task 7.

---

### Task 6: Add `Commands::Shell` to `main.rs`

Wire up the `agentbox shell` subcommand: clap variant, match arm, and a small extracted `run_session` helper to avoid duplicating ~80 lines of state-machine code between the `None` (default) and `Shell` arms.

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Write the failing clap tests**

Add to the `tests` module in `src/main.rs`:

```rust
#[test]
fn test_shell_subcommand_no_args() {
    let cli = Cli::try_parse_from(["agentbox", "shell"]).unwrap();
    assert!(matches!(cli.command, Some(Commands::Shell)));
}

#[test]
fn test_shell_subcommand_with_passthrough() {
    let raw_args: Vec<String> = vec![
        "agentbox".into(),
        "shell".into(),
        "--".into(),
        "ls".into(),
        "-la".into(),
    ];
    let (agentbox_args, passthrough_flags) = split_at_double_dash(raw_args);
    let cli = Cli::try_parse_from(agentbox_args).unwrap();
    assert!(matches!(cli.command, Some(Commands::Shell)));
    assert_eq!(passthrough_flags, vec!["ls", "-la"]);
}

#[test]
fn test_shell_subcommand_with_profile_and_passthrough() {
    let raw_args: Vec<String> = vec![
        "agentbox".into(),
        "shell".into(),
        "--profile".into(),
        "mystack".into(),
        "--".into(),
        "npm".into(),
        "test".into(),
    ];
    let (agentbox_args, passthrough_flags) = split_at_double_dash(raw_args);
    let cli = Cli::try_parse_from(agentbox_args).unwrap();
    assert!(matches!(cli.command, Some(Commands::Shell)));
    assert_eq!(cli.profile, Some("mystack".into()));
    assert_eq!(passthrough_flags, vec!["npm", "test"]);
}
```

- [ ] **Step 2: Run tests and verify they fail**

```bash
cargo test --quiet -p agentbox test_shell_subcommand 2>&1 | tail -20
```

Expected: compile error — `Commands::Shell` doesn't exist yet.

- [ ] **Step 3: Add the `Shell` variant to the `Commands` enum**

In `src/main.rs`, add a new variant to the `Commands` enum (place near `Setup`):

```rust
#[derive(Subcommand)]
enum Commands {
    /// Remove containers (by name, current project, or --all)
    Rm {
        names: Vec<String>,
        #[arg(long)]
        all: bool,
    },
    /// Show rich container status (CPU, memory, project, sessions)
    #[command(alias = "ls")]
    Status,
    /// Force rebuild the container image (--no-cache for clean build)
    Build {
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
}
```

- [ ] **Step 4: Run clap tests and verify they pass**

```bash
cargo test --quiet -p agentbox test_shell_subcommand 2>&1 | tail -10
```

Expected: all three new tests pass.

- [ ] **Step 5: Extract `run_session` helper**

In `src/main.rs`, add this function above `main()`:

```rust
fn run_session(
    cli: &Cli,
    config: &config::Config,
    mode: container::RunMode,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cwd_str = cwd.to_string_lossy().to_string();
    let name = container::container_name(&cwd_str);

    let bridge_handle = if !config.bridge.allowed_commands.is_empty() {
        match bridge::start_bridge(&config.bridge, &cwd_str) {
            Ok(handle) => {
                if cli.verbose {
                    eprintln!(
                        "[agentbox] bridge started on port {} ({} commands allowed)",
                        handle.port,
                        config.bridge.allowed_commands.len()
                    );
                }
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

    install_signal_handlers();

    let result = match container::status(&name)? {
        container::ContainerStatus::Running => {
            let env_vars = build_all_env_vars(config, bridge_handle.as_ref());
            container::exec(&name, &mode, &env_vars, cli.verbose)
        }
        container::ContainerStatus::Stopped => {
            let (dockerfile_content, image_tag) =
                image::resolve_dockerfile(&cwd, cli.profile.as_deref(), config)?;
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
                    config,
                    mode,
                    cli.verbose,
                    &cli.mount,
                    bridge_handle.as_ref(),
                )
            } else {
                container::start(&name, cli.verbose)?;
                let env_vars = build_all_env_vars(config, bridge_handle.as_ref());
                container::exec(&name, &mode, &env_vars, cli.verbose)
            }
        }
        container::ContainerStatus::NotFound => {
            let (dockerfile_content, image_tag) =
                image::resolve_dockerfile(&cwd, cli.profile.as_deref(), config)?;
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
                config,
                mode,
                cli.verbose,
                &cli.mount,
                bridge_handle.as_ref(),
            )
        }
    };

    container::maybe_stop_container(&name, cli.verbose);

    result
}
```

If the borrow checker complains about `mode` being used in multiple match arms (since the inner `if/else` in `Stopped` has one branch that consumes it and another that borrows it), the simplest fix is to clone `mode` at the top of `Stopped`'s `if` branch:

```rust
container::ContainerStatus::Stopped => {
    let (dockerfile_content, image_tag) = ...;
    let cache_key = image_tag.replace(':', "-");
    if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
        // ... rebuild path moves `mode`
        create_and_run(&name, &image_tag, &cwd_str, config, mode.clone(), cli.verbose, &cli.mount, bridge_handle.as_ref())
    } else {
        // ... reuses `mode` by borrow
        container::exec(&name, &mode, &env_vars, cli.verbose)
    }
}
```

`RunMode` is `Clone` (we derived it in Task 2 step 1), so this is cheap.

- [ ] **Step 6: Replace the inlined `None` arm body with a `run_session` call**

In `main()`, replace the `None` arm to use the helper:

```rust
None => {
    let config = config::Config::load()?;
    let task_str = if cli.task.is_empty() {
        None
    } else {
        Some(cli.task.join(" "))
    };

    let mut cli_flags: Vec<String> = config.cli_flags("claude").to_vec();
    cli_flags.extend(passthrough_flags.clone());

    let mode = container::RunMode::Claude {
        task: task_str,
        cli_flags,
    };

    run_session(&cli, &config, mode)
}
```

- [ ] **Step 7: Add the `Shell` match arm**

Add this arm in the `match cli.command` block (place it near `Setup`):

```rust
Some(Commands::Shell) => {
    let config = config::Config::load()?;
    let mode = container::RunMode::Shell {
        cmd: passthrough_flags.clone(),
    };
    run_session(&cli, &config, mode)
}
```

- [ ] **Step 8: Run the full test suite**

```bash
cargo test --quiet 2>&1 | tail -10
```

Expected: `test result: ok. 199 passed; 0 failed` (196 + 3 new shell-subcommand tests).

- [ ] **Step 9: Verify it compiles in release mode**

```bash
cargo build --release --quiet 2>&1 | tail -20
```

Expected: clean build, no warnings.

- [ ] **Step 10: Run manual smoke tests**

Now that the CLI is wired up, run the manual tests documented in Task 5 step 4 plus a few more:

1. `cargo run --quiet -- shell` → interactive bash session in container, `whoami` shows `user`, `pwd` shows the worktree path.
2. `cargo run --quiet -- shell -- ls /` → one-shot, exits with `ls`'s status. Then `cargo run --quiet -- shell -- ls /nonexistent; echo $?` (host shell) shows the non-zero exit propagated.
3. `cargo run --quiet -- shell -- echo "Don't"` → prints `Don't` cleanly (verifies the positional-args path preserves quoted args).
4. `cargo run --quiet -- shell -- bash -c "echo hello && pwd"` → shell features via an explicit inner bash invocation (matches docker exec semantics — bare `&&` between agentbox tokens won't work, use bash -c).
5. From a second terminal: `cargo run --quiet -- shell` while the first is open → both attach to the same container, `cargo run -- status` shows 2 sessions.
6. `cargo run --quiet -- shell` then Ctrl+D → exits cleanly, container auto-stops if no other sessions.
7. With `bridge.allowed_commands = ["xcodebuild"]` configured: `cargo run --quiet -- shell` → inside, `which xcodebuild` shows `/home/user/.local/bin/xcodebuild`.
8. With an existing pre-Task-1 cached image: `cargo run --quiet -- shell` should trigger `Image changed, recreating container...` once, then work.

If any test fails, debug before moving on. Do not skip this step — the bash entrypoint cannot be unit-tested.

---

### Task 7: Update README.md

Add the shell command to Quick Start, and add a custom-Dockerfile caveat noting the cold-start limitation for non-default-base images.

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add shell examples to Quick Start**

In `README.md`, find the Quick Start section. After the existing line:

```markdown
# Run a task headlessly
agentbox "fix the failing tests"
```

Add (preserving the surrounding code-block formatting):

```markdown
# Open an interactive bash shell in the container (no Claude)
agentbox shell

# Run a one-shot command in the container
agentbox shell -- npm test
```

- [ ] **Step 2: Add the custom-Dockerfile caveat**

In `README.md`, find the "Custom Dockerfiles" section. After the existing "Per-project" subsection's example dockerfile, add:

```markdown
> **Note:** `agentbox shell` requires the agentbox entrypoint script for the
> cold-start case (when the container doesn't yet exist). If your custom
> Dockerfile uses `FROM agentbox:default`, it works automatically. If your
> Dockerfile replaces the entrypoint or uses a fully different base image,
> the cold-start case won't launch a shell — run `agentbox` first to create
> the container, then `agentbox shell` works via the exec path.
```

- [ ] **Step 3: Verify the README still renders cleanly**

```bash
cargo test --quiet 2>&1 | tail -5
```

Expected: `test result: ok. 199 passed; 0 failed`. (No README tests, but confirming nothing broke.)

Inspect the file with the Read tool to make sure the markdown formatting is consistent (no broken code fences, no leftover diff markers).

---

## Self-review checklist (run after all tasks complete)

- [ ] All three states (Running / Stopped / NotFound) covered for shell mode — verified by manual smoke tests.
- [ ] HOSTEXEC bridge works in shell mode — verified by manual test 4 in Task 6.
- [ ] Auto-stop fires when shell exits and no other sessions — verified by manual test 5 in Task 6.
- [ ] Existing claude path still works — verified by all 183 baseline tests still passing after the refactor.
- [ ] One-shot exit codes propagate — verified by manual test 2 in Task 6 (exit non-zero from `ls /nonexistent`).
- [ ] One-shot args with quotes/spaces preserved — verified by `agentbox shell -- echo "Don't"` printing `Don't` cleanly (positional-args path).
- [ ] README mentions shell in Quick Start AND has the custom-Dockerfile caveat.
- [ ] Image cache auto-invalidates on entrypoint change — verified by Task 1 unit tests + manual test 6 in Task 6.
