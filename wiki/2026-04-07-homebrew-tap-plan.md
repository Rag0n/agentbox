# Homebrew Tap Distribution Implementation Plan

> **For agentic workers:** REQUIRED: Use workflow:subagent-driven-development to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a CI job that auto-bumps the Homebrew formula on every stable release, and surface brew as the recommended install method in the README.

**Architecture:** Two simple changes to the agentbox repo: a new `bump-homebrew` CI job that fires after successful release builds, and a README rewrite putting brew at the top of install options.

**Tech Stack:** GitHub Actions (mislav/bump-homebrew-formula-action), Homebrew formula Ruby.

---

## Setup Prerequisites (Manual, One-Time)

These steps are run manually by the maintainer BEFORE this plan is executed. They happen outside this repo. Refer to the design doc for full details.

**REQUIRED before implementation:** User must complete design doc bootstrap steps:
1. Create `Rag0n/homebrew-tap` repo on GitHub (empty)
2. Create and push `Formula/agentbox.rb` pointing to v0.1.0 with computed sha256
3. Verify `brew tap rag0n/tap && brew install rag0n/tap/agentbox` works locally
4. Create a fine-grained PAT (`HOMEBREW_TAP_TOKEN`) scoped to `Rag0n/homebrew-tap`
5. Store the PAT as a secret in `Rag0n/agentbox` settings

**Implementation cannot proceed without these.** Verify all 5 are done before starting Task 1.

---

### Task 1: Add bump-homebrew job to release workflow

**Files:**
- Modify: `.github/workflows/release.yml` (add new job after `build`)

**Context:**
The existing `release.yml` has a single `build` job that creates the GitHub release. You're adding a second `bump-homebrew` job that depends on `build` succeeding, then triggers the mislav action to open a PR in the homebrew-tap repo.

- [ ] **Step 1: Read the current release.yml**

Run: `cat .github/workflows/release.yml`

You should see the existing `build` job with steps: checkout, install Rust, build release, verify architecture, package binary, create GitHub release. The file ends after line 36 (the action-gh-release step).

- [ ] **Step 2: Append the new bump-homebrew job**

After the closing of the `build` job (after the `generate_release_notes: true` line and its closing brace), add the new job:

```yaml
  bump-homebrew:
    needs: build
    runs-on: ubuntu-latest
    if: ${{ !contains(github.ref_name, '-') }}
    steps:
      - name: Bump Homebrew formula
        uses: mislav/bump-homebrew-formula-action@v3
        with:
          formula-name: agentbox
          formula-path: Formula/agentbox.rb
          homebrew-tap: Rag0n/homebrew-tap
          download-url: https://github.com/Rag0n/agentbox/releases/download/${{ github.ref_name }}/agentbox-darwin-arm64.tar.gz
          commit-message: |
            agentbox ${{ github.ref_name }}
        env:
          COMMITTER_TOKEN: ${{ secrets.HOMEBREW_TAP_TOKEN }}
```

Ensure indentation is 2 spaces per YAML level. The new job is a sibling to `build` (same indentation depth).

- [ ] **Step 3: Verify the YAML is valid**

Run: `yamllint .github/workflows/release.yml` (if yamllint installed) or use a syntax checker online, or visual inspection.

Expected: No errors. The file should have two jobs: `build` and `bump-homebrew`, with `bump-homebrew` after `build` in the file.

- [ ] **Step 4: Test the if condition logic locally**

The `if: ${{ !contains(github.ref_name, '-') }}` expression should:
- Return true for `refs/tags/v0.2.0` (stable) → job runs
- Return false for `refs/tags/v0.2.0-beta.1` (prerelease) → job skipped

This is standard GitHub Actions expression syntax. No runtime test needed, but verify the condition by inspection:
- `github.ref_name` is the tag name without `refs/tags/` prefix
- `contains(string, substring)` returns true if substring found
- `!` negates it
- So `!contains("v0.2.0", "-")` = true, `!contains("v0.2.0-beta.1", "-")` = false ✓

---

### Task 2: Update README Install section

**Files:**
- Modify: `README.md` (rewrite `## Install` section)

**Context:**
The current Install section has three methods in order: shell script, manual tarball, then mentions curl. You're reordering to put Homebrew first, since it's the most familiar and easiest for macOS users.

- [ ] **Step 1: Read the current Install section**

Run: `sed -n '/^## Install/,/^## [A-Z]/p' README.md | head -30`

Expected: You should see the current Install section starting with `## Install` and the three sub-sections: curl|bash script, manual tarball, and the "Or manually" section.

- [ ] **Step 2: Locate the exact lines to replace**

Run: `grep -n "^## Install" README.md`

This gives you the line number where `## Install` starts. Then locate the next section header (e.g., `## Quick Start`) to know where Install ends.

Run: `grep -n "^## Quick Start" README.md`

Note both line numbers.

- [ ] **Step 3: Replace the Install section**

Use `sed` or your editor to replace lines from Install start to the line before Quick Start. The new text is:

```markdown
## Install

```bash
brew install rag0n/tap/agentbox
```

Or with the install script:

```bash
curl -fsSL https://raw.githubusercontent.com/Rag0n/agentbox/main/install.sh | bash
```

Or manually:

```bash
curl -fsSL https://github.com/Rag0n/agentbox/releases/latest/download/agentbox-darwin-arm64.tar.gz | tar xz
mv agentbox ~/.local/bin/
```
```

Three methods, brew first. The indentation, code block markers, and URLs must be exact.

Example using sed (replace lines 12-23 — adjust for your actual line numbers):

```bash
sed -i '12,23d' README.md
```

Then insert the new text at line 12. Or use your editor directly.

- [ ] **Step 4: Verify the file is readable**

Run: `sed -n '/^## Install/,/^## Quick Start/p' README.md`

Expected output:
```
## Install

```bash
brew install rag0n/tap/agentbox
```

Or with the install script:
...
(and so on until the next ## section)
```

The section should have the brew method first, no syntax errors, proper code block formatting.

- [ ] **Step 5: Verify no unintended changes**

Run: `git diff README.md | head -50`

Expected: Only the Install section should be modified. No changes to any other sections (Quick Start, Configuration, etc.). If you see changes outside Install, undo and redo more carefully.

---

### Task 3: Verify the changes are complete

**Files:**
- Check: `.github/workflows/release.yml` (syntax and logic)
- Check: `README.md` (Install section order)

**Context:**
Quick checklist to ensure both changes are in place and ready for merge.

- [ ] **Step 1: Verify release.yml has both jobs**

Run: `grep -c "^  [a-z]" .github/workflows/release.yml`

Expected: At least 2 (the `build` and `bump-homebrew` jobs at the same indentation level). Run `grep "^  [a-z]" .github/workflows/release.yml` to see them listed.

- [ ] **Step 2: Verify bump-homebrew has correct secret reference**

Run: `grep "HOMEBREW_TAP_TOKEN" .github/workflows/release.yml`

Expected: One line showing `COMMITTER_TOKEN: ${{ secrets.HOMEBREW_TAP_TOKEN }}`. This name must match the secret created in the bootstrap steps (see Setup Prerequisites). If it doesn't match, the job will fail at runtime.

- [ ] **Step 3: Verify README Install section is first**

Run: `grep -A 10 "^## Install" README.md | head -12`

Expected output starts with:
```
## Install

```bash
brew install rag0n/tap/agentbox
```
```

The brew command is on the second code block line (after the opening ` ```bash `). If you see the install script or manual tarball first, redo Task 2.

- [ ] **Step 4: Verify no syntax errors in YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))" && echo "YAML valid"`

Expected: `YAML valid` with no errors. If you see an error, check indentation and quotes in the YAML.

- [ ] **Step 5: Verify git status shows only expected changes**

Run: `git status --short`

Expected:
```
M .github/workflows/release.yml
M README.md
```

Only these two files modified. No untracked files (except `.DS_Store` if macOS). If you see other files, investigate and clean up.

---

## End-to-End Test (Post-Merge)

After this plan is merged to `main`, the following steps validate the full integration. **These are NOT part of implementation — they're verification steps the maintainer runs after merge.**

The maintainer will:
1. Ensure the homebrew-tap repo bootstrap is complete (Steps 1-3 from Setup Prerequisites)
2. Ensure the PAT secret is stored in this repo (Step 4 from Setup Prerequisites)
3. Push a test release tag (e.g., `v0.1.1`)
4. Watch the Actions tab: `build` job succeeds, then `bump-homebrew` runs
5. Check `Rag0n/homebrew-tap` for an auto-opened PR bumping the formula version, URL, and sha256
6. Merge that PR
7. On a test machine: `brew update && brew upgrade agentbox && agentbox --version`

If all steps pass, the full pipeline is working. If any step fails, refer to the "Failure modes" section in the design doc.

---

## Implementation Notes

- **No new tests added to agentbox.** The formula and workflow are validated by the end-to-end test (above) during the first real release.
- **The mislav action opens a PR, not pushing directly.** This is intentional — it provides an audit trail and a manual review gate before users receive the new version.
- **Prerelease tags (v0.2.0-beta.1) are intentionally skipped** by the `if: !contains(...)` condition. Only stable releases bump the formula.
- **HOMEBREW_TAP_TOKEN must already exist as a secret** in the agentbox repo. If it doesn't, the job fails with a 401 when trying to authenticate to the tap repo. This is verified during end-to-end testing.
