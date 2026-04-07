# agentbox setup — Design Document

## Problem

A first-time agentbox user has to do too many disconnected things before the tool works: install the Apple Container CLI, possibly start the container system, understand why their macOS keychain credentials aren't visible from inside the container, pick between `ANTHROPIC_API_KEY` / `CLAUDE_CODE_OAUTH_TOKEN` / in-container login, and discover that `agentbox config init` exists. Each step is documented, but the user has to assemble them into a workflow.

`agentbox setup` collapses this into one command: run it, fix whatever fails, run it again, and you're ready.

## Goal

One guided command that takes a fresh install from zero to a working state. Hybrid behaviour: silent on passing checks, interactive only where the user genuinely has to make a decision, safe to re-run as many times as the user wants.

## Scope

### In scope — four checks, in order

1. **Apple Container CLI installed.** Looks for the `container` binary on `PATH`. Not auto-fixable; prints the install URL on failure.
2. **Apple Container system running.** Runs `container system status` and parses the output. Auto-fixes (without a consent prompt) by running `container system start`.
3. **Config file exists.** Checks `~/.config/agentbox/config.toml`. Auto-fixes (without a consent prompt) by writing the full commented template from `Config::init_template()`. Never touches an existing file.
4. **Authentication reachable.** Passes if any of:
    - Config has a literal non-empty `[env] ANTHROPIC_API_KEY = "..."` or `[env] CLAUDE_CODE_OAUTH_TOKEN = "..."`.
    - Config has `[env] KEY = ""` (inherit) for one of those keys **and** the host shell env has a non-empty value for it.
    - `~/.claude/.credentials.json` exists on host and is non-empty. (Claude Code on Linux writes its OAuth token there after `/login`; because we mount `~/.claude` into the container, a one-time interactive login from inside the container persists this file and keeps the user authenticated across sessions.)

   On failure, displays a short explanation of why keychain credentials don't work from the container, then an interactive 4-option menu:

    1. Log in interactively inside the container (recommended for Claude Pro/Max).
    2. Use an API key (`ANTHROPIC_API_KEY`).
    3. Use a long-lived OAuth token (`CLAUDE_CODE_OAUTH_TOKEN`).
    4. Skip for now.

   Branches 1 and 4 print next-step text only. Branches 2 and 3 print the exact `export KEY=...` command the user should run, then prompt once: "Add `KEY = ""` under `[env]` in your config automatically? [Y/n]". On yes, the `[env]` line is added idempotently; on no, nothing is written. In all branches, setup never captures or stores the secret value — the actual authentication happens out of band, and the user re-runs `agentbox setup` to confirm.

### Out of scope

- **macOS version / Apple Silicon check.** Hardware failures from the container CLI are loud and rare; a custom check adds noise.
- **Host `claude` CLI check.** Only relevant for the OAuth-token branch; handled inline in that branch's text.
- **First image build as part of setup.** Slow (network + buildkit), and it happens lazily on the first real `agentbox` invocation anyway.
- **Standalone TOML syntax validation check.** `Config::load()` already parses the file whenever a real command runs; the auth check surfaces parse errors naturally via `Status::Errored`.
- **`doctor` alias, `--check-only`, `--fix-only`, `--json` output, spinners, colourisation.** Hybrid is the only mode; output is plain text.
- **Refactor of the existing `check_prerequisites()`.** Left as-is on purpose; see "Relation to existing code" below.
- **Writing secret values to shell profiles (`~/.zshrc`, `~/.bashrc`).** Never. Setup only prints the command; the user runs it themselves in the shell of their choice.

## User flow

### Happy re-run (everything already green)

```
$ agentbox setup
  [1/4] Apple Container CLI ......... ✓
  [2/4] Container system running .... ✓
  [3/4] Config file ................. ✓
  [4/4] Authentication .............. ✓

Ready. Run `agentbox` to start coding.
```

### Fresh install, user picks the in-container login branch

```
$ agentbox setup
  [1/4] Apple Container CLI ......... ✓
  [2/4] Container system running .... ✗
        Container system is not running. Starting it...
        ✓
  [3/4] Config file ................. ✗
        Creating ~/.config/agentbox/config.toml from the default template...
        ✓
  [4/4] Authentication .............. ✗

        macOS Keychain isn't reachable from the Linux container, so
        Claude Code needs credentials via env var, or a one-time login
        from inside the container (the token persists under ~/.claude).

        How do you want to authenticate?
          1) Log in interactively inside the container (recommended for Pro/Max)
          2) Use an API key (ANTHROPIC_API_KEY)
          3) Use a long-lived OAuth token (CLAUDE_CODE_OAUTH_TOKEN)
          4) Skip for now
        > 1

        Next step: run `agentbox`, then inside Claude type `/login`.
        Your token will be saved under ~/.claude and persist across sessions.

3 of 4 checks passed. Re-run `agentbox setup` after completing the step above.
```

### Branch 2 (API key)

```
        > 2
        Run this in your shell (and add it to ~/.zshrc / ~/.bashrc for next time):

            export ANTHROPIC_API_KEY="sk-..."

        Add `ANTHROPIC_API_KEY = ""` under [env] in your config automatically? [Y/n] y
        ✓ Updated ~/.config/agentbox/config.toml
        Then re-run `agentbox setup` in a new shell to confirm.
```

### Branch 3 (OAuth token)

Same shape as branch 2, but the `export` command is preceded by instructions to run `claude setup-token` on the host first (with a note that this requires the host `claude` CLI).

### Branch 4 (skip)

Prints a one-liner acknowledging the skip and reminding the user they can re-run setup at any time. The authentication check stays ✗.

## Architecture

### New module: `src/setup.rs`

All setup logic lives in a new module. No trait gymnastics — pure-ish check functions that return a status value, plus a thin orchestrator that iterates them and renders output.

```rust
pub enum Status {
    Ok,
    // Auto-fixable: orchestrator runs `fix` directly, no consent prompt.
    AutoFix { explanation: String, fix: Box<dyn FnOnce() -> Result<()>> },
    // User must act out of band; we print instructions and carry on.
    Manual { explanation: String, next_steps: String },
    // Menu of options; currently only used by the auth check.
    Interactive { explanation: String, menu: Vec<MenuOption> },
    // A check itself errored (e.g. TOML parse failure); report and continue.
    Errored(anyhow::Error),
}

pub struct MenuOption {
    pub label: &'static str,
    pub action: Box<dyn FnOnce() -> Result<()>>,
}
```

Each check is a free function `fn check_xxx() -> Status`. No trait, no dyn registry, no DI framework.

The `Interactive` status is deliberately a "pending" state: when the auth menu runs, no option is allowed to claim the check is fixed. Real authentication always happens out of band (user exports an env var in a new shell, or logs in inside the container), so the menu's only job is to steer the user and optionally touch the config. The user then re-runs `agentbox setup`, and the check either passes or the menu reappears.

### The four checks

1. **`check_container_cli()`** — `Command::new("container").arg("--version").output()`. On success → `Ok`. On `io::ErrorKind::NotFound` → `Manual` with the install URL. Any other error → `Errored`.

2. **`check_container_system()`** — runs `container system status`, scans stdout for a `status running` line (same parsing shape as the existing `main.rs:202`). If not running → `AutoFix { fix: run "container system start" }`.

3. **`check_config_file()`** — `Config::config_path().exists()`. If false → `AutoFix { fix: create parent dir and write Config::init_template() }`. If true → `Ok`. Never touches an existing file.

4. **`check_authentication()`** — loads config (via `Config::load()`; parse failures become `Status::Errored`) and evaluates the three conditions from the Scope section. On failure → `Interactive` with the 4-option menu. Each menu option's `action` closure handles its own side effects (printing next-step text, optionally calling `ensure_env_var_in_config`). Real authentication always happens out of band, so the menu itself never counts as "passing" the check within the current setup run.

### Orchestrator: `run_setup()`

```rust
pub fn run_setup() -> Result<()> {
    let checks: &[(&str, fn() -> Status)] = &[
        ("Apple Container CLI",      check_container_cli),
        ("Container system running", check_container_system),
        ("Config file",              check_config_file),
        ("Authentication",           check_authentication),
    ];
    let mut passed = 0;
    for (i, (name, f)) in checks.iter().enumerate() {
        print!("  [{}/{}] {:<30} ", i + 1, checks.len(), name);
        match f() {
            Status::Ok => { println!("✓"); passed += 1; }
            Status::AutoFix { explanation, fix } => {
                println!("✗");
                if !explanation.is_empty() { print_indented(&explanation); }
                match fix() {
                    Ok(()) => { println!("        ✓"); passed += 1; }
                    Err(e) => println!("        failed: {e:#}"),
                }
            }
            Status::Manual { explanation, next_steps } => {
                println!("✗");
                print_indented(&explanation);
                print_indented(&next_steps);
            }
            Status::Interactive { explanation, menu } => {
                println!("✗");
                print_indented(&explanation);
                prompt_menu(&menu)?; // menu never increments `passed`
            }
            Status::Errored(e) => println!("error: {e:#}"),
        }
    }
    if passed == checks.len() {
        println!("\nReady. Run `agentbox` to start coding.");
    } else {
        println!(
            "\n{}/{} checks passed. Re-run `agentbox setup` after completing the steps above.",
            passed, checks.len()
        );
    }
    Ok(())
}
```

### TOML editing helper: `ensure_env_var_in_config(key: &str) -> Result<()>`

Used only by auth branches 2 and 3 after the "add to config?" prompt returns yes. Uses the `toml_edit` crate (new dependency) so we preserve any comments and formatting the user has in their existing config.

```rust
use toml_edit::{DocumentMut, value, table};

fn ensure_env_var_in_config(key: &str) -> Result<()> {
    let path = Config::config_path();
    let content = std::fs::read_to_string(&path)?;
    let mut doc: DocumentMut = content.parse()?;
    let env_tbl = match doc.entry("env").or_insert(table()).as_table_mut() {
        Some(t) => t,
        None => anyhow::bail!("'env' in config is not a table"),
    };
    if env_tbl.contains_key(key) {
        return Ok(()); // idempotent: never overwrite an existing entry
    }
    env_tbl.insert(key, value(""));
    std::fs::write(&path, doc.to_string())?;
    Ok(())
}
```

Properties:

- **Idempotent.** If the key already exists under `[env]`, the function is a no-op — no overwrite, no diff.
- **Comment-preserving.** `toml_edit` is specifically designed for this; the user's comments and formatting in unrelated parts of the file are untouched.
- **Safe for re-runs.** Running setup again after this function has already written will be a no-op.

### CLI wiring (`src/main.rs`)

Add a `Setup` variant to the `Commands` enum and route it to `setup::run_setup()`. Approximately five lines of additions in `main.rs`. Also update the error message in `check_prerequisites()` that fires when the `container` binary is missing, so it recommends `agentbox setup` as the next step.

### New dependency

- `toml_edit` — used solely by `ensure_env_var_in_config`. Widely used, maintained by the same author as the `toml` crate, and the canonical choice for comment-preserving TOML edits.

## Relation to existing `check_prerequisites()`

`check_prerequisites()` in `src/main.rs:194` runs on every agentbox command as a silent gate. It already does two things that overlap with the new setup checks: it verifies the `container` binary exists (errors if not) and it silently starts the container system if it isn't running.

We leave `check_prerequisites()` alone in this change. The overlap is about fifteen lines of shelling out to `container`, and consolidating it would widen the PR's surface area for little gain. The only update to `check_prerequisites()` is a text-only change to its "not installed" error message, so it points users to `agentbox setup`.

## Existing-code touches

The change is surgical:

1. **`src/main.rs`** — add `Setup` variant to the `Commands` enum, route it to `setup::run_setup()`, update the "container CLI not installed" error text in `check_prerequisites()`.
2. **`src/setup.rs`** — new module containing all the check logic, the orchestrator, and `ensure_env_var_in_config`.
3. **`Cargo.toml`** — add `toml_edit` dependency.
4. **`README.md`** — add `agentbox setup` to the Quick Start section as the recommended first command; update the Authentication section so it points to `agentbox setup` as the starting point.

No changes to `src/config.rs`, `src/image.rs`, `src/container.rs`, `src/bridge/*`, `src/git.rs`, or `src/hostexec.rs`.

## Testing

### Unit tests

- **`ensure_env_var_in_config`** — exhaustive table of cases:
    - empty config → creates `[env]` with the key.
    - config with `[env]` but no such key → adds the key, preserves other entries.
    - config with `[env]` containing the key with a literal non-empty value → no change (idempotency).
    - config with `[env]` containing the key as an empty string → no change.
    - config with comments around `[env]` and other sections → comments preserved, only `[env]` table is touched.
    - config where `env` is not a table (e.g. `env = "oops"`) → returns error, file unchanged.
- **Authentication decision logic** — extracted as a pure function `fn decide_auth(config: &Config, host_env: &dyn Fn(&str) -> Option<String>, credentials_exists: bool) -> bool`. Test all meaningful permutations:
    - no env vars, no credentials file → false.
    - literal non-empty in config → true.
    - empty in config + host env set → true.
    - empty in config + host env unset → false.
    - credentials file exists → true, regardless of env vars.
    - credentials file exists but empty → false.
- **`check_config_file`** — override `XDG_CONFIG_HOME` to a `tempdir`, exercise the missing-file and present-file branches.
- **`parse_system_status(stdout: &str) -> bool`** — extracted parser for `container system status` output, tested directly with fixture strings. This is the only piece of `check_container_system` that's worth isolating.

### Not unit-tested

- The `run_setup()` orchestrator end-to-end. Output rendering and interactive prompting are fiddly to mock and low-value to test once the primitives are covered.
- `Command::new("container")` process spawns. Consistent with how the existing `check_prerequisites()` is tested today (it isn't).
- The interactive menu helper. Thin wrapper around `std::io::stdin().read_line()`.

### Manual smoke test checklist

(Will be carried into the implementation plan.)

1. Clean state, no container CLI installed — runs, shows ✗ with install URL, exits.
2. Container CLI present, system stopped — runs, auto-starts, shows ✓.
3. Config missing — writes the full template, ✓ on re-read.
4. Config present and customised (with comments) + auth menu branch 2 picked + "add to config" accepted — `[env]` section gains the key without disturbing comments or other fields.
5. Already-green state — all ✓, exits almost instantly.
6. `.credentials.json` present in `~/.claude/` with no env vars set — auth check passes.

## Edge cases

- **Container CLI not installed** — check 1 fails with a `Manual` fix; checks 2, 3, 4 still run (they don't depend on the CLI being installed, though check 2 will of course also fail). Output is honest: "[1/4] ✗ install the container CLI first".
- **Invalid TOML in existing config** — `check_authentication` calls `Config::load()`, which returns a parse error. We render the check as `Status::Errored` with the error message. Setup does not try to "fix" an invalid config file.
- **`~/.claude/.credentials.json` exists but is empty** — treated as "not authenticated". The on-disk credentials branch requires a non-empty file.
- **User picks auth branch 2 or 3 but declines the "add to config" prompt** — we still print the `export` command and exit. No config mutation.
- **`Ctrl+C` during the auth menu** — we bail cleanly; nothing has been written yet, so state is fine.
- **Concurrent edit to the config file between read and write in `ensure_env_var_in_config`** — we accept last-writer-wins. Setup is a human-driven command; file locking is not worth engineering.
- **Setup run inside a container** — not detected or special-cased. The container CLI check will fail naturally.

## Open questions

None. All design decisions have been confirmed through brainstorming.
