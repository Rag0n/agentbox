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
/// ```text
/// [0]      name
/// [1]      cpu_pct           e.g. "7.19%"
/// [2..7]   mem               5 tokens: "2.18", "GiB", "/", "8.00", "GiB"
/// [7..12]  net_rx_tx         5 tokens (ignored)
/// [12..17] block_io          5 tokens (ignored)
/// [17]     pids              (ignored)
/// ```
///
/// Defensive: any row with fewer than 18 tokens is skipped — this
/// naturally excludes Apple's header row (`Container ID  Cpu %  ...`)
/// which has only 11 tokens. Caller is responsible for filtering to
/// agentbox-* names.
pub fn parse_stats_text(text: &str) -> HashMap<String, (f64, u64, u64)> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 18 {
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

/// Top-level entry point: gather rows, print fast pass, then live pass if TTY.
/// Stub — full implementation lands in Task 9.
pub fn run(_verbose: bool) -> Result<()> {
    Ok(())
}

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
        // 18-token row with a bad CPU value — exercises the cpu_str.parse()
        // error branch, not just the length guard.
        let text = "agentbox-foo-aaaaaa  bogus%  1.00 GiB / 1.00 GiB  0.00 B / 0.00 B  0.00 B / 0.00 B  1";
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
}
