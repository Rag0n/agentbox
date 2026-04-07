# Homebrew Tap Distribution

## Goal

Distribute agentbox via a Homebrew tap so macOS users can install and upgrade
with `brew install rag0n/tap/agentbox`. The tap consumes the existing
`agentbox-darwin-arm64.tar.gz` release artifact — no new build outputs.

## Architecture

Two repos are involved:

1. **`Rag0n/agentbox`** (this repo) — source of truth. The release workflow
   builds the binary, publishes the GitHub release, then calls out to the tap
   repo.
2. **`Rag0n/homebrew-tap`** (new, to be created) — a plain Homebrew tap repo.
   Contains `Formula/agentbox.rb` and nothing else initially. No CI on its
   side.

### Release flow

On every `v*` tag pushed to `Rag0n/agentbox`:

```
tag v0.2.0 pushed
  │
  ▼
release.yml: build job (existing)
  │   builds binary, packages tarball, creates GitHub release
  ▼
release.yml: bump-homebrew job (new)
  │   needs: build
  │   skipped when tag matches *-* (prerelease)
  ▼
mislav/bump-homebrew-formula-action
  │   auths with HOMEBREW_TAP_TOKEN (PAT scoped to tap repo)
  │   downloads tarball from the fresh GitHub release
  │   computes sha256, bumps version + url + sha256 in Formula/agentbox.rb
  │   opens a PR in Rag0n/homebrew-tap
  ▼
Maintainer merges the PR → brew picks up the new version
```

Key choices:

- **Separate tap repo**, not inline. Standard Homebrew convention. Install
  path is `brew install rag0n/tap/agentbox`.
- **PR mode, not direct push.** The mislav action defaults to opening a PR,
  which gives an audit trail and a manual gate before users receive the new
  version.
- **Fine-grained PAT**, not GitHub App or deploy key. Lowest friction for a
  solo-maintained tap.
- **Third-party action** (`mislav/bump-homebrew-formula-action`), not custom
  shell. The author is an ex-Homebrew maintainer at GitHub, the action is
  purpose-built for this workflow, and reusing it avoids reinventing sha256
  computation and formula editing.

## The Formula

`Formula/agentbox.rb` in `Rag0n/homebrew-tap`:

```ruby
class Agentbox < Formula
  desc "Run AI coding agents in isolated Apple Containers"
  homepage "https://github.com/Rag0n/agentbox"
  version "0.1.0"
  url "https://github.com/Rag0n/agentbox/releases/download/v0.1.0/agentbox-darwin-arm64.tar.gz"
  sha256 "<computed at bootstrap time>"
  license "GPL-3.0-only"

  depends_on arch: :arm64
  depends_on macos: ">= :tahoe"

  def install
    bin.install "agentbox"
  end

  def caveats
    <<~EOS
      agentbox requires the Apple Container CLI, which is not available
      via Homebrew. Install it from:
        https://github.com/apple/container/releases
    EOS
  end

  test do
    assert_match "agentbox", shell_output("#{bin}/agentbox --version")
  end
end
```

Notes:

- **Pre-built binary formula.** No `build from source` — users get the same
  arm64 binary as the tarball release, no Rust toolchain needed at install
  time.
- **`version` is explicit.** The mislav action has a clean field to bump.
- **`:tahoe` is the Homebrew symbol for macOS 26.** Verified at implementation
  time. If Homebrew has not yet added the symbol, fall back to a runtime
  `MacOS.version` check in `caveats`, or drop the macos constraint entirely
  and rely on the caveat text. Do not block the release on this.
- **`test do` is minimal** — `brew test agentbox` just asserts the binary runs
  and prints something containing `"agentbox"`.
- **Apple Container is not a Homebrew dependency.** It is not distributed via
  brew, so it cannot be declared as `depends_on`. The caveats block is the
  only signal to the user.

## The CI Job

Added to `.github/workflows/release.yml` as a second job after `build`:

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

Design choices:

- **`needs: build`** — guarantees the GitHub release exists before the action
  tries to fetch the tarball. If `build` fails, the bump is skipped.
- **`if: !contains(github.ref_name, '-')`** — stable releases only. `v0.2.0`
  triggers the bump; `v0.2.0-beta.1`, `v0.2.0-rc.1`, etc. do not.
- **`runs-on: ubuntu-latest`** — this job only downloads and hashes a
  tarball; macOS is not required. Cheaper and faster than `macos-15`.
- **`COMMITTER_TOKEN`** — the env var the mislav action reads for the
  cross-repo PAT.
- **No `on:` override** — the job inherits the existing `push: tags: v*`
  trigger. The `if:` expression handles prerelease filtering.

## Bootstrap & One-Time Setup

Everything that must happen manually, exactly once, before the automation can
work. These steps are outside the normal code-change workflow — the
maintainer runs them against github.com by hand.

### Step 1 — Create the tap repo

1. Create a public GitHub repo `Rag0n/homebrew-tap`. No README, no license
   file, no gitignore — empty.
2. Clone locally.
3. Create `Formula/agentbox.rb` with the formula from the section above.
4. Compute the real sha256 of the current `v0.1.0` release tarball and paste
   it into the formula:

   ```bash
   curl -fsSL https://github.com/Rag0n/agentbox/releases/download/v0.1.0/agentbox-darwin-arm64.tar.gz \
     | shasum -a 256
   ```
5. Run `brew audit --strict --new rag0n/tap/agentbox` locally after tapping
   the local path. Fix any lint errors.
6. Commit and push to `main`.
7. Smoke test:

   ```bash
   brew tap rag0n/tap
   brew install rag0n/tap/agentbox
   agentbox --version
   ```

### Step 2 — Create the PAT

1. Visit https://github.com/settings/personal-access-tokens (fine-grained).
2. Click **Generate new token**.
3. Token name: `agentbox-homebrew-tap`.
4. Expiration: maintainer's choice (max 1 year for fine-grained). Set a
   calendar reminder a few days before expiry.
5. Repository access: **Only select repositories** → `Rag0n/homebrew-tap`.
6. Repository permissions:
   - **Contents**: Read and write
   - **Pull requests**: Read and write
   - **Metadata**: Read-only (auto-selected)
7. Click **Generate token**. Copy the token value — it is shown only once.

### Step 3 — Store the PAT as a secret in the agentbox repo

1. Visit https://github.com/Rag0n/agentbox/settings/secrets/actions.
2. Click **New repository secret**.
3. Name: `HOMEBREW_TAP_TOKEN` (exact match required — the workflow YAML
   references this name).
4. Value: the token from Step 2.
5. Click **Add secret**.

### Step 4 — End-to-end test

1. After the `bump-homebrew` job is merged to `main`, cut a test release tag
   (e.g., `v0.1.1` bumping `Cargo.toml` with a trivial change, or wait for the
   next real release).
2. Watch the Actions tab on `Rag0n/agentbox`: `build` succeeds, then
   `bump-homebrew` runs.
3. Check `Rag0n/homebrew-tap` for an auto-opened PR bumping url, sha256, and
   version.
4. Merge the PR.
5. On a test machine: `brew update && brew upgrade agentbox && agentbox --version`
   should print the new version.

### Rotation

When the PAT expires, regenerate it (Step 2) and update the secret (Step 3).
The `bump-homebrew` job will start failing with a 401/403 on rotation day if
this is missed — the calendar reminder is the safety net.

## README Changes

The `## Install` section in `README.md` is rewritten to put brew at the top.
`install.sh` itself is not modified.

New `## Install` section:

````markdown
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
````

No other README sections change.

## Testing & Verification

There are no automated tests added to the agentbox repo for this work. The
tap and the workflow are validated by running the actual release pipeline
once.

### Pre-merge (in the feature worktree)

- Visual review of `release.yml` — confirm YAML is valid, `needs:` is wired,
  the `if:` expression is correct, the secret name matches the PAT secret.
- Dry-read the formula file with `ruby -c Formula/agentbox.rb` in the tap
  repo locally — catches syntax errors.
- `brew audit --strict --new rag0n/tap/agentbox` in the tap repo — catches
  formula lint issues before the first release.

### Post-merge, first real release

1. Push a tag (e.g., `v0.1.1` or the next real release).
2. `build` job succeeds and creates the GitHub release.
3. `bump-homebrew` job succeeds.
4. A PR appears in `Rag0n/homebrew-tap` bumping url + sha256 + version.
5. Merge the PR.
6. `brew update && brew upgrade agentbox && agentbox --version` on a test
   machine prints the new version.

### Failure modes to watch for

- **PAT expired** → `bump-homebrew` fails with 401/403. Fix: rotate the token
  per Step 2 and update the secret per Step 3, then re-run the job.
- **sha256 mismatch** → mislav action fails with a clear error. Should not
  happen with `needs: build`, but indicates the tarball changed between jobs
  if it does.
- **Prerelease tag triggered bump anyway** → `if:` expression is wrong.
  Verify `contains(github.ref_name, '-')` logic against the actual tag name.
- **`:tahoe` symbol unknown to brew** → formula fails to parse at install
  time. Fix: fall back to a `MacOS.version` runtime check inside `caveats`,
  or drop the macos constraint.

## Out of Scope

- Linux or Intel Mac formula variants. The binary is arm64-only.
- Building from source in the formula. Users get the pre-built tarball.
- Auto-merging the tap PR. The manual merge is the feature.
- Replacing `install.sh`. It stays as an alternative for users who do not
  want brew.
- Publishing to the official homebrew-core. That is a separate, much larger
  effort with its own requirements.
