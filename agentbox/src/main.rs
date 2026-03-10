use anyhow::Result;
use clap::{Parser, Subcommand};

mod config;

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

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Rm) => todo!("rm"),
        Some(Commands::Stop) => todo!("stop"),
        Some(Commands::Ls) => todo!("ls"),
        Some(Commands::Build) => todo!("build"),
        Some(Commands::Config { command }) => match command {
            ConfigCommands::Init => todo!("config init"),
        },
        None => {
            if cli.task.is_empty() {
                todo!("interactive mode")
            } else {
                todo!("headless mode: {}", cli.task.join(" "))
            }
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
