use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

mod agent;
mod bridge;
mod config;
mod container;
mod git;
mod hostexec;
mod image;
mod notify;
mod setup;
mod status;

#[derive(Parser)]
#[command(
    name = "agentbox",
    about = "Run AI coding agents in isolated Apple Containers",
    version
)]
struct Cli {
    /// Task to run in headless mode
    #[arg(trailing_var_arg = true)]
    task: Vec<String>,

    /// Use a named profile from config
    #[arg(long, global = true)]
    profile: Option<String>,

    /// Show container commands and build output
    #[arg(long, global = true)]
    verbose: bool,

    /// Additional volume mounts (host path, or host:container)
    #[arg(long, global = true)]
    mount: Vec<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Remove containers (by name, current project, or --all)
    Rm {
        /// Container names to remove
        names: Vec<String>,
        /// Remove all agentbox containers
        #[arg(long)]
        all: bool,
    },
    /// Show rich container status (CPU, memory, project, sessions). On a
    /// TTY, refreshes every 2s until `q` or Ctrl+C.
    Status {
        /// Skip live mode even on a TTY — run a single snapshot and exit.
        #[arg(long)]
        no_stream: bool,
    },
    /// Force rebuild the container image (--no-cache for clean build)
    Build {
        /// Do not use cache when building the image
        #[arg(long)]
        no_cache: bool,
    },
    /// Configuration management
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Run the interactive setup wizard
    Setup,
    /// Open a bash shell in the container (no Claude)
    Shell,
    /// Run Claude Code (explicit; default when no subcommand is used)
    Claude {
        /// Task to run in headless mode
        #[arg(trailing_var_arg = true)]
        task: Vec<String>,
    },
    /// Run OpenAI Codex CLI
    Codex {
        /// Task to run in headless mode
        #[arg(trailing_var_arg = true)]
        task: Vec<String>,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Generate config file with commented examples
    Init,
}

fn build_env_vars(config_env: &std::collections::HashMap<String, String>) -> Vec<(String, String)> {
    let mut env_vars: Vec<(String, String)> = vec![
        ("COLORTERM".into(), "truecolor".into()),
        ("TERM".into(), "xterm-256color".into()),
    ];

    for (key, val) in config_env {
        let value = if val.is_empty() {
            std::env::var(key).unwrap_or_default()
        } else {
            val.clone()
        };
        if !value.is_empty() {
            env_vars.retain(|(k, _)| k != key);
            env_vars.push((key.clone(), value));
        }
    }

    env_vars
}

/// Resolve a volume spec into a "source:dest" string.
///
/// Rules:
/// - `~/.config/foo` → expand ~ to host home for source, /home/user for dest
/// - `/source:/dest` → pass through as-is (explicit mapping)
/// - `/absolute/path` → mount at same path in container
fn resolve_volume(spec: &str) -> Result<String> {
    if let Some(suffix) = spec.strip_prefix('~') {
        let home = dirs::home_dir().context("cannot determine home directory")?;
        let suffix = suffix.strip_prefix('/').unwrap_or(suffix);
        let source = home.join(suffix);
        let dest = format!("/home/user/{}", suffix);
        Ok(format!("{}:{}", source.display(), dest))
    } else if spec.contains(':') {
        Ok(spec.to_string())
    } else {
        Ok(format!("{}:{}", spec, spec))
    }
}

fn build_codex_mount(home: &std::path::Path) -> String {
    format!("{}:/home/user/.codex", home.join(".codex").display())
}

#[allow(clippy::too_many_arguments)]
fn create_and_run(
    name: &str,
    image_tag: &str,
    workdir: &str,
    config: &config::Config,
    mode: container::RunMode,
    verbose: bool,
    extra_volumes: &[String],
    bridge_handle: Option<&bridge::BridgeHandle>,
) -> Result<i32> {
    let home = dirs::home_dir().context("cannot determine home directory")?;

    let env_vars = build_all_env_vars(config, bridge_handle);

    // Ensure ~/.claude exists on host before mounting
    let claude_dir = home.join(".claude");
    if !claude_dir.exists() {
        std::fs::create_dir_all(&claude_dir)?;
    }

    let mut volumes = vec![
        format!("{}:{}", workdir, workdir),
        format!("{}:/home/user/.claude", claude_dir.display()),
    ];

    // Also mount at the host path so absolute paths in plugin configs resolve correctly
    let home_str = home.to_string_lossy();
    if home_str != "/home/user" {
        volumes.push(format!("{}:{}", claude_dir.display(), claude_dir.display()));
    }

    // Ensure ~/.codex exists on host before mounting
    let codex_dir = home.join(".codex");
    if !codex_dir.exists() {
        std::fs::create_dir_all(&codex_dir)?;
    }
    volumes.push(build_codex_mount(&home));

    // Seed container with host's .claude.json (read-only to avoid conflicts)
    let claude_json = home.join(".claude.json");
    if claude_json.exists() {
        volumes.push(format!(
            "{}:/tmp/claude-seed.json:ro",
            claude_json.display()
        ));
    }

    // Collect existing destination paths for deduplication
    let mut seen_dests: std::collections::HashSet<String> = volumes
        .iter()
        .filter_map(|v| {
            let parts: Vec<&str> = v.splitn(3, ':').collect();
            parts.get(1).map(|s| s.to_string())
        })
        .collect();

    // Append config volumes + CLI mounts, skipping duplicates
    for spec in config.volumes.iter().chain(extra_volumes.iter()) {
        let resolved = resolve_volume(spec)?;
        let parts: Vec<&str> = resolved.splitn(3, ':').collect();
        let source = parts.first().unwrap_or(&"");
        let dest = parts.get(1).unwrap_or(&"");
        if !std::path::Path::new(source).exists() {
            eprintln!(
                "[agentbox] warning: mount source does not exist: {}",
                source
            );
        }
        if seen_dests.insert(dest.to_string()) {
            volumes.push(resolved);
        }
    }

    let opts = container::RunOpts {
        name: name.into(),
        image: image_tag.into(),
        workdir: workdir.into(),
        cpus: config.effective_cpus(),
        memory: config.memory.clone(),
        env_vars,
        volumes,
        mode,
    };

    container::run(&opts, verbose)
}

fn check_prerequisites() -> Result<()> {
    let output = std::process::Command::new("container")
        .args(["system", "status"])
        .output();

    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let is_running = stdout.lines().any(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                parts.len() == 2 && parts[0] == "status" && parts[1] == "running"
            });
            if is_running {
                return Ok(());
            }
            // command found but system not running – try starting below
        }
        Err(_) => {
            anyhow::bail!(
                "Apple Container CLI is not installed.\n\n\
                 Run `agentbox setup` for guided setup, or install manually from:\n\
                 https://github.com/apple/container"
            );
        }
    }

    eprintln!("[agentbox] container system not running, starting it...");
    let start = std::process::Command::new("container")
        .args(["system", "start"])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .context("failed to run 'container system start'")?;

    if !start.success() {
        anyhow::bail!(
            "Failed to start container system.\n\
             Try running manually: container system start"
        );
    }
    Ok(())
}

/// Detect the host IP that containers can reach.
/// Apple Containers use 192.168.64.1 as the default gateway.
fn detect_host_ip(config: &config::BridgeConfig) -> String {
    config
        .host_ip
        .clone()
        .unwrap_or_else(|| "192.168.64.1".to_string())
}

fn build_all_env_vars(
    config: &config::Config,
    bridge_handle: Option<&bridge::BridgeHandle>,
) -> Vec<(String, String)> {
    let mut env_vars = build_env_vars(&config.env);
    env_vars.extend(git::git_env_vars());
    if let Some(handle) = bridge_handle {
        env_vars.push(("HOSTEXEC_HOST".into(), detect_host_ip(&config.bridge)));
        env_vars.push(("HOSTEXEC_PORT".into(), handle.port.to_string()));
        env_vars.push(("HOSTEXEC_TOKEN".into(), handle.token.clone()));
        env_vars.push((
            "HOSTEXEC_COMMANDS".into(),
            handle.commands_env(&config.bridge),
        ));
        if config.bridge.forward_not_found {
            env_vars.push(("HOSTEXEC_FORWARD_NOT_FOUND".into(), "true".into()));
        }
    }
    env_vars
}

/// Suppress SIGHUP and SIGTERM so the process survives long enough
/// to run cleanup after the child container process exits.
/// The child process (container exec/run) still receives these signals
/// via the process group and exits normally.
fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
}

fn split_at_double_dash(args: Vec<String>) -> (Vec<String>, Vec<String>) {
    if let Some(pos) = args.iter().position(|a| a == "--") {
        let (before, after) = args.split_at(pos);
        (before.to_vec(), after[1..].to_vec())
    } else {
        (args, vec![])
    }
}

fn run_session(
    cli: &Cli,
    config: &config::Config,
    mode: container::RunMode,
) -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let cwd_str = cwd.to_string_lossy().to_string();
    let name = container::container_name(&cwd_str);

    let bridge_handle = if !config.bridge.allowed_commands.is_empty() {
        match bridge::start_bridge(&config.bridge, &cwd_str) {
            Ok(handle) => {
                if cli.verbose {
                    eprintln!(
                        "[agentbox] bridge started on port {} ({} commands allowed)",
                        handle.port,
                        config.bridge.allowed_commands.len()
                    );
                }
                Some(handle)
            }
            Err(e) => {
                eprintln!("[agentbox] warning: bridge failed to start: {}", e);
                None
            }
        }
    } else {
        None
    };

    install_signal_handlers();

    let result = match container::status(&name)? {
        container::ContainerStatus::Running => {
            let env_vars = build_all_env_vars(config, bridge_handle.as_ref());
            container::exec(&name, &mode, &env_vars, cli.verbose)
        }
        container::ContainerStatus::Stopped => {
            let (dockerfile_content, image_tag) =
                image::resolve_dockerfile(&cwd, cli.profile.as_deref(), config)?;
            let cache_key = image_tag.replace(':', "-");
            let did_build = if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
                eprintln!("Image changed, recreating container...");
                container::rm(&name, cli.verbose)?;
                notify::run_build(
                    config,
                    &dockerfile_content,
                    &image_tag,
                    &cache_key,
                    false,
                    false,
                    cli.verbose,
                )?;
                true
            } else {
                false
            };

            if did_build {
                notify::send_success(config);
                create_and_run(
                    &name,
                    &image_tag,
                    &cwd_str,
                    config,
                    mode.clone(),
                    cli.verbose,
                    &cli.mount,
                    bridge_handle.as_ref(),
                )
            } else {
                container::start(&name, cli.verbose)?;
                let env_vars = build_all_env_vars(config, bridge_handle.as_ref());
                container::exec(&name, &mode, &env_vars, cli.verbose)
            }
        }
        container::ContainerStatus::NotFound => {
            let (dockerfile_content, image_tag) =
                image::resolve_dockerfile(&cwd, cli.profile.as_deref(), config)?;
            let cache_key = image_tag.replace(':', "-");
            if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
                eprintln!("Building image...");
                image::ensure_base_image(&dockerfile_content, false, cli.verbose)?;
                image::build(&image_tag, &dockerfile_content, false, false, cli.verbose)?;
                image::save_cache(&dockerfile_content, &cache_key, &image::cache_dir())?;
            }
            create_and_run(
                &name,
                &image_tag,
                &cwd_str,
                config,
                mode,
                cli.verbose,
                &cli.mount,
                bridge_handle.as_ref(),
            )
        }
    };

    container::maybe_stop_container(&name, cli.verbose);

    result
}

fn run_agent(
    cli: &Cli,
    config: &config::Config,
    agent: agent::CodingAgent,
    task_tokens: Vec<String>,
    passthrough_flags: Vec<String>,
) -> Result<()> {
    let task_str = if task_tokens.is_empty() {
        None
    } else {
        Some(task_tokens.join(" "))
    };

    let mut cli_flags: Vec<String> = config.cli_flags(agent.config_key()).to_vec();
    cli_flags.extend(passthrough_flags);

    let mode = container::RunMode::Agent {
        agent,
        task: task_str,
        cli_flags,
    };

    let code = run_session(cli, config, mode)?;
    if code != 0 {
        bail!("container exited with status {}", code);
    }
    Ok(())
}

fn main() -> Result<()> {
    // Check if we're invoked as hostexec (symlink mode)
    let binary_name = std::env::args()
        .next()
        .and_then(|a| {
            std::path::Path::new(&a)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
        })
        .unwrap_or_default();

    if binary_name == "hostexec" {
        std::process::exit(hostexec::run(None));
    } else if binary_name != "agentbox" && !binary_name.is_empty() {
        // Invoked via a symlink like "xcodebuild" -> hostexec
        // But only if HOSTEXEC_HOST is set (we're in a container)
        if std::env::var("HOSTEXEC_HOST").is_ok() {
            std::process::exit(hostexec::run(Some(binary_name)));
        }
    }

    check_prerequisites()?;
    let raw_args: Vec<String> = std::env::args().collect();
    let (agentbox_args, passthrough_flags) = split_at_double_dash(raw_args);
    let cli = Cli::parse_from(agentbox_args);

    match cli.command {
        Some(Commands::Rm { names, all }) => {
            let targets = if all {
                let all_names = container::list_names(cli.verbose)?;
                if all_names.is_empty() {
                    println!("No agentbox containers found.");
                    return Ok(());
                }
                all_names
            } else if names.is_empty() {
                let cwd = std::env::current_dir()?;
                vec![container::container_name(&cwd.to_string_lossy())]
            } else {
                names
            };
            for name in &targets {
                container::rm(name, cli.verbose)?;
                println!("Removed {}", name);
            }
            Ok(())
        }
        Some(Commands::Status { no_stream }) => {
            use std::io::IsTerminal;
            let is_tty = std::io::stdout().is_terminal();
            if is_tty && !no_stream {
                status::live::run(cli.verbose)?;
            } else {
                status::run(cli.verbose)?;
            }
            Ok(())
        }
        Some(Commands::Build { no_cache }) => {
            let config = config::Config::load()?;
            let cwd = std::env::current_dir()?;
            let (dockerfile_content, image_tag) =
                image::resolve_dockerfile(&cwd, cli.profile.as_deref(), &config)?;
            let cache_key = image_tag.replace(':', "-");
            image::ensure_base_image(&dockerfile_content, no_cache, cli.verbose)?;
            eprintln!("Building {}...", image_tag);
            image::build(&image_tag, &dockerfile_content, no_cache, true, cli.verbose)?;
            image::save_cache(&dockerfile_content, &cache_key, &image::cache_dir())?;
            println!("Built {}", image_tag);
            Ok(())
        }
        Some(Commands::Config { command }) => match command {
            ConfigCommands::Init => {
                let path = config::Config::config_path();
                if path.exists() {
                    anyhow::bail!(
                        "Config already exists at {}\nEdit it directly or remove it first.",
                        path.display()
                    );
                }
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, config::Config::init_template())?;
                println!("Created {}", path.display());
                Ok(())
            }
        },
        Some(Commands::Setup) => {
            setup::run_setup()?;
            Ok(())
        }
        Some(Commands::Shell) => {
            let config = config::Config::load()?;
            let mode = container::RunMode::Shell {
                cmd: passthrough_flags,
            };
            let code = run_session(&cli, &config, mode)?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        Some(Commands::Claude { ref task }) => {
            let config = config::Config::load()?;
            run_agent(
                &cli,
                &config,
                agent::CodingAgent::Claude,
                task.clone(),
                passthrough_flags,
            )
        }
        Some(Commands::Codex { ref task }) => {
            let config = config::Config::load()?;
            run_agent(
                &cli,
                &config,
                agent::CodingAgent::Codex,
                task.clone(),
                passthrough_flags,
            )
        }
        None => {
            let config = config::Config::load()?;
            let agent = config.resolve_default_agent()?;
            run_agent(&cli, &config, agent, cli.task.clone(), passthrough_flags)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_no_args_parses_as_interactive() {
        let cli = Cli::try_parse_from(["agentbox"]).unwrap();
        assert!(cli.command.is_none());
        assert!(cli.task.is_empty());
    }

    #[test]
    fn test_task_arg_parses_as_headless() {
        let cli = Cli::try_parse_from(["agentbox", "fix the tests"]).unwrap();
        assert_eq!(cli.task, vec!["fix the tests"]);
    }

    #[test]
    fn test_rm_subcommand_no_args() {
        let cli = Cli::try_parse_from(["agentbox", "rm"]).unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Rm { ref names, all }) if names.is_empty() && !all)
        );
    }

    #[test]
    fn test_rm_subcommand_with_names() {
        let cli = Cli::try_parse_from([
            "agentbox",
            "rm",
            "agentbox-foo-abc123",
            "agentbox-bar-def456",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Rm { names, all }) => {
                assert_eq!(names, vec!["agentbox-foo-abc123", "agentbox-bar-def456"]);
                assert!(!all);
            }
            _ => panic!("expected Rm"),
        }
    }

    #[test]
    fn test_rm_subcommand_all() {
        let cli = Cli::try_parse_from(["agentbox", "rm", "--all"]).unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Rm { ref names, all }) if names.is_empty() && all)
        );
    }

    #[test]
    fn test_status_subcommand() {
        let cli = Cli::parse_from(&["agentbox", "status"]);
        assert!(matches!(cli.command, Some(Commands::Status { no_stream: false })));
    }

    #[test]
    fn test_status_no_stream_flag_parses() {
        let cli = Cli::parse_from(&["agentbox", "status", "--no-stream"]);
        match cli.command {
            Some(Commands::Status { no_stream }) => assert!(no_stream),
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn test_status_default_has_no_stream_false() {
        let cli = Cli::parse_from(&["agentbox", "status"]);
        match cli.command {
            Some(Commands::Status { no_stream }) => assert!(!no_stream),
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn test_build_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "build"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Build { no_cache: false })
        ));
    }

    #[test]
    fn test_build_subcommand_no_cache() {
        let cli = Cli::try_parse_from(["agentbox", "build", "--no-cache"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Build { no_cache: true })
        ));
    }

    #[test]
    fn test_config_init_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "config", "init"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Config {
                command: ConfigCommands::Init
            })
        ));
    }

    #[test]
    fn test_profile_flag() {
        let cli = Cli::try_parse_from(["agentbox", "--profile", "mystack"]).unwrap();
        assert_eq!(cli.profile, Some("mystack".to_string()));
    }

    #[test]
    fn test_verbose_flag() {
        let cli = Cli::try_parse_from(["agentbox", "--verbose"]).unwrap();
        assert!(cli.verbose);
    }

    #[test]
    fn test_build_env_vars_defaults() {
        let env = build_env_vars(&std::collections::HashMap::new());
        assert!(env
            .iter()
            .any(|(k, v)| k == "COLORTERM" && v == "truecolor"));
        assert!(env
            .iter()
            .any(|(k, v)| k == "TERM" && v == "xterm-256color"));
    }

    #[test]
    fn test_build_env_vars_config_literal_overrides_default() {
        let mut config_env = std::collections::HashMap::new();
        config_env.insert("TERM".into(), "vt100".into());
        let env = build_env_vars(&config_env);
        let term_values: Vec<_> = env.iter().filter(|(k, _)| k == "TERM").collect();
        assert_eq!(term_values.len(), 1);
        assert_eq!(term_values[0].1, "vt100");
    }

    #[test]
    fn test_build_env_vars_config_empty_inherits_from_host() {
        std::env::set_var("AGENTBOX_TEST_VAR", "from_host");
        let mut config_env = std::collections::HashMap::new();
        config_env.insert("AGENTBOX_TEST_VAR".into(), "".into());
        let env = build_env_vars(&config_env);
        std::env::remove_var("AGENTBOX_TEST_VAR");
        assert!(env
            .iter()
            .any(|(k, v)| k == "AGENTBOX_TEST_VAR" && v == "from_host"));
    }

    #[test]
    fn test_build_env_vars_config_empty_no_host_keeps_default() {
        std::env::remove_var("COLORTERM");
        let mut config_env = std::collections::HashMap::new();
        config_env.insert("COLORTERM".into(), "".into());
        let env = build_env_vars(&config_env);
        // Empty config with no host var → default survives
        assert!(env
            .iter()
            .any(|(k, v)| k == "COLORTERM" && v == "truecolor"));
    }

    #[test]
    fn test_mount_flag_single() {
        let cli = Cli::try_parse_from(["agentbox", "--mount", "/some/path"]).unwrap();
        assert_eq!(cli.mount, vec!["/some/path"]);
    }

    #[test]
    fn test_mount_flag_multiple() {
        let cli = Cli::try_parse_from([
            "agentbox",
            "--mount",
            "~/.config/foo",
            "--mount",
            "/other/path",
        ])
        .unwrap();
        assert_eq!(cli.mount.len(), 2);
    }

    #[test]
    fn test_mount_flag_default_empty() {
        let cli = Cli::try_parse_from(["agentbox"]).unwrap();
        assert!(cli.mount.is_empty());
    }

    #[test]
    fn test_resolve_volume_tilde_path() {
        let home = dirs::home_dir().unwrap();
        let resolved = resolve_volume("~/.config/worktrunk").unwrap();
        let expected = format!(
            "{}:/home/user/.config/worktrunk",
            home.join(".config/worktrunk").display()
        );
        assert_eq!(resolved, expected);
    }

    #[test]
    fn test_resolve_volume_absolute_path() {
        let resolved = resolve_volume("/Users/alex/Dev/marketplace").unwrap();
        assert_eq!(
            resolved,
            "/Users/alex/Dev/marketplace:/Users/alex/Dev/marketplace"
        );
    }

    #[test]
    fn test_resolve_volume_explicit_mapping() {
        let resolved = resolve_volume("/source/path:/dest/path").unwrap();
        assert_eq!(resolved, "/source/path:/dest/path");
    }

    #[test]
    fn test_resolve_volume_tilde_only() {
        let home = dirs::home_dir().unwrap();
        let resolved = resolve_volume("~/mydir").unwrap();
        let expected = format!("{}:/home/user/mydir", home.join("mydir").display());
        assert_eq!(resolved, expected);
    }

    #[test]
    fn test_volume_deduplication() {
        // Simulate the dedup logic: CWD volume already present, config adds same path
        let workdir = "/Users/alex/Dev/marketplace";
        let mut volumes = vec![format!("{}:{}", workdir, workdir)];

        let mut seen_dests: std::collections::HashSet<String> = volumes
            .iter()
            .filter_map(|v| {
                let parts: Vec<&str> = v.splitn(3, ':').collect();
                parts.get(1).map(|s| s.to_string())
            })
            .collect();

        // This should be skipped (same dest as CWD)
        let resolved = resolve_volume(workdir).unwrap();
        let parts: Vec<&str> = resolved.splitn(3, ':').collect();
        let dest = parts.get(1).unwrap_or(&"");
        if seen_dests.insert(dest.to_string()) {
            volumes.push(resolved);
        }

        // This should be added (different dest)
        let resolved2 = resolve_volume("/other/path").unwrap();
        let parts2: Vec<&str> = resolved2.splitn(3, ':').collect();
        let dest2 = parts2.get(1).unwrap_or(&"");
        if seen_dests.insert(dest2.to_string()) {
            volumes.push(resolved2);
        }

        assert_eq!(volumes.len(), 2); // CWD + /other/path, not 3
        assert_eq!(volumes[0], format!("{}:{}", workdir, workdir));
        assert_eq!(volumes[1], "/other/path:/other/path");
    }

    #[test]
    fn test_split_at_double_dash_with_separator() {
        let args = vec![
            "fix".to_string(),
            "the".to_string(),
            "tests".to_string(),
            "--".to_string(),
            "--model".to_string(),
            "sonnet".to_string(),
        ];
        let (task, flags) = split_at_double_dash(args);
        assert_eq!(task, vec!["fix", "the", "tests"]);
        assert_eq!(flags, vec!["--model", "sonnet"]);
    }

    #[test]
    fn test_split_at_double_dash_no_separator() {
        let args = vec!["fix".to_string(), "tests".to_string()];
        let (task, flags) = split_at_double_dash(args);
        assert_eq!(task, vec!["fix", "tests"]);
        assert!(flags.is_empty());
    }

    #[test]
    fn test_split_at_double_dash_empty() {
        let (task, flags) = split_at_double_dash(vec![]);
        assert!(task.is_empty());
        assert!(flags.is_empty());
    }

    #[test]
    fn test_split_at_double_dash_only_flags() {
        let args = vec![
            "--".to_string(),
            "--model".to_string(),
            "sonnet".to_string(),
        ];
        let (task, flags) = split_at_double_dash(args);
        assert!(task.is_empty());
        assert_eq!(flags, vec!["--model", "sonnet"]);
    }

    #[test]
    fn test_split_at_double_dash_separator_at_end() {
        let args = vec!["fix".to_string(), "tests".to_string(), "--".to_string()];
        let (task, flags) = split_at_double_dash(args);
        assert_eq!(task, vec!["fix", "tests"]);
        assert!(flags.is_empty());
    }

    #[test]
    fn test_shell_subcommand_no_args() {
        let cli = Cli::try_parse_from(["agentbox", "shell"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Shell)));
    }

    #[test]
    fn test_shell_subcommand_with_passthrough() {
        let raw_args: Vec<String> = vec![
            "agentbox".into(),
            "shell".into(),
            "--".into(),
            "ls".into(),
            "-la".into(),
        ];
        let (agentbox_args, passthrough_flags) = split_at_double_dash(raw_args);
        let cli = Cli::try_parse_from(agentbox_args).unwrap();
        assert!(matches!(cli.command, Some(Commands::Shell)));
        assert_eq!(passthrough_flags, vec!["ls", "-la"]);
    }

    #[test]
    fn test_shell_subcommand_with_profile_and_passthrough() {
        let raw_args: Vec<String> = vec![
            "agentbox".into(),
            "shell".into(),
            "--profile".into(),
            "mystack".into(),
            "--".into(),
            "npm".into(),
            "test".into(),
        ];
        let (agentbox_args, passthrough_flags) = split_at_double_dash(raw_args);
        let cli = Cli::try_parse_from(agentbox_args).unwrap();
        assert!(matches!(cli.command, Some(Commands::Shell)));
        assert_eq!(cli.profile, Some("mystack".into()));
        assert_eq!(passthrough_flags, vec!["npm", "test"]);
    }

    #[test]
    fn test_shell_subcommand_with_verbose() {
        let cli = Cli::try_parse_from(["agentbox", "shell", "--verbose"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Shell)));
        assert!(cli.verbose);
    }

    #[test]
    fn test_claude_subcommand_no_task() {
        let cli = Cli::try_parse_from(["agentbox", "claude"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Claude { ref task }) if task.is_empty()
        ));
    }

    #[test]
    fn test_claude_subcommand_with_task() {
        let cli =
            Cli::try_parse_from(["agentbox", "claude", "fix", "the", "tests"]).unwrap();
        match cli.command {
            Some(Commands::Claude { task }) => {
                assert_eq!(task, vec!["fix", "the", "tests"]);
            }
            _ => panic!("expected Claude subcommand"),
        }
    }

    #[test]
    fn test_codex_subcommand_no_task() {
        let cli = Cli::try_parse_from(["agentbox", "codex"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Codex { ref task }) if task.is_empty()
        ));
    }

    #[test]
    fn test_codex_subcommand_with_task() {
        let cli = Cli::try_parse_from(["agentbox", "codex", "fix tests"]).unwrap();
        match cli.command {
            Some(Commands::Codex { task }) => {
                assert_eq!(task, vec!["fix tests"]);
            }
            _ => panic!("expected Codex subcommand"),
        }
    }

    #[test]
    fn test_codex_subcommand_with_passthrough_flags() {
        let raw_args: Vec<String> = vec![
            "agentbox".into(),
            "codex".into(),
            "fix".into(),
            "--".into(),
            "-c".into(),
            "model_reasoning_effort=high".into(),
        ];
        let (agentbox_args, passthrough) = split_at_double_dash(raw_args);
        let cli = Cli::try_parse_from(agentbox_args).unwrap();
        assert!(matches!(cli.command, Some(Commands::Codex { ref task }) if task == &vec!["fix"]));
        assert_eq!(
            passthrough,
            vec!["-c", "model_reasoning_effort=high"]
        );
    }

    #[test]
    fn test_bare_agentbox_uses_config_default_agent() {
        use crate::agent::CodingAgent;
        // default_agent omitted → Claude
        let c = config::Config::default();
        assert_eq!(c.resolve_default_agent().unwrap(), CodingAgent::Claude);

        // default_agent = codex → Codex
        let mut c = config::Config::default();
        c.default_agent = Some("codex".into());
        assert_eq!(c.resolve_default_agent().unwrap(), CodingAgent::Codex);
    }

    #[test]
    fn test_codex_mount_added_by_create_and_run_pipeline() {
        // We verify the mount path assembly, not the full `container run` call.
        // A pure helper keeps this testable without I/O.
        let home = dirs::home_dir().unwrap();
        let codex_mount = build_codex_mount(&home);
        let expected = format!(
            "{}:/home/user/.codex",
            home.join(".codex").display()
        );
        assert_eq!(codex_mount, expected);
    }
}
