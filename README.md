# agentbox

Run AI coding agents in isolated Apple Containers. Your project directory is mounted read/write — everything else on your filesystem is inaccessible.

Currently supports Claude Code. More agents planned.

## Requirements

- macOS 26+ on Apple Silicon
- [Apple Container CLI](https://github.com/apple/container) — download the installer package from [releases](https://github.com/apple/container/releases)

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Rag0n/agentbox/main/install.sh | bash
```

Or manually:

```bash
curl -fsSL https://github.com/Rag0n/agentbox/releases/latest/download/agentbox-darwin-arm64.tar.gz | tar xz
mv agentbox ~/.local/bin/
```

## Quick Start

```bash
# First time? Run setup to check prerequisites and configure authentication
agentbox setup

# Then use agentbox normally:

# Start interactive Claude session in current project
agentbox

# Run a task headlessly
agentbox "fix the failing tests"

# Show container status (CPU, memory, project, sessions)
agentbox status
# `agentbox ls` is an alias for `status`

# Remove current project's container
agentbox rm

# Remove specific containers
agentbox rm agentbox-myapp-abc123 agentbox-other-def456

# Remove all agentbox containers
agentbox rm --all

# Force rebuild the image
agentbox build
```

## Passing Flags to the Coding Agent

Use `--` to pass flags through to the underlying CLI (e.g., Claude Code):

```bash
# Pass a model flag
agentbox -- --model sonnet

# Append to system prompt for a headless task
agentbox "fix the tests" -- --append-system-prompt "Be concise."
```

For flags you want every time, set them in config instead of repeating on every invocation:

```toml
# ~/.config/agentbox/config.toml
[cli.claude]
flags = ["--append-system-prompt", "Be brutally honest."]
```

Config flags and `--` flags are merged. Config flags come first, `--` flags after.

## Configuration

Optional. Create with `agentbox config init`.

Located at `~/.config/agentbox/config.toml`:

```toml
# Resources
cpus = 4          # default: half of host cores
memory = "8G"     # default: 8G

# Override default Dockerfile
dockerfile = "/path/to/my.Dockerfile"

# Additional volumes to mount into containers
volumes = [
  "~/.config/worktrunk",              # tilde = home-relative mapping
  "/opt/shared-libs",                  # absolute = same path in container
  "/source/path:/dest/path",          # explicit source:dest mapping
]

# Environment variables passed into container
[env]
CLAUDE_CODE_OAUTH_TOKEN = ""  # empty = inherit from host
GH_TOKEN = ""                 # empty = inherit from host
MY_API_KEY = "abc123"         # literal value

# Extra CLI flags passed to the coding agent
[cli.claude]
flags = ["--append-system-prompt", "Be brutally honest."]

# Named profiles
[profiles.mystack]
dockerfile = "/path/to/mystack.Dockerfile"
```

## Custom Dockerfiles

### Per-project

Place an `agentbox.Dockerfile` in your project root. It's detected automatically.

Can extend the default image:

```dockerfile
FROM agentbox:default

RUN sudo apt-get update && sudo apt-get install -y nodejs
```

### Profiles

Define in config, use with `--profile`:

```bash
agentbox --profile mystack
```

## Authentication

macOS Keychain isn't accessible from inside the Linux container, so Claude Code needs credentials passed via environment variables, or a one-time login from inside the container (which persists under `~/.claude/`).

**Easiest approach: Run `agentbox setup`** — it will guide you through the options.

Alternatively, here are the three methods:

**Option A: API key** — set `ANTHROPIC_API_KEY` in your config or shell:

```toml
# ~/.config/agentbox/config.toml
[env]
ANTHROPIC_API_KEY = ""  # empty = inherit from host env
```

**Option B: OAuth token (Pro/Max subscription):**

1. Generate a long-lived token on the host:

   ```bash
   claude setup-token
   ```

   This prints an export command with your OAuth token. Copy the token value.

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

Your `~/.claude` settings directory is mounted into the container, so project settings, CLAUDE.md trust, and preferences carry over automatically. Only the secret token needs to be passed explicitly.

## What's Mounted

| Host | Container | Access |
|------|-----------|--------|
| Current directory | Same path | read/write |
| `~/.claude` | `/home/user/.claude` | read/write |
| `~/.claude.json` | `/home/user/.claude.json` | read/write |
| Additional volumes | Configured path | read/write |

Additional volumes can be mounted via [config](#configuration) or CLI:

```bash
# Mount extra directories per-invocation (see config for persistent mounts)
agentbox --mount ~/.config/worktrunk --mount /path/to/other/dir
```

Three path formats are supported:

| Format | Example | Behavior |
|--------|---------|----------|
| Tilde prefix | `~/.config/foo` | Host `~/` → container `/home/user/` |
| Absolute path | `/some/path` | Same path in container |
| Explicit mapping | `/source:/dest` | Custom source → dest |

## Sharing Screenshots

macOS clipboard lives in memory and isn't directly accessible from inside the container. However, screenshot tools save files to disk, and you can mount those directories so Claude can see pasted/dragged images.

Use absolute paths (not `~/`) so the mount path inside the container matches the host path that the terminal sends:

```toml
# ~/.config/agentbox/config.toml
volumes = [
  # CleanShot X media (adjust path for your screenshot tool)
  "/Users/YOUR_USERNAME/Library/Application Support/CleanShot/media",
  # Clop optimized images (if you use Clop)
  "/Users/YOUR_USERNAME/Library/Caches/Clop/images",
]
```

After adding these volumes, restart your container (`agentbox rm && agentbox`) and drag-and-drop or paste screenshots as usual.

> **Why absolute paths?** The `~/` prefix maps to `/home/user/` inside the container, but your terminal sends the real host path (`/Users/you/Library/...`). Using absolute paths ensures both sides match.

> **Note:** The standard macOS screenshot tool (`Cmd+Shift+3/4`) saves to Desktop by default. If your Desktop isn't already mounted, add `/Users/YOUR_USERNAME/Desktop` to your volumes. Clipboard-only copies (`Cmd+Shift+Ctrl+3/4`) create no file on disk — use `Cmd+Shift+3/4` (without Ctrl) instead.

## What's Isolated

Claude **cannot** access `~/.ssh`, `~/.aws`, `~/.gnupg`, or any other host directory.

## Host Command Execution (Experimental)

Run macOS host commands from inside the container. Useful for tools that can't run in Linux, like `xcodebuild` or `xcrun`.

Configure in `~/.config/agentbox/config.toml`:

```toml
[bridge]
allowed_commands = ["xcodebuild", "xcrun", "adb"]
```

Only commands in `allowed_commands` can be executed. The bridge starts automatically when commands are configured and stops when the session ends.

Set `forward_not_found = true` to automatically forward any command not found in the container to the host.

## How It Works

agentbox uses Apple Containers to run a lightweight Linux VM with Claude Code. Containers are persistent (reused across sessions) and auto-named by project directory. Images auto-rebuild when the Dockerfile changes.
