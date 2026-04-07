# `agentbox status` Command Implementation Plan

> **For agentic workers:** REQUIRED: Use workflow:subagent-driven-development to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `agentbox ls` with a richer `agentbox status` command that shows NAME, STATUS, PROJECT, CPU, MEM, UPTIME, SESSIONS in one table — fast pass first, then a 2-second progressive in-place update for live CPU/MEM.

**Architecture:** New `src/status.rs` module owns the table renderer, pure parsers, and progressive-update orchestration. The fast pass uses `container ls --all --format json` + `ps -eo pid,args` + `Path::exists` (~50ms total). The live pass calls `container stats --no-stream` (text mode), parses the table, and redraws the on-screen table in place via ANSI cursor sequences. Column widths are stable across passes — only CPU/MEM cell *contents* change. Pure parsers and formatters get exhaustive table-driven tests; I/O orchestration is thin.

**Tech Stack:** Rust 1.94, `clap` (subcommands + alias), `serde_json` (parse `container ls` JSON), `anyhow`, `dirs`, `std::io::IsTerminal` (TTY detection), `std::process::Command` (subprocess), ANSI escape codes (no crate).

**Spec:** [`wiki/2026-04-07-status-command-design.md`](2026-04-07-status-command-design.md)

---

## File map

| Path | Action | Responsibility |
|---|---|---|
| `src/status.rs` | **Create** | All status-command code: types, parsers, formatters, ANSI helpers, entry point, tests |
| `src/main.rs` | Modify | Replace `Ls` variant with `Status` (alias `ls`), dispatch to `status::run`, declare `mod status;`, update existing `test_ls_subcommand`, add `test_status_alias_ls` |
| `src/container.rs` | Modify | Add `count_sessions` next to `has_other_sessions`, extract shared `matches_session` helper, delete `pub fn list` (no longer used) |
| `README.md` | Modify | Replace `agentbox ls` mention in Quick Start with `agentbox status` |

---

## Task 1: Module scaffolding — types and stub `run`

**Files:**
- Create: `src/status.rs`
- Modify: `src/main.rs:1-10` (add `mod status;`)

This task is plumbing only — gets the module compiling so subsequent tasks can add tests against real types. No business logic, no tests yet.

- [ ] **Step 1: Create `src/status.rs` with types and a stub entry point**

```rust
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
```

- [ ] **Step 2: Declare the module in `main.rs`**

In `src/main.rs`, after the existing `mod image;` line (around line 9), add:

```rust
mod status;
```

The `mod` declarations should now read:

```rust
mod bridge;
mod config;
mod container;
mod git;
mod hostexec;
mod image;
mod status;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build 2>&1 | tail -20`
Expected: Compiles cleanly (warnings about unused code are fine).

---

## Task 2: `parse_ls_json` — pure parser for `container ls --all --format json`

**Files:**
- Modify: `src/status.rs` (add function + tests)

This parser turns the `container ls` JSON output into `Vec<Row>`. It filters to `agentbox-*` names, extracts `workingDirectory` and `startedDate`, and converts Mac Absolute Time → Unix epoch. It does *not* fill in `sessions`, `cpu_pct`, `mem_used`, `mem_total`, or compute `Stale` — those happen in later tasks.

- [ ] **Step 1: Add the failing test for the basic parse case**

In `src/status.rs`, add at the bottom (above any existing `#[cfg(test)]`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// One running agentbox container, minimal fields.
    const LS_JSON_ONE_RUNNING: &str = r#"[{
        "status": "running",
        "startedDate": 797208589.076146,
        "configuration": {
            "id": "agentbox-myapp-abc123",
            "initProcess": {
                "workingDirectory": "/Users/alex/Dev/myapp"
            }
        }
    }]"#;

    #[test]
    fn test_parse_ls_json_one_running() {
        let rows = parse_ls_json(LS_JSON_ONE_RUNNING);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.name, "agentbox-myapp-abc123");
        assert_eq!(row.state, State::Running);
        assert_eq!(row.workdir, "/Users/alex/Dev/myapp");
        // 797208589 + 978307200 = 1775515789
        assert_eq!(row.started_unix, Some(1_775_515_789));
        // Live fields default None — populated later
        assert!(row.sessions.is_none());
        assert!(row.cpu_pct.is_none());
        assert!(row.mem_used.is_none());
        assert!(row.mem_total.is_none());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib status::tests::test_parse_ls_json_one_running 2>&1 | tail -10`
Expected: FAIL — `parse_ls_json` is not defined.

- [ ] **Step 3: Implement `parse_ls_json`**

Add this function in `src/status.rs` (above the `#[cfg(test)]` block):

```rust
/// Apple epoch (2001-01-01 UTC) → Unix epoch (1970-01-01 UTC) offset, in seconds.
const APPLE_EPOCH_OFFSET: i64 = 978_307_200;

/// Parse `container ls --all --format json` output into rows. Filters to
/// containers whose id starts with `agentbox-`. Live fields (sessions,
/// cpu_pct, mem_*) are left as None — they get populated by later passes.
/// Stale detection is *not* done here; the caller adds it.
///
/// Returns an empty vec on parse failure (matches the existing
/// `parse_container_list` behavior in `container.rs`).
pub fn parse_ls_json(json: &str) -> Vec<Row> {
    let containers: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
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
    rows
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib status::tests::test_parse_ls_json_one_running 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Add tests covering filtering, mixed states, and edge cases**

Append inside the `mod tests` block:

```rust
#[test]
fn test_parse_ls_json_filters_non_agentbox() {
    let json = r#"[
        {"status":"running","configuration":{"id":"buildkit","initProcess":{"workingDirectory":"/"}}},
        {"status":"running","configuration":{"id":"agentbox-x-aaaaaa","initProcess":{"workingDirectory":"/tmp/x"}}}
    ]"#;
    let rows = parse_ls_json(json);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "agentbox-x-aaaaaa");
}

#[test]
fn test_parse_ls_json_stopped_state() {
    let json = r#"[{
        "status":"stopped",
        "configuration":{"id":"agentbox-x-aaaaaa","initProcess":{"workingDirectory":"/tmp/x"}}
    }]"#;
    let rows = parse_ls_json(json);
    assert_eq!(rows[0].state, State::Stopped);
}

#[test]
fn test_parse_ls_json_missing_started_date() {
    let json = r#"[{
        "status":"stopped",
        "configuration":{"id":"agentbox-x-aaaaaa","initProcess":{"workingDirectory":"/tmp/x"}}
    }]"#;
    let rows = parse_ls_json(json);
    assert_eq!(rows[0].started_unix, None);
}

#[test]
fn test_parse_ls_json_missing_workdir() {
    let json = r#"[{
        "status":"running",
        "configuration":{"id":"agentbox-x-aaaaaa","initProcess":{}}
    }]"#;
    let rows = parse_ls_json(json);
    assert_eq!(rows[0].workdir, "");
}

#[test]
fn test_parse_ls_json_invalid_json_returns_empty() {
    assert!(parse_ls_json("not json").is_empty());
    assert!(parse_ls_json("").is_empty());
}

#[test]
fn test_parse_ls_json_sorted_by_name() {
    let json = r#"[
        {"status":"running","configuration":{"id":"agentbox-zz-aaaaaa","initProcess":{"workingDirectory":"/z"}}},
        {"status":"running","configuration":{"id":"agentbox-aa-aaaaaa","initProcess":{"workingDirectory":"/a"}}}
    ]"#;
    let rows = parse_ls_json(json);
    assert_eq!(rows[0].name, "agentbox-aa-aaaaaa");
    assert_eq!(rows[1].name, "agentbox-zz-aaaaaa");
}

#[test]
fn test_parse_ls_json_multiple_mixed() {
    let json = r#"[
        {"status":"running","startedDate":797208589.0,"configuration":{"id":"agentbox-a-111111","initProcess":{"workingDirectory":"/a"}}},
        {"status":"stopped","startedDate":797000000.0,"configuration":{"id":"agentbox-b-222222","initProcess":{"workingDirectory":"/b"}}},
        {"status":"running","configuration":{"id":"buildkit","initProcess":{"workingDirectory":"/"}}}
    ]"#;
    let rows = parse_ls_json(json);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].state, State::Running);
    assert_eq!(rows[1].state, State::Stopped);
}
```

- [ ] **Step 6: Run all `parse_ls_json` tests**

Run: `cargo test --lib status::tests::test_parse_ls_json 2>&1 | tail -20`
Expected: 7 tests pass.

---

## Task 3: `parse_stats_text` — pure parser for `container stats --no-stream` text output

**Files:**
- Modify: `src/status.rs` (add function + tests)

Parses Apple Container's column-aligned stats text into a map keyed by container name. Memory units are converted to bytes for arithmetic and consistent rendering downstream.

- [ ] **Step 1: Add the failing test using real sample output**

Append inside the `mod tests` block:

```rust
const STATS_TEXT_SAMPLE: &str = "\
Container ID                 Cpu %  Memory Usage           Net Rx/Tx                Block I/O                Pids
agentbox-agentbox-71e6bc     7.19%  2.18 GiB / 8.00 GiB    28.20 MiB / 19.53 MiB    819.63 MiB / 146.74 MiB  83
agentbox-marketplace-ad3430  2.52%  771.08 MiB / 8.00 GiB  311.84 KiB / 351.12 KiB  394.19 MiB / 168.00 KiB  40
buildkit                     0.01%  1.50 GiB / 2.00 GiB    2.11 GiB / 7.49 MiB      14.32 GiB / 19.17 GiB    21";

#[test]
fn test_parse_stats_text_three_rows() {
    let map = parse_stats_text(STATS_TEXT_SAMPLE);
    assert_eq!(map.len(), 3);
    let (cpu, used, total) = map["agentbox-agentbox-71e6bc"];
    assert!((cpu - 7.19).abs() < 0.001);
    // 2.18 GiB = 2.18 * 1024^3 bytes
    assert_eq!(used, (2.18 * 1024.0 * 1024.0 * 1024.0) as u64);
    // 8.00 GiB = 8 * 1024^3
    assert_eq!(total, 8 * 1024 * 1024 * 1024);

    let (cpu2, used2, _total2) = map["agentbox-marketplace-ad3430"];
    assert!((cpu2 - 2.52).abs() < 0.001);
    // 771.08 MiB
    assert_eq!(used2, (771.08 * 1024.0 * 1024.0) as u64);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib status::tests::test_parse_stats_text_three_rows 2>&1 | tail -10`
Expected: FAIL — `parse_stats_text` not defined.

- [ ] **Step 3: Implement `parse_stats_text` and `parse_mem_value`**

Add these functions in `src/status.rs` (above the test module):

```rust
/// Parse a memory value like "2.18" with unit "GiB" into bytes.
/// Returns None on unrecognized unit.
fn parse_mem_value(value: &str, unit: &str) -> Option<u64> {
    let v: f64 = value.parse().ok()?;
    let mult: f64 = match unit {
        "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((v * mult) as u64)
}

/// Parse `container stats --no-stream` text output. Returns a map from
/// container name to (cpu_pct, mem_used_bytes, mem_total_bytes).
///
/// The expected row layout (18 whitespace-separated tokens) is:
///
/// ```
/// [0]      name
/// [1]      cpu_pct           e.g. "7.19%"
/// [2..7]   mem               5 tokens: "2.18", "GiB", "/", "8.00", "GiB"
/// [7..12]  net_rx_tx         5 tokens (ignored)
/// [12..17] block_io          5 tokens (ignored)
/// [17]     pids              (ignored)
/// ```
///
/// Defensive: any row with fewer than 18 tokens is skipped. The header
/// row (first token "Container") is also skipped. Caller is responsible
/// for filtering to agentbox-* names.
pub fn parse_stats_text(text: &str) -> HashMap<String, (f64, u64, u64)> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 18 {
            continue;
        }
        // Skip header row
        if tokens[0] == "Container" {
            continue;
        }
        let name = tokens[0].to_string();
        let cpu_str = tokens[1].trim_end_matches('%');
        let cpu_pct: f64 = match cpu_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mem_used = match parse_mem_value(tokens[2], tokens[3]) {
            Some(v) => v,
            None => continue,
        };
        // tokens[4] is "/"
        let mem_total = match parse_mem_value(tokens[5], tokens[6]) {
            Some(v) => v,
            None => continue,
        };
        out.insert(name, (cpu_pct, mem_used, mem_total));
    }
    out
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --lib status::tests::test_parse_stats_text_three_rows 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Add edge-case tests**

Append inside `mod tests`:

```rust
#[test]
fn test_parse_stats_text_skips_header() {
    let text = "Container ID  Cpu %  Memory Usage  Net Rx/Tx  Block I/O  Pids";
    let map = parse_stats_text(text);
    assert!(map.is_empty());
}

#[test]
fn test_parse_stats_text_empty_input() {
    assert!(parse_stats_text("").is_empty());
}

#[test]
fn test_parse_stats_text_skips_malformed_row() {
    let text = "agentbox-foo-aaaaaa  7.19%  not-enough-tokens";
    assert!(parse_stats_text(text).is_empty());
}

#[test]
fn test_parse_stats_text_kib_unit() {
    let text = "agentbox-x-aaaaaa  0.01%  512.00 KiB / 1.00 MiB  0.00 B / 0.00 B  0.00 B / 0.00 B  1";
    let map = parse_stats_text(text);
    let (_, used, total) = map["agentbox-x-aaaaaa"];
    assert_eq!(used, 512 * 1024);
    assert_eq!(total, 1024 * 1024);
}

#[test]
fn test_parse_mem_value_units() {
    assert_eq!(parse_mem_value("1", "B"), Some(1));
    assert_eq!(parse_mem_value("1", "KiB"), Some(1024));
    assert_eq!(parse_mem_value("1", "MiB"), Some(1024 * 1024));
    assert_eq!(parse_mem_value("1", "GiB"), Some(1024 * 1024 * 1024));
    assert_eq!(parse_mem_value("0.5", "GiB"), Some(512 * 1024 * 1024));
    assert_eq!(parse_mem_value("1", "PiB"), None);
    assert_eq!(parse_mem_value("notanumber", "GiB"), None);
}
```

- [ ] **Step 6: Run all stats parser tests**

Run: `cargo test --lib status::tests::test_parse_stats 2>&1 | tail -20` and `cargo test --lib status::tests::test_parse_mem 2>&1 | tail -10`
Expected: 6 tests pass.

---

## Task 4: `format_uptime` — duration formatter

**Files:**
- Modify: `src/status.rs`

Pure formatter that turns a number of elapsed seconds into a compact string like `2h 15m`, `45m`, `3d 4h`, or `0m` for zero/negative.

- [ ] **Step 1: Add the failing test**

Append inside `mod tests`:

```rust
#[test]
fn test_format_uptime_seconds() {
    assert_eq!(format_uptime(0), "0m");
    assert_eq!(format_uptime(45), "0m");
    assert_eq!(format_uptime(60), "1m");
    assert_eq!(format_uptime(59), "0m");
}

#[test]
fn test_format_uptime_minutes() {
    assert_eq!(format_uptime(60 * 45), "45m");
    assert_eq!(format_uptime(60 * 59), "59m");
}

#[test]
fn test_format_uptime_hours() {
    assert_eq!(format_uptime(60 * 60), "1h 0m");
    assert_eq!(format_uptime(60 * 60 * 2 + 60 * 15), "2h 15m");
    assert_eq!(format_uptime(60 * 60 * 23 + 60 * 59), "23h 59m");
}

#[test]
fn test_format_uptime_days() {
    assert_eq!(format_uptime(60 * 60 * 24), "1d 0h");
    assert_eq!(format_uptime(60 * 60 * 24 * 3 + 60 * 60 * 4), "3d 4h");
}

#[test]
fn test_format_uptime_negative_clock_skew() {
    assert_eq!(format_uptime(-5), "0m");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib status::tests::test_format_uptime 2>&1 | tail -10`
Expected: FAIL — `format_uptime` not defined.

- [ ] **Step 3: Implement `format_uptime`**

Add in `src/status.rs`:

```rust
/// Format a number of elapsed seconds as a compact uptime string.
/// Returns "0m" for zero, sub-minute, or negative durations (clock skew).
pub fn format_uptime(elapsed_secs: i64) -> String {
    if elapsed_secs <= 0 {
        return "0m".to_string();
    }
    let secs = elapsed_secs as u64;
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    if days > 0 {
        format!("{}d {}h", days, hours)
    } else if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else {
        format!("{}m", mins)
    }
}
```

- [ ] **Step 4: Run to verify all uptime tests pass**

Run: `cargo test --lib status::tests::test_format_uptime 2>&1 | tail -15`
Expected: 5 tests pass.

---

## Task 5: `format_mem` — byte formatter

**Files:**
- Modify: `src/status.rs`

Formats a byte count into a compact string like `2.2G`, `812M`, `512K`. Used for both individual cells (`2.2/8.0G`) and the totals row.

- [ ] **Step 1: Add the failing test**

Append inside `mod tests`:

```rust
#[test]
fn test_format_mem_gib() {
    let two_gib = 2 * 1024u64 * 1024 * 1024;
    assert_eq!(format_mem(two_gib), "2.0G");
    let two_point_two_gib = ((2.2 * 1024.0 * 1024.0 * 1024.0) as u64).max(1);
    assert_eq!(format_mem(two_point_two_gib), "2.2G");
}

#[test]
fn test_format_mem_mib() {
    let half_gib_in_mib = 512 * 1024 * 1024;
    assert_eq!(format_mem(half_gib_in_mib), "512M");
    let mib_812 = 812 * 1024 * 1024;
    assert_eq!(format_mem(mib_812), "812M");
}

#[test]
fn test_format_mem_kib() {
    let kib_512 = 512 * 1024;
    assert_eq!(format_mem(kib_512), "512K");
}

#[test]
fn test_format_mem_bytes() {
    assert_eq!(format_mem(0), "0B");
    assert_eq!(format_mem(512), "512B");
    assert_eq!(format_mem(1023), "1023B");
}

#[test]
fn test_format_mem_boundary() {
    // Exactly 1 KiB = 1024 bytes → "1K"
    assert_eq!(format_mem(1024), "1K");
    // Exactly 1 MiB → "1M"
    assert_eq!(format_mem(1024 * 1024), "1M");
    // Exactly 1 GiB → "1.0G"
    assert_eq!(format_mem(1024 * 1024 * 1024), "1.0G");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib status::tests::test_format_mem 2>&1 | tail -10`
Expected: FAIL — `format_mem` not defined.

- [ ] **Step 3: Implement `format_mem`**

Add in `src/status.rs`:

```rust
/// Format a byte count as a compact string. GiB use one decimal place;
/// smaller units are integers. Used in cell rendering and totals.
pub fn format_mem(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes >= GIB {
        format!("{:.1}G", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{}M", bytes / MIB)
    } else if bytes >= KIB {
        format!("{}K", bytes / KIB)
    } else {
        format!("{}B", bytes)
    }
}
```

- [ ] **Step 4: Run to verify all mem tests pass**

Run: `cargo test --lib status::tests::test_format_mem 2>&1 | tail -20`
Expected: 5 tests pass.

---

## Task 6: `shorten_path` — home-relative + ellipsize

**Files:**
- Modify: `src/status.rs`

Replaces `$HOME` prefix with `~` and ellipsizes overly long paths from the right (preserves the leading `~/` and the start of the path so the most identifying part stays visible).

- [ ] **Step 1: Add the failing test**

Append inside `mod tests`:

```rust
#[test]
fn test_shorten_path_home_prefix() {
    let home = Path::new("/Users/alex");
    assert_eq!(shorten_path("/Users/alex/Dev/myapp", home, 40), "~/Dev/myapp");
}

#[test]
fn test_shorten_path_home_exact() {
    let home = Path::new("/Users/alex");
    assert_eq!(shorten_path("/Users/alex", home, 40), "~");
}

#[test]
fn test_shorten_path_no_home_prefix() {
    let home = Path::new("/Users/alex");
    assert_eq!(shorten_path("/opt/foo", home, 40), "/opt/foo");
}

#[test]
fn test_shorten_path_with_spaces() {
    let home = Path::new("/Users/alex");
    assert_eq!(
        shorten_path("/Users/alex/Library/Mobile Documents/x", home, 40),
        "~/Library/Mobile Documents/x"
    );
}

#[test]
fn test_shorten_path_ellipsize_when_too_long() {
    let home = Path::new("/Users/alex");
    // ~/Dev/Personal/marketplace = 26 chars; max 20 → ellipsized
    let result = shorten_path("/Users/alex/Dev/Personal/marketplace", home, 20);
    assert_eq!(result.chars().count(), 20);
    assert!(result.ends_with("…"));
    // Should preserve the leading ~/
    assert!(result.starts_with("~/"));
}

#[test]
fn test_shorten_path_short_max_no_panic() {
    let home = Path::new("/Users/alex");
    // max=3 should not panic, even if result can't fit anything meaningful
    let result = shorten_path("/Users/alex/very/long/path", home, 3);
    assert!(result.chars().count() <= 3);
}

#[test]
fn test_shorten_path_exact_fit() {
    let home = Path::new("/Users/alex");
    // ~/Dev/myapp is 11 chars; max 11 → not ellipsized
    assert_eq!(shorten_path("/Users/alex/Dev/myapp", home, 11), "~/Dev/myapp");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib status::tests::test_shorten_path 2>&1 | tail -10`
Expected: FAIL — `shorten_path` not defined.

- [ ] **Step 3: Implement `shorten_path`**

Add in `src/status.rs`:

```rust
/// Shorten a filesystem path for display:
/// 1. If `path` starts with `home`, replace prefix with `~`.
/// 2. If the result exceeds `max_width`, truncate from the right and
///    append `…`. Operates on chars, not bytes — safe for UTF-8.
pub fn shorten_path(path: &str, home: &Path, max_width: usize) -> String {
    let home_str = home.to_string_lossy();
    let home_str = home_str.trim_end_matches('/');
    let with_tilde: String = if path == home_str {
        "~".to_string()
    } else if let Some(suffix) = path.strip_prefix(&format!("{}/", home_str)) {
        format!("~/{}", suffix)
    } else {
        path.to_string()
    };
    let char_count = with_tilde.chars().count();
    if char_count <= max_width {
        return with_tilde;
    }
    if max_width == 0 {
        return String::new();
    }
    // Reserve 1 char for the ellipsis.
    let keep = max_width.saturating_sub(1);
    let truncated: String = with_tilde.chars().take(keep).collect();
    format!("{}…", truncated)
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --lib status::tests::test_shorten_path 2>&1 | tail -20`
Expected: 7 tests pass.

---

## Task 7: `count_sessions` and `matches_session` in `container.rs`

**Files:**
- Modify: `src/container.rs:120-137` (refactor `has_other_sessions` to use a shared helper, add `count_sessions`)

Adds the count helper next to the existing boolean helper. Both delegate to a private `matches_session` so the row-matching logic exists in one place.

- [ ] **Step 1: Add the failing test for `count_sessions`**

In `src/container.rs`, inside the existing `mod tests` block (around line 365), add:

```rust
#[test]
fn test_count_sessions_zero() {
    let ps_output = "  PID ARGS\n  100 vim main.rs\n";
    assert_eq!(count_sessions(ps_output, "agentbox-myapp-abc123"), 0);
}

#[test]
fn test_count_sessions_one() {
    let ps_output = "  PID ARGS\n  100 container exec --tty agentbox-myapp-abc123 bash\n";
    assert_eq!(count_sessions(ps_output, "agentbox-myapp-abc123"), 1);
}

#[test]
fn test_count_sessions_multiple() {
    let ps_output = "  PID ARGS\n  100 container exec --tty agentbox-myapp-abc123 bash\n  200 container exec agentbox-myapp-abc123 bash -lc claude\n  300 container run --name agentbox-myapp-abc123 --cpus 4 agentbox:default\n";
    assert_eq!(count_sessions(ps_output, "agentbox-myapp-abc123"), 3);
}

#[test]
fn test_count_sessions_ignores_runtime_process() {
    let ps_output = "  PID ARGS\n  100 /usr/local/libexec/container/plugins/container-runtime-linux/bin/container-runtime-linux start --root /Users/alex/Library/Application Support/com.apple.container/containers/agentbox-myapp-abc123 --uuid agentbox-myapp-abc123\n";
    assert_eq!(count_sessions(ps_output, "agentbox-myapp-abc123"), 0);
}

#[test]
fn test_count_sessions_different_container() {
    let ps_output = "  PID ARGS\n  100 container exec --tty agentbox-other-def456 bash\n";
    assert_eq!(count_sessions(ps_output, "agentbox-myapp-abc123"), 0);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib container::tests::test_count_sessions 2>&1 | tail -15`
Expected: FAIL — `count_sessions` not defined.

- [ ] **Step 3: Refactor — extract `matches_session` and add `count_sessions`**

In `src/container.rs`, replace the existing `has_other_sessions` (around lines 120-137) with this block:

```rust
/// Returns true if a `ps -eo pid,args` row represents a `container exec`
/// or `container run` invocation that references the given container name.
/// The always-on `container-runtime-linux` process does not match because
/// it does not contain `container exec` or `container run`.
fn matches_session(line: &str, container_name: &str) -> Option<u32> {
    let trimmed = line.trim();
    let (pid_str, args) = trimmed.split_once(char::is_whitespace)?;
    let pid: u32 = pid_str.trim().parse().ok()?;
    let is_session = (args.contains("container exec") || args.contains("container run"))
        && args.contains(container_name);
    if is_session {
        Some(pid)
    } else {
        None
    }
}

/// Check if other processes are using the same container.
/// Parses `ps -eo pid,args` output, looking for `container exec` or
/// `container run` rows that reference the given container name,
/// excluding our own PID.
pub fn has_other_sessions(ps_output: &str, container_name: &str, our_pid: u32) -> bool {
    ps_output
        .lines()
        .filter_map(|line| matches_session(line, container_name))
        .any(|pid| pid != our_pid)
}

/// Count attached sessions for a container by parsing `ps -eo pid,args`.
/// Counts every `container exec` / `container run` row that references the
/// container name. Used by `agentbox status` to populate the SESSIONS column.
pub fn count_sessions(ps_output: &str, container_name: &str) -> usize {
    ps_output
        .lines()
        .filter_map(|line| matches_session(line, container_name))
        .count()
}
```

- [ ] **Step 4: Run all container tests to verify nothing regressed**

Run: `cargo test --lib container::tests 2>&1 | tail -20`
Expected: All existing `has_other_sessions` tests still pass, plus the 5 new `count_sessions` tests.

---

## Task 8: `format_table` — column-aligned table renderer

**Files:**
- Modify: `src/status.rs`

The biggest pure formatter. Computes column widths, builds header + rows + totals, applies bolding to the current project's row when `use_color=true`. No I/O, fully testable.

- [ ] **Step 1: Add the failing test**

Append inside `mod tests`:

```rust
fn sample_rows() -> Vec<Row> {
    vec![
        Row {
            name: "agentbox-aaa-111111".to_string(),
            state: State::Running,
            workdir: "/Users/alex/Dev/aaa".to_string(),
            started_unix: Some(1_775_515_789),
            sessions: Some(1),
            cpu_pct: Some(7.19),
            mem_used: Some(2_340_000_000),
            mem_total: Some(8_589_934_592),
        },
        Row {
            name: "agentbox-bbb-222222".to_string(),
            state: State::Stopped,
            workdir: "/Users/alex/Dev/bbb".to_string(),
            started_unix: Some(1_775_000_000),
            sessions: Some(0),
            cpu_pct: None,
            mem_used: None,
            mem_total: None,
        },
    ]
}

#[test]
fn test_format_table_has_header_and_rows_and_totals() {
    let rows = sample_rows();
    let home = Path::new("/Users/alex");
    let now = 1_775_515_789 + 60 * 60 * 2 + 60 * 15; // 2h 15m after start
    let table = format_table(&rows, None, false, home, now);
    assert!(table.contains("NAME"));
    assert!(table.contains("STATUS"));
    assert!(table.contains("PROJECT"));
    assert!(table.contains("CPU"));
    assert!(table.contains("MEM"));
    assert!(table.contains("UPTIME"));
    assert!(table.contains("SESSIONS"));
    assert!(table.contains("agentbox-aaa-111111"));
    assert!(table.contains("agentbox-bbb-222222"));
    assert!(table.contains("TOTALS"));
}

#[test]
fn test_format_table_running_shows_live_data() {
    let rows = sample_rows();
    let home = Path::new("/Users/alex");
    let table = format_table(&rows, None, false, home, 1_775_515_789 + 60 * 75);
    assert!(table.contains("7.2%"));
    assert!(table.contains("2h 15m") || table.contains("1h 15m"));
}

#[test]
fn test_format_table_stopped_shows_dashes() {
    let rows = sample_rows();
    let home = Path::new("/Users/alex");
    let table = format_table(&rows, None, false, home, 0);
    // Find the bbb row line
    let bbb_line = table.lines().find(|l| l.contains("agentbox-bbb-222222")).unwrap();
    // CPU and MEM and UPTIME cells should be "--"
    assert!(bbb_line.contains("--"));
}

#[test]
fn test_format_table_fast_pass_cpu_mem_dashes() {
    let mut rows = sample_rows();
    // Simulate fast pass: clear live fields
    rows[0].cpu_pct = None;
    rows[0].mem_used = None;
    rows[0].mem_total = None;
    let home = Path::new("/Users/alex");
    let table = format_table(&rows, None, false, home, 1_775_515_789 + 60 * 75);
    let aaa_line = table.lines().find(|l| l.contains("agentbox-aaa-111111")).unwrap();
    // No CPU% number visible for the running row in the fast pass
    assert!(!aaa_line.contains("7.2%"));
    assert!(aaa_line.contains("--"));
}

#[test]
fn test_format_table_bolds_current_row_when_color_enabled() {
    let rows = sample_rows();
    let home = Path::new("/Users/alex");
    let table = format_table(
        &rows,
        Some("agentbox-aaa-111111"),
        true,
        home,
        1_775_515_789 + 60 * 75,
    );
    let aaa_line = table.lines().find(|l| l.contains("agentbox-aaa-111111")).unwrap();
    assert!(aaa_line.contains("\x1b[1m"));
    assert!(aaa_line.contains("\x1b[22m"));
}

#[test]
fn test_format_table_no_bolding_when_color_disabled() {
    let rows = sample_rows();
    let home = Path::new("/Users/alex");
    let table = format_table(
        &rows,
        Some("agentbox-aaa-111111"),
        false,
        home,
        1_775_515_789 + 60 * 75,
    );
    assert!(!table.contains("\x1b[1m"));
    assert!(!table.contains("\x1b[22m"));
}

#[test]
fn test_format_table_no_bolding_when_no_current() {
    let rows = sample_rows();
    let home = Path::new("/Users/alex");
    let table = format_table(&rows, None, true, home, 1_775_515_789 + 60 * 75);
    assert!(!table.contains("\x1b[1m"));
}

#[test]
fn test_format_table_totals_running_count() {
    let rows = sample_rows();
    let home = Path::new("/Users/alex");
    let table = format_table(&rows, None, false, home, 1_775_515_789 + 60 * 75);
    let totals_line = table.lines().find(|l| l.contains("TOTALS")).unwrap();
    // 1 running container in sample
    assert!(totals_line.contains("1 run"));
}

#[test]
fn test_format_table_empty_still_renders_header_and_totals() {
    let home = Path::new("/Users/alex");
    let table = format_table(&[], None, false, home, 0);
    assert!(table.contains("NAME"));
    assert!(table.contains("TOTALS"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib status::tests::test_format_table 2>&1 | tail -10`
Expected: FAIL — `format_table` not defined (and `sample_rows` is unused).

- [ ] **Step 3: Implement `format_table` and the cell helpers**

Add these in `src/status.rs` (above the test module):

```rust
const PROJECT_MAX_WIDTH: usize = 40;
const HEADERS: [&str; 7] = ["NAME", "STATUS", "PROJECT", "CPU", "MEM", "UPTIME", "SESSIONS"];

/// Render a row as 7 cells of strings.
fn row_cells(row: &Row, home: &Path, now_unix: i64) -> [String; 7] {
    let project = shorten_path(&row.workdir, home, PROJECT_MAX_WIDTH);
    let cpu = match row.cpu_pct {
        Some(v) => format!("{:.1}%", v),
        None => "--".to_string(),
    };
    let mem = match (row.mem_used, row.mem_total) {
        (Some(u), Some(t)) => format!("{}/{}", format_mem(u), format_mem(t)),
        _ => "--".to_string(),
    };
    let uptime = match (row.state.clone(), row.started_unix) {
        (State::Running, Some(start)) => format_uptime(now_unix - start),
        _ => "--".to_string(),
    };
    let sessions = match row.sessions {
        Some(n) => n.to_string(),
        None => "-".to_string(),
    };
    [
        row.name.clone(),
        row.state.as_str().to_string(),
        project,
        cpu,
        mem,
        uptime,
        sessions,
    ]
}

/// Render the totals row as 7 cells.
fn totals_cells(rows: &[Row]) -> [String; 7] {
    let running_count = rows.iter().filter(|r| r.state == State::Running).count();
    let cpu_sum: f64 = rows.iter().filter_map(|r| r.cpu_pct).sum();
    let any_cpu = rows.iter().any(|r| r.cpu_pct.is_some());
    let cpu = if any_cpu {
        format!("{:.1}%", cpu_sum)
    } else {
        "--".to_string()
    };
    let used_sum: u64 = rows.iter().filter_map(|r| r.mem_used).sum();
    let total_sum: u64 = rows.iter().filter_map(|r| r.mem_total).sum();
    let mem = if total_sum > 0 {
        format!("{}/{}", format_mem(used_sum), format_mem(total_sum))
    } else {
        "--".to_string()
    };
    let sessions_sum: usize = rows.iter().filter_map(|r| r.sessions).sum();
    [
        "TOTALS".to_string(),
        format!("{} run", running_count),
        "-".to_string(),
        cpu,
        mem,
        "-".to_string(),
        sessions_sum.to_string(),
    ]
}

/// Render the full table as a single newline-terminated string.
/// `current_name` (when Some + `use_color`) bolds the matching row.
/// `now_unix` is supplied so the function stays pure (testable).
pub fn format_table(
    rows: &[Row],
    current_name: Option<&str>,
    use_color: bool,
    home: &Path,
    now_unix: i64,
) -> String {
    // Build all cell strings (header, data rows, totals).
    let mut all_rows: Vec<[String; 7]> = Vec::new();
    all_rows.push(HEADERS.map(String::from));
    for row in rows {
        all_rows.push(row_cells(row, home, now_unix));
    }
    all_rows.push(totals_cells(rows));

    // Compute per-column max widths.
    let mut widths = [0usize; 7];
    for r in &all_rows {
        for (i, cell) in r.iter().enumerate() {
            let w = cell.chars().count();
            if w > widths[i] {
                widths[i] = w;
            }
        }
    }

    // Render rows. Two-space gutter between columns. The last column is
    // left-aligned without trailing pad.
    //
    // Bolding rules: a "data row" is any row that's neither the header (idx
    // 0) nor the totals (idx data_count + 1). When `use_color` is true and
    // the data row's name matches `current_name`, wrap the entire line in
    // ANSI bold (`\x1b[1m` … `\x1b[22m`).
    let data_count = rows.len();
    let mut out = String::new();
    for (idx, r) in all_rows.iter().enumerate() {
        let mut line = String::new();
        for (i, cell) in r.iter().enumerate() {
            if i == r.len() - 1 {
                line.push_str(cell);
            } else {
                line.push_str(&pad_right(cell, widths[i]));
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

fn pad_right(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        let pad = width - len;
        let mut out = String::with_capacity(s.len() + pad);
        out.push_str(s);
        for _ in 0..pad {
            out.push(' ');
        }
        out
    }
}
```

- [ ] **Step 4: Run all `format_table` tests**

Run: `cargo test --lib status::tests::test_format_table 2>&1 | tail -25`
Expected: 9 tests pass.

---

## Task 9: I/O glue — `fetch_basic`, `fetch_live`, ANSI helpers, and `run`

**Files:**
- Modify: `src/status.rs`

This task wires the pure parsers and formatters into the actual command flow. It's the only task with subprocess calls and terminal manipulation. Tests cover the wiring lightly via dependency injection of the merge step.

- [ ] **Step 1: Add the failing test for `merge_stats_into_rows`**

The mergeing of stats results into rows is pure and testable. Append inside `mod tests`:

```rust
#[test]
fn test_merge_stats_into_rows_populates_running() {
    let mut rows = sample_rows();
    let mut stats = HashMap::new();
    stats.insert(
        "agentbox-aaa-111111".to_string(),
        (3.5, 1_000_000_000u64, 4_000_000_000u64),
    );
    merge_stats_into_rows(&mut rows, &stats);
    assert_eq!(rows[0].cpu_pct, Some(3.5));
    assert_eq!(rows[0].mem_used, Some(1_000_000_000));
    assert_eq!(rows[0].mem_total, Some(4_000_000_000));
}

#[test]
fn test_merge_stats_into_rows_skips_stopped() {
    let mut rows = sample_rows();
    let mut stats = HashMap::new();
    stats.insert(
        "agentbox-bbb-222222".to_string(),
        (1.0, 100u64, 200u64),
    );
    merge_stats_into_rows(&mut rows, &stats);
    // bbb is Stopped — should NOT be populated even if present in stats
    assert_eq!(rows[1].cpu_pct, None);
}

#[test]
fn test_merge_stats_into_rows_no_match_leaves_dashes() {
    let mut rows = sample_rows();
    let stats: HashMap<String, (f64, u64, u64)> = HashMap::new();
    merge_stats_into_rows(&mut rows, &stats);
    assert_eq!(rows[0].cpu_pct, None);
}

#[test]
fn test_apply_sessions_to_rows() {
    let mut rows = sample_rows();
    let ps = "  PID ARGS\n  100 container exec agentbox-aaa-111111 bash\n  200 container exec agentbox-aaa-111111 bash\n";
    apply_sessions_to_rows(&mut rows, ps);
    assert_eq!(rows[0].sessions, Some(2));
    assert_eq!(rows[1].sessions, Some(0));
}

#[test]
fn test_apply_stale_to_rows_marks_missing_workdir() {
    let mut rows = vec![Row {
        name: "agentbox-x-aaaaaa".to_string(),
        state: State::Stopped,
        workdir: "/definitely/does/not/exist/here/xyz".to_string(),
        started_unix: None,
        sessions: None,
        cpu_pct: None,
        mem_used: None,
        mem_total: None,
    }];
    apply_stale_to_rows(&mut rows);
    assert_eq!(rows[0].state, State::Stale);
}

#[test]
fn test_apply_stale_to_rows_keeps_running_state_even_if_workdir_missing() {
    // A running container whose workdir was deleted on the host: still
    // Running, just stale. We mark stale only for non-Running states to
    // avoid hiding the fact that something is actively running.
    let mut rows = vec![Row {
        name: "agentbox-x-aaaaaa".to_string(),
        state: State::Running,
        workdir: "/definitely/does/not/exist/here/xyz".to_string(),
        started_unix: None,
        sessions: None,
        cpu_pct: None,
        mem_used: None,
        mem_total: None,
    }];
    apply_stale_to_rows(&mut rows);
    assert_eq!(rows[0].state, State::Running);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib status::tests::test_merge 2>&1 | tail -10` and `cargo test --lib status::tests::test_apply 2>&1 | tail -10`
Expected: FAIL — functions not defined.

- [ ] **Step 3: Implement the merge helpers, I/O glue, ANSI helpers, and `run`**

In `src/status.rs`, replace the stub `pub fn run` with the full implementation, and add the helpers above it. Add these at the top of the file alongside existing imports:

```rust
use std::io::{self, IsTerminal, Write};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
```

Then add (above the test module, replacing the existing `pub fn run`):

```rust
/// Apply session counts to rows by parsing `ps -eo pid,args` output.
pub fn apply_sessions_to_rows(rows: &mut [Row], ps_output: &str) {
    for row in rows.iter_mut() {
        row.sessions = Some(crate::container::count_sessions(ps_output, &row.name));
    }
}

/// Mark non-Running rows whose workdir no longer exists on the host as Stale.
pub fn apply_stale_to_rows(rows: &mut [Row]) {
    for row in rows.iter_mut() {
        if row.state == State::Running {
            continue;
        }
        if !row.workdir.is_empty() && !Path::new(&row.workdir).exists() {
            row.state = State::Stale;
        }
    }
}

/// Merge a stats map into the rows. Only populates Running rows.
pub fn merge_stats_into_rows(rows: &mut [Row], stats: &HashMap<String, (f64, u64, u64)>) {
    for row in rows.iter_mut() {
        if row.state != State::Running {
            continue;
        }
        if let Some(&(cpu, used, total)) = stats.get(&row.name) {
            row.cpu_pct = Some(cpu);
            row.mem_used = Some(used);
            row.mem_total = Some(total);
        }
    }
}

/// Move the cursor up `n` lines and clear from cursor to end of screen.
fn ansi_redraw_prefix(n: usize) -> String {
    if n == 0 {
        "\x1b[J".to_string()
    } else {
        format!("\x1b[{}A\x1b[J", n)
    }
}

/// Fetch the basic (fast) data: container ls JSON, ps output, stale check.
fn fetch_basic(verbose: bool) -> Result<Vec<Row>> {
    if verbose {
        eprintln!("[agentbox] container ls --all --format json");
    }
    let output = Command::new("container")
        .args(["ls", "--all", "--format", "json"])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run 'container ls': {}", e))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut rows = parse_ls_json(&stdout);

    // Sessions
    let ps_out = Command::new("ps").args(["-eo", "pid,args"]).output();
    if let Ok(ps_out) = ps_out {
        let ps_text = String::from_utf8_lossy(&ps_out.stdout);
        apply_sessions_to_rows(&mut rows, &ps_text);
    }
    // Note: if ps fails, sessions stay None — column shows "-".

    // Stale detection
    apply_stale_to_rows(&mut rows);
    Ok(rows)
}

/// Fetch live stats from `container stats --no-stream` and merge into rows.
/// Blocks ~2 seconds (Apple's hardcoded sample interval). Returns Err if
/// the subprocess fails — caller should leave the fast table on screen.
fn fetch_live(rows: &mut [Row], verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("[agentbox] container stats --no-stream");
    }
    let output = Command::new("container")
        .args(["stats", "--no-stream"])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run 'container stats': {}", e))?;
    if !output.status.success() {
        anyhow::bail!("container stats exited with non-zero status");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let stats = parse_stats_text(&text);
    merge_stats_into_rows(rows, &stats);
    Ok(())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Top-level entry point: fast pass, then progressive live pass if TTY.
pub fn run(verbose: bool) -> Result<()> {
    let mut rows = fetch_basic(verbose)?;
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));

    // Determine "current project" by computing the same name agentbox would
    // use for cwd. We tolerate failures here — bolding is best-effort.
    let current_name = std::env::current_dir()
        .ok()
        .map(|cwd| crate::container::container_name(&cwd.to_string_lossy()));
    let current_ref = current_name.as_deref();

    let stdout = io::stdout();
    let is_tty = stdout.is_terminal();
    let use_color = is_tty && std::env::var_os("NO_COLOR").is_none();

    let now = now_unix();
    let table = format_table(&rows, current_ref, use_color, &home, now);
    {
        let mut handle = stdout.lock();
        handle.write_all(table.as_bytes())?;
        handle.flush()?;
    }

    if !is_tty {
        return Ok(());
    }
    let any_running = rows.iter().any(|r| r.state == State::Running);
    if !any_running {
        return Ok(());
    }

    // Live pass — best-effort. ~2s blocking call.
    if fetch_live(&mut rows, verbose).is_err() {
        return Ok(());
    }
    let now2 = now_unix();
    let table2 = format_table(&rows, current_ref, use_color, &home, now2);

    // Move cursor up over the previously printed table and clear to end.
    // Line count = header + data rows + totals = rows.len() + 2
    let line_count = rows.len() + 2;
    let mut handle = stdout.lock();
    handle.write_all(ansi_redraw_prefix(line_count).as_bytes())?;
    handle.write_all(table2.as_bytes())?;
    handle.flush()?;
    Ok(())
}

#[cfg(test)]
mod ansi_tests {
    use super::*;

    #[test]
    fn test_ansi_redraw_prefix_nonzero() {
        let s = ansi_redraw_prefix(5);
        assert_eq!(s, "\x1b[5A\x1b[J");
    }

    #[test]
    fn test_ansi_redraw_prefix_zero() {
        let s = ansi_redraw_prefix(0);
        assert_eq!(s, "\x1b[J");
    }
}
```

- [ ] **Step 4: Run all merge/apply tests**

Run: `cargo test --lib status::tests::test_merge 2>&1 | tail -10`, `cargo test --lib status::tests::test_apply 2>&1 | tail -15`, `cargo test --lib status::ansi_tests 2>&1 | tail -10`
Expected: 6 merge/apply tests pass, 2 ansi tests pass.

- [ ] **Step 5: Run the entire status module test suite to confirm everything still works together**

Run: `cargo test --lib status 2>&1 | tail -30`
Expected: All status-module tests pass (~35 tests).

---

## Task 10: Wire up `Status` command in `main.rs`

**Files:**
- Modify: `src/main.rs:38-67` (replace `Ls` variant with `Status`)
- Modify: `src/main.rs:313-470` (replace `Some(Commands::Ls)` arm)
- Modify: `src/main.rs:524-528` (update `test_ls_subcommand`, add `test_status_alias_ls`)
- Modify: `src/container.rs:343-363` (delete `pub fn list`)

- [ ] **Step 1: Add the failing tests in `main.rs`**

In `src/main.rs`, find the existing `test_ls_subcommand` test (around line 524-528). Replace it with:

```rust
#[test]
fn test_status_subcommand() {
    let cli = Cli::try_parse_from(["agentbox", "status"]).unwrap();
    assert!(matches!(cli.command, Some(Commands::Status)));
}

#[test]
fn test_status_alias_ls() {
    let cli = Cli::try_parse_from(["agentbox", "ls"]).unwrap();
    assert!(matches!(cli.command, Some(Commands::Status)));
}
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo test --lib tests::test_status 2>&1 | tail -10`
Expected: FAIL — `Commands::Status` does not exist.

- [ ] **Step 3: Replace `Ls` variant with `Status` in the `Commands` enum**

In `src/main.rs`, find the existing `Ls` variant (around line 48-49):

```rust
    /// List all agentbox containers
    Ls,
```

Replace with:

```rust
    /// Show rich container status (CPU, memory, project, sessions)
    #[command(alias = "ls")]
    Status,
```

- [ ] **Step 4: Replace the `Some(Commands::Ls)` match arm**

In `src/main.rs`, find the existing match arm (around line 334-337):

```rust
        Some(Commands::Ls) => {
            container::list(cli.verbose)?;
            Ok(())
        }
```

Replace with:

```rust
        Some(Commands::Status) => {
            status::run(cli.verbose)?;
            Ok(())
        }
```

- [ ] **Step 5: Delete `pub fn list` in `src/container.rs`**

In `src/container.rs`, find and delete this entire function (around line 343-363):

```rust
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
    let containers = parse_container_list(&stdout);
    if containers.is_empty() {
        println!("No agentbox containers found.");
    } else {
        for (name, state) in &containers {
            println!("{}\t{}", name, state);
        }
    }
    Ok(())
}
```

`parse_container_list` is still used by `list_names` (called from `agentbox rm --all`), so keep that function and its tests.

- [ ] **Step 6: Run the test suite**

Run: `cargo test 2>&1 | tail -20`
Expected: All tests pass (`test_status_subcommand`, `test_status_alias_ls`, all 117 prior tests, all new status-module tests).

- [ ] **Step 7: Build a release binary and smoke-test the parsing**

Run: `cargo build 2>&1 | tail -10`
Expected: Builds cleanly with no warnings about unused code.

If there are warnings about unused imports in `container.rs` (e.g. `Context` if `list` was its only consumer), remove them.

---

## Task 11: Update README

**Files:**
- Modify: `README.md:34-36` (Quick Start examples)

- [ ] **Step 1: Replace the `agentbox ls` line in the Quick Start section**

In `README.md`, find:

```markdown
# List all containers
agentbox ls
```

Replace with:

```markdown
# Show container status (CPU, memory, project, sessions)
agentbox status
# `agentbox ls` is an alias for `status`
```

- [ ] **Step 2: Verify nothing else in the README references `agentbox ls` as the canonical command**

Run: `grep -n "agentbox ls" README.md` (use the Grep tool, not bash)
Expected: Only the line we just updated mentions it as an alias. No other references.

If other references exist, update them to either say `agentbox status` or note that `ls` is an alias.

---

## Task 12: Final verification

- [ ] **Step 1: Run the full test suite**

Run: `cargo test 2>&1 | tail -10`
Expected: All tests pass. Should be ~117 (previous) + ~40 (new status module) + 5 (new container::count_sessions) + 2 (status command parsing) ≈ 164 total. The exact number may differ slightly; the requirement is **0 failures**.

- [ ] **Step 2: Build a release binary**

Run: `cargo build --release 2>&1 | tail -5`
Expected: Builds cleanly.

- [ ] **Step 3: Verify clippy is clean** (if used in this repo)

Run: `cargo clippy 2>&1 | tail -20`
Expected: No new warnings introduced. (If clippy is not part of the existing CI flow, this step is optional.)

---

## Notes for the implementer

- **Don't add features beyond the spec.** `--json`, `--watch`, `--live`/`--quick`, sort flags, filter flags, image column, color-by-status — all explicitly out of scope. Adding them now expands review surface for no benefit.

- **The tests use real data.** The `LS_JSON_ONE_RUNNING` and `STATS_TEXT_SAMPLE` constants are derived from actual `container ls --format json` and `container stats --no-stream` output. Don't replace them with hand-rolled minimal versions when adding more tests.

- **The 2-second wait is real.** When testing manually with `cargo run -- status`, you'll see the fast table appear immediately and then ~2 seconds later the CPU/MEM cells will fill in. That's correct behavior. If it feels too slow, that's a future `--quick` flag — out of scope for this PR.

- **Bolding only fires when stdout is a TTY *and* `NO_COLOR` is unset *and* current cwd's container is in the list.** All three conditions must hold. Test manually by running from inside an agentbox project directory and from outside one.

- **Stale detection.** A `Stopped` row whose workdir has been deleted shows `stale` instead of `stopped`. A `Running` row's workdir missing keeps the `Running` state — by design (we don't want to hide an active container).

- **`parse_container_list` stays.** It's still used by `list_names` for `agentbox rm --all`. Don't delete it.
