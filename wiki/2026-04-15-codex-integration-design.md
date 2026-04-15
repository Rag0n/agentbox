# Codex integration

## Problem

agentbox only launches Claude Code. The config system was built for multiple
agents (`[cli.<name>]` sections already parse), but nothing wires a second
coding agent into the runtime. Users who want to run OpenAI's Codex CLI on
their projects must drop the agentbox isolation model.

## Solution

Add OpenAI Codex CLI as a peer to Claude Code: a dedicated `agentbox codex`
subcommand (with `agentbox claude` as its sibling), one image with both CLIs
installed, and matching behavior for mounts, config, setup, and docs. The
pre-1.0 window lets us simplify along the way. The always-on bypass flag
(`--dangerously-skip-permissions`) moves out of hardcoded Rust and into the
default config template, so agentbox becomes a launcher and the config
becomes the sole source of agent knobs.

## Non-goals

- **No `OPENAI_API_KEY` scaffolding.** Codex's file-based credential store
  (`~/.codex/auth.json`) is mounted into the container read/write. One login
  persists for host and container.
- **No automatic edits to `~/.codex/config.toml`.** Setup detects non-file
  credential backends and prints a hint. It never writes the user's codex
  config.
- **No codex-specific profile UX.** The existing `--profile` mechanism swaps
  Dockerfiles only, not agents.
- **No `codex login` invocation from agentbox.** Always a user action.
- **No backwards compatibility shim for the bypass-flag move.** Pre-1.0.
  Upgrade users add the flag to their `[cli.claude]` section or let
  `agentbox config init` regenerate the template.

## CLI surface

```bash
agentbox                        # runs default_agent (config); unchanged UX
agentbox "fix the tests"        # runs default_agent headlessly; unchanged
agentbox claude [task]          # explicit Claude
agentbox codex  [task]          # explicit Codex
agentbox shell [-- cmd...]      # unchanged
agentbox -- --model sonnet      # passthrough flags go to default_agent
agentbox codex "fix" -- -c model_reasoning_effort=high   # codex + passthrough + headless
```

Bare `agentbox` remains the primary workflow. The `claude` and `codex`
subcommands are clap subcommands. Each captures a trailing task argument via
`trailing_var_arg = true`, the same way the root command does today. Users
who never touch codex see no UX change.

## Config

```toml
# ~/.config/agentbox/config.toml, written by `agentbox config init` or setup

# default_agent = "claude"    # or "codex"; set by `agentbox setup`

[cli.claude]
flags = ["--dangerously-skip-permissions"]

[cli.codex]
flags = ["--dangerously-bypass-approvals-and-sandbox"]

[env]
# No OPENAI_API_KEY scaffolding. Codex uses ~/.codex/auth.json (mounted RW).
```

Resolution rule: if the user's config has `[cli.<agent>]` with a `flags`
array, agentbox uses that array verbatim when invoking that agent. If the
section is missing, agentbox passes no flags. No code-level defaults and no
merging. The template is the source of truth for good defaults.

**Flag placement for codex headless.** Codex's CLI structure puts the
subcommand first (`codex exec <prompt>`); per codex's docs, flags belong
after the subcommand when running headlessly, i.e.
`codex exec <flags> <prompt>`. Interactive runs use `codex <flags>` as
expected. Both forms come out of the same `CodingAgent::invocation` helper
(see Internal code model). Users should only put TUI-compatible flags in
`[cli.codex].flags` — for example, `--skip-git-repo-check` exists only
on `codex exec`, so interactive codex would reject it. We deliberately
leave it out of the default flags; users who only run headless codex on
non-git directories can add it themselves, accepting that it breaks
interactive for them.

`default_agent` is stored as a free-form string in the config
(`Option<String>`) and validated at runtime by a helper,
`Config::resolve_default_agent() -> Result<CodingAgent>`. Valid values are
`"claude"` and `"codex"`; anything else yields a runtime error with a
useful message. Making it string-typed means setup can repair an invalid
value interactively instead of the whole config failing to parse. Missing
value resolves to `CodingAgent::Claude`, the code-level fallback for bare
`agentbox` when the config file does not exist or leaves the key
commented out.

The full `agentbox config init` template, written verbatim:

```toml
# agentbox configuration

# Default agent used by bare `agentbox`. `agentbox setup` will write this
# for you; uncomment and edit to change it.
# default_agent = "claude"   # or "codex"

# Resources (auto-detected from host if not set)
# cpus = 4
# memory = "8G"

# Additional volumes
# volumes = [...]

# Override default Dockerfile
# dockerfile = "~/.config/agentbox/Dockerfile.custom"

# Environment variables passed into container
# [env]
# KEY = ""        # empty = inherit from host env
# KEY = "value"

# Named profiles
# [profiles.name]
# dockerfile = "/path/to/Dockerfile"

# Default flags for each coding agent.
# Replace to override. The "dangerously-*" flags bypass in-agent
# sandboxing because the container already isolates the agent.
[cli.claude]
flags = ["--dangerously-skip-permissions"]

[cli.codex]
flags = ["--dangerously-bypass-approvals-and-sandbox"]

# Host bridge
# [bridge]
# allowed_commands = [...]
```

## Image changes

### Dockerfile.default

Add a codex install step after Claude's install line, staying in the `user`
context. Both binaries land in `~/.local/bin`. The warm path reaches them
via `bash -lc`; the cold path is handled by an explicit `PATH` export at the
top of `entrypoint.sh` (see next section).

```dockerfile
RUN curl -fsSL https://claude.ai/install.sh | bash

# NEW — codex CLI from GitHub releases
RUN ARCH=$(dpkg --print-architecture) \
 && case "$ARCH" in \
      amd64) TRIPLE="x86_64-unknown-linux-musl"  ;; \
      arm64) TRIPLE="aarch64-unknown-linux-musl" ;; \
      *) echo "unsupported arch: $ARCH" >&2; exit 1 ;; \
    esac \
 && mkdir -p /home/user/.local/bin \
 && curl -fsSL "https://github.com/openai/codex/releases/latest/download/codex-${TRIPLE}.tar.gz" \
  | tar xz -C /tmp \
 && mv "/tmp/codex-${TRIPLE}" /home/user/.local/bin/codex \
 && chmod +x /home/user/.local/bin/codex
```

The tarball contains a single entry named `codex-<triple>` (per codex's
README). The `mv` renames it to plain `codex`. The triple is `musl`, not
`gnu`.

The existing `image::needs_build` logic already hashes `Dockerfile.default`,
so users' images rebuild automatically on next invocation. Custom
Dockerfiles that `FROM agentbox:default` inherit codex automatically.
Dockerfiles with a different base are the user's responsibility.

### Entrypoint dispatch

`resources/entrypoint.sh` generalizes its ad-hoc `[ "$1" = "--shell" ]`
branch to a first-arg dispatch. The hardcoded `claude
--dangerously-skip-permissions` line goes away. The flags come from
`cli_flags` via the Rust caller.

```bash
#!/bin/bash
set -e

export PATH="/home/user/.local/bin:$PATH"

# ... existing setup (.claude.json seed, HOSTEXEC symlinks, etc.) unchanged ...

AGENT="$1"; shift || true
case "$AGENT" in
  --claude) exec claude "$@" ;;
  --codex)  exec codex  "$@" ;;
  --shell)
    if [ $# -eq 0 ]; then exec bash -l
    else exec bash -lc 'exec "$@"' bash "$@"
    fi ;;
  *)
    echo "agentbox entrypoint: unknown agent '$AGENT'" >&2
    exit 2 ;;
esac
```

The `PATH` export makes the cold-start contract explicit: both `claude` and
`codex` resolve from `~/.local/bin` without relying on login-shell files
(`entrypoint.sh` runs under `#!/bin/bash`, not `bash -l`).

Cache invalidation for the entrypoint is already handled. The existing
shell-command design hashes `entrypoint.sh` alongside `Dockerfile.default`.

## Mounts and auth

Add `~/.codex` to the auto-mount list alongside the existing `~/.claude` and
`~/.claude.json` mounts in `main.rs:138`. Create the host directory if
missing (same pattern as `.claude` on line 133). Dedup logic in
`create_and_run` already skips duplicate dest paths, so no collisions.

Mount table:

| Host                | Container                 | Access    | Notes                                                         |
|---------------------|---------------------------|-----------|---------------------------------------------------------------|
| Current directory   | Same path                 | read/write |                                                              |
| `~/.claude`         | `/home/user/.claude`      | read/write |                                                              |
| `~/.claude.json`    | `/tmp/claude-seed.json`   | read-only  | Seed only; `entrypoint.sh` `jq`-merges into `~/.claude.json` |
| `~/.codex`          | `/home/user/.codex`       | read/write |                                                              |
| Additional volumes  | Configured path           | read/write |                                                              |

A note on the credential store: Codex's `cli_auth_credentials_store`
setting in `~/.codex/config.toml` controls whether auth goes into a file or
the OS keyring. The Linux container cannot reach the macOS Keychain, so
only the `"file"` backend works end-to-end through the mount. Effective
defaults have varied across codex versions, so setup requires users to set
the value explicitly to `"file"` rather than relying on whatever default
codex picks. Setup prints the one-line fix; it never edits the codex
config.

## Internal code model

### New module: `src/agent.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodingAgent { Claude, Codex }

impl CodingAgent {
    pub fn binary(&self) -> &'static str { /* "claude" | "codex" */ }
    pub fn entrypoint_arg(&self) -> &'static str { /* "--claude" | "--codex" */ }
    pub fn config_key(&self) -> &'static str { /* "claude" | "codex" */ }

    /// Build the args that follow the binary name.
    /// Per-agent CLI shape:
    ///   Claude interactive: <flags>
    ///   Claude headless:    <flags> -p <task>
    ///   Codex  interactive: <flags>
    ///   Codex  headless:    exec <flags> <task>
    pub fn invocation(&self, flags: &[String], task: Option<&str>) -> Vec<String> {
        match (self, task) {
            (CodingAgent::Claude, None)       => flags.to_vec(),
            (CodingAgent::Claude, Some(t))    => [flags, &["-p".into(), t.into()]].concat(),
            (CodingAgent::Codex,  None)       => flags.to_vec(),
            (CodingAgent::Codex,  Some(t))    => {
                let mut v = vec!["exec".to_string()];
                v.extend(flags.iter().cloned());
                v.push(t.to_string());
                v
            }
        }
    }
}

impl std::str::FromStr for CodingAgent { /* parses "claude" | "codex" */ }
```

The `invocation` helper replaces the separate `headless_args` method from
earlier drafts: it owns all agent-specific ordering so callers don't have
to know that codex puts flags after `exec`.

### `container.rs`

`RunMode::Claude` becomes `RunMode::Agent`:

```rust
pub enum RunMode {
    Agent {
        agent: CodingAgent,
        task: Option<String>,
        cli_flags: Vec<String>,
    },
    Shell { cmd: Vec<String> },
}
```

`RunOpts::to_run_args` (cold-start path) branches on the variant. For the
`Agent` variant it appends `agent.entrypoint_arg()` right after the image
name, then the tokens from `agent.invocation(&cli_flags, task.as_deref())`.
All per-agent ordering (e.g. codex's `exec` subcommand position) lives in
that helper.

`build_exec_args` (warm container path) branches the same way. The
`bash -lc` payload becomes:

```
<setup_prefix>; exec <binary> <escaped tokens from agent.invocation(...)>
```

The existing single-quote escaping (`'\''` substitution) runs over every
token that comes out of `invocation()`. Headless Codex example:

- `agent.invocation(["--dangerously-bypass-approvals-and-sandbox"], Some("fix tests"))`
- → `["exec", "--dangerously-bypass-approvals-and-sandbox", "fix tests"]`
- → `exec codex 'exec' '--dangerously-bypass-approvals-and-sandbox' 'fix tests'`

Headless Claude example:

- `agent.invocation(["--dangerously-skip-permissions"], Some("fix tests"))`
- → `["--dangerously-skip-permissions", "-p", "fix tests"]`
- → `exec claude '--dangerously-skip-permissions' '-p' 'fix tests'`

Interactive-vs-headless derivation updates to:

- `Agent { task: None, .. }` runs interactive (TTY)
- `Agent { task: Some(_), .. }` runs headless (no TTY)
- `Shell { cmd }`: existing logic

### `main.rs`

Add `Claude { task: Vec<String> }` and `Codex { task: Vec<String> }`
variants to the `Commands` enum. Each uses `trailing_var_arg = true`. Each
match arm constructs a `RunMode::Agent` with the right `CodingAgent` and
merges `cli_flags` from `config.cli_flags(agent.config_key())` plus
`passthrough_flags`.

The bare-command arm (`None` match) resolves the default via the
`Config::resolve_default_agent()` helper:

```rust
let agent = config.resolve_default_agent()?; // error on unknown string
let cli_flags = [
    config.cli_flags(agent.config_key()),
    &passthrough_flags,
].concat();
RunMode::Agent { agent, task, cli_flags }
```

### `config.rs`

Add `default_agent: Option<String>` to `Config` (plain string, no serde
enum). A new method performs runtime validation:

```rust
impl Config {
    pub fn resolve_default_agent(&self) -> Result<CodingAgent> {
        match self.default_agent.as_deref() {
            None => Ok(CodingAgent::Claude),
            Some(s) => s.parse::<CodingAgent>()
                .with_context(|| format!("invalid default_agent = {:?}; expected \"claude\" or \"codex\"", s)),
        }
    }
}
```

Update `init_template()` to produce the full template above. No
fallback-flags helper added. `cli_flags()` stays as-is and returns `[]` on
missing section.

Keeping `default_agent` as a string at the TOML-parse layer lets setup
repair an invalid value interactively instead of failing the whole config
parse.

## Setup wizard changes

New pipeline in `setup.rs:271`:

```
[1/6] Apple Container CLI        (existing)
[2/6] Container system running   (existing)
[3/6] Config file                (existing, writes updated template)
[4/6] Default agent              NEW
[5/6] Claude authentication      (existing, label clarified)
[6/6] Codex authentication       NEW
```

### Step 4, `check_default_agent`

Inspects the raw `default_agent` field. The runtime helper
`resolve_default_agent()` silently falls back to Claude on `None`, which
is wrong for setup — we want to prompt explicitly so the user picks.

- If `default_agent = Some(s)` and `s.parse::<CodingAgent>()` succeeds:
  returns `Ok`.
- Otherwise (key absent or unknown string): returns `AutoFix` whose
  `fix` closure calls a new `prompt_default_agent()` helper and writes
  the result via `ensure_default_agent_in_config`.

**Why AutoFix instead of Interactive.** The current orchestrator does not
increment `passed` after `Interactive` (`setup.rs:308`); it assumes the
user will re-run setup. For step 4 the prompt *is* the fix — once the user
picks, the config is correct and setup should count the step as passed.
AutoFix fits that shape: on successful `fix()` the orchestrator bumps
`passed` (`setup.rs:296`).

**Why a dedicated prompt, not `prompt_menu`.** The existing `prompt_menu`
at `setup.rs:250` treats invalid input (empty, out-of-range) as a no-op
that still returns `Ok(())`. If we reused it inside AutoFix, pressing
Enter or typing `3` would mark step 4 passed without writing
`default_agent`. A dedicated prompt loops until it gets `1` or `2`, then
returns the chosen `CodingAgent`:

```rust
fn prompt_default_agent() -> Result<CodingAgent> {
    loop {
        println!("\n        Which agent should be the default?");
        println!("          1) Claude");
        println!("          2) Codex");
        print!("        > ");
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        match input.trim() {
            "1" => return Ok(CodingAgent::Claude),
            "2" => return Ok(CodingAgent::Codex),
            _ => println!("        Invalid choice. Please enter 1 or 2."),
        }
    }
}
```

The fix closure then writes and confirms:

```rust
fix: Box::new(|| {
    let choice = prompt_default_agent()?;
    ensure_default_agent_in_config(&Config::config_path(), choice)?;
    println!("        ✓ Set default_agent = {:?}", choice.config_key());
    Ok(())
})
```

New helper `ensure_default_agent_in_config(path, agent)` mirrors the
existing `ensure_env_var_in_config` at `setup.rs:136`. It uncomments the
line if present, or inserts a new one if missing. Because the template
leaves `default_agent` commented out, step 4 always prompts on a fresh
setup — the user explicitly picks their default, and step 4 passes in
the same session.

### Step 6, `check_codex_authentication`

**Always returns `Ok`.** This step is purely informational — codex is
optional, so a Claude-only user should not be blocked by codex config
concerns. The step inspects `~/.codex/config.toml`'s
`cli_auth_credentials_store` and prints notes conditionally:

- Key absent, file missing, or value is `"keyring"` / `"auto"` /
  `"ephemeral"`: print the credential-store warning.

  > Heads-up: codex in the container cannot reach the macOS Keychain. For
  > auth to persist, add this line to `~/.codex/config.toml`:
  >
  >     cli_auth_credentials_store = "file"
  >
  > If you already signed in with a non-file backend, sign in again from
  > within `agentbox codex` (or run `codex login` on the Mac) after
  > changing the setting.

- Value is explicitly `"file"`: skip the warning.

Then always print the first-run sign-in hint:

> First time using codex? Run `agentbox codex`. On an unauthenticated
> container, codex's onboarding menu appears; pick **Sign in with Device
> Code**, then enter the code shown at the URL on your Mac.

Step 5 is renamed `"Claude authentication"` (was `"Authentication"`) to
match step 6.

### Step 5 short-circuits for codex-default users

`check_authentication` at `setup.rs:221` currently returns `Interactive`
when Claude credentials are missing. `Interactive` does not increment
`passed` (`setup.rs:308`), which blocks setup completion for a user who
has explicitly picked codex and doesn't care about Claude auth.

Fix: at the top of `check_authentication`, load the config and short-circuit:

```rust
if let Ok(config) = Config::load() {
    if matches!(config.resolve_default_agent(), Ok(CodingAgent::Codex)) {
        return Status::Ok;   // (with a printed note from the orchestrator)
    }
}
// existing Claude-auth logic unchanged
```

When short-circuited, setup prints a one-line info note: "Skipped —
`default_agent = codex`. Re-run `agentbox setup` after changing
`default_agent` to `claude` if you want Claude auth configured." Users
who want both agents can add Claude auth manually via the documented env
vars (README); the setup wizard remains opinionated around the default.

Setup never writes to `~/.codex/`.

## Documentation updates

### README.md

1. Top of file: replace "Currently supports Claude Code. More agents
   planned." with "Supported agents: Claude Code, OpenAI Codex."
2. Quick Start: add an `agentbox codex "task"` example and mention
   `agentbox claude` and `agentbox codex` as the explicit variants.
3. Passing Flags: add a codex example next to the Claude ones.
4. Configuration: update the example block to include `default_agent` and
   `[cli.codex]`.
5. Authentication: split into parallel Claude and Codex subsections. The
   Codex section covers (a) the `~/.codex` mount, (b) the required explicit
   `cli_auth_credentials_store = "file"` setting (one-line fix), and (c)
   first-run sign-in via codex's onboarding menu — pick **Sign in with
   Device Code**, enter the code on the Mac. No separate `codex login`
   command needed from agentbox.
6. What's Mounted: add the `~/.codex` row.
7. New "Breaking change (pre-1.0)" callout near the install instructions:

   > Upgrading from an earlier pre-1.0 build? agentbox no longer bakes
   > `--dangerously-skip-permissions` into the claude invocation. The flag
   > now lives in `[cli.claude] flags` in the config template.
   >
   > - If your `config.toml` has `[cli.claude] flags = [...]`, add
   >   `--dangerously-skip-permissions` to the list.
   > - If your `config.toml` has no `[cli.claude]` section, add one with
   >   `flags = ["--dangerously-skip-permissions"]`.
   > - If you have no `config.toml`, run `agentbox config init` (which
   >   errors if the file already exists) or `agentbox setup`.

## Testing

### Unit tests

`agent.rs`:

- `CodingAgent::from_str` round-trips `"claude"` and `"codex"`.
- `from_str` rejects unknown strings.
- `binary`, `entrypoint_arg`, `config_key` return the expected strings for
  each variant.
- `invocation` produces the right token sequence for each of the four
  (agent × interactive/headless) cases. In particular:
  - Claude headless places `-p <task>` *after* flags.
  - Codex headless places `exec` *before* flags and `<task>` *after* them.
  - Both interactive cases return just the flag tokens.

`config.rs`:

- `default_agent = "claude"` and `"codex"` parse and round-trip as strings.
- `default_agent = "invalid"` also parses (as a string) — validation lives
  in `resolve_default_agent()`.
- `resolve_default_agent()` returns `Ok(Claude)` on `None`,
  `Ok(Claude)/Ok(Codex)` on the corresponding strings, `Err(_)` with a
  useful message on anything else.
- `default_agent` omitted returns `None` at the TOML layer.
- `[cli.codex] flags = [...]` round-trips.
- `init_template()` contains both `[cli.claude]` and `[cli.codex]` with the
  bypass flags and leaves `default_agent` commented out.

`container.rs`:

- `RunOpts::to_run_args` with `Agent { agent: Codex, task: None, .. }`:
  `--codex` follows the image name, then flags. No `exec`.
- `RunOpts::to_run_args` with `Agent { agent: Codex, task: Some("fix"), .. }`:
  `--codex`, then `exec`, then flags, then `fix`. (Verifies flag placement
  after the subcommand.)
- `RunOpts::to_run_args` with `Agent { agent: Claude, task: Some("fix"), .. }`:
  `--claude`, then flags, then `-p fix`. (Regression check for existing
  behavior.)
- `build_exec_args` variants: warm-path Codex interactive, warm-path Codex
  headless (with `exec` correctly placed before flags), and Claude
  regression cases.
- Interactive-vs-TTY derivation correct for each Codex case.

`main.rs`:

- `agentbox codex` parses with no task.
- `agentbox codex "fix"` captures the task.
- `agentbox codex "fix" -- --model gpt-5` splits correctly. Passthrough
  flags reach the exec args.
- `agentbox claude "fix"` mirrors the codex parsing.
- Bare `agentbox` with `default_agent = "codex"` in config resolves to
  `CodingAgent::Codex`.

`setup.rs`:

- `check_default_agent` returns `Ok` when `default_agent` is set to a
  recognized string.
- Returns `AutoFix` when `default_agent` is `None` or an unknown string.
  (Unlike `resolve_default_agent()`, setup distinguishes "unset" from
  "fallback to claude".) The fix closure writes the picked value via
  `ensure_default_agent_in_config`, so a successful fix lets the
  orchestrator mark the step passed in the same session.
- `prompt_default_agent` is tested by injecting stdin: valid input (`1`,
  `2`) returns the matching variant on the first iteration; invalid
  input loops and accepts on the next valid entry. (A thin
  `read_line` trait or a closure parameter makes this mockable without
  touching real stdin.)
- `ensure_default_agent_in_config` writes (or uncomments) the key without
  disturbing existing keys or comments.
- `check_authentication` short-circuits to `Ok` when
  `resolve_default_agent()` returns `CodingAgent::Codex`. Existing Claude
  logic (`decide_auth`, file credentials, env var detection) is exercised
  unchanged only when the resolved default is Claude or when config
  loading fails (fail-safe to the Claude path).
- `check_codex_authentication`:
  - Always returns `Ok` (non-blocking for Claude-only users).
  - Prints the credential-store warning when `~/.codex/config.toml` is
    missing, lacks `cli_auth_credentials_store`, or sets it to a non-file
    backend.
  - Skips the warning when the key is explicitly `"file"`.
  - Always prints the device-code sign-in hint.

### Manual smoke tests

Documented in the implementation plan, not automated (Apple Container CLI
is not available in CI):

1. Fresh install: `agentbox setup` walks through all 6 steps. Config file
   written with the new template.
2. `agentbox codex` cold-start (no container): image rebuilds with codex
   installed, entrypoint dispatches to `--codex`, TUI launches.
3. `agentbox codex` warm-start (existing container): exec path dispatches
   to codex, TUI launches.
4. `agentbox codex "fix the tests"` headless: runs `codex exec "fix the tests"`
   inside (task passed as a single positional arg), prints output, exits
   with codex's exit code.
5. `agentbox claude` still works unchanged (regression).
6. Bare `agentbox` with `default_agent = "codex"` in config runs codex.
7. `agentbox -- -c model_reasoning_effort=high` with
   `default_agent = "codex"`: passthrough flags reach codex.
8. `agentbox shell` unaffected.
9. Device-code sign-in: on a container with no prior auth, run
   `agentbox codex`. Codex onboarding menu appears; pick **Sign in with
   Device Code**, visit the printed URL on the Mac, enter the code. Exit,
   run `agentbox codex` again. Auth persists because `~/.codex/auth.json`
   is mounted RW (and `cli_auth_credentials_store = "file"` is set).
10. Upgrade path: existing user with `[cli.claude] flags = ["--model",
    "sonnet"]` runs `agentbox`. Claude launches without the bypass flag,
    hits a permission prompt, the user sees the breaking-change note in the
    README, and adds the flag.
11. Codex-first setup: fresh install, step 4 picks Codex, step 5 reports
    "skipped — default_agent = codex", step 6 prints the credential-store
    warning + sign-in hint (both non-blocking), final line reads
    "6/6 checks passed. Ready."
12. Invalid menu input in step 4: typing `3` or pressing Enter prints
    "Invalid choice." and re-prompts; step 4 does NOT pass until a valid
    choice is entered.
