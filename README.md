# agentbox

Run AI coding agents in isolated Apple Containers. Your project directory is mounted read/write — everything else on your filesystem is inaccessible.

Supported agents: Claude Code, OpenAI Codex.

## Requirements

- macOS 26+ on Apple Silicon
- [Apple Container CLI](https://github.com/apple/container) — download the installer package from [releases](https://github.com/apple/container/releases)

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

### Breaking change (pre-1.0)

agentbox no longer hardcodes `--dangerously-skip-permissions` into the claude invocation. The flag now lives in `[cli.claude] flags` in the config template.

- If your `~/.config/agentbox/config.toml` has a `[cli.claude] flags = [...]` entry, add `--dangerously-skip-permissions` to the list.
- If your config has no `[cli.claude]` section, add one with `flags = ["--dangerously-skip-permissions"]`.
- If you have no `~/.config/agentbox/config.toml`, run `agentbox setup` — it will create the file with correct defaults.

## Quick Start

```bash
# First time? Run setup to check prerequisites and configure authentication
agentbox setup

# Then use agentbox normally:

# Start interactive Claude session (default, unless default_agent is set in config)
agentbox

# Explicit agent subcommands
agentbox claude
agentbox codex

# Headless tasks
agentbox "fix the failing tests"
agentbox codex "fix the failing tests"

# Open an interactive bash shell in the container (no Claude)
agentbox shell

# Run a one-shot command in the container
agentbox shell -- npm test

# Show container status (CPU, memory, project, sessions)
# On a TTY it refreshes every 2s like top — exit with q or Ctrl+C.
# Use --no-stream for a single snapshot (or pipe to skip live mode).
agentbox status

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

# Pass a codex config override (reasoning effort)
agentbox codex -- -c model_reasoning_effort=high
```

For flags you want every time, set them in config instead of repeating on every invocation:

```toml
# ~/.config/agentbox/config.toml
[cli.claude]
flags = ["--append-system-prompt", "Be brutally honest."]
```

For codex, use a `[cli.codex]` section the same way:

```toml
# Pass flags to codex via config
# [cli.codex]
# flags = ["--dangerously-bypass-approvals-and-sandbox", "-c", "model_reasoning_effort=medium"]
```

Config flags and `--` flags are merged. Config flags come first, `--` flags after.

## Configuration

Optional. Create with `agentbox config init`.

Located at `~/.config/agentbox/config.toml`:

```toml
# ~/.config/agentbox/config.toml

# Default agent for bare `agentbox`. Omit for "claude".
default_agent = "claude"   # or "codex"

# Resources
cpus = 4          # default: half of host cores
memory = "8G"     # default: 8G

# Override default Dockerfile
dockerfile = "/path/to/my.Dockerfile"

# Additional volumes to mount into containers
volumes = [
  "~/.config/worktrunk",
  "/opt/shared-libs",
  "/source/path:/dest/path",
]

# Environment variables passed into container
[env]
CLAUDE_CODE_OAUTH_TOKEN = ""
GH_TOKEN = ""
MY_API_KEY = "abc123"

# Default flags for each agent. Replace to override.
[cli.claude]
flags = ["--dangerously-skip-permissions"]

[cli.codex]
flags = ["--dangerously-bypass-approvals-and-sandbox"]

# Named profiles
[profiles.mystack]
dockerfile = "/path/to/mystack.Dockerfile"
```

### Terminal notifications

After a long image rebuild, agentbox sends a terminal notification so you can tab away without missing when it's done. Notifications fire only when a rebuild actually runs (not on cached session starts, not for the coding agent's own prompts — those are covered by agent-specific plugins like `agent-notifications`).

Supported terminals (native, no extra install): Ghostty, WezTerm, iTerm2, Kitty. Other terminals silently skip — no visible garbage.

On by default. Disable:

```toml
# ~/.config/agentbox/config.toml
[notifications]
enabled = false
```

Success fires with title `agentbox: build complete`; failure with `agentbox: build failed`. Body is the project directory name.

## Custom Dockerfiles

### Per-project

Place an `agentbox.Dockerfile` in your project root. It's detected automatically.

Can extend the default image:

```dockerfile
FROM agentbox:default

RUN sudo apt-get update && sudo apt-get install -y nodejs
```

> **Note:** `agentbox shell` requires the agentbox entrypoint script for the
> cold-start case (when the container doesn't yet exist). If your custom
> Dockerfile uses `FROM agentbox:default`, it works automatically. If your
> Dockerfile replaces the entrypoint or uses a fully different base image,
> the cold-start case won't launch a shell — run `agentbox` first to create
> the container, then `agentbox shell` works via the exec path.

### Profiles

Define in config, use with `--profile`:

```bash
agentbox --profile mystack
```

## Authentication

### Claude Code

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

### OpenAI Codex

Codex stores auth under `~/.codex/auth.json`, which agentbox mounts into the container read/write. Sign in once (host or container); the credentials persist for both.

**First sign-in.** Run `agentbox codex`. On an unauthenticated container, codex's onboarding menu appears; pick the device-code sign-in flow (intended for remote/headless machines), then open the URL on your Mac and enter the code shown in the terminal. Auth persists automatically via the `~/.codex` mount. If the onboarding menu only shows ChatGPT / API-key options up front, pick ChatGPT and press Esc on the browser screen — codex falls back to device code for headless environments.

**Credential storage — default case.** Codex stores credentials in `~/.codex/auth.json` by default (file-based). You don't need to configure anything; the mount makes the file reachable from both host and container.

**Credential storage — if you've customized it.** If you previously set `cli_auth_credentials_store = "keyring"` (or `"auto"`, `"ephemeral"`) in `~/.codex/config.toml`, auth won't propagate into the container — the Linux container can't reach the macOS Keychain. Switch the setting back to `"file"` and re-login:

```toml
# ~/.codex/config.toml
cli_auth_credentials_store = "file"
```

`agentbox setup` detects this case and prints the same hint.

## What's Mounted

| Host                | Container                 | Access     | Notes                                                         |
|---------------------|---------------------------|------------|---------------------------------------------------------------|
| Current directory   | Same path                 | read/write |                                                               |
| `~/.claude`         | `/home/user/.claude`      | read/write |                                                               |
| `~/.claude.json`    | `/tmp/claude-seed.json`   | read-only  | Seed only; `entrypoint.sh` `jq`-merges into `~/.claude.json` |
| `~/.codex`          | `/home/user/.codex`       | read/write |                                                               |
| Additional volumes  | Configured path           | read/write |                                                               |

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
