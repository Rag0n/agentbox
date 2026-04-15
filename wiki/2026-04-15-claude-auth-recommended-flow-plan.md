# Claude Code auth: recommended-flow restructuring — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restructure the `agentbox setup` Claude-auth menu and the README `## Authentication` section so the in-container login is the clearly recommended path (with Pro/Max qualifier visible to Console-only users), OAuth token is the alternative for headless/CI, and API key is the path for Console billing.

**Architecture:** Additive change to `MenuOption` (one optional field), extract menu rendering from `prompt_menu` into a testable helper, rewrite `build_auth_menu`, replace README auth section. No changes to auth detection logic (`decide_auth`). No new dependencies. No new modules or files.

**Tech Stack:** Rust (std), `cargo test`.

**Spec reference:** `wiki/2026-04-15-claude-auth-recommended-flow-design.md`.

---

## Task 1: Add `header_before` field to `MenuOption`

**Files:**
- Modify: `src/setup.rs` (struct definition near line 42; all construction sites in `build_auth_menu` lines 179-214)

This task is a refactor with no behavior change — every existing call site receives `header_before: None`, so output is identical to today. No new test; existing tests must continue to pass.

- [ ] **Step 1: Add `header_before` field to the struct**

Modify `src/setup.rs` at the `MenuOption` struct definition (currently around line 42):

```rust
pub struct MenuOption {
    pub label: &'static str,
    pub action: Box<dyn FnOnce() -> Result<()>>,
    pub header_before: Option<&'static str>,
}
```

- [ ] **Step 2: Update all four `MenuOption` construction sites in `build_auth_menu`**

In `src/setup.rs` inside `build_auth_menu()` (lines 179-214), add `header_before: None,` to each of the four `MenuOption { ... }` literals. Do not change any other field yet.

Example of what each construction site becomes:

```rust
MenuOption {
    label: "Log in interactively inside the container (recommended for Pro/Max)",
    action: Box::new(|| {
        println!("\n        Next step: run `agentbox`, then inside Claude type `/login`.");
        println!("        Your token will be saved under ~/.claude and persist across sessions.");
        Ok(())
    }),
    header_before: None,
},
```

Apply the same `header_before: None,` addition to the other three entries (API key, OAuth token, skip) without changing their other fields.

- [ ] **Step 3: Run cargo build to confirm compilation**

Run: `cargo build`
Expected: Clean build, no errors or warnings related to `MenuOption`.

- [ ] **Step 4: Run cargo test to confirm no regressions**

Run: `cargo test`
Expected: All 233 tests pass. No new failures.

---

## Task 2: Extract `render_menu` helper and update `prompt_menu` to delegate to it

**Files:**
- Modify: `src/setup.rs` (add `render_menu`, update `prompt_menu` around lines 250-269, add one test)

`render_menu` knows how to print a menu with optional section headers at 10-space indent and options at 12-space indent. Written TDD-style: failing synthetic test first, then implementation.

- [ ] **Step 1: Add the failing unit test**

Add inside the `#[cfg(test)] mod tests { ... }` block in `src/setup.rs`:

```rust
#[test]
fn test_render_menu_formats_headers_and_options() {
    let menu = vec![
        MenuOption {
            label: "First",
            action: Box::new(|| Ok(())),
            header_before: Some("Recommended:"),
        },
        MenuOption {
            label: "Second",
            action: Box::new(|| Ok(())),
            header_before: Some("Alternatives:"),
        },
        MenuOption {
            label: "Third",
            action: Box::new(|| Ok(())),
            header_before: None,
        },
    ];
    let mut buf = Vec::new();
    render_menu(&menu, &mut buf).unwrap();
    let rendered = String::from_utf8(buf).unwrap();

    let expected = "\n          Recommended:\n            1) First\n\n          Alternatives:\n            2) Second\n            3) Third\n";
    assert_eq!(rendered, expected);
}
```

- [ ] **Step 2: Run the new test to confirm it fails**

Run: `cargo test test_render_menu_formats_headers_and_options`
Expected: Compilation error — `cannot find function 'render_menu' in this scope`.

- [ ] **Step 3: Implement `render_menu`**

Add a `use std::io::Write;` to the imports if not already present (it is — line 12).

Add the `render_menu` function in `src/setup.rs`, immediately before `fn prompt_menu` (around line 250):

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

- [ ] **Step 4: Update `prompt_menu` to call `render_menu`**

Replace the inline rendering loop at the top of `prompt_menu` (currently lines 251-253) with a `render_menu` call. The function becomes:

```rust
fn prompt_menu(mut menu: Vec<MenuOption>) -> Result<()> {
    let mut stdout = std::io::stdout();
    render_menu(&menu, &mut stdout)?;
    print!("        > ");
    stdout.flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let choice = input.trim().parse::<usize>().unwrap_or(0);

    if choice > 0 && choice <= menu.len() {
        let option = menu.remove(choice - 1);
        (option.action)()?;
    } else {
        println!("        Invalid choice.");
    }

    Ok(())
}
```

Key change: the original loop printed each option at 10-space indent. `render_menu` prints at 12-space indent. This changes the visible indent of the menu today (since `build_auth_menu` hasn't been rewritten yet and still has no headers, the menu will briefly show options at 12-space indent with no section headers). Task 3 rewrites `build_auth_menu` to add headers and the new labels.

- [ ] **Step 5: Run the render test**

Run: `cargo test test_render_menu_formats_headers_and_options`
Expected: PASS.

- [ ] **Step 6: Run the full test suite**

Run: `cargo test`
Expected: All 234 tests pass (233 original + 1 new).

---

## Task 3: Rewrite `build_auth_menu` + update `AUTH_EXPLANATION`

**Files:**
- Modify: `src/setup.rs` — `AUTH_EXPLANATION` constant (around line 157), `build_auth_menu()` (around lines 179-214), and add three new tests.

Three tests go in together: a metadata-level one, a full-render one using the real `build_auth_menu()` output, and a content-check for `AUTH_EXPLANATION`. The third test is above the design spec's Testing section; see self-review at the bottom of this plan for why.

- [ ] **Step 1: Add the three failing unit tests**

Add all three tests to `src/setup.rs`'s `mod tests`:

```rust
#[test]
fn test_build_auth_menu_structure() {
    let menu = build_auth_menu();
    assert_eq!(menu.len(), 4);
    assert_eq!(
        menu[0].label,
        "Log in once inside the container (Pro/Max subscription)"
    );
    assert_eq!(menu[0].header_before, Some("Recommended:"));
    assert_eq!(
        menu[1].label,
        "Use a long-lived OAuth token (CLAUDE_CODE_OAUTH_TOKEN)"
    );
    assert_eq!(menu[1].header_before, Some("Alternatives:"));
    assert_eq!(menu[2].label, "Use an API key (ANTHROPIC_API_KEY)");
    assert_eq!(menu[2].header_before, None);
    assert_eq!(menu[3].label, "Skip for now");
    assert_eq!(menu[3].header_before, None);
}

#[test]
fn test_render_menu_matches_expected_layout() {
    let menu = build_auth_menu();
    let mut buf = Vec::new();
    render_menu(&menu, &mut buf).unwrap();
    let rendered = String::from_utf8(buf).unwrap();

    let expected = "\n          Recommended:\n            1) Log in once inside the container (Pro/Max subscription)\n\n          Alternatives:\n            2) Use a long-lived OAuth token (CLAUDE_CODE_OAUTH_TOKEN)\n            3) Use an API key (ANTHROPIC_API_KEY)\n            4) Skip for now\n";
    assert_eq!(rendered, expected);
}

#[test]
fn test_auth_explanation_mentions_pro_max_and_in_container_login() {
    // Guards against accidentally dropping the Pro/Max qualifier
    // (which would re-mislead Console-only users) and the persistence hint.
    assert!(AUTH_EXPLANATION.contains("Pro/Max"));
    assert!(AUTH_EXPLANATION.contains("one-time login"));
    assert!(AUTH_EXPLANATION.contains("~/.claude"));
}
```

- [ ] **Step 2: Run the three new tests to confirm they fail**

Run: `cargo test test_build_auth_menu_structure test_render_menu_matches_expected_layout test_auth_explanation_mentions_pro_max_and_in_container_login`
Expected: All three FAIL.
- `test_build_auth_menu_structure` — fails because `menu[0].label` is still the old `"Log in interactively inside the container (recommended for Pro/Max)"`.
- `test_render_menu_matches_expected_layout` — fails with a string-mismatch diff because the rendered output uses the old labels/ordering.
- `test_auth_explanation_mentions_pro_max_and_in_container_login` — fails on the `"Pro/Max"` assertion. Current `AUTH_EXPLANATION` contains `"one-time login"` and `"~/.claude"` already, but not `"Pro/Max"` — Step 4 below introduces that qualifier, which makes this test green.

- [ ] **Step 3: Rewrite `build_auth_menu`**

Replace the entire body of `build_auth_menu()` in `src/setup.rs` (currently lines 179-215) with:

```rust
fn build_auth_menu() -> Vec<MenuOption> {
    vec![
        MenuOption {
            label: "Log in once inside the container (Pro/Max subscription)",
            action: Box::new(|| {
                println!("\n        Next step: run `agentbox`, then type `/login` inside Claude.");
                println!("        The credentials persist under ~/.claude — you only do this once.");
                Ok(())
            }),
            header_before: Some("Recommended:"),
        },
        MenuOption {
            label: "Use a long-lived OAuth token (CLAUDE_CODE_OAUTH_TOKEN)",
            action: Box::new(|| {
                println!("\n        Requires the host `claude` CLI. Run this on your Mac first:");
                println!("\n            claude setup-token");
                println!("\n        Copy the token, then run in your shell (and add it to ~/.zshrc / ~/.bashrc):");
                println!("\n            export {}=\"your-token-here\"", CLAUDE_CODE_OAUTH_TOKEN);
                prompt_and_add_env_var(CLAUDE_CODE_OAUTH_TOKEN)
            }),
            header_before: Some("Alternatives:"),
        },
        MenuOption {
            label: "Use an API key (ANTHROPIC_API_KEY)",
            action: Box::new(|| {
                println!("\n        Run this in your shell (and add it to ~/.zshrc / ~/.bashrc for next time):");
                println!("\n            export {}=\"sk-...\"", ANTHROPIC_API_KEY);
                prompt_and_add_env_var(ANTHROPIC_API_KEY)
            }),
            header_before: None,
        },
        MenuOption {
            label: "Skip for now",
            action: Box::new(|| {
                println!("\n        You can re-run `agentbox setup` at any time to set up authentication.");
                Ok(())
            }),
            header_before: None,
        },
    ]
}
```

Key changes vs. today:
- Option 1 moves from label `"Log in interactively inside the container (recommended for Pro/Max)"` to `"Log in once inside the container (Pro/Max subscription)"` and has `header_before: Some("Recommended:")`. Its post-selection text is updated per spec.
- Options 2 and 3 swap positions. OAuth is now index 1 (with `header_before: Some("Alternatives:")`). API key is now index 2 (with `header_before: None`). Both action bodies are taken verbatim from their current form.
- Option 4 (`Skip for now`) is unchanged except for the new `header_before: None` field.

- [ ] **Step 4: Update `AUTH_EXPLANATION` constant**

Replace the constant at `src/setup.rs:157` with:

```rust
const AUTH_EXPLANATION: &str = "macOS Keychain isn't reachable from the Linux container.\n\
Claude Code needs either a one-time login from inside the container\n\
(Pro/Max subscribers; persists under ~/.claude) or credentials via env var.";
```

- [ ] **Step 5: Run all three new tests to confirm they pass**

Run: `cargo test test_build_auth_menu_structure test_render_menu_matches_expected_layout test_auth_explanation_mentions_pro_max_and_in_container_login`
Expected: All three PASS.

- [ ] **Step 6: Run the full test suite**

Run: `cargo test`
Expected: All 237 tests pass (233 original + 1 from Task 2 + 3 from Task 3).

---

## Task 4: Rewrite the README `## Authentication` section

**Files:**
- Modify: `README.md` (lines 150-190, the full `## Authentication` section through the closing `...passed explicitly.` paragraph)

No unit tests — this is documentation. Verification is a visual/markdown rendering check.

- [ ] **Step 1: Replace the existing `## Authentication` section**

Open `README.md`. Find the `## Authentication` heading at line 150. The section currently runs from line 150 through line 190 (ending with `"... Only the secret token needs to be passed explicitly."`). Replace that entire range with:

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

Everything outside this section is untouched.

- [ ] **Step 2: Verify the section replaced correctly**

Run: `grep -n "^## " README.md`
Expected: `## Authentication` still appears exactly once. Other top-level headings (`## Requirements`, `## Install`, `## Quick Start`, `## Passing Flags to the Coding Agent`, `## Configuration`, `## Custom Dockerfiles`, `## What's Mounted`, `## Sharing Screenshots`, `## What's Isolated`, `## Host Command Execution (Experimental)`, `## How It Works`) are preserved.

Run: `grep -c "^\*\*Option " README.md`
Expected: `3` (three Option headers: A, B, C).

- [ ] **Step 3: Check no stale references to pre-rewrite text**

Run: `grep -n "Option A: API key\|Option B: OAuth token" README.md`
Expected: No matches (exit code 1). The new headers use `**Option A (...): Log in...**` and `**Option B (...): Long-lived OAuth...**` — neither `Option A: API key` nor `Option B: OAuth token` should survive anywhere.

Run: `grep -cE "^\*\*Option [ABC] \(" README.md`
Expected: `3` — all three new-form headers (`**Option A (...`, `**Option B (...`, `**Option C (...`) are in place.

---

## Task 5: Final verification

**Files:** None modified — verification only.

- [ ] **Step 1: Run the full test suite**

Run: `cargo test`
Expected: 237 tests pass (233 original + 4 new).

- [ ] **Step 2: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: No warnings.

- [ ] **Step 3: Run rustfmt check**

Run: `cargo fmt --check`
Expected: Exit code 0 (no diffs). If it reports unformatted code, run `cargo fmt` and re-run the test suite.

- [ ] **Step 4: Build a release binary**

Run: `cargo build --release`
Expected: Clean build.

- [ ] **Step 5: Manual smoke test (on an isolated auth state)**

Use a temp `HOME` and `XDG_CONFIG_HOME` to ensure nothing from the developer's real auth state leaks in — this isolates against `~/.claude/.credentials.json`, host env vars for the auth keys, *and* any literal non-empty value for either key already present in the developer's real `config.toml` (all three paths are accepted by `decide_auth` in `src/setup.rs:117`).

```bash
TEMP_HOME=$(mktemp -d)
env -u ANTHROPIC_API_KEY -u CLAUDE_CODE_OAUTH_TOKEN \
  HOME="$TEMP_HOME" \
  XDG_CONFIG_HOME="$TEMP_HOME/.config" \
  ./target/release/agentbox setup
```

Setup auto-creates a fresh `config.toml` in the temp config dir from the default template (no `[env]` entries), so the auth check will reach the Interactive branch and render the menu.

Verify the Claude auth menu renders as:

```
  Recommended:
    1) Log in once inside the container (Pro/Max subscription)

  Alternatives:
    2) Use a long-lived OAuth token (CLAUDE_CODE_OAUTH_TOKEN)
    3) Use an API key (ANTHROPIC_API_KEY)
    4) Skip for now
> _
```

(With the actual 8/10/12-space indent — visually, the headers should be two spaces less indented than the numbered options, and a blank line should precede each header.)

Then type `1` and verify the printed next-step text is:

```
Next step: run `agentbox`, then type `/login` inside Claude.
The credentials persist under ~/.claude — you only do this once.
```

Remove the temp dir afterwards: `rm -rf "$TEMP_HOME"`. The developer's real `~/.claude`, shell env, and `config.toml` were never touched.

- [ ] **Step 6: Visual README check**

Render the README in a local markdown viewer or GitHub preview. Verify:
- The `## Authentication` heading still appears.
- Option A is `(recommended, Pro/Max subscription)`, B is `(Pro/Max subscription)`, C is `(Console API billing)`.
- The closing paragraph mentions the `~/.claude` mount carries settings across all three options.

---

## Self-review against the spec

All requirements from `wiki/2026-04-15-claude-auth-recommended-flow-design.md` map to tasks above:

- **Setup menu rework** — Tasks 1, 2, 3.
- **README rewrite** — Task 4.
- **`MenuOption` field** — Task 1.
- **`render_menu` extraction** — Task 2.
- **Two spec-required unit tests** (metadata + full render) — Task 3.
- **One extra test for `AUTH_EXPLANATION` content** (above spec) — Task 3. Added in response to code-review feedback that spec-required string constants were only covered by the manual smoke test.
- **`AUTH_EXPLANATION` refinement** — Task 3.
- **Option 1 Pro/Max qualifier** — Task 3 (in label and in post-selection text).
- **OAuth-token and API-key reorder** — Task 3.
- **New post-selection text for Option 1** — Task 3.
- **Manual smoke test** — Task 5.

No placeholders. All code is shown inline. All commands include expected output.

### Note on a post-spec design-doc amendment

The committed design doc (`0348f97`) had the same Option-C bug Codex caught in the plan — the README snippet showed only `ANTHROPIC_API_KEY = ""` (inherit from host env), leaving a user who follows it verbatim without a working configuration. Both the plan and the design doc were updated to a 2-step recipe (export in shell profile + config entry). The design-doc amendment needs a second commit before plan execution begins, so the in-repo spec and the plan stay in sync.
