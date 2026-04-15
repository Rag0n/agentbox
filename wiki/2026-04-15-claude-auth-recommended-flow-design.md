# Claude Code auth: surface in-container login as the recommended flow — Design Document

## Baseline

This spec is anchored to the worktree `feature/claude-auth-flow` branched from `main @ cea459f` ("Simplify feature/live-status after review"). At this commit, `src/setup.rs` has four checks (CLI, system, config file, authentication — no codex auth), a single `check_authentication()` function (no `_with_config` split, no codex-default short-circuit), and a `MenuOption` struct with only `label` and `action`. `README.md` has a flat `## Authentication` section with no `### Claude Code` / `### OpenAI Codex` subsections.

Any codex-integration work happening in parallel on another branch is out of scope and not referenced below. If codex work merges into main before this lands, the two changes touch orthogonal parts of `setup.rs` (codex splits `check_authentication`; this change touches `build_auth_menu`, `MenuOption`, `prompt_menu`, `AUTH_EXPLANATION`) — conflicts should be mechanical.

## Problem

`agentbox setup` and the README disagree about which Claude Code auth path to recommend.

The setup menu lists four options with `(recommended for Pro/Max)` as a parenthetical on Option 1 ("Log in interactively inside the container"). At a glance, four equally-numbered lines look like peers; the trailing "(recommended)" is a weak signal that fails during fast scanning.

The README is worse. The intro in `## Authentication` promises "three methods" but then lists only two (`**Option A: API key**`, `**Option B: OAuth token (Pro/Max subscription)**`). The in-container login — the simplest path, and the one that writes `~/.claude/.credentials.json` for persistent auth — is mentioned only in a passing sentence in the section preamble, never as a structured option.

The underlying mechanism already works: `decide_auth` in `src/setup.rs:117` treats a non-empty `~/.claude/.credentials.json` as passing auth, and `~/.claude` is mounted into the container so the login persists. The gap is purely surfacing and recommendation.

## Goal

Both surfaces (setup menu and README) should make the in-container login the clearly primary path for Claude subscribers, with `CLAUDE_CODE_OAUTH_TOKEN` as the explicit alternative for headless/CI contexts and `ANTHROPIC_API_KEY` as the option for users who bill via the Anthropic Console instead of a subscription.

Users who don't have a Claude Pro/Max subscription should be able to tell at a glance that the in-container login path isn't for them. Users who do have one should walk away understanding that the login is done *once* and persists.

## Scope

### In scope

1. **Setup menu rework** (`src/setup.rs`):
    - New section-header visual structure (`Recommended:` / `Alternatives:`).
    - Relabel Option 1 from `"Log in interactively inside the container (recommended for Pro/Max)"` to `"Log in once inside the container (Pro/Max subscription)"`. Swaps "recommended for" framing for a constraint-style qualifier, and adds "once" to reinforce the persist-after-one-login behavior.
    - Swap positions of the OAuth-token option (was 3, becomes 2) and the API-key option (was 2, becomes 3). OAuth before API key because OAuth is also subscription-based; API key is specifically for Console billing.
    - Refine the explanation paragraph that prints above the menu, leading with the in-container login path.
    - Refine the "next step" text printed after Option 1 to reinforce the one-time nature.
2. **README rewrite** of the `## Authentication` section:
    - Fix the factual bug where "three methods" are promised but only two are listed.
    - Promote the in-container login to **Option A (recommended; Pro/Max)**.
    - Move OAuth token to **Option B** with "Pro/Max; best for headless/CI" framing.
    - Move API key to **Option C** with "Console API billing" framing.
    - Rewrite the closing paragraph, since the previous claim that "only the secret token needs to be passed explicitly" is option-specific (true for B/C, false for A).
3. **`MenuOption` grows one optional field** (`header_before`) to support section headers in a minimally-invasive way.
4. **Extract menu rendering into a testable helper** (`render_menu`) so rendering behavior is covered by unit tests.
5. **Two new unit tests**:
    - One asserting `build_auth_menu()` produces four options with the right labels and `header_before` values.
    - One asserting `render_menu()` produces the exact expected output string (headers, indentation, numbering).

### Out of scope

- **Any change to auth detection logic.** `decide_auth` already treats `~/.claude/.credentials.json` as passing auth. The menu is UX; detection is unchanged.
- **Auto-launching `agentbox claude /login` from inside `setup`.** Tempting but crosses a boundary: setup is a declarative diagnostic, not an orchestrator. Mixing them stacks failure modes (image-build failures, browser flow quirks) inside `setup` and muddies the error surface. The existing re-run pattern ("do X out-of-band, then re-run `agentbox setup` to confirm") is consistent with Options B/C and is worth preserving.
- **Dropping the API-key option.** `ANTHROPIC_API_KEY` serves users on Console pay-as-you-go billing who don't have a Claude subscription. Demoting to Option C is enough.
- **Generalizing `prompt_menu` into a section-aware UI framework.** This is the only interactive menu in the codebase; the minimum viable change is one `Option<&'static str>` field on `MenuOption`.
- **Codex authentication.** No codex-auth check exists in this worktree's baseline; if the codex-integration branch lands first, that work has its own auth UX to deliver.

## User flow

### Setup menu — before

```
  [4/4] Authentication             ✗

        macOS Keychain isn't reachable from the Linux container, so
        Claude Code needs credentials via env var, or a one-time login
        from inside the container (the token persists under ~/.claude).

          1) Log in interactively inside the container (recommended for Pro/Max)
          2) Use an API key (ANTHROPIC_API_KEY)
          3) Use a long-lived OAuth token (CLAUDE_CODE_OAUTH_TOKEN)
          4) Skip for now
        > _
```

### Setup menu — after

```
  [4/4] Authentication             ✗

        macOS Keychain isn't reachable from the Linux container.
        Claude Code needs either a one-time login from inside the container
        (Pro/Max subscribers; persists under ~/.claude) or credentials via env var.

          Recommended:
            1) Log in once inside the container (Pro/Max subscription)

          Alternatives:
            2) Use a long-lived OAuth token (CLAUDE_CODE_OAUTH_TOKEN)
            3) Use an API key (ANTHROPIC_API_KEY)
            4) Skip for now
        > _
```

### Post-Option-1 "next step" text — before

```
Next step: run `agentbox`, then inside Claude type `/login`.
Your token will be saved under ~/.claude and persist across sessions.
```

### Post-Option-1 "next step" text — after

```
Next step: run `agentbox`, then type `/login` inside Claude.
The credentials persist under ~/.claude — you only do this once.
```

Options 2, 3, and 4's post-selection text remains unchanged (they keep their current `export`-command printing and config-update prompts).

## Architecture

### `MenuOption` grows one optional field

```rust
pub struct MenuOption {
    pub label: &'static str,
    pub action: Box<dyn FnOnce() -> Result<()>>,
    pub header_before: Option<&'static str>,  // NEW
}
```

`header_before` carries an optional section label (`"Recommended:"`, `"Alternatives:"`) that rendering prints immediately above the option's numbered line.

**Alternative considered:** a richer `MenuItem` enum with `Section(&str)` and `Option(MenuOption)` variants. Rejected — rendering and `prompt_menu` would need separate numbering and selection-indexing logic, and this is the only interactive menu in the entire codebase. The colocated field is honest about that.

### Render helper extracted from `prompt_menu`

`prompt_menu` today bundles rendering and input handling in one function (`src/setup.rs:250-269`). To make rendering unit-testable, split the rendering out:

```rust
fn render_menu<W: Write>(menu: &[MenuOption], out: &mut W) -> std::io::Result<()> {
    for (i, option) in menu.iter().enumerate() {
        if let Some(header) = option.header_before {
            writeln!(out)?;
            writeln!(out, "          {}", header)?;
        }
        writeln!(out, "            {}) {}", i + 1, option.label)?;
    }
    Ok(())
}
```

Notes:
- Headers print at 10-space indent (today's option indent).
- Options print at 12-space indent — two spaces deeper, so they visually nest under the nearest preceding header. Increases indent vs. today's 10 spaces; longest post-change line is 70 cols (Option 1: 12-space indent + `"1) "` prefix + 55-char label).
- A blank line precedes each header (`writeln!(out)?`), separating sections visually.
- Numbering continues to use `i + 1`, ignoring headers. This matches the selection logic in `prompt_menu`, which reads the user's `"1".."4"` input and indexes into the menu directly.

`prompt_menu` then calls `render_menu(&menu, &mut std::io::stdout())?` instead of printing inline. The rest of `prompt_menu` (prompt `> `, read line, parse, dispatch) is unchanged.

### `build_auth_menu()` rewrite

Four `MenuOption` entries, in the new order, with the new labels, the new headers, and the new post-selection text for Option 1:

- **1** — `label: "Log in once inside the container (Pro/Max subscription)"`, `header_before: Some("Recommended:")`, action prints the new next-step text.
- **2** — `label: "Use a long-lived OAuth token (CLAUDE_CODE_OAUTH_TOKEN)"`, `header_before: Some("Alternatives:")`, action body is today's OAuth-token closure verbatim.
- **3** — `label: "Use an API key (ANTHROPIC_API_KEY)"`, `header_before: None`, action body is today's API-key closure verbatim.
- **4** — `label: "Skip for now"`, `header_before: None`, action unchanged.

### `AUTH_EXPLANATION` constant is refined

```rust
const AUTH_EXPLANATION: &str = "macOS Keychain isn't reachable from the Linux container.\n\
Claude Code needs either a one-time login from inside the container\n\
(Pro/Max subscribers; persists under ~/.claude) or credentials via env var.";
```

Leads with the in-container login to mirror the menu ordering, and names the Pro/Max prerequisite at the paragraph level too — a Console-only user reading this knows immediately that the primary path isn't for them.

### What does not change

- `decide_auth` — detection logic is already correct for all three paths.
- `check_authentication` — signature and control flow unchanged; only the `Interactive` payload differs.
- `ensure_env_var_in_config`, `prompt_and_add_env_var` — still called by Options 2 and 3's action closures.
- `AUTH_KEYS`, `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN` constants.
- `parse_system_status`, `check_container_cli`, `check_container_system`, `check_config_file`, `run_setup`.
- Any other module in the crate.

## README restructure

Target: the `## Authentication` section of `README.md` (currently lines 150-190).

### New content (full replacement of the section)

````markdown
## Authentication

macOS Keychain isn't reachable from inside the Linux container. Claude Code needs either a one-time login from inside the container or credentials passed via environment variable.

**Easiest approach: Run `agentbox setup`** — it will guide you through the options.

Three methods, in order of recommendation:

**Option A (recommended, Pro/Max subscription): Log in once inside the container.**

Run `agentbox`, type `/login` inside Claude, and complete the browser flow. Claude Code writes `~/.claude/.credentials.json`. Because agentbox mounts `~/.claude` into the container, the login persists across all future sessions — you only do this once.

Nothing to configure ahead of time. This is the simplest path for Pro/Max subscribers.

**Option B (Pro/Max subscription): Long-lived OAuth token (`CLAUDE_CODE_OAUTH_TOKEN`).**

Best when an interactive login isn't practical — headless machines, CI, or automated provisioning.

1. Generate a token on the host:

   ```bash
   claude setup-token
   ```

2. Add it to your shell profile (`~/.zshrc`, `~/.bashrc`, etc.):

   ```bash
   export CLAUDE_CODE_OAUTH_TOKEN="your-token-here"
   ```

3. Tell agentbox to pass it into the container:

   ```toml
   # ~/.config/agentbox/config.toml
   [env]
   CLAUDE_CODE_OAUTH_TOKEN = ""  # empty = inherit from host env
   ```

**Option C (Console API billing): API key (`ANTHROPIC_API_KEY`).**

Use this if you bill via the Anthropic Console (pay-as-you-go) rather than a Claude subscription.

1. Export the key in your shell profile (`~/.zshrc`, `~/.bashrc`, etc.):

   ```bash
   export ANTHROPIC_API_KEY="sk-..."
   ```

2. Tell agentbox to pass it into the container:

   ```toml
   # ~/.config/agentbox/config.toml
   [env]
   ANTHROPIC_API_KEY = ""  # empty = inherit from host env
   ```

Regardless of which option you pick, `~/.claude` is mounted into the container, so project settings, CLAUDE.md trust, and preferences carry over automatically.
````

### Diff summary vs. current README

- **Intro sentence** lightly reworded (`isn't reachable` vs `isn't accessible`), preserves the two-path framing.
- **Transition line** `"Alternatively, here are the three methods:"` → `"Three methods, in order of recommendation:"`. Fixes the factual bug (three promised, two listed) and makes recommendation order explicit.
- **Option A (new)** — introduces the in-container login path with the `~/.claude/.credentials.json` and "once" framing. Header notes Pro/Max eligibility inline.
- **Option B (was: Option B, OAuth)** — near-verbatim, with an added one-line "when to use this" lead-in and the mid-list `"This prints an export command"` prose dropped as redundant. The `(Pro/Max subscription)` qualifier moves from after the option name to after the option letter, to parallel Option A's `(recommended, Pro/Max subscription)` framing.
- **Option C (was: Option A, API key)** — moved to last, with "Console API billing" framing and a one-line "when to use this" that calls out the Console vs. subscription distinction.
- **Closing paragraph** — rewritten. The previous claim that "only the secret token needs to be passed explicitly" is option-specific (true for B/C, false for A). Replaced with a cleaner statement about the `~/.claude` mount that holds across all three options.

### What does not change in the README

- The `## Authentication` heading itself and everything outside the section (breaking-change note, Configuration, Mounts table, etc.).

## Testing

### New unit test: `test_build_auth_menu_structure`

Constructs the menu via `build_auth_menu()` and asserts:

- Exactly four options.
- `menu[0].label == "Log in once inside the container (Pro/Max subscription)"` and `menu[0].header_before == Some("Recommended:")`.
- `menu[1].label == "Use a long-lived OAuth token (CLAUDE_CODE_OAUTH_TOKEN)"` and `menu[1].header_before == Some("Alternatives:")`.
- `menu[2].label == "Use an API key (ANTHROPIC_API_KEY)"` and `menu[2].header_before == None`.
- `menu[3].label == "Skip for now"` and `menu[3].header_before == None`.

Catches: swapped ordering, typo in labels, missing or extra headers.

### New unit test: `test_render_menu_matches_expected_layout`

Calls `render_menu(&build_auth_menu(), &mut buf)`, then asserts the full `String::from_utf8(buf)` equals this expected layout:

```

          Recommended:
            1) Log in once inside the container (Pro/Max subscription)

          Alternatives:
            2) Use a long-lived OAuth token (CLAUDE_CODE_OAUTH_TOKEN)
            3) Use an API key (ANTHROPIC_API_KEY)
            4) Skip for now
```

(Expected string starts with a newline from the leading blank-before-header, ends with a newline from the final `writeln!`.)

Catches:
- Wrong indentation (headers at 10, options at 12).
- Missing or duplicated blank lines.
- Wrong numbering (e.g. if headers were accidentally counted).
- Any ordering change to labels.

Keeps rendered numbering aligned with the current (unchanged) selection logic in `prompt_menu`. Does *not* exercise `prompt_menu`'s input-dispatch path, so if selection logic later changes (e.g. to `i` instead of `i + 1`), this test alone would not catch the resulting render/selection mismatch — a dispatch-level test would be needed then.

This is the test that meets Codex's Finding 3 concern about rendering not being covered.

### Existing tests that stay green unchanged

- All `decide_auth_*` tests in `src/setup.rs`.
- `ensure_env_var_*` tests (the OAuth/API-key action closures still call `ensure_env_var_in_config` with unchanged keys).
- `parse_system_status_*` tests.
- All 233 tests currently passing in `cargo test`.

### Manual smoke test

1. On a clean state (no `~/.claude/.credentials.json`, no `ANTHROPIC_API_KEY`/`CLAUDE_CODE_OAUTH_TOKEN` in env or config), run `agentbox setup`. Verify:
    - `Recommended:` appears above Option 1.
    - `Alternatives:` appears above Option 2.
    - Options are ordered: in-container login, OAuth token, API key, skip.
    - Option 1's label contains "(Pro/Max subscription)".
    - Entering `1` prints the updated "next step" text with "you only do this once".
2. With the same clean state, re-run and pick Option 2. Verify the OAuth-token flow still prints `claude setup-token` and the `export` command, and still prompts to add `CLAUDE_CODE_OAUTH_TOKEN = ""` under `[env]`.
3. Re-run and pick Option 3. Verify the API-key flow still prints the `export` command and prompts for config update.

## Edge cases

- **A user with `~/.claude/.credentials.json` already present** — the auth check passes (`decide_auth` returns true), the menu is never shown, and the changes here have no user-visible effect. Correct behavior, unchanged.
- **Console-only user (no Pro/Max)** — sees `(Pro/Max subscription)` both in the explanation paragraph and on Option 1's label; also sees Option C explicitly called out for Console billing in the README. Should self-route to Option 3 in the menu / Option C in the README.
- **Narrow terminal** — the longest option label under the change is Option 1 (`"Log in once inside the container (Pro/Max subscription)"`, 55 characters). With the 3-character numbering prefix and the 12-space indent, the widest menu line is 70 columns. Fits in 80. No dynamic wrapping logic.
- **Selection by number still works the same way** — headers are print-only, never selectable; `prompt_menu`'s parsing of `"1".."4"` is unchanged.

## Open questions

None. All decisions confirmed through brainstorming.
