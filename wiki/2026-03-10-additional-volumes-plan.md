# Additional Volume Mounts Implementation Plan

> REQUIRED SUB-SKILL: Use workflow:executing-plans to implement this plan task-by-task.

**Goal:** Allow users to mount additional host folders into agentbox containers via config and CLI flag.

**Architecture:** Add `volumes` field to Config, `--mount` CLI flag, and a `resolve_volume()` function that handles tilde-prefix, absolute, and explicit source:dest path formats. Resolved volumes are appended to the existing hardcoded mounts in `create_and_run()`.

**Tech Stack:** Rust, clap, serde/toml

---

### Task 0: Add `volumes` field to Config struct

**Files:**
- Modify: `src/config.rs:6-16` (Config struct)
- Modify: `src/config.rs:23-32` (Default impl)
- Modify: `src/config.rs:85-154` (tests)

**Step 1: Write the failing test**

Add to the `tests` module in `src/config.rs`:

```rust
#[test]
fn test_parse_config_with_volumes() {
    let toml_str = r#"
        volumes = [
            "~/.config/worktrunk",
            "/Users/alex/Dev/marketplace",
            "/source/path:/dest/path",
        ]
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.volumes.len(), 3);
    assert_eq!(config.volumes[0], "~/.config/worktrunk");
    assert_eq!(config.volumes[1], "/Users/alex/Dev/marketplace");
    assert_eq!(config.volumes[2], "/source/path:/dest/path");
}

#[test]
fn test_default_config_has_empty_volumes() {
    let config = Config::default();
    assert!(config.volumes.is_empty());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test test_parse_config_with_volumes -- --nocapture`
Expected: FAIL — `Config` has no field `volumes`

**Step 3: Write minimal implementation**

In `Config` struct, add after the `profiles` field:

```rust
#[serde(default)]
pub volumes: Vec<String>,
```

In `Default for Config`, add to the struct literal:

```rust
volumes: Vec::new(),
```

**Step 4: Run tests to verify they pass**

Run: `cargo test test_parse_config_with_volumes test_default_config_has_empty_volumes -- --nocapture`
Expected: PASS

**Step 5: Commit**

Use the `workflow:commit` skill to stage and commit.

---

### Task 1: Add volume resolution function

**Files:**
- Modify: `src/main.rs` (add `resolve_volume` function and tests)

**Step 1: Write the failing tests**

Add to the `tests` module in `src/main.rs`:

```rust
#[test]
fn test_resolve_volume_tilde_path() {
    let home = dirs::home_dir().unwrap();
    let resolved = resolve_volume("~/.config/worktrunk");
    let expected = format!(
        "{}:/home/user/.config/worktrunk",
        home.join(".config/worktrunk").display()
    );
    assert_eq!(resolved, expected);
}

#[test]
fn test_resolve_volume_absolute_path() {
    let resolved = resolve_volume("/Users/alex/Dev/marketplace");
    assert_eq!(resolved, "/Users/alex/Dev/marketplace:/Users/alex/Dev/marketplace");
}

#[test]
fn test_resolve_volume_explicit_mapping() {
    let resolved = resolve_volume("/source/path:/dest/path");
    assert_eq!(resolved, "/source/path:/dest/path");
}

#[test]
fn test_resolve_volume_tilde_only() {
    let home = dirs::home_dir().unwrap();
    let resolved = resolve_volume("~/mydir");
    let expected = format!("{}:/home/user/mydir", home.join("mydir").display());
    assert_eq!(resolved, expected);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test test_resolve_volume -- --nocapture`
Expected: FAIL — `resolve_volume` not defined

**Step 3: Write minimal implementation**

Add this function in `src/main.rs` (above `create_and_run`):

```rust
/// Resolve a volume spec into a "source:dest" string.
///
/// Rules:
/// - `~/.config/foo` → expand ~ to host home for source, /home/user for dest
/// - `/source:/dest` → pass through as-is (explicit mapping)
/// - `/absolute/path` → mount at same path in container
fn resolve_volume(spec: &str) -> String {
    if let Some(suffix) = spec.strip_prefix('~') {
        let home = dirs::home_dir().unwrap_or_default();
        let suffix = suffix.strip_prefix('/').unwrap_or(suffix);
        let source = home.join(suffix);
        let dest = format!("/home/user/{}", suffix);
        format!("{}:{}", source.display(), dest)
    } else if spec.contains(':') {
        spec.to_string()
    } else {
        format!("{}:{}", spec, spec)
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test test_resolve_volume -- --nocapture`
Expected: PASS (all 4 tests)

**Step 5: Commit**

Use the `workflow:commit` skill to stage and commit.

---

### Task 2: Add `--mount` CLI flag

**Files:**
- Modify: `src/main.rs:9-30` (Cli struct)
- Modify: `src/main.rs:223-285` (tests)

**Step 1: Write the failing tests**

Add to the `tests` module in `src/main.rs`:

```rust
#[test]
fn test_mount_flag_single() {
    let cli = Cli::try_parse_from(["agentbox", "--mount", "/some/path"]).unwrap();
    assert_eq!(cli.mount, vec!["/some/path"]);
}

#[test]
fn test_mount_flag_multiple() {
    let cli = Cli::try_parse_from([
        "agentbox",
        "--mount", "~/.config/foo",
        "--mount", "/other/path",
    ]).unwrap();
    assert_eq!(cli.mount.len(), 2);
}

#[test]
fn test_mount_flag_default_empty() {
    let cli = Cli::try_parse_from(["agentbox"]).unwrap();
    assert!(cli.mount.is_empty());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test test_mount_flag -- --nocapture`
Expected: FAIL — `Cli` has no field `mount`

**Step 3: Write minimal implementation**

Add to the `Cli` struct, after the `verbose` field:

```rust
/// Additional volume mounts (host path, or host:container)
#[arg(long)]
mount: Vec<String>,
```

**Step 4: Run tests to verify they pass**

Run: `cargo test test_mount_flag -- --nocapture`
Expected: PASS (all 3 tests)

**Step 5: Commit**

Use the `workflow:commit` skill to stage and commit.

---

### Task 3: Wire volumes into `create_and_run()`

**Files:**
- Modify: `src/main.rs:55-105` (create_and_run function signature and body)
- Modify: `src/main.rs` (call sites in `main()`)

**Step 1: Update `create_and_run` to accept extra volumes**

Add `extra_volumes: &[String]` parameter to `create_and_run`:

```rust
fn create_and_run(
    name: &str,
    image_tag: &str,
    workdir: &str,
    config: &config::Config,
    task: Option<&str>,
    verbose: bool,
    extra_volumes: &[String],
) -> Result<()> {
```

Inside the function, after building the hardcoded volumes vec, resolve and append extra volumes:

```rust
        let mut volumes = vec![
            format!("{}:{}", workdir, workdir),
            format!("{}:/home/user/.claude", home.join(".claude").display()),
            format!("{}:/home/user/.claude.json", claude_json.display()),
        ];

        // Append config volumes + CLI mounts
        for spec in config.volumes.iter().chain(extra_volumes.iter()) {
            let resolved = resolve_volume(spec);
            let source = resolved.split(':').next().unwrap_or("");
            if !std::path::Path::new(source).exists() {
                eprintln!("[agentbox] warning: mount source does not exist: {}", source);
            }
            volumes.push(resolved);
        }
```

Use `volumes` in the `RunOpts` struct (replace the existing `volumes:` field).

**Step 2: Update call sites in `main()`**

Pass `&cli.mount` to `create_and_run` at both call sites (lines ~200 and ~215):

```rust
create_and_run(&name, &image_tag, &cwd_str, &config, task_str.as_deref(), cli.verbose, &cli.mount)?;
```

**Step 3: Run all tests to verify nothing broke**

Run: `cargo test`
Expected: All existing tests PASS

**Step 4: Commit**

Use the `workflow:commit` skill to stage and commit.

---

### Task 4: Update config template

**Files:**
- Modify: `src/config.rs:63-82` (init_template)
- Modify: `src/config.rs:147-153` (test_config_init_content)

**Step 1: Write the failing test**

Update the existing `test_config_init_content` test — add an assertion:

```rust
assert!(content.contains("# volumes"));
```

**Step 2: Run test to verify it fails**

Run: `cargo test test_config_init_content -- --nocapture`
Expected: FAIL — template doesn't contain `# volumes`

**Step 3: Update the template**

Add this block to `init_template()`, after the memory line and before the dockerfile line:

```rust
# Additional volumes to mount into containers
# volumes = [
#   "~/.config/worktrunk",              # tilde = home-relative mapping
#   "/Users/alex/Dev/marketplace",      # absolute = same path in container
#   "/source/path:/dest/path",          # explicit source:dest mapping
# ]
```

**Step 4: Run test to verify it passes**

Run: `cargo test test_config_init_content -- --nocapture`
Expected: PASS

**Step 5: Commit**

Use the `workflow:commit` skill to stage and commit.

---

### Task 5: Final verification

**Step 1: Run full test suite**

Run: `cargo test`
Expected: All tests PASS

**Step 2: Run clippy**

Run: `cargo clippy`
Expected: No warnings

**Step 3: Run fmt**

Run: `cargo fmt`
Expected: No changes needed

**Step 4: Final commit if any formatting changes**

Use the `workflow:commit` skill if `cargo fmt` made changes.
