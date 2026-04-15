# Live `agentbox status`

## Problem

`agentbox status` prints a snapshot and exits. To watch resource usage evolve — during a
long build, while debugging runaway memory, or when comparing containers — users must
re-run the command repeatedly. Apple's own `container stats` streams by default; our
wrapper does not.

## Solution

Make `agentbox status` continuously refresh on a TTY, in a `top`-like display that redraws
every two seconds. On quit, restore the terminal and print the last snapshot to normal
stdout so it remains visible in scrollback.

```
NAME                       STATUS   PROJECT                  CPU    MEM       UPTIME  SESSIONS
agentbox-agentbox-71e6bc   running  ~/Dev/Personal/agentbox  7.2%   2.2/8.0G  2h 15m  1
agentbox-marketplace-...   running  ~/Dev/Personal/market.   2.5%   0.8/8.0G  45m     0
agentbox-other-def456      stopped  ~/Dev/other              --     --        --      0
TOTALS                     2 run    -                        9.7%   3.0/16G   -        1

press q or Ctrl+C to exit
```

## CLI behavior

| Invocation | Behavior |
|-----------|----------|
| `agentbox status` on a TTY | **Live mode.** Alt screen buffer, refresh every 2s, quit with `q` / `Ctrl+C` / `Esc` |
| `agentbox status` piped / redirected | **Fast pass only.** Current non-TTY behavior preserved: `ls` + `ps` + stale check, no blocking stats call. Scripts get quick predictable output |
| `agentbox status --no-stream` on TTY | **One-shot with live pass.** Current TTY one-shot behavior: fast pass, then one `container stats --no-stream` call (~2s), then exit. For users on a TTY who want a snapshot without entering live mode |
| `agentbox status --no-stream` piped | Same as piped without the flag (the flag is a no-op — piped output never enters live mode) |

The existing `agentbox ls` alias is removed as part of this work — an explicit product
decision, not incidental cleanup. `agentbox status` becomes the one name for this
command.

On live-mode exit, the terminal is restored (cursor shown, raw mode off, alt screen
exited) and a final one-shot snapshot is printed to normal stdout, so the last state
remains visible in the shell's scrollback.

## Why polling with JSON (not streaming table)

Apple's `container stats` streaming mode is a full-screen TUI — it enters the alt screen
buffer (`ESC[?1049h`), redraws with cursor-home + clear-to-end every two seconds, and
restores on exit. Its stdout is not line-oriented parseable output; it's a terminal
application. We can't consume it from another program.

`container stats --format json` is explicitly one-shot in Apple's source: both `--no-stream`
and `--format json` route to the same `runStatic()` code path.

Given those two constraints, the options are:

1. **Poll `container stats --format json` every 2s.** Structured data, compute CPU% from
   consecutive `cpuUsageUsec` deltas using wall-clock elapsed time. We control the
   interval, data is testable, no ANSI parsing.
2. **Poll `container stats --no-stream` (table) every 2s.** Each call blocks ~2s
   internally (Apple takes two samples before emitting), parser already exists. Worse:
   unnecessary blocking, fragile text parsing.
3. **Stream table output.** Not viable — the stream is an alt-screen TUI, not consumable.

We choose option 1. The CPU% formula is the same one Apple uses:

```
cpu_pct = (cpu_usec_now - cpu_usec_prev) / elapsed_usec * 100
```

where 100% = one fully utilized core. Using wall-clock elapsed time instead of Apple's
hardcoded 2-second assumption makes the result accurate even when the event loop has
drift.

The first tick of live mode has no previous sample, so CPU shows `--`. Starting with the
second tick, real values appear. This is the same first-frame behavior as `top`.

## Architecture

Two concurrent concerns: fetching data and handling input. Since `tokio` is already a
dependency (the bridge uses it), live mode uses async.

```
main live loop (tokio::select!)

  stats_tick (2s)          stdin reader (raw mode)     shutdown_rx.changed()
       │                          │                           │
       ▼                          ▼                           ▼
  fetch_once (races          on q / Esc / Ctrl+C:       break loop
  read_to_end vs             shutdown_tx.send(true)
  shutdown_rx)
  compute CPU delta
  refresh `ls` if set changed OR every 5th tick (~10s)
  refresh `ps` every 3rd tick (~6s)
       │
       ▼
  redraw table
```

### Shutdown is a persistent flag, not a one-shot notification

Shutdown uses `tokio::sync::watch::channel(false)`, not `Notify`. This matters because
`Notify::notify_waiters()` only wakes *currently-registered* waiters — a later
`.notified()` call on a new waiter can miss the signal, which would leave a
post-shutdown subprocess waiting forever. A `watch<bool>` channel is a persistent flag:
once flipped to `true`, every existing and future receiver sees it, and
`Receiver::changed()` returns immediately if the current value already changed since
the receiver last observed it.

```rust
let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
```

The sender is held by the select loop and the signal handler; cloned receivers flow
into every subprocess call.

Triggers:
- `q` / `Esc` / `Ctrl+C` key events from the stdin reader task → `shutdown_tx.send(true)`
- SIGINT / SIGTERM from the OS signal handler → `shutdown_tx.send(true)`

### Subprocess calls race read-to-end vs shutdown

`tokio::select!` alone doesn't interrupt an already-started subprocess. Every
subprocess call in the live loop spawns the child, splits `stdout`, and races
`read_to_end` (which only borrows stdout and the buffer) against shutdown. This
ownership split is necessary because `wait_with_output()` consumes the `Child`,
leaving nothing for the kill branch.

```rust
async fn fetch_once(
    program: &str,
    args: &[&str],
    shutdown: &mut watch::Receiver<bool>,
) -> anyhow::Result<Vec<u8>> {
    // Both stdout and stderr are captured — the default is inherit, which would
    // let child error output splash onto the alt-screen UI.
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();

    // Drain stdout and stderr concurrently so a small stderr doesn't block stdout
    // (or vice versa). `try_join` returns early on the first error.
    let drain = async {
        tokio::try_join!(
            stdout.read_to_end(&mut out_buf),
            stderr.read_to_end(&mut err_buf),
        )
    };

    tokio::select! {
        res = drain => {
            res?;
            let status = child.wait().await?;
            if !status.success() {
                anyhow::bail!(
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
            let _ = child.start_kill();    // non-blocking SIGKILL
            let _ = child.wait().await;    // reap zombie
            anyhow::bail!("shutdown requested");
        }
    }
}
```

Key properties of this pattern:

- **stderr is piped**, not inherited — child diagnostics never reach the alt screen.
- **Non-zero exit is an error**, not silently ignored. The diagnostic includes the
  captured stderr so the in-memory log after exit is actionable.
- **Concurrent drain** via `tokio::try_join!` avoids the classic pipe-deadlock
  pattern where a large stderr fills its buffer and blocks the child before we read
  stdout (or vice versa).

Once shutdown fires, in-flight children are killed, every pending `fetch_once`
returns an error, the select loop breaks, and the RAII guard's `Drop` runs
terminal restoration. The contract "Ctrl+C during in-flight subprocess → child
killed" is enforced by this race, not by the outer select loop alone.

### Data sources

| Source | Cadence | Purpose |
|--------|---------|---------|
| `container stats --format json` | Every 2s | Raw CPU counter, memory used/limit |
| `container ls --all --format json` | Initial + when running-container set changes + every 5th stats tick (~10s) | NAME, STATUS, PROJECT, startedDate |
| `ps -eo pid,args` | Every 3rd stats tick (~6s) | SESSIONS count |
| `Path::exists(workdir)` | Piggybacks on every `ls` refresh | Stale detection |

The cadence rules exist to minimize work on the hot path: the stats tick is the only
unconditional poll. The `ls` refresh has two triggers:

1. **Event-driven:** compare the set of running container IDs from the current stats JSON
   against the previous tick; re-run `ls` immediately when a container appeared or
   disappeared.
2. **Periodic fallback (every ~10s):** `container stats` only reports running containers,
   so events that affect only non-running rows — a stopped container being removed
   (`container rm`), or a stopped row becoming stale when its workdir is deleted on the
   host — are invisible to the event trigger. The 10-second periodic refresh catches
   these without making every tick expensive.

Session counting is relatively static, so `ps` runs on a 6s cadence rather than every
tick.

### Per-container state

The live loop keeps one `Option<PrevSample>` per container:

```rust
struct PrevSample {
    cpu_usec: u64,
    taken_at: std::time::Instant,
}
```

On each stats tick: for each container in the new JSON, look up its previous sample,
compute CPU%, update the stored sample. New containers get `cpu_pct = None` until
their second sample arrives.

### Module layout

New submodule `src/status/live.rs`. The existing one-shot code becomes
`src/status/mod.rs` (unchanged in behavior, just moved). Shared types (`Row`, `State`,
parsers, formatters) stay at the module root and are used by both modes.

```
src/status/
├── mod.rs      Row, State, parsers, format_table, one-shot run()
└── live.rs     live loop, CPU delta tracking, input handling
```

Live mode builds `Row` values the same way the one-shot path does. `format_table` is
extended (not replaced) to accept optional precomputed column widths:

```rust
// current:
fn format_table(rows: &[Row], current: Option<&str>, color: bool, ...) -> String

// extended:
fn format_table(rows: &[Row], current: Option<&str>, color: bool, ...,
                widths: Option<&ColumnWidths>) -> String
```

One-shot callers pass `None` and get the current behavior (widths computed from the
rows being rendered). Live mode maintains a `ColumnWidths` value across ticks and
passes `Some(&widths)` on every render. Widths change only at three points: first
render (seeded), terminal resize, and `ls` refreshes that introduce wider rows (see
"Monotonic width growth across `ls` refreshes" below).

### Seeding widths for live numeric columns

Fixing widths at first render is not enough on its own: the first live frame has `--`
placeholders (2 chars) for CPU and MEM, while later frames may render `999.9%` (6 chars)
or `99.9/99.9G` (10 chars). Pure max-of-current-cells width selection would grow the
columns mid-loop, which is exactly the jitter we're trying to prevent.

Live mode seeds the `ColumnWidths` with representative *maximum-plausible* values for
the numeric columns before measuring:

| Column | Seed value | Rationale |
|--------|-----------|-----------|
| CPU    | `999.9%`   | Multi-core containers can exceed 100% — reserve up to ~10 cores |
| MEM    | `99.9/99.9G` | Host memory well above typical agentbox limits |

The seed is a floor, not a clamp. If actual values ever exceed the seed, the formatter
will still render them correctly on that frame — widths only grow, never shrink,
bounded by terminal width.

### Monotonic width growth across `ls` refreshes

NAME and PROJECT widths are computed from the actual rows, not seeded. But because
`ls` refreshes can introduce a newly-started container with a longer name or workdir
path, the widths need to grow when that happens. The rule is **monotonic growth**:

```rust
widths.name    = max(widths.name,    rows.iter().map(|r| r.name.len()).max());
widths.project = max(widths.project, rows.iter().map(shortened_project_len).max());
```

Applied every time the row set changes (via event or periodic `ls` refresh). Widths
never shrink — if a long-named container stops and disappears, its column stays the
width it was, so remaining rows don't jitter left. Over long sessions this may waste
a few characters of horizontal space, which is an acceptable cost for zero mid-session
jitter.

### UPTIME is intentionally not seeded

Its width can grow at boundaries (`59m` → `1h 0m`, or `23h 59m` → `1d 0h` which
actually shrinks), but these transitions happen at most once per container per hour
and the deltas are 1–2 characters. Seeding UPTIME to accommodate `99d 23h` would
reserve horizontal space that's wasted 99% of sessions. The occasional 1-char shift
at an hour boundary is an acceptable cost. The columns where jitter actually matters —
CPU% and MEM refreshing every 2s — are the ones we seed.

One-shot mode does not seed and does not apply monotonic growth — it computes widths
purely from the rendered rows on each invocation.

## Terminal management

Uses the [`crossterm`](https://docs.rs/crossterm) crate (~500KB compiled). The
alternative is rolling our own raw mode with `libc::termios` — doable in ~200 lines
of unsafe code but not worth the maintenance cost.

```toml
crossterm = "0.28"
```

### Lifecycle

```
startup (before alt screen — errors here land on normal terminal):
  check_prerequisites()  — auto-starts container system if needed; fail → exit
  verify terminal is large enough to render the header row; fail → exit

enter TUI:
  enter alt screen   (crossterm::terminal::EnterAlternateScreen)
  hide cursor        (crossterm::cursor::Hide)
  enable raw mode    (crossterm::terminal::enable_raw_mode)
  install panic hook that restores terminal
  install SIGINT/SIGTERM handler that triggers graceful shutdown

main loop:
  tokio::select! {
    _   = stats_tick           => { fetch_once (inner select! vs shutdown_rx), redraw }
    key = stdin_events         => { on q / Esc / Ctrl+C: shutdown_tx.send(true); break }
    _   = resize_events        => { recompute widths, redraw }
    _   = shutdown_rx.changed() => { break }
  }

cleanup (always runs via RAII guard's Drop):
  disable raw mode
  show cursor
  exit alt screen
  print final one-shot snapshot to normal stdout
```

### Cleanup must be bulletproof

If the process dies without restoring terminal state, the user's shell is broken
(no echo, weird cursor, keystrokes invisible). Three independent mechanisms all funnel
through a single RAII guard:

1. **`Drop` on the guard struct** — restores terminal on normal exit and during unwind.
2. **Panic hook** — custom hook restores terminal *before* the default panic handler
   prints, so the panic message lands on a sane terminal.
3. **Signal handler** — SIGTERM/SIGINT trigger graceful shutdown of the select loop,
   which lets the guard's `Drop` run.

All paths converge on one restoration routine. No duplicated cleanup code.

### Redraw strategy

- Full redraw each frame: `ESC[H ESC[J` then reprint the table. Matches Apple's
  approach. We already have `format_table`, which returns a complete string.
- Column widths are seeded with representative-maximum values for CPU/MEM on first
  render and on resize (SIGWINCH). NAME and PROJECT grow monotonically as `ls`
  refreshes introduce wider rows; they never shrink. This prevents jitter on every-2s
  updates — a CPU% shifting from `9.2%` to `10.1%` doesn't move columns, and a long-
  named container disappearing doesn't pull columns leftward. UPTIME can still shift
  by a character at hour/day boundaries (rare, by design — see "Seeding widths for
  live numeric columns" above).
- `NO_COLOR` and current-project bolding work the same as one-shot mode — both honor
  the existing `use_color` logic.
- A footer area below the totals row holds two lines:
  - Line 1: transient status (`stats unavailable`, `parse error (see log after exit)`,
    or blank when healthy)
  - Line 2: `press q or Ctrl+C to exit` — always visible so the quit hotkey never hides

## Error handling

| Scenario | Behavior |
|----------|----------|
| `container stats` subprocess fails | Keep last-known values, show `stats unavailable` in footer, retry next tick |
| JSON parse error | In-band: footer shows `parse error (see log after exit)`, tick is skipped, one-line diagnostic buffered in-memory. Buffered diagnostics are printed to stderr *after* terminal restore so they don't corrupt the alt-screen view |
| `container ls` fails during refresh (subprocess error or parse error) | Keep existing container set — `parse_ls_json` returns `Err` for malformed output, which the refresh path treats the same as a subprocess failure. Retry on next refresh (event or periodic). Prevents a malformed-but-successful `container ls` from blanking the table |
| `ps` fails | SESSIONS column keeps last-known values |
| Terminal too small for full table | Gracefully truncate PROJECT column (same logic as one-shot mode) |
| Ctrl+C during in-flight subprocess | Child killed via the shutdown race, guard restores terminal, final snapshot printed |
| Panic | Panic hook restores terminal, default panic output runs |
| `container system` not running at startup | Live mode calls `check_prerequisites()` first — same as one-shot and other commands — which auto-starts the container system via `container system start`. Only if the auto-start itself fails do we exit cleanly with an error on normal stdout. All of this happens **before** entering alt screen, so error output lands on a normal terminal |

Principle: no single transient failure kills the live view. All transient errors stay
in-band (footer indicator, in-memory log). Only unrecoverable conditions detected at
startup — `container system start` failing, terminal too small to render anything —
exit cleanly, and they exit *before* the alt screen is entered so their error output
lands on a normal terminal.

## Testing

| Target | Type | Approach |
|--------|------|----------|
| `compute_cpu_pct(prev, curr, elapsed)` | Unit | Pure function, table-driven: first sample (None), normal delta, counter reset, zero elapsed |
| `parse_stats_json(text)` | Unit | Real JSON samples from `container stats --format json`. Signature is `Result<HashMap<String, RawStats>, ParseError>` — malformed input returns `Err`, valid-but-empty input returns `Ok(HashMap::new())`. The live loop distinguishes the two: `Err` triggers the footer indicator, `Ok(empty)` just means no running containers |
| `parse_ls_json(text)` | Unit | Signature changes from `Vec<Row>` to `Result<Vec<Row>, ParseError>` for the same reason: malformed JSON must be distinguishable from "no containers". Existing one-shot callers adapt (treat `Err` the same as they currently treat subprocess failure). Existing table-driven tests keep their inputs; assertions update to match the new return type |
| `detect_container_set_change(prev, curr)` | Unit | New container, removed container, no change, both empty |
| `ColumnWidths::update(rows)` | Unit | First call seeds CPU/MEM floors; subsequent calls grow NAME/PROJECT monotonically; NAME/PROJECT never shrink when rows removed; CPU/MEM seed values are floor, not clamp |
| Live loop orchestration | Integration | `StatsSource` trait with a stub implementation so the loop drives without spawning subprocesses |
| Terminal lifecycle | Manual smoke test | Documented in PR description — crossterm's terminal state is not reasonably unit-testable |
| Panic restoration | Manual smoke test | Force a panic inside the live loop, verify terminal restores |
| Clap parsing | Unit | `--no-stream` flag parses, `ls` alias removed |

`parse_stats_text` and the formatters `format_uptime`, `format_mem`, `shorten_path`
stay unchanged. `parse_ls_json` changes signature to `Result<Vec<Row>, ParseError>` so
that malformed JSON is distinguishable from valid-but-empty output — symmetric with
`parse_stats_json`. `format_table` gains an optional `widths: Option<&ColumnWidths>`
parameter (one-shot callers pass `None` and see no rendering change). `parse_stats_json`
is a new sibling for the streaming-mode input format.

## Out of scope for this iteration

- Configurable refresh interval (hardcoded to 2s, matching Apple)
- Sort / filter hotkeys in live mode
- Sparkline or history graphs
- JSON output mode for piped consumers
- Network and block I/O columns (the JSON has the counters, but the table doesn't
  use them yet)

## Files changed

- `src/status/mod.rs` — renamed from `src/status.rs`, unchanged behavior
- `src/status/live.rs` — new, live loop + terminal management + CPU delta computation
- `src/main.rs` — add `--no-stream` flag on `Status`, drop `ls` alias, dispatch to
  live or one-shot based on TTY + flag
- `Cargo.toml` — add `crossterm`
- `README.md` — update Quick Start (remove `ls` example, add live mode description)
