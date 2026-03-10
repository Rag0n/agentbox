# agentbox

Run AI coding agents in isolated Apple Containers. Your project directory is mounted read/write — everything else on your filesystem is inaccessible.

Currently supports Claude Code. More agents planned.

## Requirements

- macOS 26+ on Apple Silicon
- [Apple Container CLI](https://github.com/apple/container)

## Install

### Cargo

```bash
cargo install agentbox
```

### Pre-built binary

```bash
curl -fsSL https://github.com/<user>/agentbox/releases/latest/download/agentbox-darwin-arm64.tar.gz | tar xz
mv agentbox ~/.local/bin/
```

### Homebrew (coming soon)

```bash
brew install agentbox
```

## Quick Start

```bash
# Start interactive Claude session in current project
agentbox

# Run a task headlessly
agentbox "fix the failing tests"

# List all containers
agentbox ls

# Stop the container
agentbox stop

# Remove the container
agentbox rm

# Force rebuild the image
agentbox build
```

## Configuration

Optional. Create with `agentbox config init`.

Located at `~/.config/agentbox/config.toml`:

```toml
# Resources
cpus = 4          # default: half of host cores
memory = "8G"     # default: 8G

# Override default Dockerfile
dockerfile = "/path/to/my.Dockerfile"

# Environment variables passed into container
[env]
GH_TOKEN = ""           # empty = inherit from host
LINEAR_API_KEY = "abc"  # literal value

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

## What's Mounted

| Host | Container | Access |
|------|-----------|--------|
| Current directory | Same path | read/write |
| `~/.claude` | `/home/user/.claude` | read/write |
| `~/.claude.json` | `/home/user/.claude.json` | read/write |

## What's Isolated

Claude **cannot** access `~/.ssh`, `~/.aws`, `~/.gnupg`, or any other host directory.

## How It Works

agentbox uses Apple Containers to run a lightweight Linux VM with Claude Code. Containers are persistent (reused across sessions) and auto-named by project directory. Images auto-rebuild when the Dockerfile changes.
