# `agentbox status` command

## Problem

`agentbox ls` shows only `name<TAB>state` per container. To investigate resource usage or
sessions, users have to run separate `container stats` and `container inspect` invocations
and correlate output by name. There's no single view that answers "what's running, where,
how much is it using, and which session am I in?".

## Solution

Replace `Ls` with a richer `Status` subcommand. `agentbox ls` continues to work as a clap
alias for `Status` — same handler, same output, no behavior split. The new command shows
seven columns in a single table:

```
NAME                       STATUS   PROJECT                  CPU    MEM       UPTIME  SESSIONS
agentbox-agentbox-71e6bc   running  ~/Dev/Personal/agentbox  7.2%   2.2/8.0G  2h 15m  1
agentbox-marketplace-...   running  ~/Dev/Personal/market.   2.5%   0.8/8.0G  45m     0
agentbox-other-def456      stopped  ~/Dev/other              --     --        --      0
TOTALS                     2 run    -                        9.7%   3.0/16G   -        1
```

The current project's row (matched by `container::container_name(cwd)`) is bolded when
stdout is a TTY. Stale containers — those whose `workingDirectory` no longer exists on
the host — show `stale` in the STATUS column instead of `stopped`.

## Constraints discovered during design

Two facts from the upstream Apple Container source
([`Sources/ContainerCommands/Container/ContainerStats.swift`](https://github.com/apple/container/blob/main/Sources/ContainerCommands/Container/ContainerStats.swift))
shaped the design:

1. **`container stats` blocks for ~2 seconds.** Apple's `collectStats()` always takes
   two snapshots with a hardcoded 2-second sleep between them, regardless of `--no-stream`
   or output format. There is no fast path. Every `container stats` invocation pays this
   cost.

2. **JSON mode is strictly worse than text mode.** `--format json` pays the same 2-second
   cost but emits only the second snapshot, throwing away the first. So we cannot compute
   CPU% from JSON output ourselves — the delta is gone. Text mode pays the same 2 seconds
   and gives us the pre-computed `7.19%` for free.

These two facts force the progressive-update design below.

## Progressive rendering

`agentbox status` runs in two passes:

**Fast pass (~50ms):** Print the full 7-column table immediately, with `CPU` and `MEM`
cells showing `--`. The user can read NAME / STATUS / PROJECT / UPTIME / SESSIONS
right away.

**Live pass (~2s, TTY only):** Call `container stats --no-stream`, parse the text output,
and redraw the same table in place with CPU and MEM cells filled in for running containers.

Critically, **column widths and headers are identical between passes.** The fast pass
already reserves the CPU and MEM columns with `--` placeholders, so the live pass only
swaps cell contents — no visual reflow, no jump.

```
fn run(verbose) -> Result<()>:
    rows         = fetch_basic()
    current_name = container::container_name(cwd)
    is_tty       = io::stdout().is_terminal()
    use_color    = is_tty && env::var("NO_COLOR").is_err()

    table = format_table(&rows, current_name, use_color)
    print(table); flush()

    if !is_tty:                       return Ok(())
    if no running in &rows:           return Ok(())   # skip 2s wait if nothing to sample

    let line_count = rows.len() + 2   # header + rows + totals
    match fetch_live(&mut rows):
        Ok(_):
            move_cursor_up(line_count)
            clear_to_end()
            print(format_table(&rows, current_name, use_color))
        Err(_): pass                  # leave fast table; no error spam
```

### When the live pass is skipped

| Condition                     | Behavior                                                |
|-------------------------------|---------------------------------------------------------|
| `stdout` is not a TTY (piped) | Fast pass only. CPU and MEM stay `--`. No 2-second wait. |
| No running containers         | Fast pass only. Nothing to sample.                      |
| `container stats` exits non-0 | Fast pass stays on screen unchanged. No error spam.     |
| `NO_COLOR` env var set        | Live pass still runs; bolding is suppressed.            |

The piped case is intentional: scripts that want live data should use a future `--json`
mode (out of scope for v1), not parse the table.

### Ctrl+C during the 2s wait

`main.rs` already calls `install_signal_handlers()` to suppress SIGHUP and SIGTERM. For
`status` we additionally need to make Ctrl+C clean up: kill the child `container stats`
process (otherwise it keeps running for the rest of its 2s window) and print a newline so
the shell prompt doesn't land on top of our table. Approximately 10 lines of handling
local to `status::run`.

## Data sources

Three subprocess calls plus one fs scan, total. Two of them happen in the fast pass.

| Call                                       | When       | Purpose                                          |
|--------------------------------------------|------------|--------------------------------------------------|
| `container ls --all --format json`         | Fast pass  | Names, states, project paths, uptime, totals    |
| `ps -eo pid,args`                          | Fast pass  | Session counting per container                  |
| `Path::exists(workdir)` per container      | Fast pass  | Stale detection                                  |
| `container stats --no-stream` (text mode)  | Live pass  | CPU%, memory used / limit                        |

### `container ls --all --format json`

Each entry contains the full configuration block. We extract:

```
configuration.id                                  -> name
status                                            -> "running" | "stopped" | other
configuration.initProcess.workingDirectory        -> project path
startedDate                                       -> Mac Absolute Time (Apple epoch)
```

`startedDate` is seconds since 2001-01-01 00:00:00 UTC. Convert to Unix epoch by adding
`978_307_200`. `startedDate` exists for stopped containers too, but it's the *last*
started time, not currently-running uptime, so we only render UPTIME for `Running` rows.

Filter to entries whose `id` starts with `agentbox-` (this also excludes the always-on
`buildkit` container).

### `container stats --no-stream` (text mode)

Output is column-aligned text with a header row:

```
Container ID                 Cpu %  Memory Usage           Net Rx/Tx                Block I/O                Pids
agentbox-agentbox-71e6bc     7.19%  2.18 GiB / 8.00 GiB    28.20 MiB / 19.53 MiB    819.63 MiB / 146.74 MiB  83
```

Stats reports running containers only. Stopped containers won't appear in the map and
keep their fast-pass `--` placeholders.

Parser strategy: split each row on whitespace, expect 18 tokens in fixed positions:

```
[0]      name              e.g. "agentbox-agentbox-71e6bc"
[1]      cpu_pct           e.g. "7.19%"
[2..7]   mem               5 tokens: "2.18", "GiB", "/", "8.00", "GiB"
[7..12]  net_rx_tx         5 tokens (ignored)
[12..17] block_io          5 tokens (ignored)
[17]     pids              (ignored)
```

Defensive: any row with fewer than 18 tokens is skipped. Header row is
identified by leading token `Container` (or by it being the first non-empty line) and
also skipped. Memory units handled: `B`, `KiB`, `MiB`, `GiB`, `TiB` (binary).

### `ps -eo pid,args` and session counting

Reuses the existing row-matching logic from `container::has_other_sessions`: a line counts
as a session if its args contain `container exec` or `container run` *and* the container
name. Both `has_other_sessions` (boolean, excludes own pid) and the new `count_sessions`
(usize, no exclusion) delegate to a shared helper. The always-on
`container-runtime-linux` process is naturally excluded because it doesn't contain
`container exec` or `container run`. Existing tests for `has_other_sessions` keep
passing.

### Stale detection

`std::path::Path::new(&row.workdir).exists()`. One stat() per container. Folded into the
`State` enum rather than a separate column — `STATUS` displays `stale` in place of
`stopped` when the workdir is gone. Keeps the table at 7 columns.

## Module layout

New file `src/status.rs`. The existing `src/container.rs` is already ~700 lines and
conflates RPC with parsing; adding a 7-column table renderer to it would push it past
1000 and mix display concerns into the RPC layer. A separate module is the right cut.

```rust
// src/status.rs

struct Row {
    name: String,
    state: State,
    workdir: String,
    started_unix: Option<i64>,     // None if startedDate missing
    sessions: Option<usize>,        // None if ps failed
    cpu_pct: Option<f64>,           // None until live pass
    mem_used: Option<u64>,          // None until live pass
    mem_total: Option<u64>,         // None until live pass
}

enum State { Running, Stopped, Stale }

pub fn run(verbose: bool) -> Result<()>;             // entry point
fn fetch_basic(verbose: bool) -> Result<Vec<Row>>;   // ls + ps + stale
fn fetch_live(rows: &mut [Row], verbose: bool) -> Result<()>;  // stats

// Pure parsers (table-driven tests):
fn parse_ls_json(json: &str) -> Vec<Row>;
fn parse_stats_text(text: &str) -> HashMap<String, (f64, u64, u64)>;

// Pure formatters (table-driven tests):
fn shorten_path(path: &str, home: &Path, max: usize) -> String;
fn format_uptime(elapsed_secs: i64) -> String;       // "2h 15m" | "45m" | "3d 4h"
fn format_mem(bytes: u64) -> String;                 // "2.2G" | "812M"
fn format_table(rows: &[Row], current: Option<&str>, color: bool) -> String;

// TTY/ANSI helpers (light tests):
fn move_cursor_up(n: usize);
fn clear_to_end();
```

### Changes to existing files

**`src/main.rs`**

Replace the `Ls` variant with `Status`, declaring `ls` as a clap alias:

```rust
/// Show rich container status (CPU, memory, project, sessions)
#[command(alias = "ls")]
Status,
```

Replace the `Some(Commands::Ls) => ...` arm with:

```rust
Some(Commands::Status) => {
    status::run(cli.verbose)?;
    Ok(())
}
```

Add `mod status;` near the existing module declarations.

**`src/container.rs`**

- Delete `pub fn list(verbose)` — its only caller is the old `Ls` arm, which is replaced.
- Keep `pub fn list_names(verbose)` — used by `rm --all`. Either give it its own minimal
  parser over `container ls --format json`, or have it call `status::parse_ls_json` and
  project to names. Either is fine.
- Delete `parse_container_list` if no longer used after the above change. Otherwise keep
  as is.
- Add a sibling `pub fn count_sessions(ps_output: &str, name: &str) -> usize` next to
  `has_other_sessions`. Both delegate to a private `matches_session(line, name)` helper
  to share the row-matching logic. Or keep `count_sessions` in `status.rs` and import
  the helper — call site preference.

**`README.md`**

Update the Quick Start section:

```diff
-# List all containers
-agentbox ls
+# Show container status (CPU, memory, project, sessions)
+agentbox status
+# `agentbox ls` is an alias for `status`
```

## Bolding the current row

Match `row.name` against `container::container_name(cwd)`. If they match and `use_color`
is true, wrap that row's printed line in `\x1b[1m...\x1b[22m`. The redraw applies the
same logic so bolding survives the live pass.

`use_color` is `is_tty && env::var("NO_COLOR").is_err()` — honors the
[`NO_COLOR`](https://no-color.org/) convention.

## Path shortening

`format_path(path, home, max_width)`:

1. If `path` starts with `home`, replace the prefix with `~`. So
   `/Users/alex_guschin/Dev/Personal/agentbox` → `~/Dev/Personal/agentbox`.
2. If the result is longer than `max_width`, ellipsize the middle:
   `~/Dev/Personal/marketplace` (26) becomes `~/Dev/Personal/market…` (22) at width 22.
3. Paths with spaces (e.g.
   `/Users/alex_guschin/Library/Mobile Documents/iCloud~md~obsidian/Documents/SecondBrain`)
   work the same — no special handling needed since the table renderer pads cells, not
   tokens.

`max_width` for the PROJECT column is determined dynamically: `min(longest_shortened_path,
configurable_cap)`. Cap is `40` characters in v1, hardcoded.

## Totals row

A single row at the bottom of the table summing across rows:

| Column   | Value                                                   |
|----------|---------------------------------------------------------|
| NAME     | `TOTALS`                                                |
| STATUS   | `<n> run` where n = count of `Running` rows             |
| PROJECT  | `-`                                                     |
| CPU      | sum of `cpu_pct` (only meaningful after live pass)     |
| MEM      | `format_mem(sum_used)/format_mem(sum_total)`            |
| UPTIME   | `-`                                                     |
| SESSIONS | sum of `sessions` (defaults zero where None)            |

The totals row is always rendered, even with one container, so the table layout is
predictable and the line count for the cursor-up redraw is stable.

## Test plan

Pure functions get table-driven tests in the same style as `container.rs`:

- **`parse_ls_json`**: agentbox-only filter (rejects `buildkit`), missing `workingDirectory`,
  missing `startedDate`, mixed states (running / stopped), the 6 real containers from the
  user's sample data, malformed JSON returns empty.
- **`parse_stats_text`**: header row skipped, the 3-row sample from real data, `buildkit`
  passes through (caller filters), missing rows for stopped containers, malformed row with
  too few tokens skipped, GiB/MiB/KiB/B unit conversion, decimal value rounding.
- **`count_sessions`**: 0/1/many matches, `container-runtime-linux` exclusion (the row
  doesn't contain `container exec` or `container run`), runs across multiple containers
  in the same `ps` output.
- **`shorten_path`**: exact `$HOME`, home-prefix substitution, non-home absolute path,
  path with spaces, exact-fit at `max_width`, ellipsize when too long, very short
  `max_width` (`<5`) doesn't panic.
- **`format_uptime`**: `<60s` → `0m` or `Ns`, `<1h` → `Nm`, `<1d` → `Nh Nm`, multi-day →
  `Nd Nh`, `0` → `0m`, negative (clock skew) → `0m`.
- **`format_mem`**: bytes/KiB/MiB/GiB rounding boundaries, exactly 1024.
- **`format_table`**: column alignment with mixed states, current-row bolding when
  `use_color=true`, no bolding when `use_color=false`, `NO_COLOR` honored, totals math
  for both passes, empty container list still prints header + totals.
- **Clap parsing**: `Status` parses (`test_status_subcommand`), `ls` alias parses to
  `Status` (`test_ls_alias_status`), neither flag conflicts with existing top-level
  flags.

`fetch_basic` and `fetch_live` get light coverage by injecting raw subprocess outputs
into the parsers in unit tests; they're orchestration glue with no hard logic of their
own.

## Out of scope for v1

Listed explicitly so reviewers don't expect them and so the next iteration has a queue:

- `--json` output mode (structured serialization, schema)
- `--watch` mode (live refresh like `top`, alternate-screen-buffer logic)
- `--live` / `--quick` flag to control whether live pass runs (default: progressive)
- IMAGE / PROFILE column (would widen the table)
- `--running` / `--stopped` filter flags
- `--sort cpu` / `--sort uptime` sort flags
- Color coding by status (green=running, gray=stopped, red=stale)
- Tall-table support via alternate screen buffer (current limit: terminal-row count)
