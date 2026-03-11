# Buildkit Crash Recovery Implementation Plan

> REQUIRED SUB-SKILL: Use workflow:executing-plans to implement this plan task-by-task.

**Goal:** Prevent and recover from Apple Container buildkit crashes during image builds.

**Architecture:** Two-part fix: (1) remove `--pull` from implicit auto-builds to avoid triggering the known progress bar crash, keep it for explicit `agentbox build`; (2) add stderr-tee crash detection with automatic buildkit reset and retry.

**Tech Stack:** Rust, `std::process`, `std::io::BufRead`

---

### Task 1: Add `pull` parameter to `build_args()`

**Files:**
- Modify: `src/image.rs:122-146` (`build_args` function and its call comment)

**Step 1: Update existing tests for new signature**

Update the three `build_args` tests to pass the new `pull: bool` parameter. The existing `test_build_args_pull_for_remote_base` becomes a test for `pull: true`, not an automatic behavior based on Dockerfile content.

In `src/image.rs`, replace the three test functions:

```rust
#[test]
fn test_build_args_pull_when_requested() {
    let args = build_args(
        "agentbox:default",
        "FROM debian:bookworm-slim\nRUN echo hi",
        "/tmp/Dockerfile",
        "/tmp",
        false,
        true, // pull
    );
    assert!(args.contains(&"--pull".to_string()));
}

#[test]
fn test_build_args_no_pull_when_not_requested() {
    let args = build_args(
        "agentbox:default",
        "FROM debian:bookworm-slim\nRUN echo hi",
        "/tmp/Dockerfile",
        "/tmp",
        false,
        false, // no pull
    );
    assert!(!args.contains(&"--pull".to_string()));
}

#[test]
fn test_build_args_no_pull_for_local_base_even_when_requested() {
    let args = build_args(
        "agentbox:project-myapp",
        "FROM agentbox:default\nRUN apt-get install -y nodejs",
        "/tmp/Dockerfile",
        "/tmp",
        false,
        true, // pull requested, but local base overrides
    );
    assert!(!args.contains(&"--pull".to_string()));
}

#[test]
fn test_build_args_no_cache_with_pull() {
    let args = build_args(
        "agentbox:default",
        "FROM debian:bookworm-slim",
        "/tmp/Dockerfile",
        "/tmp",
        true,
        true,
    );
    assert!(args.contains(&"--no-cache".to_string()));
    assert!(args.contains(&"--pull".to_string()));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test test_build_args -- --nocapture`
Expected: compilation error — `build_args` doesn't accept 6 args yet.

**Step 3: Update `build_args` signature and logic**

In `src/image.rs`, replace the `build_args` function:

```rust
/// Build args for `container build`. Extracted for testability.
fn build_args(
    tag: &str,
    dockerfile_content: &str,
    dockerfile_path: &str,
    context_path: &str,
    no_cache: bool,
    pull: bool,
) -> Vec<String> {
    let mut args = vec!["build".to_string()];
    // --pull only when explicitly requested AND base image is remote.
    if pull && !references_default_base(dockerfile_content) {
        args.push("--pull".into());
    }
    args.extend([
        "-t".into(),
        tag.to_string(),
        "-f".into(),
        dockerfile_path.to_string(),
    ]);
    if no_cache {
        args.push("--no-cache".into());
    }
    args.push(context_path.to_string());
    args
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test test_build_args -- --nocapture`
Expected: compilation error — `build()` still calls `build_args` with 5 args. That's OK, we'll fix it in Task 2.

**Step 5: Commit**

Use the `workflow:commit` skill to stage and commit.
Message: "Add pull parameter to build_args for explicit control over --pull flag"

---

### Task 2: Add `pull` parameter to `build()` and update all callers

**Files:**
- Modify: `src/image.rs:108-120` (`ensure_base_image`) and `src/image.rs:148-181` (`build`)
- Modify: `src/main.rs:277` (explicit build), `src/main.rs:324` (implicit build, stopped), `src/main.rs:349` (implicit build, new)

**Step 1: Update `build()` signature**

In `src/image.rs`, change `build` to accept `pull`:

```rust
/// Build an image using `container build`.
pub fn build(tag: &str, dockerfile_content: &str, no_cache: bool, pull: bool, verbose: bool) -> Result<()> {
```

And update the `build_args` call inside it:

```rust
    let args = build_args(
        tag,
        dockerfile_content,
        &df_path.to_string_lossy(),
        &tmp.path().to_string_lossy(),
        no_cache,
        pull,
    );
```

**Step 2: Update `ensure_base_image` call**

In `src/image.rs`, change line 116 from:
```rust
build("agentbox:default", DEFAULT_DOCKERFILE, false, verbose)?;
```
to:
```rust
build("agentbox:default", DEFAULT_DOCKERFILE, false, false, verbose)?;
```

**Step 3: Update `main.rs` callers**

In `src/main.rs`, there are 3 calls to `image::build()`. Update each:

Line 277 (explicit `agentbox build` — pull: true):
```rust
image::build(&image_tag, &dockerfile_content, no_cache, true, cli.verbose)?;
```

Line 324 (implicit, stopped container — pull: false):
```rust
image::build(&image_tag, &dockerfile_content, false, false, cli.verbose)?;
```

Line 349 (implicit, new container — pull: false):
```rust
image::build(&image_tag, &dockerfile_content, false, false, cli.verbose)?;
```

**Step 4: Run all tests**

Run: `cargo test`
Expected: all 63 tests pass (the old `test_build_args_pull_for_remote_base` was replaced in Task 1).

**Step 5: Commit**

Use the `workflow:commit` skill.
Message: "Only pass --pull on explicit agentbox build to avoid buildkit crash"

---

### Task 3: Add `reset_buildkit()` function

**Files:**
- Modify: `src/image.rs` (add new function before `build`)

**Step 1: Add the function**

In `src/image.rs`, add before the `/// Build an image` doc comment:

```rust
/// Reset the buildkit builder after a crash.
/// Uses `container builder` commands (more reliable than stopping the buildkit container directly).
/// See: https://github.com/apple/container/issues/284
fn reset_buildkit(verbose: bool) {
    if verbose {
        eprintln!("[agentbox] resetting buildkit...");
    }
    let _ = std::process::Command::new("container")
        .args(["builder", "stop", "--force"])
        .output();
    let _ = std::process::Command::new("container")
        .args(["builder", "delete"])
        .output();
}
```

**Step 2: Run all tests**

Run: `cargo test`
Expected: all tests pass (function exists but isn't called yet, no new tests needed — it's not unit-testable).

**Step 3: Commit**

Use the `workflow:commit` skill.
Message: "Add reset_buildkit helper for crash recovery"

---

### Task 4: Add crash detection and retry to `build()`

**Files:**
- Modify: `src/image.rs:1-4` (imports)
- Modify: `src/image.rs` (`build` function body)

**Step 1: Add imports**

In `src/image.rs`, replace the imports:

```rust
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
```

**Step 2: Rewrite `build()` with stderr tee and crash recovery**

Replace the entire `build` function:

```rust
/// Build an image using `container build`.
/// Automatically detects and recovers from buildkit crashes by resetting and retrying once.
pub fn build(tag: &str, dockerfile_content: &str, no_cache: bool, pull: bool, verbose: bool) -> Result<()> {
    let tmp = tempfile::tempdir().context("failed to create temp dir")?;
    let df_path = tmp.path().join("Dockerfile");
    std::fs::write(&df_path, dockerfile_content)?;
    // Write entrypoint script so Dockerfile COPY can find it
    let ep_path = tmp.path().join("entrypoint.sh");
    std::fs::write(&ep_path, ENTRYPOINT_SCRIPT)?;

    let args = build_args(
        tag,
        dockerfile_content,
        &df_path.to_string_lossy(),
        &tmp.path().to_string_lossy(),
        no_cache,
        pull,
    );

    if verbose {
        eprintln!("[agentbox] container {}", args.join(" "));
    }

    // Pipe stderr so we can detect buildkit crashes while still showing output in real-time.
    let mut child = Command::new("container")
        .args(&args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run 'container build'")?;

    let stderr_pipe = child.stderr.take().unwrap();
    let reader = BufReader::new(stderr_pipe);
    let mut captured_stderr = String::new();

    for line in reader.lines() {
        if let Ok(line) = line {
            eprintln!("{}", line);
            captured_stderr.push_str(&line);
            captured_stderr.push('\n');
        }
    }

    let status = child.wait().context("failed to wait for 'container build'")?;

    if status.success() {
        return Ok(());
    }

    // Detect buildkit crash (Apple Container framework bug) and auto-recover.
    if captured_stderr.contains("Negative count not allowed") {
        eprintln!("Detected buildkit crash, resetting builder and retrying...");
        reset_buildkit(verbose);

        let retry_status = Command::new("container")
            .args(&args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("failed to run 'container build' (retry)")?;

        if retry_status.success() {
            return Ok(());
        }
        anyhow::bail!("container build failed after buildkit reset");
    }

    anyhow::bail!("container build failed");
}
```

**Step 3: Update remaining `std::process::Command` references**

After adding the `Command` and `Stdio` imports, update `reset_buildkit` to use the short form:

```rust
fn reset_buildkit(verbose: bool) {
    if verbose {
        eprintln!("[agentbox] resetting buildkit...");
    }
    let _ = Command::new("container")
        .args(["builder", "stop", "--force"])
        .output();
    let _ = Command::new("container")
        .args(["builder", "delete"])
        .output();
}
```

**Step 4: Run all tests**

Run: `cargo test`
Expected: all tests pass.

**Step 5: Commit**

Use the `workflow:commit` skill.
Message: "Detect buildkit crashes and auto-recover with builder reset and retry"

---

### Task 5: Final verification

**Step 1: Run full test suite**

Run: `cargo test`
Expected: all tests pass.

**Step 2: Build the binary**

Run: `cargo build`
Expected: compiles without warnings.

**Step 3: Smoke test (manual)**

Run `agentbox build` in a project directory. Verify:
- Build succeeds
- `--pull` appears in verbose output for explicit build
- Implicit builds (just running `agentbox`) do NOT show `--pull` in verbose output
