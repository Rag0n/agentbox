# Additional Volume Mounts Design

**Date:** 2026-03-10
**Feature:** Allow mounting additional host folders into agentbox containers

## Problem

agentbox currently mounts only 3 volumes: the project directory, `~/.claude`, and `~/.claude.json`. Users need additional host folders mounted — for example, Claude Code plugin marketplaces, config directories for tools like worktrunk or ccstatusline.

## Solution

Add two mechanisms for specifying additional volumes:

1. **Global config** — `volumes` array in `~/.config/agentbox/config.toml`
2. **CLI flag** — `--mount` repeatable flag for per-invocation mounts

Both use the same path resolution rules. CLI mounts are additive with config mounts.

## Path Resolution Rules

Three formats, detected in order:

| Format | Example | Behavior |
|--------|---------|----------|
| Tilde prefix | `~/.config/worktrunk` | Source: expand `~` to host home. Dest: expand `~` to `/home/user` |
| Explicit mapping | `/source/path:/dest/path` | Use as-is |
| Absolute path | `/Users/alex/Dev/marketplace` | Mount at same path in container |

Detection: if the string starts with `~`, apply tilde rule. Else if it contains `:` with valid path-like strings on both sides, treat as explicit mapping. Otherwise, mount at same path.

## Config Example

```toml
volumes = [
  "~/.config/worktrunk",
  "~/.config/ccstatusline",
  "/Users/alex/Dev/Personal/marketplace",
  "/custom/source:/custom/dest",
]
```

## CLI Example

```bash
agentbox --mount ~/.config/foo --mount /some/path
```

## Changes Required

### `config.rs`
- Add `volumes: Vec<String>` field to `Config` struct (default: empty vec)
- Update `init_template()` with volumes example

### `main.rs`
- Add `--mount` repeatable CLI flag to `Cli` struct
- Add `resolve_volume(spec: &str) -> String` function implementing path resolution
- In `create_and_run()`: resolve config volumes + CLI mounts, append to hardcoded volumes
- Pass CLI mounts into `create_and_run()`

### `container.rs`
- No changes needed — already handles arbitrary `Vec<String>` volumes

## Validation

- Warn (don't error) if source path doesn't exist on host
- No deduplication needed — container runtime handles duplicate mounts

## Edge Cases

- Windows paths: not relevant (macOS-only via Apple Container CLI)
- Relative paths: not supported, must be absolute or tilde-prefixed
- Empty volumes list: no-op, same behavior as today
