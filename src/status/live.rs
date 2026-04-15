//! Live-mode implementation for `agentbox status`.
//!
//! Contains the tokio-based polling loop, terminal-mode RAII guard, and
//! the subprocess helper that races stdout/stderr reads against a
//! shutdown watch channel.

use anyhow::{bail, Context, Result};
use std::io::{self, IsTerminal, Write};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::watch;

use crossterm::{cursor, terminal, ExecutableCommand};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;

use crate::status::{
    apply_sessions_to_rows, apply_stale_to_rows, compute_cpu_pct,
    detect_container_set_change, format_table, ColumnWidths, State,
};

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
pub async fn run_live_loop(
    source: Box<dyn StatsSource>,
    shutdown_tx: watch::Sender<bool>,
    opts: LiveOptions,
) -> Result<LiveResult> {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
    let current_name = std::env::current_dir()
        .ok()
        .map(|cwd| crate::container::container_name(&cwd.to_string_lossy()));

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

    let mut last_rows: Vec<Row> = Vec::new();
    // Seeded with placeholders so a resize that arrives before the first
    // frame can still render something sensible. The frame arm overwrites
    // these on every tick; the initial values are defensive, not load-bearing.
    #[allow(unused_assignments)]
    let mut last_widths = ColumnWidths::seeded();
    #[allow(unused_assignments)]
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
                match evt {
                    Some(Ok(Event::Key(KeyEvent { code, modifiers, kind, .. }))) => {
                        if kind == KeyEventKind::Release {
                            continue;
                        }
                        let quit = matches!(code, KeyCode::Char('q') | KeyCode::Esc)
                            || (modifiers.contains(KeyModifiers::CONTROL)
                                && matches!(code, KeyCode::Char('c')));
                        if quit {
                            let _ = shutdown_tx.send(true);
                            break;
                        }
                    }
                    Some(Ok(Event::Resize(_, _))) => {
                        // Terminal resized: the emulator may have reflowed or cleared
                        // parts of the alt screen. Redraw with the last known frame so
                        // the table reflects the new geometry immediately instead of
                        // waiting up to a full tick.
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
                    _ => {}
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
                        break;
                    }
                }
            }
        }
    }

    drop(frame_rx);

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

fn is_shutdown_err(msg: &str) -> bool {
    msg.contains("shutdown requested")
}

fn log_non_shutdown(log: &mut Vec<String>, label: &str, err: &anyhow::Error) {
    let msg = err.to_string();
    if !is_shutdown_err(&msg) {
        log.push(format!("{}: {}", label, msg));
    }
}

async fn fetcher_task(
    mut source: Box<dyn StatsSource>,
    frame_tx: tokio::sync::mpsc::Sender<Frame>,
    mut shutdown_rx: watch::Receiver<bool>,
    opts: LiveOptions,
    home: std::path::PathBuf,
) -> Vec<String> {
    let mut stderr_log: Vec<String> = Vec::new();

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

    let _ = frame_tx.send(Frame {
        rows: rows.clone(),
        widths: widths.clone(),
        footer_msg: None,
    }).await;

    let mut prev_samples: HashMap<String, PrevSample> = HashMap::new();
    let mut prev_ids: Vec<String> = Vec::new();

    let mut ticker = tokio::time::interval(Duration::from_millis(opts.tick_ms));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
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
    let use_color = std::env::var_os("NO_COLOR").is_none();
    let table = format_table(rows, current_name, use_color, home, now_unix, Some(widths));

    // In raw mode, LF does not carriage-return; every line after the first
    // would drift right by the previous line's width. Translate \n to \r\n
    // at the edge.
    let mut body = String::with_capacity(table.len() + 64);
    body.push_str(&table);
    body.push('\n');
    body.push_str(footer_msg.unwrap_or(""));
    body.push('\n');
    body.push_str("press q or Ctrl+C to exit");
    body.push('\n');
    let body = body.replace('\n', "\r\n");

    let mut stdout = io::stdout();
    let _ = write!(stdout, "\x1b[H\x1b[J");
    let _ = stdout.write_all(body.as_bytes());
    let _ = stdout.flush();
}

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

    #[test]
    fn test_terminal_guard_construction_is_safe_without_tty() {
        // In CI / test environments stdout is usually not a TTY. The guard
        // should gracefully skip the real terminal-mode switches in that
        // case instead of panicking. This just verifies no panic / no
        // process death.
        let _ = TerminalGuard::new_if_tty();
    }

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

    #[test]
    fn test_min_terminal_cols_matches_seeded_widths() {
        // Guard against silent drift: if someone changes a seed value in
        // `ColumnWidths::seeded()` without updating the minimum, this
        // recomputes the expected minimum from the same seeds and
        // compares.
        let w = ColumnWidths::seeded();
        let expected = w.name + w.status + w.project + w.cpu + w.mem + w.uptime + w.sessions + 12;
        assert_eq!(min_terminal_cols(), expected);
        // Sanity: with current seeds (4+6+7+6+10+6+8 + 12), min is 59.
        assert_eq!(min_terminal_cols(), 59);
    }

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
}

/// Minimum terminal width required to render the seeded header row:
/// the sum of seeded column widths plus a two-space separator between
/// each of the seven columns. Derived from `ColumnWidths::seeded()` at
/// runtime so the two stay in sync if seeds change.
fn min_terminal_cols() -> usize {
    let w = ColumnWidths::seeded();
    w.name + w.status + w.project + w.cpu + w.mem + w.uptime + w.sessions + 6 * 2
}

/// Fail-fast check run before entering alt screen so the error lands on
/// the normal terminal. If the terminal size query fails, we proceed —
/// not knowing is different from knowing it's too small, and the user
/// gets line-wrapping at worst.
fn check_terminal_width() -> Result<()> {
    let (cols, _rows) = match crossterm::terminal::size() {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    let min = min_terminal_cols();
    if (cols as usize) < min {
        bail!(
            "terminal is too narrow for live status ({} cols; need at least {}). \
             Resize the window, or use `agentbox status --no-stream` / pipe the output \
             to skip live mode.",
            cols,
            min
        );
    }
    Ok(())
}

/// Top-level entry point: installs terminal guard, runs the loop,
/// then (after guard drop) flushes buffered diagnostics to stderr
/// and prints the last rendered frame to normal stdout so the last
/// state stays in scrollback.
pub fn run(verbose: bool) -> Result<()> {
    check_terminal_width()?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;

    let (tx, _rx) = watch::channel(false);

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
        let _guard = TerminalGuard::new_if_tty();
        result = rt.block_on(run_live_loop(source, tx, LiveOptions::default()))?;
    }

    for line in &result.stderr_log {
        eprintln!("[agentbox status live] {}", line);
    }

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
