# Per-CLI Flags Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users configure default CLI flags per coding agent in config.toml and pass additional flags at invocation time via `--`.

**Architecture:** Add a `[cli.<name>]` config section with a `flags` array. Split `std::env::args()` at `--` to collect passthrough flags. Merge config flags + passthrough flags and inject them into both the `container exec` and `container run` code paths. The entrypoint already uses `"$@"`, so no env var is needed — extra args pass through naturally.

**Tech Stack:** Rust, clap, serde/toml, bash (entrypoint)

---

### Task 1: Add `CliConfig` to config

**Files:**
- Modify: `src/config.rs:6-12` (add struct after BridgeConfig)
- Modify: `src/config.rs:14-28` (add field to Config)
- Modify: `src/config.rs:35-47` (add default)
- Modify: `src/config.rs:49-112` (add method + init template)

- [ ] **Step 1: Write tests for CliConfig parsing and cli_flags helper**

Add these tests at the end of the `mod tests` block in `src/config.rs`:

```rust
#[test]
fn test_parse_cli_config() {
    let toml_str = r#"
        [cli.claude]
        flags = ["--append-system-prompt", "Be careful.", "--model", "sonnet"]
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let claude_cli = config.cli.get("claude").unwrap();
    assert_eq!(
        claude_cli.flags,
        vec!["--append-system-prompt", "Be careful.", "--model", "sonnet"]
    );
}

#[test]
fn test_parse_multiple_cli_configs() {
    let toml_str = r#"
        [cli.claude]
        flags = ["--model", "sonnet"]

        [cli.codex]
        flags = ["--full-auto"]
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.cli.get("claude").unwrap().flags, vec!["--model", "sonnet"]);
    assert_eq!(config.cli.get("codex").unwrap().flags, vec!["--full-auto"]);
}

#[test]
fn test_default_config_has_empty_cli() {
    let config = Config::default();
    assert!(config.cli.is_empty());
}

#[test]
fn test_cli_flags_helper_found() {
    let toml_str = r#"
        [cli.claude]
        flags = ["--model", "sonnet"]
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.cli_flags("claude"), &["--model", "sonnet"]);
}

#[test]
fn test_cli_flags_helper_not_found() {
    let config = Config::default();
    assert!(config.cli_flags("claude").is_empty());
}

#[test]
fn test_cli_config_omitted() {
    let config: Config = toml::from_str("").unwrap();
    assert!(config.cli.is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib config::tests -- test_parse_cli_config test_parse_multiple_cli_configs test_default_config_has_empty_cli test_cli_flags_helper_found test_cli_flags_helper_not_found test_cli_config_omitted`
Expected: compilation errors — `CliConfig` doesn't exist yet, `cli` field missing from Config

- [ ] **Step 3: Add CliConfig struct**

Add after the `BridgeConfig` struct (after line 12) in `src/config.rs`:

```rust
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct CliConfig {
    #[serde(default)]
    pub flags: Vec<String>,
}
```

- [ ] **Step 4: Add `cli` field to Config struct**

In `src/config.rs`, add to the `Config` struct (after the `bridge` field, line 27):

```rust
    #[serde(default)]
    pub cli: HashMap<String, CliConfig>,
```

- [ ] **Step 5: Add `cli` to Config::default()**

In the `Default` impl for `Config`, add after `bridge: BridgeConfig::default(),`:

```rust
            cli: HashMap::new(),
```

- [ ] **Step 6: Add `cli_flags()` helper method**

In the `impl Config` block, add after `effective_cpus()`:

```rust
    pub fn cli_flags(&self, cli_name: &str) -> &[String] {
        self.cli
            .get(cli_name)
            .map(|c| c.flags.as_slice())
            .unwrap_or(&[])
    }
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test --lib config::tests`
Expected: all tests pass, including the 6 new ones

- [ ] **Step 8: Commit**

```bash
git add src/config.rs
git commit -m "feat: add [cli.<name>] config section for per-CLI flags"
```

---

### Task 2: Add cli_flags to `build_exec_args` (container exec path)

**Files:**
- Modify: `src/container.rs:197-240` (build_exec_args function)

- [ ] **Step 1: Write tests for cli_flags in build_exec_args**

Add these tests at the end of `mod tests` in `src/container.rs`:

```rust
#[test]
fn test_exec_args_with_cli_flags() {
    let cli_flags = vec![
        "--append-system-prompt".to_string(),
        "Be careful.".to_string(),
        "--model".to_string(),
        "sonnet".to_string(),
    ];
    let args = build_exec_args("mycontainer", None, &[], &cli_flags);
    let cmd = args.last().unwrap();
    assert!(cmd.contains("claude --dangerously-skip-permissions"));
    assert!(cmd.contains("'--append-system-prompt' 'Be careful.'"));
    assert!(cmd.contains("'--model' 'sonnet'"));
}

#[test]
fn test_exec_args_cli_flags_before_task() {
    let cli_flags = vec!["--model".to_string(), "sonnet".to_string()];
    let args = build_exec_args("mycontainer", Some("fix tests"), &[], &cli_flags);
    let cmd = args.last().unwrap();
    // Flags should appear between --dangerously-skip-permissions and -p
    let dsp_pos = cmd.find("--dangerously-skip-permissions").unwrap();
    let model_pos = cmd.find("'--model'").unwrap();
    let task_pos = cmd.find("-p '").unwrap();
    assert!(dsp_pos < model_pos);
    assert!(model_pos < task_pos);
}

#[test]
fn test_exec_args_cli_flags_empty() {
    let args = build_exec_args("mycontainer", None, &[], &[]);
    let cmd = args.last().unwrap();
    assert_eq!(cmd, "claude --dangerously-skip-permissions");
}

#[test]
fn test_exec_args_cli_flags_with_single_quotes() {
    let cli_flags = vec![
        "--append-system-prompt".to_string(),
        "Don't break things".to_string(),
    ];
    let args = build_exec_args("mycontainer", None, &[], &cli_flags);
    let cmd = args.last().unwrap();
    // Single quotes in values must be escaped
    assert!(cmd.contains("Don'\\''t break things"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib container::tests -- test_exec_args_with_cli_flags test_exec_args_cli_flags_before_task test_exec_args_cli_flags_empty test_exec_args_cli_flags_with_single_quotes`
Expected: compilation error — `build_exec_args` doesn't accept `cli_flags` parameter yet

- [ ] **Step 3: Add cli_flags parameter to build_exec_args**

Change the function signature in `src/container.rs:198` from:

```rust
fn build_exec_args(name: &str, task: Option<&str>, env_vars: &[(String, String)]) -> Vec<String> {
```

to:

```rust
fn build_exec_args(name: &str, task: Option<&str>, env_vars: &[(String, String)], cli_flags: &[String]) -> Vec<String> {
```

Then change lines 234-237 from:

```rust
    cmd.push_str("claude --dangerously-skip-permissions");
    if let Some(t) = task {
        cmd.push_str(&format!(" -p '{}'", t.replace('\'', "'\\''")));
    }
```

to:

```rust
    cmd.push_str("claude --dangerously-skip-permissions");
    for flag in cli_flags {
        cmd.push_str(&format!(" '{}'", flag.replace('\'', "'\\''")));
    }
    if let Some(t) = task {
        cmd.push_str(&format!(" -p '{}'", t.replace('\'', "'\\''")));
    }
```

- [ ] **Step 4: Fix existing callers and tests**

Update the `exec` function in `src/container.rs` to accept and forward `cli_flags`. Change the signature at line 243 from:

```rust
pub fn exec(
    name: &str,
    task: Option<&str>,
    env_vars: &[(String, String)],
    verbose: bool,
) -> Result<()> {
    let args = build_exec_args(name, task, env_vars);
```

to:

```rust
pub fn exec(
    name: &str,
    task: Option<&str>,
    env_vars: &[(String, String)],
    cli_flags: &[String],
    verbose: bool,
) -> Result<()> {
    let args = build_exec_args(name, task, env_vars, cli_flags);
```

Update all existing `build_exec_args` test calls to pass `&[]` as the new 4th argument. There are 5 existing tests that call it:

- `test_exec_args_with_env_vars`: change `build_exec_args("mycontainer", Some("fix tests"), &env_vars)` to `build_exec_args("mycontainer", Some("fix tests"), &env_vars, &[])`
- `test_exec_args_interactive_no_task`: change `build_exec_args("mycontainer", None, &[])` to `build_exec_args("mycontainer", None, &[], &[])`
- `test_exec_args_with_hostexec_env`: change `build_exec_args("mycontainer", None, &env_vars)` to `build_exec_args("mycontainer", None, &env_vars, &[])`
- `test_exec_args_with_forward_not_found`: change `build_exec_args("mycontainer", None, &env_vars)` to `build_exec_args("mycontainer", None, &env_vars, &[])`
- `test_exec_args_no_hostexec_without_env`: change `build_exec_args("mycontainer", None, &[])` to `build_exec_args("mycontainer", None, &[], &[])`

**Do NOT update callers in `main.rs` yet** — that happens in Task 4.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib container::tests`
Expected: all container tests pass, including the 4 new ones

- [ ] **Step 6: Commit**

```bash
git add src/container.rs
git commit -m "feat: add cli_flags parameter to container exec path"
```

---

### Task 3: Add cli_flags to `RunOpts` (container run path)

**Files:**
- Modify: `src/container.rs:27-37` (RunOpts struct)
- Modify: `src/container.rs:40-71` (to_run_args method)

- [ ] **Step 1: Write tests for cli_flags in RunOpts**

Add these tests at the end of `mod tests` in `src/container.rs`:

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
        interactive: false,
        task: Some("fix tests".into()),
        cli_flags: vec!["--model".into(), "sonnet".into()],
    };
    let args = opts.to_run_args();
    let image_pos = args.iter().position(|a| a == "agentbox:default").unwrap();
    let model_pos = args.iter().position(|a| a == "--model").unwrap();
    let p_pos = args.iter().position(|a| a == "-p").unwrap();
    // cli_flags come after image, before -p
    assert!(image_pos < model_pos);
    assert!(model_pos < p_pos);
}

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
        interactive: true,
        task: None,
        cli_flags: vec![],
    };
    let args = opts.to_run_args();
    // Last arg should be the image since no task and no cli_flags
    assert_eq!(args.last().unwrap(), "agentbox:default");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib container::tests -- test_run_args_with_cli_flags test_run_args_cli_flags_empty`
Expected: compilation error — `cli_flags` field doesn't exist on `RunOpts`

- [ ] **Step 3: Add cli_flags field to RunOpts**

In `src/container.rs`, add to the `RunOpts` struct (after the `task` field):

```rust
    pub cli_flags: Vec<String>,
```

Then in `to_run_args()`, change lines 63-68 from:

```rust
        args.push(self.image.clone());

        // Append task args after image (passed to entrypoint)
        if let Some(task) = &self.task {
            args.extend(["-p".into(), task.clone()]);
        }
```

to:

```rust
        args.push(self.image.clone());

        // CLI flags (passed through to entrypoint's "$@")
        for flag in &self.cli_flags {
            args.push(flag.clone());
        }

        // Append task args after image (passed to entrypoint)
        if let Some(task) = &self.task {
            args.extend(["-p".into(), task.clone()]);
        }
```

- [ ] **Step 4: Fix existing RunOpts test instances**

Add `cli_flags: vec![],` to all existing `RunOpts` constructors in tests:

- `test_build_run_args` (around line 379)
- `test_build_run_args_headless` (around line 574)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib container::tests`
Expected: all container tests pass

- [ ] **Step 6: Commit**

```bash
git add src/container.rs
git commit -m "feat: add cli_flags to RunOpts for container run path"
```

---

### Task 4: Wire everything together in main.rs

**Files:**
- Modify: `src/main.rs:11-36` (Cli struct — no change needed if we post-process task)
- Modify: `src/main.rs:383-477` (None branch — thread flags through)
- Modify: `src/main.rs:118-197` (create_and_run — add cli_flags param)

- [ ] **Step 1: Write test for split_at_double_dash**

Add a new test module and function at the bottom of `src/main.rs`, inside the existing `mod tests` block:

```rust
#[test]
fn test_split_at_double_dash_with_separator() {
    let args = vec![
        "fix".to_string(),
        "the".to_string(),
        "tests".to_string(),
        "--".to_string(),
        "--model".to_string(),
        "sonnet".to_string(),
    ];
    let (task, flags) = split_at_double_dash(args);
    assert_eq!(task, vec!["fix", "the", "tests"]);
    assert_eq!(flags, vec!["--model", "sonnet"]);
}

#[test]
fn test_split_at_double_dash_no_separator() {
    let args = vec!["fix".to_string(), "tests".to_string()];
    let (task, flags) = split_at_double_dash(args);
    assert_eq!(task, vec!["fix", "tests"]);
    assert!(flags.is_empty());
}

#[test]
fn test_split_at_double_dash_empty() {
    let (task, flags) = split_at_double_dash(vec![]);
    assert!(task.is_empty());
    assert!(flags.is_empty());
}

#[test]
fn test_split_at_double_dash_only_flags() {
    let args = vec![
        "--".to_string(),
        "--model".to_string(),
        "sonnet".to_string(),
    ];
    let (task, flags) = split_at_double_dash(args);
    assert!(task.is_empty());
    assert_eq!(flags, vec!["--model", "sonnet"]);
}

#[test]
fn test_split_at_double_dash_separator_at_end() {
    let args = vec!["fix".to_string(), "tests".to_string(), "--".to_string()];
    let (task, flags) = split_at_double_dash(args);
    assert_eq!(task, vec!["fix", "tests"]);
    assert!(flags.is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tests::test_split_at_double_dash`
Expected: compilation error — `split_at_double_dash` doesn't exist

- [ ] **Step 3: Implement split_at_double_dash**

Add this function in `src/main.rs` (before `fn main()`):

```rust
fn split_at_double_dash(args: Vec<String>) -> (Vec<String>, Vec<String>) {
    if let Some(pos) = args.iter().position(|a| a == "--") {
        let (before, after) = args.split_at(pos);
        (before.to_vec(), after[1..].to_vec())
    } else {
        (args, vec![])
    }
}
```

- [ ] **Step 4: Run split tests to verify they pass**

Run: `cargo test --lib tests::test_split_at_double_dash`
Expected: all 5 tests pass

- [ ] **Step 5: Thread cli_flags through main.rs**

In the `None` branch of `main()` (around line 383), after `task_str` is computed (line 391), add the flag merging logic. Replace the block at lines 388-392:

```rust
            let task_str = if cli.task.is_empty() {
                None
            } else {
                Some(cli.task.join(" "))
            };
```

with:

```rust
            let (task_parts, passthrough_flags) = split_at_double_dash(cli.task);
            let task_str = if task_parts.is_empty() {
                None
            } else {
                Some(task_parts.join(" "))
            };

            // Merge config cli flags + CLI passthrough flags
            let mut cli_flags: Vec<String> = config.cli_flags("claude").to_vec();
            cli_flags.extend(passthrough_flags);
```

Then update all 3 calls to `container::exec` in the `None` branch to pass `&cli_flags`. Change each:

```rust
container::exec(&name, task_str.as_deref(), &env_vars, cli.verbose)
```

to:

```rust
container::exec(&name, task_str.as_deref(), &env_vars, &cli_flags, cli.verbose)
```

There are 2 such calls (lines 422 and 447).

- [ ] **Step 6: Add cli_flags to create_and_run**

Change the `create_and_run` function signature (line 119) to accept cli_flags:

```rust
#[allow(clippy::too_many_arguments)]
fn create_and_run(
    name: &str,
    image_tag: &str,
    workdir: &str,
    config: &config::Config,
    task: Option<&str>,
    verbose: bool,
    extra_volumes: &[String],
    cli_flags: &[String],
    bridge_handle: Option<&bridge::BridgeHandle>,
) -> Result<()> {
```

In the `RunOpts` construction (around line 185), add the `cli_flags` field:

```rust
        let opts = container::RunOpts {
            name: name.into(),
            image: image_tag.into(),
            workdir: workdir.into(),
            cpus: config.effective_cpus(),
            memory: config.memory.clone(),
            env_vars,
            volumes,
            interactive: task.is_none(),
            task: task.map(String::from),
            cli_flags: cli_flags.to_vec(),
        };
```

Update both calls to `create_and_run` in `main()` to pass `&cli_flags`. There are 2 calls (around lines 434 and 460). Add `&cli_flags,` after `&cli.mount,` in each:

```rust
                        create_and_run(
                            &name,
                            &image_tag,
                            &cwd_str,
                            &config,
                            task_str.as_deref(),
                            cli.verbose,
                            &cli.mount,
                            &cli_flags,
                            bridge_handle.as_ref(),
                        )
```

- [ ] **Step 7: Verify full build and all tests pass**

Run: `cargo test`
Expected: all tests pass, no compilation errors

- [ ] **Step 8: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire cli_flags through arg splitting, config, and container paths"
```

---

### Task 5: Update init template

**Files:**
- Modify: `src/config.rs:80-111` (init_template)

- [ ] **Step 1: Update the init template**

In `src/config.rs`, in the `init_template()` method, add before the `# Host bridge` section:

```rust
# Extra CLI flags passed to the coding agent
# [cli.claude]
# flags = ["--append-system-prompt", "Your instructions here"]

```

- [ ] **Step 2: Update the init template test**

In `src/config.rs`, update `test_config_init_content` to also check for the new section. Add this assertion:

```rust
assert!(content.contains("# [cli.claude]"));
```

- [ ] **Step 3: Run tests to verify**

Run: `cargo test --lib config::tests::test_config_init_content`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/config.rs
git commit -m "feat: add cli flags example to config init template"
```
