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

    /// Print container commands being executed
    #[arg(long)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Remove the container for current project
    Rm,
    /// Stop the container for current project
    Stop,
    /// List all agentbox containers
    Ls,
    /// Force rebuild the container image
    Build,
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

fn create_and_run(
    name: &str,
    image_tag: &str,
    workdir: &str,
    config: &config::Config,
    task: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let home = dirs::home_dir().context("cannot determine home directory")?;

    let mut env_vars: Vec<(String, String)> = Vec::new();

    // Config env vars (empty value = inherit from host)
    for (key, val) in &config.env {
        let value = if val.is_empty() {
            std::env::var(key).unwrap_or_default()
        } else {
            val.clone()
        };
        if !value.is_empty() {
            env_vars.push((key.clone(), value));
        }
    }

    // Git identity
    env_vars.extend(git::git_env_vars());

    // Ensure ~/.claude.json exists
    let claude_json = home.join(".claude.json");
    if !claude_json.exists() {
        std::fs::write(&claude_json, "{}")?;
    }

    let opts = container::RunOpts {
        name: name.into(),
        image: image_tag.into(),
        workdir: workdir.into(),
        cpus: config.effective_cpus(),
        memory: config.memory.clone(),
        env_vars,
        volumes: vec![
            format!("{}:{}", workdir, workdir),
            format!("{}:/home/user/.claude", home.join(".claude").display()),
            format!("{}:/home/user/.claude.json", claude_json.display()),
        ],
        interactive: task.is_none(),
        task: task.map(String::from),
    };

    container::run(&opts, verbose)
}

fn check_prerequisites() -> Result<()> {
    let output = std::process::Command::new("container")
        .args(["system", "version"])
        .output();

    match output {
        Ok(o) if o.status.success() => Ok(()),
        _ => {
            eprintln!("Error: Apple Container CLI is not installed or not running.");
            eprintln!();
            eprintln!("Install it from: https://github.com/apple/container");
            eprintln!("Then run: container system start");
            std::process::exit(1);
        }
    }
}

fn main() -> Result<()> {
    check_prerequisites()?;
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Rm) => {
            let cwd = std::env::current_dir()?;
            let name = container::container_name(&cwd.to_string_lossy());
            container::rm(&name, cli.verbose)?;
            println!("Removed {}", name);
            Ok(())
        }
        Some(Commands::Stop) => {
            let cwd = std::env::current_dir()?;
            let name = container::container_name(&cwd.to_string_lossy());
            container::stop(&name, cli.verbose)?;
            println!("Stopped {}", name);
            Ok(())
        }
        Some(Commands::Ls) => {
            container::list(cli.verbose)?;
            Ok(())
        }
        Some(Commands::Build) => {
            let config = config::Config::load()?;
            let cwd = std::env::current_dir()?;
            let (dockerfile_content, image_tag) =
                image::resolve_dockerfile(&cwd, cli.profile.as_deref(), &config)?;
            let cache_key = image_tag.replace(':', "-");
            eprintln!("Building {}...", image_tag);
            image::build(&image_tag, &dockerfile_content, cli.verbose)?;
            image::save_cache(&dockerfile_content, &cache_key, &image::cache_dir())?;
            println!("Built {}", image_tag);
            Ok(())
        }
        Some(Commands::Config { command }) => match command {
            ConfigCommands::Init => {
                let path = config::Config::config_path();
                if path.exists() {
                    eprintln!("Config already exists at {}", path.display());
                    eprintln!("Edit it directly or remove it first.");
                    std::process::exit(1);
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
                    container::exec(&name, task_str.as_deref(), cli.verbose)?;
                }
                container::ContainerStatus::Stopped => {
                    let (dockerfile_content, image_tag) =
                        image::resolve_dockerfile(&cwd, cli.profile.as_deref(), &config)?;
                    let cache_key = image_tag.replace(':', "-");
                    if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
                        eprintln!("Image changed, recreating container...");
                        container::rm(&name, cli.verbose)?;
                        image::build(&image_tag, &dockerfile_content, cli.verbose)?;
                        image::save_cache(&dockerfile_content, &cache_key, &image::cache_dir())?;
                        create_and_run(&name, &image_tag, &cwd_str, &config, task_str.as_deref(), cli.verbose)?;
                    } else {
                        container::start(&name, cli.verbose)?;
                        container::exec(&name, task_str.as_deref(), cli.verbose)?;
                    }
                }
                container::ContainerStatus::NotFound => {
                    let (dockerfile_content, image_tag) =
                        image::resolve_dockerfile(&cwd, cli.profile.as_deref(), &config)?;
                    let cache_key = image_tag.replace(':', "-");
                    if image::needs_build(&dockerfile_content, &cache_key, &image::cache_dir()) {
                        eprintln!("Building image...");
                        image::build(&image_tag, &dockerfile_content, cli.verbose)?;
                        image::save_cache(&dockerfile_content, &cache_key, &image::cache_dir())?;
                    }
                    create_and_run(&name, &image_tag, &cwd_str, &config, task_str.as_deref(), cli.verbose)?;
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
    fn test_rm_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "rm"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Rm)));
    }

    #[test]
    fn test_ls_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "ls"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Ls)));
    }

    #[test]
    fn test_stop_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "stop"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Stop)));
    }

    #[test]
    fn test_build_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "build"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Build)));
    }

    #[test]
    fn test_config_init_subcommand() {
        let cli = Cli::try_parse_from(["agentbox", "config", "init"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Config { command: ConfigCommands::Init })
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
}
