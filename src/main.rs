use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod config;
mod container;
mod git;
mod image;

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
    #[arg(long)]
    profile: Option<String>,

    /// Show container commands and build output
    #[arg(long)]
    verbose: bool,

    /// Additional volume mounts (host path, or host:container)
    #[arg(long)]
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
    /// Stop containers (by name, current project, or --all)
    Stop {
        /// Container names to stop
        names: Vec<String>,
        /// Stop all agentbox containers
        #[arg(long)]
        all: bool,
    },
    /// List all agentbox containers
    Ls,
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

fn create_and_run(
    name: &str,
    image_tag: &str,
    workdir: &str,
    config: &config::Config,
    task: Option<&str>,
    verbose: bool,
    extra_volumes: &[String],
) -> Result<()> {
    let home = dirs::home_dir().context("cannot determine home directory")?;

    let mut env_vars = build_env_vars(&config.env);

    // Git identity
    env_vars.extend(git::git_env_vars());

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
        interactive: task.is_none(),
        task: task.map(String::from),
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
                 Install it from: https://github.com/apple/container"
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

fn main() -> Result<()> {
    check_prerequisites()?;
    let cli = Cli::parse();

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
        Some(Commands::Stop { names, all }) => {
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
                container::stop(name, cli.verbose)?;
                println!("Stopped {}", name);
            }
            Ok(())
        }
        Some(Commands::Ls) => {
            container::list(cli.verbose)?;
            Ok(())
        }
        Some(Commands::Build { no_cache }) => {
            let config = config::Config::load()?;
            let cwd = std::env::current_dir()?;
            let (dockerfile_content, image_tag) =
                image::resolve_dockerfile(&cwd, cli.profile.as_deref(), &config)?;
            let cache_key = image_tag.replace(':', "-");
            image::ensure_base_image(&dockerfile_content, cli.verbose)?;
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
        None => {
            let config = config::Config::load()?;
            let cwd = std::env::current_dir()?;
            let cwd_str = cwd.to_string_lossy().to_string();
            let name = container::container_name(&cwd_str);
            let task_str = if cli.task.is_empty() {
                None
            } else {
                Some(cli.task.join(" "))
            };

            match container::status(&name)? {
                container::ContainerStatus::Running => {
                    let mut env_vars = build_env_vars(&config.env);
                    env_vars.extend(git::git_env_vars());
                    container::exec(&name, task_str.as_deref(), &env_vars, cli.verbose)?;
                }
                container::ContainerStatus::Stopped => {
                    let (dockerfile_content, image_tag) =
                        image::resolve_dockerfile(&cwd, cli.profile.as_deref(), &config)?;
                    let cache_key = image_tag.replace(':', "-");
                    if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
                        eprintln!("Image changed, recreating container...");
                        container::rm(&name, cli.verbose)?;
                        image::ensure_base_image(&dockerfile_content, cli.verbose)?;
                        image::build(&image_tag, &dockerfile_content, false, false, cli.verbose)?;
                        image::save_cache(&dockerfile_content, &cache_key, &image::cache_dir())?;
                        create_and_run(
                            &name,
                            &image_tag,
                            &cwd_str,
                            &config,
                            task_str.as_deref(),
                            cli.verbose,
                            &cli.mount,
                        )?;
                    } else {
                        container::start(&name, cli.verbose)?;
                        let mut env_vars = build_env_vars(&config.env);
                        env_vars.extend(git::git_env_vars());
                        container::exec(&name, task_str.as_deref(), &env_vars, cli.verbose)?;
                    }
                }
                container::ContainerStatus::NotFound => {
                    let (dockerfile_content, image_tag) =
                        image::resolve_dockerfile(&cwd, cli.profile.as_deref(), &config)?;
                    let cache_key = image_tag.replace(':', "-");
                    if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
                        eprintln!("Building image...");
                        image::ensure_base_image(&dockerfile_content, cli.verbose)?;
                        image::build(&image_tag, &dockerfile_content, false, false, cli.verbose)?;
                        image::save_cache(&dockerfile_content, &cache_key, &image::cache_dir())?;
                    }
                    create_and_run(
                        &name,
                        &image_tag,
                        &cwd_str,
                        &config,
                        task_str.as_deref(),
                        cli.verbose,
                        &cli.mount,
                    )?;
                }
            }
            Ok(())
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
    fn test_ls_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "ls"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Ls)));
    }

    #[test]
    fn test_stop_subcommand_no_args() {
        let cli = Cli::try_parse_from(["agentbox", "stop"]).unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Stop { ref names, all }) if names.is_empty() && !all)
        );
    }

    #[test]
    fn test_stop_subcommand_with_names() {
        let cli = Cli::try_parse_from([
            "agentbox",
            "stop",
            "agentbox-foo-abc123",
            "agentbox-bar-def456",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Stop { names, all }) => {
                assert_eq!(names, vec!["agentbox-foo-abc123", "agentbox-bar-def456"]);
                assert!(!all);
            }
            _ => panic!("expected Stop"),
        }
    }

    #[test]
    fn test_stop_subcommand_all() {
        let cli = Cli::try_parse_from(["agentbox", "stop", "--all"]).unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Stop { ref names, all }) if names.is_empty() && all)
        );
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
}
