# agentbox Design Document

## Overview

agentbox is a CLI tool that runs AI coding agents (currently Claude Code) inside isolated Apple Containers on macOS. It provides filesystem sandboxing while giving the agent full access to the current project directory.

## Architecture

- **Language:** Rust
- **Runtime:** Apple Containers (macOS only, Apple Silicon, macOS 26+)
- **Container interaction:** CLI wrapper around `container` command, using JSON output for structured data where available
- **Distribution:** `cargo install`, pre-built binaries, Homebrew (future)

## CLI Interface

```
USAGE:
    agentbox [OPTIONS] [TASK]
    agentbox <COMMAND>

COMMANDS:
    rm        Remove the container for current project
    stop      Stop the container for current project
    ls        List all agentbox containers
    build     Force rebuild the container image
    config    Configuration management
      init    Generate config file with commented examples

OPTIONS:
    --profile <NAME>    Use a named profile from config
    --verbose           Print container commands being executed
    --help              Show help
    --version           Show version

EXAMPLES:
    agentbox                        # Interactive Claude session
    agentbox "fix the tests"        # Headless mode
    agentbox --profile mystack      # Use custom profile
    agentbox rm                     # Remove container
    agentbox ls                     # List all containers
    agentbox build                  # Force rebuild image
    agentbox config init            # Create config file
```

## Configuration

**Location:** `~/.config/agentbox/config.toml` (optional, all defaults built-in)

```toml
# Resources (auto-detected from host if not set)
# cpus = 4          # default: half of host cores
# memory = "8G"     # default: 8G

# Override the default Dockerfile
# dockerfile = "/path/to/my-default.Dockerfile"

# Environment variables to pass into container
# [env]
# GH_TOKEN = ""           # empty = inherit from host env
# LINEAR_API_KEY = "abc"  # literal value

# Named profiles with custom Dockerfiles
# [profiles.mystack]
# dockerfile = "/path/to/mystack.Dockerfile"
```

**Dockerfile resolution order:**
1. `./agentbox.Dockerfile` in project root (per-project)
2. `--profile <name>` flag
3. Top-level `dockerfile` in config.toml (global default override)
4. Built-in default (embedded in binary)

## Default Dockerfile

```dockerfile
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       git jq less procps curl sudo ca-certificates ripgrep \
    && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
       -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
       > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends gh \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash -G sudo user \
    && echo "user ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/user

USER user
WORKDIR /home/user

RUN curl -fsSL https://claude.ai/install.sh | bash

ENTRYPOINT ["claude", "--dangerously-skip-permissions"]
```

**Base image:** `debian:bookworm-slim` (~138 MB). Claude Code native installer downloads a self-contained binary — no Node.js needed.

**Custom Dockerfiles** can extend the default: `FROM agentbox:default`

## Container Lifecycle

**Naming:** `agentbox-{dir_name}-{path_hash_6chars}` (e.g. `agentbox-myapp-a3b2c1`)

**On `agentbox` (start/reattach):**
1. Container running? -> attach
2. Container exists but stopped? -> start, then attach
3. Container doesn't exist? -> build image if needed, create & run

**Volume mounts:**
- `$(pwd):$(pwd)` — project dir at same path
- `~/.claude:/home/user/.claude` — auth, sessions, settings
- `~/.claude.json:/home/user/.claude.json` — user config

**Headless mode (`agentbox "task"`):**
Same as above but passes `-p "task"` to claude. If container already running, exec into it.

**Resources:**
- CPU: half of host cores (auto-detected, configurable)
- Memory: 8G default (configurable)

## Image Build & Caching

**Cache location:** `~/.cache/agentbox/`
```
~/.cache/agentbox/
├── default.sha256
├── profiles/
│   └── mystack.sha256
└── projects/
    └── myapp.sha256
```

**Implicit builds:** On every `agentbox` run, hash the applicable Dockerfile, compare with stored checksum. Rebuild only on mismatch.

**Explicit builds:** `agentbox build` forces rebuild regardless of checksum.

**Image naming:**
- Default: `agentbox:default`
- Profile: `agentbox:profile-mystack`
- Per-project: `agentbox:project-myapp`

## Git Identity

Auto-detected from host via `git config --global user.name` and `git config --global user.email`. Injected into container as `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, `GIT_COMMITTER_NAME`, `GIT_COMMITTER_EMAIL` environment variables.

## Error Handling

- **Container name collisions:** Path hash suffix ensures uniqueness
- **Apple Container CLI missing:** Clear error with install instructions
- **Config file missing:** All defaults built-in, config is optional
- **Image changed:** Auto-recreate container, warn user
- **Headless on running container:** Exec into existing container

## Security Model

- Container provides filesystem isolation (only project dir + claude config mounted)
- Full network access (HTTP proxy for credential injection is a future enhancement)
- `--dangerously-skip-permissions` — container is the sandbox
- Non-root user with passwordless sudo inside container
- No access to `~/.ssh`, `~/.aws`, `~/.gnupg`, or other host directories

## Future Enhancements

- HTTP proxy with auto credential injection (for gh/git)
- Support for additional agents (Codex, Copilot, etc.)
- Homebrew formula
- Network allowlisting
- Linux support via Docker

## Installation

```bash
# Cargo
cargo install agentbox

# Pre-built binary (macOS ARM)
curl -fsSL https://github.com/<user>/agentbox/releases/latest/download/agentbox-darwin-arm64.tar.gz | tar xz
mv agentbox ~/.local/bin/

# Homebrew (coming soon)
brew install agentbox
```

## Project Structure

```
agentbox/
├── Cargo.toml
├── src/
│   ├── main.rs              # CLI entry point (clap)
│   ├── config.rs            # TOML config loading & defaults
│   ├── container.rs         # Apple Container CLI wrapper
│   ├── image.rs             # Image build logic (Dockerfile management)
│   └── git.rs               # Git user identity detection
├── resources/
│   └── Dockerfile.default   # Default base image (embedded in binary)
└── README.md
```
