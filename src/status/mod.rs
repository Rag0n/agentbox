pub mod live;

use anyhow::Result;
use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug)]
pub struct ParseError {
    message: String,
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

/// Apple epoch (2001-01-01 UTC) → Unix epoch (1970-01-01 UTC) offset, in seconds.
const APPLE_EPOCH_OFFSET: i64 = 978_307_200;

/// Parse `container ls --all --format json` output into rows. Filters to
/// containers whose id starts with `agentbox-`. Live fields (sessions,
/// cpu_pct, mem_*) are left as None — they get populated by later passes.
/// Stale detection is *not* done here; the caller adds it.
///
/// Returns Err if JSON parsing fails, distinguishing it from "no agentbox containers".
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

    /// Total rendered width of a row at these widths — sum of all
    /// columns plus a 2-space separator between each adjacent pair.
    /// Used to check whether the terminal is wide enough to render
    /// without wrapping.
    pub fn total_width(&self) -> usize {
        self.name + self.status + self.project + self.cpu
            + self.mem + self.uptime + self.sessions
            + (HEADERS.len() - 1) * 2
    }
}

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
    let uptime = match (row.state, row.started_unix) {
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

/// Merge a stats map into the rows. Only populates Running rows when
/// there is a matching entry in `stats`. Non-Running rows are left
/// alone. Rows whose names aren't in `stats` are also left alone — in
/// the production flow they always arrive with `cpu_pct: None` from
/// `parse_ls_json`, so unmatched rows naturally render as `--`.
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
    let mut rows = parse_ls_json(&stdout)
        .unwrap_or_else(|e| {
            eprintln!("[agentbox] warning: could not parse container ls output: {}", e);
            Vec::new()
        });

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
    let table = format_table(&rows, current_ref, use_color, &home, now, None);
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
    let table2 = format_table(&rows, current_ref, use_color, &home, now2, None);

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
        assert_eq!(w.cpu, 6);
        assert_eq!(w.mem, 10);
    }

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

    #[test]
    fn test_parse_ls_json_one_running() {
        let rows = parse_ls_json(LS_JSON_ONE_RUNNING).unwrap();
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
        let rows = parse_ls_json(json).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "agentbox-x-aaaaaa");
    }

    #[test]
    fn test_parse_ls_json_stopped_state() {
        let json = r#"[{
            "status":"stopped",
            "configuration":{"id":"agentbox-x-aaaaaa","initProcess":{"workingDirectory":"/tmp/x"}}
        }]"#;
        let rows = parse_ls_json(json).unwrap();
        assert_eq!(rows[0].state, State::Stopped);
    }

    #[test]
    fn test_parse_ls_json_missing_started_date() {
        let json = r#"[{
            "status":"stopped",
            "configuration":{"id":"agentbox-x-aaaaaa","initProcess":{"workingDirectory":"/tmp/x"}}
        }]"#;
        let rows = parse_ls_json(json).unwrap();
        assert_eq!(rows[0].started_unix, None);
    }

    #[test]
    fn test_parse_ls_json_missing_workdir() {
        let json = r#"[{
            "status":"running",
            "configuration":{"id":"agentbox-x-aaaaaa","initProcess":{}}
        }]"#;
        let rows = parse_ls_json(json).unwrap();
        assert_eq!(rows[0].workdir, "");
    }

    #[test]
    fn test_parse_ls_json_sorted_by_name() {
        let json = r#"[
            {"status":"running","configuration":{"id":"agentbox-zz-aaaaaa","initProcess":{"workingDirectory":"/z"}}},
            {"status":"running","configuration":{"id":"agentbox-aa-aaaaaa","initProcess":{"workingDirectory":"/a"}}}
        ]"#;
        let rows = parse_ls_json(json).unwrap();
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
        let rows = parse_ls_json(json).unwrap();
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
        let table = format_table(&rows, None, false, home, now, None);
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
        let table = format_table(&rows, None, false, home, 1_775_515_789 + 60 * 75, None);
        assert!(table.contains("7.2%"));
        assert!(table.contains("2h 15m") || table.contains("1h 15m"));
    }

    #[test]
    fn test_format_table_stopped_shows_dashes() {
        let rows = sample_rows();
        let home = Path::new("/Users/alex");
        let table = format_table(&rows, None, false, home, 0, None);
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
        let table = format_table(&rows, None, false, home, 1_775_515_789 + 60 * 75, None);
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
            None,
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
            None,
        );
        assert!(!table.contains("\x1b[1m"));
        assert!(!table.contains("\x1b[22m"));
    }

    #[test]
    fn test_format_table_no_bolding_when_no_current() {
        let rows = sample_rows();
        let home = Path::new("/Users/alex");
        let table = format_table(&rows, None, true, home, 1_775_515_789 + 60 * 75, None);
        assert!(!table.contains("\x1b[1m"));
    }

    #[test]
    fn test_format_table_totals_running_count() {
        let rows = sample_rows();
        let home = Path::new("/Users/alex");
        let table = format_table(&rows, None, false, home, 1_775_515_789 + 60 * 75, None);
        let totals_line = table.lines().find(|l| l.contains("TOTALS")).unwrap();
        // 1 running container in sample
        assert!(totals_line.contains("1 run"));
    }

    #[test]
    fn test_format_table_empty_still_renders_header_and_totals() {
        let home = Path::new("/Users/alex");
        let table = format_table(&[], None, false, home, 0, None);
        assert!(table.contains("NAME"));
        assert!(table.contains("TOTALS"));
    }

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
        // Mirror the production call site: rows arrive from parse_ls_json with
        // live fields already None. Merging an empty stats map leaves them None.
        let mut rows = vec![Row {
            name: "agentbox-aaa-111111".to_string(),
            state: State::Running,
            workdir: "/tmp/aaa".to_string(),
            started_unix: None,
            sessions: None,
            cpu_pct: None,
            mem_used: None,
            mem_total: None,
        }];
        let stats: HashMap<String, (f64, u64, u64)> = HashMap::new();
        merge_stats_into_rows(&mut rows, &stats);
        assert_eq!(rows[0].cpu_pct, None);
        assert_eq!(rows[0].mem_used, None);
        assert_eq!(rows[0].mem_total, None);
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
