# Buildkit Crash Recovery

## Problem

Apple Container's buildkit builder intermittently crashes with `Swift/StringLegacy.swift:31: Fatal error: Negative count not allowed` during image builds. The crash was fixed upstream in [PR #894](https://github.com/apple/container/pull/894) (released in 0.7.0), but agentbox re-triggers it by always passing `--pull` to `container build`, which forces the old `pull()` code path that bypasses the fix.

Recovery currently requires manual steps: stop buildkit, remove it, restart the container system.

Related issues: [#883](https://github.com/apple/container/issues/883), [#284](https://github.com/apple/container/issues/284), [#677](https://github.com/apple/container/issues/677).

## Solution

Two changes:

### 1. Fix `--pull` placement

Stop passing `--pull` on implicit auto-builds (when agentbox detects a Dockerfile change). Keep `--pull` for explicit `agentbox build` commands where the user is intentionally rebuilding.

| Caller | `--pull` |
|--------|----------|
| `agentbox` (implicit auto-build) | no |
| `agentbox build` | yes |
| `agentbox build --no-cache` | yes |
| `ensure_base_image()` (local base) | no (unchanged) |

### 2. Crash recovery with retry

If `container build` fails and stderr contains `"Negative count not allowed"`:

1. Run `container builder stop --force`
2. Run `container builder delete`
3. Retry the build once
4. If retry fails, bail with error

Detection: pipe stderr through `BufReader`, print each line in real-time while accumulating. Stdout remains inherited directly.

The recovery commands (`container builder stop --force` / `container builder delete`) come from the [GitHub issue workarounds](https://github.com/apple/container/issues/284) and are more reliable than stopping/removing the buildkit container directly.

## Changes

- `image.rs`: Add `pull` parameter to `build()` and `build_args()`. Add `reset_buildkit()` function. Add stderr tee + crash detection logic to `build()`.
- `main.rs`: Pass `pull: true` for explicit `agentbox build`, `pull: false` for implicit builds.
- Tests: Update `build_args()` tests for new `pull` parameter.

## Non-goals

- No new CLI subcommands (no `agentbox doctor`)
- No integration tests for crash recovery (requires external process)
