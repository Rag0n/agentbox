# agentbox

Run AI coding agents in isolated [Apple Containers](https://github.com/apple/container). Your project directory is mounted read/write — everything else on your filesystem is inaccessible.

Supported agents: Claude Code, OpenAI Codex.

## How It Works

agentbox uses Apple Containers to run a lightweight Linux VM with Claude Code. Containers are persistent (reused across sessions) and auto-named by project directory. Images auto-rebuild when the Dockerfile changes.

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

## Quick Start

```bash
# First time? Run setup to check prerequisites and configure authentication
agentbox setup

# Start interactive Claude session
agentbox

# Headless task
agentbox "fix the failing tests"

# Use a different agent
agentbox codex
agentbox codex "fix the failing tests"
```

## Authentication

macOS Keychain isn't reachable from inside the Linux container. Run `agentbox setup` to configure authentication — it will guide you through the options.

**Simplest method (Pro/Max subscription):** run `agentbox`, type `/login` inside Claude, and complete the browser flow. The login persists across all sessions via the `~/.claude` mount.

<details>
<summary>Other authentication methods</summary>

**OAuth token (`CLAUDE_CODE_OAUTH_TOKEN`).** Best for headless machines, CI, or automated provisioning.

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

**API key (`ANTHROPIC_API_KEY`).** For Console (pay-as-you-go) billing.

1. Export the key in your shell profile:

   ```bash
   export ANTHROPIC_API_KEY="sk-..."
   ```

2. Tell agentbox to pass it into the container:

   ```toml
   # ~/.config/agentbox/config.toml
   [env]
   ANTHROPIC_API_KEY = ""  # empty = inherit from host env
   ```

**OpenAI Codex.** Run `agentbox codex`. On an unauthenticated container, pick the device-code sign-in flow, then open the URL on your Mac and enter the code shown in the terminal. Auth persists via the `~/.codex` mount. `agentbox setup` detects and warns about credential store issues (e.g. keyring mode that can't work inside Linux).

</details>

## Configuration

Created automatically by `agentbox setup`. You can also create or reset it with `agentbox config init`.

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

# Default flags for each agent
[cli.claude]
flags = ["--dangerously-skip-permissions"]

[cli.codex]
flags = ["--dangerously-bypass-approvals-and-sandbox"]

# Named profiles
[profiles.mystack]
dockerfile = "/path/to/mystack.Dockerfile"
```

### Passing flags to the coding agent

Use `--` to pass flags through to the underlying CLI:

```bash
agentbox -- --model sonnet
agentbox "fix the tests" -- --append-system-prompt "Be concise."
agentbox codex -- -c model_reasoning_effort=high
```

For flags you want every time, use `[cli.claude]` or `[cli.codex]` in config (see above). Config flags and `--` flags are merged — config first, `--` after.

### Volumes

Additional volumes can be mounted via config (`volumes = [...]`) or per-invocation:

```bash
agentbox --mount ~/.config/worktrunk --mount /path/to/other/dir
```

Three path formats are supported:

| Format | Example | Behavior |
|--------|---------|----------|
| Tilde prefix | `~/.config/foo` | Host `~/` → container `/home/user/` |
| Absolute path | `/some/path` | Same path in container |
| Explicit mapping | `/source:/dest` | Custom source → dest |

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

### Global

Set `dockerfile` in config to use a custom Dockerfile for all projects:

```toml
# ~/.config/agentbox/config.toml
dockerfile = "~/.config/agentbox/Dockerfile.custom"
```

<details>
<summary>Example: multi-language development environment</summary>

```dockerfile
FROM agentbox:default

# Build dependencies for Ruby/Python compilation via asdf
RUN sudo apt-get update \
    && sudo apt-get install -y --no-install-recommends \
        build-essential pkg-config autoconf patch rustc xz-utils \
        libssl-dev zlib1g-dev libyaml-dev libffi-dev libgmp-dev \
        libreadline-dev libbz2-dev libsqlite3-dev libncurses-dev liblzma-dev libgdbm-dev \
    && sudo rm -rf /var/lib/apt/lists/*

# Install worktrunk (git worktree manager for AI agent workflows)
RUN ARCH=$(uname -m) \
    && sudo mkdir -p /tmp/worktrunk \
    && sudo curl -fL "https://github.com/max-sixty/worktrunk/releases/download/v0.27.0/worktrunk-${ARCH}-unknown-linux-musl.tar.xz" \
        | sudo tar -xJ --strip-components=1 -C /tmp/worktrunk/ \
    && sudo cp /tmp/worktrunk/wt /tmp/worktrunk/git-wt /usr/local/bin/ \
    && sudo chmod +x /usr/local/bin/wt /usr/local/bin/git-wt \
    && sudo rm -rf /tmp/worktrunk

# wt shell integration: make `wt switch` auto-cd in non-interactive bash
RUN wt config shell init bash | sudo tee /etc/wt-init.sh > /dev/null
ENV BASH_ENV=/etc/wt-init.sh

# Install asdf version manager (pre-built binary)
RUN mkdir -p ~/.local/bin \
    && ARCH=$(dpkg --print-architecture) \
    && curl -fsSL "https://github.com/asdf-vm/asdf/releases/download/v0.18.1/asdf-v0.18.1-linux-${ARCH}.tar.gz" \
        | tar xz -C ~/.local/bin/
ENV PATH="/home/user/.local/bin:/home/user/.asdf/shims:${PATH}"

# Install Node.js via asdf
RUN asdf plugin add nodejs \
    && asdf install nodejs 23.6.0 \
    && asdf set --home nodejs 23.6.0

# Install Ruby via asdf
RUN asdf plugin add ruby \
    && asdf install ruby 3.2.2 \
    && asdf set --home ruby 3.2.2

# Install Python via asdf
RUN asdf plugin add python \
    && asdf install python 3.13.9 \
    && asdf set --home python 3.13.9

# TypeScript LSP dependencies
RUN npm install -g typescript-language-server typescript yarn

# Defuddle (web content extractor CLI)
RUN npm install -g defuddle

# Ruby bundler
RUN gem install bundler -v 2.4.12

# Rust toolchain + rust-analyzer LSP
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/home/user/.cargo/bin:${PATH}"
RUN rustup component add rust-analyzer

# Git credential helper for GitHub
RUN git config --global credential.helper '!/usr/bin/env gh auth git-credential' \
    && git config --global url."https://github.com/".insteadOf "git@github.com:"

RUN echo 'export PATH="$HOME/.cargo/bin:$HOME/.asdf/shims:$HOME/.local/bin:$PATH"' >> ~/.profile
```

</details>

### Profiles

Define in config, use with `--profile`:

```bash
agentbox --profile mystack
```

## What's Mounted

| Host                | Container                 | Access     | Notes                                                         |
|---------------------|---------------------------|------------|---------------------------------------------------------------|
| Current directory   | Same path                 | read/write |                                                               |
| `~/.claude`         | `/home/user/.claude`      | read/write |                                                               |
| `~/.claude.json`    | `/tmp/claude-seed.json`   | read-only  | Seed only; `entrypoint.sh` `jq`-merges into `~/.claude.json` |
| `~/.codex`          | `/home/user/.codex`       | read/write |                                                               |
| Additional volumes  | Configured path           | read/write |                                                               |

<details>
<summary>Sharing screenshots with the container</summary>

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

</details>

## Container Management

```bash
# Open a bash shell in the container (no agent)
agentbox shell

# Run a one-shot command
agentbox shell -- npm test

# Show container status (CPU, memory, sessions)
# Refreshes every 2s on a TTY — exit with q or Ctrl+C
agentbox status

# Remove current project's container
agentbox rm

# Remove all agentbox containers
agentbox rm --all

# Force rebuild the image
agentbox build
```

## Host Command Execution (Experimental)

Run macOS host commands from inside the container. Useful for tools that can't run in Linux, like `xcodebuild` or `xcrun`.

```toml
# ~/.config/agentbox/config.toml
[bridge]
allowed_commands = ["xcodebuild", "xcrun", "adb"]
```

Only commands in `allowed_commands` can be executed. The bridge starts automatically when commands are configured and stops when the session ends.

Set `forward_not_found = true` to automatically forward any command not found in the container to the host.
