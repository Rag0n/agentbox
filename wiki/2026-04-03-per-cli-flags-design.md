# Per-CLI Flags Design

## Problem

Users can pass CLI flags to the underlying coding agent via `--`, but there is no way to configure default flags that persist across invocations. Flags like `--append-system-prompt` or `--model` must be repeated every time.

`--append-system-prompt` matters here because it injects instructions into the system prompt, which carries higher authority than CLAUDE.md content (injected as a user message). When the CLI invocation is controlled programmatically, this is the natural way to provide persistent, high-priority instructions.

## Design

### Config: `[cli.<name>]` sections

A new `[cli.<name>]` section in `~/.config/agentbox/config.toml` holds per-CLI configuration. Each section contains a `flags` array of strings passed verbatim to that CLI.

```toml
[cli.claude]
flags = ["--append-system-prompt", "You are a careful code reviewer."]
```

Multiple CLIs can be configured independently:

```toml
[cli.claude]
flags = ["--append-system-prompt", "Be thorough.", "--model", "sonnet"]

[cli.codex]
flags = ["--full-auto"]
```

Only `cli.claude` is used today. Other CLI names are parsed and stored but ignored until agentbox supports launching them.

### CLI passthrough via `--`

Users can also pass flags at invocation time using the standard `--` separator:

```bash
# Interactive with extra flags
agentbox -- --model sonnet

# Headless with extra flags
agentbox "fix the tests" -- --append-system-prompt "Be careful."
```

Everything after `--` is forwarded to the underlying CLI.

### Flag ordering and precedence

Agentbox assembles flags in this order:

1. Hardcoded flags (`--dangerously-skip-permissions`)
2. Config flags from `[cli.claude].flags`
3. CLI passthrough flags (after `--`)
4. Task flag (`-p '<task>'`) if running headless

```bash
# Config: [cli.claude] flags = ["--append-system-prompt", "Be careful."]
# Invocation: agentbox "fix tests" -- --model sonnet
# Produces:
claude --dangerously-skip-permissions --append-system-prompt "Be careful." --model sonnet -p 'fix tests'
```

Later flags override earlier ones when the underlying CLI supports it.

### Data model changes

New structs in `config.rs`:

```rust
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct CliConfig {
    #[serde(default)]
    pub flags: Vec<String>,
}
```

Add to `Config`:

```rust
pub struct Config {
    // ... existing fields ...
    #[serde(default)]
    pub cli: HashMap<String, CliConfig>,
}
```

Helper method:

```rust
impl Config {
    pub fn cli_flags(&self, cli_name: &str) -> &[String] {
        self.cli
            .get(cli_name)
            .map(|c| c.flags.as_slice())
            .unwrap_or(&[])
    }
}
```

### Injection points

Two code paths launch Claude, and both need to inject flags.

The `container exec` path (`build_exec_args` in `container.rs`) currently hardcodes:

```rust
cmd.push_str("claude --dangerously-skip-permissions");
```

This changes to accept extra flags and splice them between `--dangerously-skip-permissions` and `-p`.

The `container run` path (`entrypoint.sh`) currently runs:

```bash
exec claude --dangerously-skip-permissions "$@"
```

Extra flags reach the entrypoint through an environment variable, `AGENTBOX_CLI_FLAGS`. The entrypoint reads it and splices the flags in:

```bash
# shellcheck disable=SC2086
exec claude --dangerously-skip-permissions $AGENTBOX_CLI_FLAGS "$@"
```

The unquoted expansion (`$AGENTBOX_CLI_FLAGS`) splits individual flags correctly. Agentbox sets this env var from the merged config and CLI passthrough flags.

### CLI argument parsing changes

The current clap setup uses `trailing_var_arg = true` for the task field. Supporting `--` means reworking argument parsing to separate agentbox's own args from passthrough args.

The approach: manually split `std::env::args()` at `--`. Everything before `--` goes to clap as normal. Everything after becomes passthrough flags.

```rust
struct Cli {
    #[arg(trailing_var_arg = true)]
    task: Vec<String>,

    // ... existing fields ...
}

// In main():
let raw_args: Vec<String> = std::env::args().collect();
let (agentbox_args, passthrough_flags) = split_at_double_dash(&raw_args);
let cli = Cli::parse_from(agentbox_args);
```

### Init template update

```toml
# Extra CLI flags passed to the coding agent
# [cli.claude]
# flags = ["--append-system-prompt", "Your instructions here"]
```

## Scope

This design adds only the flag passthrough mechanism. It does not:

- Add support for launching CLIs other than `claude`
- Validate that passed flags are recognized by the target CLI
- Interpret specific flags at the agentbox level (no special `model` field, for example)
