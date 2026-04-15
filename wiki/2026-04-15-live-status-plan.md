# Live `agentbox status` Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `agentbox status` continuously refresh on a TTY in a `top`-like display, polling `container stats --format json` every 2s, while preserving the existing one-shot behavior when piped or when `--no-stream` is set.

**Architecture:** Add `src/status/live.rs` alongside the existing one-shot code. Use `tokio` async (already a dep) for concurrent stats polling and keyboard input. Use `crossterm` for terminal management (alt screen buffer, raw mode, RAII cleanup). Shutdown is a persistent `watch<bool>` channel; subprocess calls race `read_to_end` against shutdown so `q` / Ctrl+C kills in-flight children.

**Tech Stack:** Rust 2021, `tokio` 1.x (multi-thread runtime already configured for the bridge), `crossterm` 0.28 (new), `serde_json`.

**Spec:** `wiki/2026-04-14-live-status-design.md`

---

## File Structure

Files to create:
- `src/status/live.rs` — live loop, `StatsSource` trait, `TerminalGuard`, `fetch_once`

Files to modify:
- `src/status.rs` → **rename to** `src/status/mod.rs` (no content change in Task 1)
- `src/status/mod.rs` — add `ParseError`, `parse_stats_json`, `compute_cpu_pct`, `detect_container_set_change`, `ColumnWidths`; extend `format_table`; change `parse_ls_json` signature
- `src/main.rs` — add `--no-stream` flag on `Status`, drop `ls` alias, dispatch live vs one-shot
- `Cargo.toml` — add `crossterm` dep
- `README.md` — remove `ls` example, add live mode description

---

## Task 1: Split `src/status.rs` into a submodule and add `crossterm`

Pure prep. Move the existing file into a submodule directory and add the new dependency. No behavior change.

**Files:**
- Rename: `src/status.rs` → `src/status/mod.rs`
- Modify: `Cargo.toml`

- [x] **Step 1.1: Move the file**

Run:
```
mkdir -p src/status && git mv src/status.rs src/status/mod.rs
```

- [x] **Step 1.2: Add `crossterm` to `Cargo.toml`**

Modify the `[dependencies]` section. Add after `libc = "0.2"`:

```toml
crossterm = { version = "0.28", features = ["event-stream"] }
```

The `event-stream` feature is required for the async `EventStream` used by the live loop.

- [x] **Step 1.3: Verify the build still passes**

Run: `cargo build`
Expected: clean build, no errors.

- [x] **Step 1.4: Verify existing tests still pass**

Run: `cargo test`
Expected: 201 passed; 0 failed.

---

## Task 2: Change `parse_ls_json` signature to `Result`

Make parse failures distinguishable from "no agentbox containers" by returning `Result<Vec<Row>, ParseError>`. Symmetric with `parse_stats_json` added later.

**Files:**
- Modify: `src/status/mod.rs` (lines currently around 47-86)

- [x] **Step 2.1: Write failing tests for the new signature**

At the top of the `#[cfg(test)] mod tests` block in `src/status/mod.rs`, add:

```rust
#[test]
fn test_parse_ls_json_malformed_returns_err() {
    assert!(parse_ls_json("not json").is_err());
    assert!(parse_ls_json("").is_err());
    assert!(parse_ls_json("{not an array}").is_err());
}

#[test]
fn test_parse_ls_json_valid_empty_returns_ok_empty() {
    assert_eq!(parse_ls_json("[]").unwrap(), vec![]);
}
```

Also update every existing `test_parse_ls_json_*` call site in the file. Search for `parse_ls_json(` in the tests and change `let rows = parse_ls_json(...)` to `let rows = parse_ls_json(...).unwrap()`. For `test_parse_ls_json_malformed` (if present) assert `is_err()` instead.

- [x] **Step 2.2: Run tests to verify they fail**

Run: `cargo test parse_ls_json -- --nocapture`
Expected: FAIL — either compile errors (signature mismatch) or test assertion failures.

- [x] **Step 2.3: Add `ParseError` enum and change `parse_ls_json`**

In `src/status/mod.rs`, near the top (after the existing `use` lines), add:

```rust
#[derive(Debug)]
pub struct ParseError {
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "parse error: {}", self.message)
    }
}

impl std::error::Error for ParseError {}

impl ParseError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self { message: msg.into() }
    }
}
```

Replace the existing `parse_ls_json` body with:

```rust
pub fn parse_ls_json(json: &str) -> Result<Vec<Row>, ParseError> {
    let containers: Vec<serde_json::Value> = serde_json::from_str(json)
        .map_err(|e| ParseError::new(format!("container ls JSON: {}", e)))?;
    let mut rows = Vec::new();
    for c in &containers {
        let name = c
            .pointer("/configuration/id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !name.starts_with("agentbox-") {
            continue;
        }
        let status_str = c.pointer("/status").and_then(|v| v.as_str()).unwrap_or("");
        let state = match status_str {
            "running" => State::Running,
            _ => State::Stopped,
        };
        let workdir = c
            .pointer("/configuration/initProcess/workingDirectory")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let started_unix = c
            .pointer("/startedDate")
            .and_then(|v| v.as_f64())
            .map(|d| d as i64 + APPLE_EPOCH_OFFSET);

        rows.push(Row {
            name: name.to_string(),
            state,
            workdir,
            started_unix,
            sessions: None,
            cpu_pct: None,
            mem_used: None,
            mem_total: None,
        });
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}
```

- [x] **Step 2.4: Update the one in-tree caller**

In the same file, find `fetch_basic` (currently around line 392). Change the line `let mut rows = parse_ls_json(&stdout);` to:

```rust
let mut rows = parse_ls_json(&stdout)
    .unwrap_or_else(|e| {
        eprintln!("[agentbox] warning: could not parse container ls output: {}", e);
        Vec::new()
    });
```

This keeps one-shot mode tolerant of parse failures (warns + continues with empty list) — the live path handles it differently via `StatsSource`.

- [x] **Step 2.5: Run all tests to verify**

Run: `cargo test`
Expected: all pass — both new tests and existing tests (now calling `.unwrap()`).

---

## Task 3: Add `parse_stats_json`

New pure parser for the JSON output of `container stats --format json`. Returns a map from container name to raw counters, which are fed into CPU% delta computation.

**Files:**
- Modify: `src/status/mod.rs`

- [ ] **Step 3.1: Write failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/status/mod.rs`:

```rust
const STATS_JSON_SAMPLE: &str = r#"[
    {
        "id": "agentbox-agentbox-71e6bc",
        "cpuUsageUsec": 1315142153,
        "memoryUsageBytes": 4180971520,
        "memoryLimitBytes": 8589934592,
        "numProcesses": 94,
        "networkRxBytes": 265925507,
        "networkTxBytes": 171185114,
        "blockReadBytes": 1142607872,
        "blockWriteBytes": 250761216
    }
]"#;

#[test]
fn test_parse_stats_json_one_container() {
    let map = parse_stats_json(STATS_JSON_SAMPLE).unwrap();
    assert_eq!(map.len(), 1);
    let s = map.get("agentbox-agentbox-71e6bc").unwrap();
    assert_eq!(s.cpu_usage_usec, 1_315_142_153);
    assert_eq!(s.memory_usage_bytes, 4_180_971_520);
    assert_eq!(s.memory_limit_bytes, 8_589_934_592);
}

#[test]
fn test_parse_stats_json_empty_array_is_ok() {
    let map = parse_stats_json("[]").unwrap();
    assert!(map.is_empty());
}

#[test]
fn test_parse_stats_json_malformed_returns_err() {
    assert!(parse_stats_json("not json").is_err());
    assert!(parse_stats_json("").is_err());
    assert!(parse_stats_json("{not array}").is_err());
}

#[test]
fn test_parse_stats_json_skips_entries_missing_required_fields() {
    let json = r#"[
        {"id": "agentbox-ok", "cpuUsageUsec": 100, "memoryUsageBytes": 200, "memoryLimitBytes": 300},
        {"id": "agentbox-missing-cpu", "memoryUsageBytes": 1, "memoryLimitBytes": 2}
    ]"#;
    let map = parse_stats_json(json).unwrap();
    assert_eq!(map.len(), 1);
    assert!(map.contains_key("agentbox-ok"));
}
```

- [ ] **Step 3.2: Run tests to verify they fail**

Run: `cargo test parse_stats_json`
Expected: FAIL — `parse_stats_json` and `RawStats` don't exist yet.

- [ ] **Step 3.3: Implement `RawStats` and `parse_stats_json`**

In `src/status/mod.rs`, near the existing `parse_stats_text`, add:

```rust
/// Raw counters from `container stats --format json`. The CPU counter is
/// cumulative microseconds since the container started; compute percentage
/// from a delta via `compute_cpu_pct`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawStats {
    pub cpu_usage_usec: u64,
    pub memory_usage_bytes: u64,
    pub memory_limit_bytes: u64,
}

/// Parse `container stats --format json` output. Returns a map from
/// container id to raw counters. Entries missing any of the three
/// required fields are skipped silently (defensive against partial
/// JSON). Malformed top-level JSON returns `Err`.
pub fn parse_stats_json(json: &str) -> Result<HashMap<String, RawStats>, ParseError> {
    let entries: Vec<serde_json::Value> = serde_json::from_str(json)
        .map_err(|e| ParseError::new(format!("container stats JSON: {}", e)))?;
    let mut out = HashMap::new();
    for e in &entries {
        let id = match e.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let cpu = match e.get("cpuUsageUsec").and_then(|v| v.as_u64()) {
            Some(v) => v,
            None => continue,
        };
        let mu = match e.get("memoryUsageBytes").and_then(|v| v.as_u64()) {
            Some(v) => v,
            None => continue,
        };
        let ml = match e.get("memoryLimitBytes").and_then(|v| v.as_u64()) {
            Some(v) => v,
            None => continue,
        };
        out.insert(id, RawStats {
            cpu_usage_usec: cpu,
            memory_usage_bytes: mu,
            memory_limit_bytes: ml,
        });
    }
    Ok(out)
}
```

- [ ] **Step 3.4: Run tests to verify they pass**

Run: `cargo test parse_stats_json`
Expected: all four tests pass.

---

## Task 4: Add `compute_cpu_pct`

Pure function that computes CPU percentage from two consecutive CPU usage samples and the wall-clock elapsed time between them.

**Files:**
- Modify: `src/status/mod.rs`

- [ ] **Step 4.1: Write failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/status/mod.rs`:

```rust
#[test]
fn test_compute_cpu_pct_normal_delta() {
    // 1_000_000 usec over 2 seconds = 50% of one core
    let r = compute_cpu_pct(0, 1_000_000, 2_000_000);
    assert!((r.unwrap() - 50.0).abs() < 0.001);
}

#[test]
fn test_compute_cpu_pct_hundred_percent_one_core() {
    // 2s of CPU over 2s wall = 100% one core
    let r = compute_cpu_pct(0, 2_000_000, 2_000_000);
    assert!((r.unwrap() - 100.0).abs() < 0.001);
}

#[test]
fn test_compute_cpu_pct_multi_core() {
    // 4s of CPU over 2s wall = 200% (two cores fully utilized)
    let r = compute_cpu_pct(0, 4_000_000, 2_000_000);
    assert!((r.unwrap() - 200.0).abs() < 0.001);
}

#[test]
fn test_compute_cpu_pct_counter_reset_returns_none() {
    // Previous sample greater than current → container restarted.
    // Skip this sample rather than emit a negative value.
    assert_eq!(compute_cpu_pct(5_000_000, 1_000_000, 2_000_000), None);
}

#[test]
fn test_compute_cpu_pct_zero_elapsed_returns_none() {
    assert_eq!(compute_cpu_pct(0, 1_000_000, 0), None);
}

#[test]
fn test_compute_cpu_pct_no_work_done() {
    // No CPU used, non-zero elapsed = 0%
    assert_eq!(compute_cpu_pct(100, 100, 2_000_000), Some(0.0));
}
```

- [ ] **Step 4.2: Run tests to verify they fail**

Run: `cargo test compute_cpu_pct`
Expected: FAIL — function not defined.

- [ ] **Step 4.3: Implement `compute_cpu_pct`**

In `src/status/mod.rs`, add:

```rust
/// Compute CPU percentage from two consecutive samples.
///
/// Inputs are in microseconds. `prev_usec` and `curr_usec` are the
/// cumulative CPU time samples; `elapsed_usec` is the wall-clock time
/// between samples.
///
/// Returns `None` when:
/// - `elapsed_usec` is zero (division by zero)
/// - `curr_usec < prev_usec` (counter reset, e.g. container restarted)
///
/// 100% = one fully utilized core. Multi-core containers can exceed 100%.
pub fn compute_cpu_pct(prev_usec: u64, curr_usec: u64, elapsed_usec: u64) -> Option<f64> {
    if elapsed_usec == 0 {
        return None;
    }
    if curr_usec < prev_usec {
        return None;
    }
    let delta = (curr_usec - prev_usec) as f64;
    Some(delta / elapsed_usec as f64 * 100.0)
}
```

- [ ] **Step 4.4: Run tests to verify they pass**

Run: `cargo test compute_cpu_pct`
Expected: all six tests pass.

---

## Task 5: Add `detect_container_set_change`

Pure function returning `true` when the set of running container ids differs between two stats-map snapshots. Used in live mode to trigger an event-driven `ls` refresh.

**Files:**
- Modify: `src/status/mod.rs`

- [ ] **Step 5.1: Write failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/status/mod.rs`:

```rust
#[test]
fn test_detect_container_set_change_no_change() {
    let a: Vec<&str> = vec!["x", "y"];
    let b: Vec<&str> = vec!["y", "x"]; // order shouldn't matter
    assert!(!detect_container_set_change(&a, &b));
}

#[test]
fn test_detect_container_set_change_container_added() {
    let a: Vec<&str> = vec!["x"];
    let b: Vec<&str> = vec!["x", "y"];
    assert!(detect_container_set_change(&a, &b));
}

#[test]
fn test_detect_container_set_change_container_removed() {
    let a: Vec<&str> = vec!["x", "y"];
    let b: Vec<&str> = vec!["x"];
    assert!(detect_container_set_change(&a, &b));
}

#[test]
fn test_detect_container_set_change_both_empty() {
    let a: Vec<&str> = vec![];
    let b: Vec<&str> = vec![];
    assert!(!detect_container_set_change(&a, &b));
}

#[test]
fn test_detect_container_set_change_same_size_different_members() {
    let a: Vec<&str> = vec!["x"];
    let b: Vec<&str> = vec!["y"];
    assert!(detect_container_set_change(&a, &b));
}
```

- [ ] **Step 5.2: Run tests to verify they fail**

Run: `cargo test detect_container_set_change`
Expected: FAIL — function not defined.

- [ ] **Step 5.3: Implement `detect_container_set_change`**

In `src/status/mod.rs`, add:

```rust
/// Returns `true` if the set of container ids differs between the two
/// slices. Order-independent. Intended for cheaply detecting when the
/// live loop should re-run `container ls --all --format json`.
pub fn detect_container_set_change(prev: &[&str], curr: &[&str]) -> bool {
    if prev.len() != curr.len() {
        return true;
    }
    let prev_set: std::collections::HashSet<&str> = prev.iter().copied().collect();
    for id in curr {
        if !prev_set.contains(id) {
            return true;
        }
    }
    false
}
```

- [ ] **Step 5.4: Run tests to verify they pass**

Run: `cargo test detect_container_set_change`
Expected: all five tests pass.

---

## Task 6: Add `ColumnWidths` with seeding + monotonic growth

Captures per-column widths for the live table. Seeded with representative-maximum values for CPU/MEM (prevents jitter as CPU% shifts from 9% to 10%). NAME and PROJECT grow monotonically as new wider rows arrive.

**Files:**
- Modify: `src/status/mod.rs`

- [ ] **Step 6.1: Write failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/status/mod.rs`:

```rust
fn mk_row(name: &str, workdir: &str) -> Row {
    Row {
        name: name.to_string(),
        state: State::Running,
        workdir: workdir.to_string(),
        started_unix: Some(0),
        sessions: Some(0),
        cpu_pct: None,
        mem_used: None,
        mem_total: None,
    }
}

#[test]
fn test_column_widths_seeded_defaults() {
    let w = ColumnWidths::seeded();
    // CPU seed: "999.9%" = 6 chars
    assert!(w.cpu >= 6);
    // MEM seed: "99.9/99.9G" = 10 chars
    assert!(w.mem >= 10);
}

#[test]
fn test_column_widths_update_grows_name_and_project() {
    let home = std::path::PathBuf::from("/home/u");
    let mut w = ColumnWidths::seeded();
    let rows = vec![mk_row("agentbox-a-123456", "/home/u/short")];
    w.update(&rows, &home);
    assert_eq!(w.name, "agentbox-a-123456".len());

    let longer = vec![mk_row("agentbox-aaaaaaaaaaaaa-123456", "/home/u/short")];
    w.update(&longer, &home);
    assert_eq!(w.name, "agentbox-aaaaaaaaaaaaa-123456".len());
}

#[test]
fn test_column_widths_never_shrink_name() {
    let home = std::path::PathBuf::from("/home/u");
    let mut w = ColumnWidths::seeded();
    w.update(&[mk_row("agentbox-longname-111111", "/home/u/p")], &home);
    let before = w.name;

    // Now a shorter row arrives — width must not shrink.
    w.update(&[mk_row("ab-x-1", "/home/u/p")], &home);
    assert_eq!(w.name, before);
}

#[test]
fn test_column_widths_cpu_is_floor_not_clamp() {
    let w = ColumnWidths::seeded();
    // Seed is 6 chars ("999.9%"). `1234.5%` is 7 chars — widths grow.
    // The struct itself doesn't expose growth for CPU (seeded only),
    // but format_table must not truncate longer values. This is a
    // format_table test, so just verify the seed floor here.
    assert_eq!(w.cpu, 6);
    assert_eq!(w.mem, 10);
}
```

- [ ] **Step 6.2: Run tests to verify they fail**

Run: `cargo test column_widths`
Expected: FAIL — `ColumnWidths` not defined.

- [ ] **Step 6.3: Implement `ColumnWidths`**

In `src/status/mod.rs`, add (after `HEADERS` constant, near line 212):

```rust
/// Per-column width tracker for live mode. CPU and MEM are seeded with
/// representative-maximum values so jitter at the every-2s update rate
/// is eliminated. NAME and PROJECT grow monotonically as new wider rows
/// arrive (they never shrink — a long-named container stopping doesn't
/// pull columns leftward).
#[derive(Debug, Clone)]
pub struct ColumnWidths {
    pub name: usize,
    pub status: usize,
    pub project: usize,
    pub cpu: usize,
    pub mem: usize,
    pub uptime: usize,
    pub sessions: usize,
}

impl ColumnWidths {
    /// Build a fresh `ColumnWidths` with seeded CPU/MEM floors and
    /// every other column initialized to the header width floor.
    pub fn seeded() -> Self {
        Self {
            name: "NAME".len(),
            status: "STATUS".len(),
            project: "PROJECT".len(),
            cpu: "999.9%".len(),           // 6
            mem: "99.9/99.9G".len(),       // 10
            uptime: "UPTIME".len(),
            sessions: "SESSIONS".len(),
        }
    }

    /// Grow NAME and PROJECT widths based on the actual rows being
    /// rendered. Never shrinks. Intended to be called after every
    /// `ls` refresh in live mode. CPU/MEM/UPTIME/SESSIONS are not
    /// updated here — CPU and MEM are floor-seeded in `seeded()`;
    /// UPTIME is intentionally not tracked to avoid reserving space
    /// for `99d 23h` on every frame (see design doc).
    pub fn update(&mut self, rows: &[Row], home: &std::path::Path) {
        for r in rows {
            let n = r.name.chars().count();
            if n > self.name {
                self.name = n;
            }
            let p = shorten_path(&r.workdir, home, PROJECT_MAX_WIDTH);
            let pw = p.chars().count();
            if pw > self.project {
                self.project = pw;
            }
        }
    }
}
```

- [ ] **Step 6.4: Run tests to verify they pass**

Run: `cargo test column_widths`
Expected: all four tests pass.

---

## Task 7: Extend `format_table` to accept optional widths

Add an optional `widths: Option<&ColumnWidths>` parameter. When provided, widths act as floors (grown to fit actual cell content, never below the floor). When `None`, behavior is unchanged.

**Files:**
- Modify: `src/status/mod.rs` (function around line 276)

- [ ] **Step 7.1: Write failing tests**

Add to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn test_format_table_no_widths_matches_old_behavior() {
    let home = std::path::PathBuf::from("/home/u");
    let rows = vec![mk_row("agentbox-a-1", "/home/u/p")];
    let old = format_table(&rows, None, false, &home, 0, None);
    // Sanity: old includes NAME header and the one row
    assert!(old.contains("NAME"));
    assert!(old.contains("agentbox-a-1"));
}

#[test]
fn test_format_table_widths_act_as_floor() {
    let home = std::path::PathBuf::from("/home/u");
    let rows = vec![mk_row("agentbox-a-1", "/home/u/p")];
    let w = ColumnWidths::seeded();
    let out = format_table(&rows, None, false, &home, 0, Some(&w));
    // CPU column is seeded to 6 chars ("999.9%") but rendered value is
    // "--" (2 chars). With floor, the header row "CPU   " is padded
    // to 6+.
    assert!(out.contains("CPU   "));
}

#[test]
fn test_format_table_widths_allow_growth_beyond_floor() {
    let home = std::path::PathBuf::from("/home/u");
    // Name way longer than any seed
    let rows = vec![mk_row("agentbox-very-very-very-long-name-abc", "/home/u/p")];
    let w = ColumnWidths::seeded();
    let out = format_table(&rows, None, false, &home, 0, Some(&w));
    assert!(out.contains("agentbox-very-very-very-long-name-abc"));
}
```

- [ ] **Step 7.2: Run tests to verify they fail**

Run: `cargo test format_table`
Expected: FAIL — `format_table` signature mismatch (6 args, not 5).

- [ ] **Step 7.3: Extend `format_table`**

In `src/status/mod.rs`, replace the existing `pub fn format_table(...)` with:

```rust
/// Render the full table as a single newline-terminated string.
///
/// `current_name` (when Some + `use_color`) bolds the matching row.
/// `now_unix` is supplied so the function stays pure (testable).
/// `widths` (when Some) supplies per-column floor widths — actual cell
/// content is still allowed to exceed them (monotonic growth). Pass
/// `None` for one-shot behavior where widths are derived purely from
/// the rows being rendered.
pub fn format_table(
    rows: &[Row],
    current_name: Option<&str>,
    use_color: bool,
    home: &Path,
    now_unix: i64,
    widths: Option<&ColumnWidths>,
) -> String {
    let mut all_rows: Vec<[String; 7]> = Vec::new();
    all_rows.push(HEADERS.map(String::from));
    for row in rows {
        all_rows.push(row_cells(row, home, now_unix));
    }
    all_rows.push(totals_cells(rows));

    let mut col_widths = [0usize; 7];
    if let Some(w) = widths {
        col_widths[0] = w.name;
        col_widths[1] = w.status;
        col_widths[2] = w.project;
        col_widths[3] = w.cpu;
        col_widths[4] = w.mem;
        col_widths[5] = w.uptime;
        col_widths[6] = w.sessions;
    }
    for r in &all_rows {
        for (i, cell) in r.iter().enumerate() {
            let w = cell.chars().count();
            if w > col_widths[i] {
                col_widths[i] = w;
            }
        }
    }

    let data_count = rows.len();
    let mut out = String::new();
    for (idx, r) in all_rows.iter().enumerate() {
        let mut line = String::new();
        for (i, cell) in r.iter().enumerate() {
            if i == r.len() - 1 {
                line.push_str(cell);
            } else {
                line.push_str(&pad_right(cell, col_widths[i]));
                line.push_str("  ");
            }
        }
        let is_data_row = idx > 0 && idx <= data_count;
        let is_current = is_data_row
            && current_name.map(|cn| r[0] == cn).unwrap_or(false);
        if use_color && is_current {
            out.push_str("\x1b[1m");
            out.push_str(&line);
            out.push_str("\x1b[22m");
        } else {
            out.push_str(&line);
        }
        out.push('\n');
    }
    out
}
```

- [ ] **Step 7.4: Update the one existing caller**

In the same file, find the one-shot `run()` function (around line 444). Update the two `format_table(...)` call sites to pass `None` as the last argument:

```rust
let table = format_table(&rows, current_ref, use_color, &home, now, None);
// ...
let table2 = format_table(&rows, current_ref, use_color, &home, now2, None);
```

- [ ] **Step 7.5: Run all tests**

Run: `cargo test`
Expected: all existing tests pass + three new `format_table_*` tests pass.

---

## Task 8: Create `src/status/live.rs` with `fetch_once`

New file for live-mode code. First addition: `fetch_once` — the subprocess-with-shutdown-race helper used by all stats/ls/ps polls in live mode.

**Files:**
- Create: `src/status/live.rs`
- Modify: `src/status/mod.rs` (to add `mod live;`)

- [ ] **Step 8.1: Declare the module**

Add to the top of `src/status/mod.rs` (after the `use` lines):

```rust
#[cfg(not(test))]
pub mod live;

#[cfg(test)]
pub mod live;
```

(Single `pub mod live;` would work; using both `cfg(...)` bodies makes it explicit the module is available in test builds too.)

Actually — simplify to just:

```rust
pub mod live;
```

- [ ] **Step 8.2: Create `src/status/live.rs` with `fetch_once` and a smoke test**

Create the new file with:

```rust
//! Live-mode implementation for `agentbox status`.
//!
//! Contains the tokio-based polling loop, terminal-mode RAII guard, and
//! the subprocess helper that races stdout/stderr reads against a
//! shutdown watch channel.

use anyhow::{bail, Context, Result};
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::watch;

/// Spawn a subprocess, drain stdout+stderr concurrently, and race the
/// drain against a shutdown signal.
///
/// - Both stdout and stderr are piped (not inherited) — child diagnostics
///   never reach the alt-screen UI.
/// - On non-zero exit, returns `Err` with captured stderr included.
/// - On shutdown, the child is killed (SIGKILL via `start_kill`) and
///   reaped, and the function returns an error.
pub async fn fetch_once(
    program: &str,
    args: &[&str],
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Vec<u8>> {
    // Synchronous check: is shutdown already set? `borrow()` returns the
    // current value independent of this receiver's version-tracking, so
    // this catches the "already shut down" case that a later
    // `changed()` call would miss (since `changed()` only fires on a
    // *new* change relative to the receiver's last observed version).
    if *shutdown.borrow() {
        bail!("shutdown requested");
    }

    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)   // critical: if the future is cancelled
                              // (e.g. outer select picks a different arm),
                              // Tokio must SIGKILL the child. Without this,
                              // dropping a Child leaves an orphan process.
        .spawn()
        .with_context(|| format!("failed to spawn `{} {}`", program, args.join(" ")))?;
    let mut stdout = child.stdout.take().expect("stdout was requested");
    let mut stderr = child.stderr.take().expect("stderr was requested");
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();

    let drain = async {
        tokio::try_join!(
            stdout.read_to_end(&mut out_buf),
            stderr.read_to_end(&mut err_buf),
        )
    };

    tokio::select! {
        res = drain => {
            res.with_context(|| format!("failed reading output of `{}`", program))?;
            let status = child.wait().await
                .with_context(|| format!("failed waiting on `{}`", program))?;
            if !status.success() {
                bail!(
                    "`{} {}` exited with {}: {}",
                    program,
                    args.join(" "),
                    status,
                    String::from_utf8_lossy(&err_buf).trim(),
                );
            }
            Ok(out_buf)
        }
        _ = shutdown.changed() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            bail!("shutdown requested");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fetch_once_success_returns_stdout() {
        let (_tx, mut rx) = watch::channel(false);
        let out = fetch_once("printf", &["hello"], &mut rx).await.unwrap();
        assert_eq!(out, b"hello");
    }

    #[tokio::test]
    async fn test_fetch_once_nonzero_exit_is_error() {
        let (_tx, mut rx) = watch::channel(false);
        // `false` always exits non-zero
        let err = fetch_once("false", &[], &mut rx).await.unwrap_err();
        assert!(err.to_string().contains("exited with"));
    }

    #[tokio::test]
    async fn test_fetch_once_shutdown_kills_child() {
        let (tx, mut rx) = watch::channel(false);
        // sleep 30s in the background; shutdown within 100ms.
        let handle = tokio::spawn(async move {
            fetch_once("sleep", &["30"], &mut rx).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        tx.send(true).unwrap();
        let start = std::time::Instant::now();
        let res = handle.await.unwrap();
        assert!(res.is_err());
        // Should return in well under the sleep duration.
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[tokio::test]
    async fn test_fetch_once_already_shutdown_bails_synchronously() {
        // Regression test: previously, a receiver that had already
        // observed the flip wouldn't see it again via `changed()`, so
        // a second subprocess call after an interrupted one could
        // silently run to completion. The sync `borrow()` check at
        // the top of fetch_once prevents that.
        let (tx, mut rx) = watch::channel(false);
        tx.send(true).unwrap();
        // Mark rx as having seen the change by calling changed once.
        rx.changed().await.unwrap();

        let start = std::time::Instant::now();
        let res = fetch_once("sleep", &["10"], &mut rx).await;
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("shutdown"));
        // Must bail before spawning — well under any subprocess start time.
        assert!(start.elapsed() < std::time::Duration::from_millis(500));
    }
}
```

- [ ] **Step 8.3: Run the new tests**

Run: `cargo test status::live::tests`
Expected: 3 tests pass. (May take a second or two due to the shutdown test.) Note: this is a binary crate (no `[lib]` target in Cargo.toml), so the `--lib` flag is not applicable — `cargo test <filter>` runs unit tests compiled into the binary.

- [ ] **Step 8.4: Run all tests to confirm nothing regressed**

Run: `cargo test`
Expected: everything passes.

---

## Task 9: Add `TerminalGuard` for RAII terminal management

RAII guard that enters alt screen + hides cursor + enables raw mode on construction, and restores everything on `Drop` (including during panic unwind). Also installs a panic hook that restores the terminal before the default panic handler prints.

**Files:**
- Modify: `src/status/live.rs`

- [ ] **Step 9.1: Write a smoke test**

Append to the `#[cfg(test)] mod tests` block in `src/status/live.rs`:

```rust
#[test]
fn test_terminal_guard_construction_is_safe_without_tty() {
    // In CI / test environments stdout is usually not a TTY. The guard
    // should gracefully skip the real terminal-mode switches in that
    // case instead of panicking. This just verifies no panic / no
    // process death.
    let _ = TerminalGuard::new_if_tty();
}
```

- [ ] **Step 9.2: Run to verify failure**

Run: `cargo test test_terminal_guard`
Expected: FAIL — `TerminalGuard` doesn't exist.

- [ ] **Step 9.3: Implement `TerminalGuard`**

Add to `src/status/live.rs` (above the tests module):

```rust
use std::io::{self, IsTerminal, Write};

use crossterm::{cursor, terminal, ExecutableCommand};

/// RAII guard for TUI terminal state. On construction (when stdout is a
/// TTY) it enters the alternate screen, hides the cursor, and enables
/// raw mode. On drop (including panic unwind) it reverses all three.
///
/// The guard is also paired with a process-global panic hook that runs
/// the same restoration inline, so a panic reaches a sane terminal
/// before the default panic handler prints.
pub struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    /// Construct a guard. If stdout is not a TTY, returns an inactive
    /// guard that does nothing on drop (live mode should not be invoked
    /// off-TTY, but this avoids surprising callers).
    pub fn new_if_tty() -> Self {
        if !io::stdout().is_terminal() {
            return Self { active: false };
        }
        install_panic_hook_once();
        let mut stdout = io::stdout();
        let _ = stdout.execute(terminal::EnterAlternateScreen);
        let _ = stdout.execute(cursor::Hide);
        let _ = terminal::enable_raw_mode();
        let _ = stdout.flush();
        Self { active: true }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        restore_terminal();
    }
}

fn restore_terminal() {
    let mut stdout = io::stdout();
    let _ = terminal::disable_raw_mode();
    let _ = stdout.execute(cursor::Show);
    let _ = stdout.execute(terminal::LeaveAlternateScreen);
    let _ = stdout.flush();
}

fn install_panic_hook_once() {
    use std::sync::OnceLock;
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            default(info);
        }));
    });
}
```

- [ ] **Step 9.4: Run tests to verify they pass**

Run: `cargo test test_terminal_guard`
Expected: PASS. (The guard returns inactive off-TTY, so no real terminal mutation happens under `cargo test`.)

- [ ] **Step 9.5: Run all tests**

Run: `cargo test`
Expected: everything passes.

---

## Task 10: Add `StatsSource` trait + production impl

Abstraction over the subprocess calls so the live loop can be tested with a stub. The production implementation wraps `fetch_once` + `parse_*`. Also adds `PrevSample` for per-container delta tracking.

**Files:**
- Modify: `src/status/live.rs`

- [ ] **Step 10.1: Write a failing test for the stub**

Append to `#[cfg(test)] mod tests` in `src/status/live.rs`:

```rust
use std::collections::HashMap;
use crate::status::RawStats;

struct StubSource {
    stats: Vec<HashMap<String, RawStats>>,
    idx: std::cell::Cell<usize>,
}

#[async_trait::async_trait]
impl StatsSource for StubSource {
    async fn fetch_stats(&mut self) -> Result<HashMap<String, RawStats>> {
        let i = self.idx.get();
        self.idx.set(i + 1);
        Ok(self.stats[i.min(self.stats.len() - 1)].clone())
    }

    async fn fetch_ls(&mut self) -> Result<Vec<crate::status::Row>> {
        Ok(vec![])
    }

    async fn fetch_ps(&mut self) -> Result<String> {
        Ok(String::new())
    }
}

#[tokio::test]
async fn test_stub_stats_source_returns_sequential_frames() {
    let frames = vec![
        HashMap::from([("a".to_string(), RawStats {
            cpu_usage_usec: 0,
            memory_usage_bytes: 100,
            memory_limit_bytes: 1000,
        })]),
        HashMap::from([("a".to_string(), RawStats {
            cpu_usage_usec: 1_000_000,
            memory_usage_bytes: 200,
            memory_limit_bytes: 1000,
        })]),
    ];
    let mut src = StubSource { stats: frames, idx: std::cell::Cell::new(0) };
    let f1 = src.fetch_stats().await.unwrap();
    let f2 = src.fetch_stats().await.unwrap();
    assert_eq!(f1.get("a").unwrap().cpu_usage_usec, 0);
    assert_eq!(f2.get("a").unwrap().cpu_usage_usec, 1_000_000);
}
```

- [ ] **Step 10.2: Add `async_trait` to `Cargo.toml`**

The trait needs async methods. Add to `[dependencies]` in `Cargo.toml`:

```toml
async-trait = "0.1"
```

- [ ] **Step 10.3: Run test to verify it fails**

Run: `cargo test test_stub_stats_source`
Expected: FAIL — `StatsSource` trait doesn't exist.

- [ ] **Step 10.4: Implement `StatsSource` trait, `PrevSample`, and production impl**

Add to `src/status/live.rs`:

```rust
use std::collections::HashMap;
use std::time::Instant;

use async_trait::async_trait;

use crate::status::{parse_ls_json, parse_stats_json, RawStats, Row};

/// Per-container previous-sample tracking for CPU% delta computation.
#[derive(Debug, Clone, Copy)]
pub struct PrevSample {
    pub cpu_usec: u64,
    pub taken_at: Instant,
}

/// Abstraction over the three subprocess calls live mode makes.
/// Real code uses `ContainerSource`; tests use a stub.
#[async_trait]
pub trait StatsSource: Send {
    async fn fetch_stats(&mut self) -> Result<HashMap<String, RawStats>>;
    async fn fetch_ls(&mut self) -> Result<Vec<Row>>;
    async fn fetch_ps(&mut self) -> Result<String>;
}

/// Production `StatsSource` — shells out to the `container` and `ps`
/// binaries, racing each subprocess against the shutdown signal.
pub struct ContainerSource {
    pub verbose: bool,
    pub shutdown: watch::Receiver<bool>,
}

#[async_trait]
impl StatsSource for ContainerSource {
    async fn fetch_stats(&mut self) -> Result<HashMap<String, RawStats>> {
        if self.verbose {
            eprintln!("[agentbox] container stats --format json");
        }
        let bytes = fetch_once(
            "container",
            &["stats", "--format", "json"],
            &mut self.shutdown,
        ).await?;
        let text = std::str::from_utf8(&bytes)
            .context("container stats produced non-UTF-8 output")?;
        Ok(parse_stats_json(text)?)
    }

    async fn fetch_ls(&mut self) -> Result<Vec<Row>> {
        if self.verbose {
            eprintln!("[agentbox] container ls --all --format json");
        }
        let bytes = fetch_once(
            "container",
            &["ls", "--all", "--format", "json"],
            &mut self.shutdown,
        ).await?;
        let text = std::str::from_utf8(&bytes)
            .context("container ls produced non-UTF-8 output")?;
        Ok(parse_ls_json(text)?)
    }

    async fn fetch_ps(&mut self) -> Result<String> {
        let bytes = fetch_once(
            "ps",
            &["-eo", "pid,args"],
            &mut self.shutdown,
        ).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}
```

- [ ] **Step 10.5: Run tests**

Run: `cargo test`
Expected: new stub test passes + all existing pass.

---

## Task 11: Live loop orchestration

The main entry point for live mode. Sets up the watch channel, spawns input and signal readers, runs the tokio::select! loop, and after exit prints a final one-shot snapshot.

**Files:**
- Modify: `src/status/live.rs`

- [ ] **Step 11.1: Write an integration-style test using the stub**

Append to `#[cfg(test)] mod tests` in `src/status/live.rs`:

```rust
#[tokio::test]
async fn test_run_live_with_stub_terminates_on_shutdown() {
    use std::collections::HashMap;

    let frames = (0..10).map(|i| {
        HashMap::from([("agentbox-x-aaaaaa".to_string(), RawStats {
            cpu_usage_usec: (i * 1_000_000) as u64,
            memory_usage_bytes: 100 * 1024 * 1024,
            memory_limit_bytes: 8 * 1024 * 1024 * 1024,
        })])
    }).collect();

    let stub = StubSource { stats: frames, idx: std::cell::Cell::new(0) };
    let (tx, _rx) = watch::channel(false);

    // Trigger shutdown after a short delay.
    let tx2 = tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        tx2.send(true).ok();
    });

    // Headless mode: render disabled, tick interval short.
    let result = run_live_loop(
        Box::new(stub),
        tx,
        LiveOptions { tick_ms: 10, render_enabled: false, ..Default::default() },
    ).await;
    assert!(result.is_ok());
}
```

- [ ] **Step 11.2: Run test to verify it fails**

Run: `cargo test test_run_live_with_stub`
Expected: FAIL — `run_live_loop`, `LiveOptions` don't exist.

- [ ] **Step 11.3: Implement the live loop**

Add to `src/status/live.rs`:

```rust
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;

use crate::status::{
    apply_sessions_to_rows, apply_stale_to_rows, compute_cpu_pct,
    detect_container_set_change, format_table, ColumnWidths, State,
};

/// Options to configure the live loop. Tests set `render_enabled=false`
/// to drive the loop without touching the terminal.
#[derive(Clone)]
pub struct LiveOptions {
    pub tick_ms: u64,
    pub render_enabled: bool,
    /// Refresh ls every Nth tick (periodic fallback).
    pub ls_every_n: u32,
    /// Refresh ps every Nth tick.
    pub ps_every_n: u32,
}

impl Default for LiveOptions {
    fn default() -> Self {
        Self {
            tick_ms: 2000,
            render_enabled: true,
            ls_every_n: 5,
            ps_every_n: 3,
        }
    }
}

/// A single rendered snapshot emitted by the fetcher task. The main
/// loop receives these via mpsc and redraws (or stashes for the final
/// snapshot).
pub struct Frame {
    pub rows: Vec<Row>,
    pub widths: ColumnWidths,
    pub footer_msg: Option<String>,
}

/// Final state returned from `run_live_loop` so the caller can print a
/// final snapshot (using the last-known rows) and flush buffered
/// diagnostics *after* the terminal is restored.
pub struct LiveResult {
    pub rows: Vec<Row>,
    pub current_name: Option<String>,
    pub home: std::path::PathBuf,
    pub stderr_log: Vec<String>,
}

/// Main live-mode loop. Spawns a fetcher task that owns `source` and
/// emits frames over an mpsc channel; the outer select reacts to
/// input, frame arrivals, and shutdown. Because the outer select has
/// no long-running arm bodies, input events are always polled within
/// the scheduler tick — `q`/`Esc`/`Ctrl+C` can interrupt an in-flight
/// fetch.
///
/// Does NOT restore the terminal or print the final snapshot — that
/// is the caller's responsibility, executed *after* dropping the
/// `TerminalGuard`.
///
/// `shutdown_tx` is needed both so the key-handler arm can flip it
/// (ensuring in-flight subprocess children see the shutdown via the
/// receiver passed into `ContainerSource`) and so we can derive
/// receivers for the fetcher task and for the outer select's own
/// shutdown arm.
pub async fn run_live_loop(
    source: Box<dyn StatsSource>,
    shutdown_tx: watch::Sender<bool>,
    opts: LiveOptions,
) -> Result<LiveResult> {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
    let current_name = std::env::current_dir()
        .ok()
        .map(|cwd| crate::container::container_name(&cwd.to_string_lossy()));

    // Spawn the fetcher. It owns source and all per-tick state,
    // emits Frames via mpsc, and returns the diagnostic log when it
    // observes shutdown. Channel capacity 2 lets it get one frame
    // ahead without blocking the main loop.
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<Frame>(2);
    let fetcher_shutdown = shutdown_tx.subscribe();
    let fetcher_opts = opts.clone();
    let fetcher_home = home.clone();
    let fetcher_handle = tokio::spawn(fetcher_task(
        source,
        frame_tx,
        fetcher_shutdown,
        fetcher_opts,
        fetcher_home,
    ));

    // Main-loop local state: last-received frame. On exit, this
    // feeds the final snapshot.
    let mut last_rows: Vec<Row> = Vec::new();
    let mut last_widths = ColumnWidths::seeded();
    let mut last_footer: Option<String> = None;

    let mut shutdown_rx = shutdown_tx.subscribe();
    let mut events: Option<EventStream> = if opts.render_enabled {
        Some(EventStream::new())
    } else {
        None
    };

    loop {
        let maybe_evt = async {
            match events.as_mut() {
                Some(s) => s.next().await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            _ = shutdown_rx.changed() => {
                break;
            }
            evt = maybe_evt => {
                if let Some(Ok(Event::Key(KeyEvent { code, modifiers, kind, .. }))) = evt {
                    if kind == KeyEventKind::Release {
                        continue;
                    }
                    let quit = matches!(code, KeyCode::Char('q') | KeyCode::Esc)
                        || (modifiers.contains(KeyModifiers::CONTROL)
                            && matches!(code, KeyCode::Char('c')));
                    if quit {
                        // Flip shutdown so the fetcher's in-flight
                        // fetch_once observes it and kills its child.
                        let _ = shutdown_tx.send(true);
                        break;
                    }
                }
            }
            maybe_frame = frame_rx.recv() => {
                match maybe_frame {
                    Some(f) => {
                        last_rows = f.rows;
                        last_widths = f.widths;
                        last_footer = f.footer_msg;
                        if opts.render_enabled {
                            render_frame(
                                &last_rows,
                                current_name.as_deref(),
                                &home,
                                &last_widths,
                                last_footer.as_deref(),
                            );
                        }
                    }
                    None => {
                        // Fetcher task ended (shutdown path or panic).
                        break;
                    }
                }
            }
        }
    }

    // Close the frame channel explicitly. If we don't, the fetcher can
    // deadlock on `frame_tx.send()` when the buffer is full and we've
    // stopped receiving — `fetcher_handle.await` would then hang
    // forever. Dropping the receiver causes the next send to return
    // Err, which the fetcher treats as shutdown.
    drop(frame_rx);

    // Give the fetcher a chance to drain (it observes shutdown via its
    // own watch receiver, or via the closed channel above). Collect its
    // stderr log.
    let stderr_log: Vec<String> = match fetcher_handle.await {
        Ok(log) => log,
        Err(_) => Vec::new(),
    };

    Ok(LiveResult {
        rows: last_rows,
        current_name,
        home,
        stderr_log,
    })
}

/// Returns true when the error message came from `fetch_once`'s
/// shutdown path — those aren't user-visible diagnostics, they're
/// just normal shutdown plumbing.
fn is_shutdown_err(msg: &str) -> bool {
    msg.contains("shutdown requested")
}

/// Append a diagnostic line to `log` *unless* it's a shutdown error.
/// Keeps the per-error filter logic in one place.
fn log_non_shutdown(log: &mut Vec<String>, label: &str, err: &anyhow::Error) {
    let msg = err.to_string();
    if !is_shutdown_err(&msg) {
        log.push(format!("{}: {}", label, msg));
    }
}

/// Fetcher task: owns `source`, runs the tick loop, emits `Frame` on
/// each successful (or failed) tick, and returns the accumulated
/// stderr log when shutdown is observed. This is the only place the
/// subprocess-heavy work happens, and by running in its own task it
/// does not block the main loop's input polling.
async fn fetcher_task(
    mut source: Box<dyn StatsSource>,
    frame_tx: tokio::sync::mpsc::Sender<Frame>,
    mut shutdown_rx: watch::Receiver<bool>,
    opts: LiveOptions,
    home: std::path::PathBuf,
) -> Vec<String> {
    let mut stderr_log: Vec<String> = Vec::new();

    // Initial state.
    let mut rows: Vec<Row> = match source.fetch_ls().await {
        Ok(r) => r,
        Err(e) => {
            log_non_shutdown(&mut stderr_log, "initial container ls", &e);
            Vec::new()
        }
    };
    if let Ok(ps_text) = source.fetch_ps().await {
        apply_sessions_to_rows(&mut rows, &ps_text);
    }
    apply_stale_to_rows(&mut rows);

    let mut widths = ColumnWidths::seeded();
    widths.update(&rows, &home);

    // Emit an initial frame so the table appears before the first tick.
    let _ = frame_tx.send(Frame {
        rows: rows.clone(),
        widths: widths.clone(),
        footer_msg: None,
    }).await;

    let mut prev_samples: HashMap<String, PrevSample> = HashMap::new();
    let mut prev_ids: Vec<String> = Vec::new();

    let mut ticker = tokio::time::interval(Duration::from_millis(opts.tick_ms));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // First interval tick fires immediately; consume it so we don't
    // duplicate the initial frame we just sent.
    ticker.tick().await;

    let mut frame_idx: u32 = 0;

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                return stderr_log;
            }
            _ = ticker.tick() => {
                frame_idx = frame_idx.wrapping_add(1);
                let mut footer_msg: Option<String> = None;

                match source.fetch_stats().await {
                    Ok(stats) => {
                        let curr_ids: Vec<&str> = stats.keys().map(String::as_str).collect();
                        let prev_ref: Vec<&str> = prev_ids.iter().map(String::as_str).collect();
                        let set_changed = detect_container_set_change(&prev_ref, &curr_ids);
                        prev_ids = stats.keys().cloned().collect();

                        if set_changed || frame_idx % opts.ls_every_n == 0 {
                            match source.fetch_ls().await {
                                Ok(new_rows) => {
                                    rows = new_rows;
                                    apply_stale_to_rows(&mut rows);
                                    widths.update(&rows, &home);
                                }
                                Err(e) => {
                                    log_non_shutdown(&mut stderr_log, "container ls", &e);
                                }
                            }
                        }

                        if frame_idx % opts.ps_every_n == 0 {
                            match source.fetch_ps().await {
                                Ok(ps) => apply_sessions_to_rows(&mut rows, &ps),
                                Err(e) => log_non_shutdown(&mut stderr_log, "ps", &e),
                            }
                        }

                        let now = Instant::now();
                        for row in rows.iter_mut() {
                            if row.state != State::Running {
                                continue;
                            }
                            if let Some(s) = stats.get(&row.name) {
                                row.mem_used = Some(s.memory_usage_bytes);
                                row.mem_total = Some(s.memory_limit_bytes);
                                if let Some(prev) = prev_samples.get(&row.name) {
                                    let elapsed_us = now
                                        .saturating_duration_since(prev.taken_at)
                                        .as_micros() as u64;
                                    row.cpu_pct = compute_cpu_pct(
                                        prev.cpu_usec,
                                        s.cpu_usage_usec,
                                        elapsed_us,
                                    );
                                } else {
                                    row.cpu_pct = None;
                                }
                                prev_samples.insert(row.name.clone(), PrevSample {
                                    cpu_usec: s.cpu_usage_usec,
                                    taken_at: now,
                                });
                            }
                        }
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        // Shutdown errors from fetch_once are not
                        // user-visible diagnostics — we're shutting down.
                        if !is_shutdown_err(&msg) {
                            stderr_log.push(format!("container stats: {}", msg));
                            footer_msg = Some(if msg.contains("parse error") {
                                "parse error (see log after exit)".to_string()
                            } else {
                                "stats unavailable".to_string()
                            });
                        }
                    }
                }

                // Send the frame. If the receiver is closed (main loop
                // exited), break so we don't loop forever.
                if frame_tx.send(Frame {
                    rows: rows.clone(),
                    widths: widths.clone(),
                    footer_msg,
                }).await.is_err() {
                    return stderr_log;
                }
            }
        }
    }
}

fn render_frame(
    rows: &[Row],
    current_name: Option<&str>,
    home: &std::path::Path,
    widths: &ColumnWidths,
    footer_msg: Option<&str>,
) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut stdout = io::stdout();
    // ESC[H = cursor home; ESC[J = clear to end of screen.
    let _ = write!(stdout, "\x1b[H\x1b[J");
    let use_color = std::env::var_os("NO_COLOR").is_none();
    let table = format_table(rows, current_name, use_color, home, now_unix, Some(widths));
    let _ = stdout.write_all(table.as_bytes());
    let _ = writeln!(stdout, "\n{}", footer_msg.unwrap_or(""));
    let _ = writeln!(stdout, "press q or Ctrl+C to exit");
    let _ = stdout.flush();
}
```

- [ ] **Step 11.4: Run the stub test**

Run: `cargo test test_run_live_with_stub`
Expected: PASS (may take ~100ms due to the shutdown timer).

- [ ] **Step 11.5: Add the entry-point wrapper**

Append to `src/status/live.rs`:

```rust
/// Top-level entry point: installs terminal guard, runs the loop,
/// then (after guard drop) flushes buffered diagnostics to stderr
/// and prints the last rendered frame to normal stdout so the last
/// state stays in scrollback.
pub fn run(verbose: bool) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;

    let (tx, _rx) = watch::channel(false);

    // Spawn a signal listener on the runtime.
    let sig_tx = tx.clone();
    rt.spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => return,
        };
        tokio::select! {
            _ = sigint.recv() => { let _ = sig_tx.send(true); }
            _ = sigterm.recv() => { let _ = sig_tx.send(true); }
        }
    });

    let source = Box::new(ContainerSource {
        verbose,
        shutdown: tx.subscribe(),
    });

    let result: LiveResult;
    {
        // The guard must be scoped tightly so Drop runs *before* we
        // write anything to the real stdout/stderr — otherwise the
        // final snapshot and diagnostics would land on the alt screen.
        let _guard = TerminalGuard::new_if_tty();
        result = rt.block_on(run_live_loop(source, tx, LiveOptions::default()))?;
    }

    // Terminal is restored. Now print buffered diagnostics to stderr.
    for line in &result.stderr_log {
        eprintln!("[agentbox status live] {}", line);
    }

    // And the final snapshot to normal stdout, using the last rows we
    // rendered live. This avoids re-running the 2s blocking stats call
    // that the one-shot path does — the data we have is already fresher.
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let use_color = std::io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none();
    let table = format_table(
        &result.rows,
        result.current_name.as_deref(),
        use_color,
        &result.home,
        now_unix,
        None,
    );
    print!("{}", table);
    std::io::stdout().flush().ok();

    Ok(())
}
```

Note: `crate::status::run` (one-shot mode) is the existing entry point. Its tree of calls matches what we want for the final snapshot.

- [ ] **Step 11.6: Run all tests**

Run: `cargo test`
Expected: everything passes. The `run` function isn't unit-tested (it's terminal-coupled); manual test comes later.

---

## Task 12: Wire CLI — `--no-stream` flag, drop `ls` alias, dispatch

Hook the new live mode into the CLI. Add `--no-stream` flag. Remove the `ls` alias. Dispatch to live mode when stdout is a TTY and `--no-stream` is not set.

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 12.1: Write failing tests**

In the existing `#[cfg(test)] mod tests` block in `src/main.rs` (around line 559), add:

```rust
#[test]
fn test_status_no_stream_flag_parses() {
    let cli = Cli::parse_from(&["agentbox", "status", "--no-stream"]);
    match cli.command {
        Some(Commands::Status { no_stream }) => assert!(no_stream),
        _ => panic!("expected Status"),
    }
}

#[test]
fn test_status_default_has_no_stream_false() {
    let cli = Cli::parse_from(&["agentbox", "status"]);
    match cli.command {
        Some(Commands::Status { no_stream }) => assert!(!no_stream),
        _ => panic!("expected Status"),
    }
}

#[test]
fn test_ls_alias_is_removed() {
    let res = Cli::try_parse_from(&["agentbox", "ls"]);
    assert!(res.is_err(), "`ls` alias should no longer parse");
}
```

Also update any existing `test_status_alias_ls` / `test_status_subcommand` tests if present. Find them by searching `grep -n 'test_status' src/main.rs`. Delete `test_status_alias_ls` (or similar) entirely. Update `test_status_subcommand` to match the new shape:

```rust
#[test]
fn test_status_subcommand() {
    let cli = Cli::parse_from(&["agentbox", "status"]);
    assert!(matches!(cli.command, Some(Commands::Status { no_stream: false })));
}
```

- [ ] **Step 12.2: Run tests to verify they fail**

Run: `cargo test test_status -- --nocapture`
Expected: FAIL — `Status` variant doesn't have `no_stream` field, `ls` alias still works.

- [ ] **Step 12.3: Update the `Status` variant in `Commands` enum**

In `src/main.rs`, find the existing `Status` variant (around line 50-52):

```rust
/// Show rich container status (CPU, memory, project, sessions)
#[command(alias = "ls")]
Status,
```

Replace with:

```rust
/// Show rich container status (CPU, memory, project, sessions). On a
/// TTY, refreshes every 2s until `q` or Ctrl+C.
Status {
    /// Skip live mode even on a TTY — run a single snapshot and exit.
    #[arg(long)]
    no_stream: bool,
},
```

- [ ] **Step 12.4: Update the dispatch**

Find the dispatch arm (around line 430):

```rust
Some(Commands::Status) => {
    status::run(cli.verbose)?;
    Ok(())
}
```

Replace with:

```rust
Some(Commands::Status { no_stream }) => {
    use std::io::IsTerminal;
    let is_tty = std::io::stdout().is_terminal();
    if is_tty && !no_stream {
        status::live::run(cli.verbose)?;
    } else {
        status::run(cli.verbose)?;
    }
    Ok(())
}
```

- [ ] **Step 12.5: Run tests**

Run: `cargo test`
Expected: all pass, including the three new `test_status_*` tests.

---

## Task 13: Update README

Remove the `ls` reference and document live mode.

**Files:**
- Modify: `README.md`

- [ ] **Step 13.1: Find the section to update**

Run: `grep -n "agentbox status\|agentbox ls" README.md`
Note the line numbers.

- [ ] **Step 13.2: Replace any `agentbox ls` reference**

Replace the existing status/ls block in `README.md` (around line 47 per the design doc's reference) with:

```markdown
## Status

`agentbox status` shows a rich table of your agentbox containers — CPU, memory,
project path, uptime, and number of attached sessions.

On a TTY the status refreshes every two seconds (like `top`); exit with `q` or
Ctrl+C. Use `--no-stream` to get a single snapshot instead. When stdout is
piped to another command (e.g. `agentbox status | less`) the live loop is
skipped automatically.
```

If the README already has a different structure around the status section, keep the structure — just ensure the `ls` example is gone and the live-mode behavior is described.

- [ ] **Step 13.3: Verify no remaining `agentbox ls` references**

Run: `grep -n "agentbox ls" README.md`
Expected: no matches.

- [ ] **Step 13.4: Final test run**

Run: `cargo test`
Expected: all pass. Total should be 201 (existing) + new tests added in this plan = **approximately 220+ passing tests**, 0 failures.

- [ ] **Step 13.5: Manual smoke test (requires macOS + `container` CLI)**

These are manual — document results in the PR description rather than automating:

1. **Live mode renders**: Run `cargo run -- status` on macOS. Expect alt screen, table updating every ~2s, `q` exits cleanly, terminal restored.
2. **Ctrl+C mid-fetch kills child**: Run `cargo run -- status`, press Ctrl+C (or `q`) during a tick. Expect prompt returns without a ~2s delay, no orphan `container stats` process (`ps aux | grep "container stats"`).
3. **Piped mode stays fast-only**: Run `cargo run -- status | cat`. Expect fast output (<200ms), no ~2s stats call.
4. **`--no-stream` on TTY**: Run `cargo run -- status --no-stream`. Expect single snapshot including CPU/MEM row, then exit.
5. **`ls` alias gone**: Run `cargo run -- ls`. Expect clap error `unrecognized subcommand`.
6. **Final one-shot on exit**: Press `q` in live mode. Expect the last snapshot to remain in scrollback below the shell prompt.
7. **Panic restores terminal**: Inject a temporary `panic!()` in `run_live_loop`, run live mode. Expect the panic message to appear on a normal (non-alt-screen) terminal, cursor visible, echo working. Remove the panic injection after.
