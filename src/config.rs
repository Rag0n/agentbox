use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct BridgeConfig {
    pub allowed_commands: Vec<String>,
    pub forward_not_found: bool,
    pub host_ip: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct CliConfig {
    #[serde(default)]
    pub flags: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub cpus: Option<usize>,
    pub memory: String,
    pub dockerfile: Option<PathBuf>,
    pub default_agent: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub profiles: HashMap<String, Profile>,
    #[serde(default)]
    pub volumes: Vec<String>,
    #[serde(default)]
    pub bridge: BridgeConfig,
    #[serde(default)]
    pub cli: HashMap<String, CliConfig>,
}

#[derive(Debug, Deserialize)]
pub struct Profile {
    pub dockerfile: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cpus: None,
            memory: "8G".to_string(),
            dockerfile: None,
            default_agent: None,
            env: HashMap::new(),
            profiles: HashMap::new(),
            volumes: Vec::new(),
            bridge: BridgeConfig::default(),
            cli: HashMap::new(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path();
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)
                .with_context(|| format!("failed to read {}", config_path.display()))?;
            toml::from_str(&content)
                .with_context(|| format!("failed to parse {}", config_path.display()))
        } else {
            Ok(Self::default())
        }
    }

    pub fn config_path() -> PathBuf {
        let config_dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join(".config")
            });
        config_dir.join("agentbox").join("config.toml")
    }

    pub fn effective_cpus(&self) -> usize {
        self.cpus.unwrap_or_else(|| {
            let total = num_cpus::get();
            (total / 2).max(1)
        })
    }

    pub fn cli_flags(&self, cli_name: &str) -> &[String] {
        self.cli
            .get(cli_name)
            .map(|c| c.flags.as_slice())
            .unwrap_or(&[])
    }

    /// Resolve `default_agent` into a `CodingAgent`. Missing value falls
    /// back to `Claude`. Unknown strings produce an error with a useful
    /// message; the caller (runtime or setup) decides how to surface it.
    pub fn resolve_default_agent(&self) -> Result<crate::agent::CodingAgent> {
        use std::str::FromStr;

        match self.default_agent.as_deref() {
            None => Ok(crate::agent::CodingAgent::Claude),
            Some(s) => crate::agent::CodingAgent::from_str(s).with_context(|| {
                format!(
                    "invalid default_agent = {:?}; expected \"claude\" or \"codex\"",
                    s
                )
            }),
        }
    }

    pub fn init_template() -> &'static str {
        r#"# agentbox configuration

# Default agent used by bare `agentbox`. `agentbox setup` will write this
# for you; uncomment and edit to change it manually.
# default_agent = "claude"   # or "codex"

# Resources (auto-detected from host if not set)
# cpus = 4          # default: half of host cores
# memory = "8G"     # default: 8G

# Additional volumes to mount into containers
# volumes = [
#   "~/.config/tool",            # tilde = home-relative mapping
#   "/opt/libs",                 # absolute = same path in container
#   "/src/path:/dest/path",     # explicit source:dest mapping
# ]

# Override the default Dockerfile for all projects
# dockerfile = "~/.config/agentbox/Dockerfile.custom"

# Environment variables to pass into container
# [env]
# KEY = ""        # empty = inherit from host env
# KEY = "value"   # literal value

# Named profiles with custom Dockerfiles
# [profiles.name]
# dockerfile = "/path/to/Dockerfile"

# Default flags for each coding agent.
# Replace to override. The "dangerously-*" flags bypass in-agent
# sandboxing because the container already isolates the agent.
[cli.claude]
flags = ["--dangerously-skip-permissions"]

[cli.codex]
flags = ["--dangerously-bypass-approvals-and-sandbox"]

# Host bridge: execute commands on macOS host from container
# [bridge]
# allowed_commands = ["xcodebuild", "xcrun", "adb", "emulator"]
# forward_not_found = false
"#
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.memory, "8G");
        assert!(config.cpus.is_none());
        assert!(config.dockerfile.is_none());
        assert!(config.env.is_empty());
        assert!(config.profiles.is_empty());
    }

    #[test]
    fn test_parse_full_config() {
        let toml_str = r#"
            cpus = 4
            memory = "16G"
            dockerfile = "/path/to/Dockerfile"

            [env]
            GH_TOKEN = ""
            LINEAR_API_KEY = "abc123"

            [profiles.mystack]
            dockerfile = "/path/to/mystack.Dockerfile"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.cpus, Some(4));
        assert_eq!(config.memory, "16G");
        assert_eq!(
            config.dockerfile,
            Some(PathBuf::from("/path/to/Dockerfile"))
        );
        assert_eq!(config.env.get("GH_TOKEN").unwrap(), "");
        assert_eq!(config.env.get("LINEAR_API_KEY").unwrap(), "abc123");
        assert_eq!(
            config.profiles.get("mystack").unwrap().dockerfile,
            PathBuf::from("/path/to/mystack.Dockerfile")
        );
    }

    #[test]
    fn test_parse_empty_config() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.memory, "8G");
    }

    #[test]
    fn test_effective_cpus_from_config() {
        let mut config = Config::default();
        config.cpus = Some(2);
        assert_eq!(config.effective_cpus(), 2);
    }

    #[test]
    fn test_effective_cpus_default_half_host() {
        let config = Config::default();
        let cpus = config.effective_cpus();
        assert!(cpus >= 1);
        assert!(cpus <= num_cpus::get());
    }

    #[test]
    fn test_parse_config_with_volumes() {
        let toml_str = r#"
            volumes = [
                "~/.config/worktrunk",
                "/Users/alex/Dev/marketplace",
                "/source/path:/dest/path",
            ]
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.volumes.len(), 3);
        assert_eq!(config.volumes[0], "~/.config/worktrunk");
        assert_eq!(config.volumes[1], "/Users/alex/Dev/marketplace");
        assert_eq!(config.volumes[2], "/source/path:/dest/path");
    }

    #[test]
    fn test_default_config_has_empty_volumes() {
        let config = Config::default();
        assert!(config.volumes.is_empty());
    }

    #[test]
    fn test_config_init_content() {
        let content = Config::init_template();
        assert!(content.contains("# cpus"));
        assert!(content.contains("# memory"));
        assert!(content.contains("# [env]"));
        assert!(content.contains("# [profiles."));
        assert!(content.contains("# volumes"));
        assert!(content.contains("# default_agent"));
        assert!(content.contains("[cli.claude]"));
        assert!(content.contains("--dangerously-skip-permissions"));
        assert!(content.contains("[cli.codex]"));
        assert!(content.contains("--dangerously-bypass-approvals-and-sandbox"));
        // default_agent stays commented out so setup prompts on fresh install
        assert!(content.contains("# default_agent ="));
        assert!(!content.lines().any(|l| l.trim_start().starts_with("default_agent =")));
    }

    #[test]
    fn test_parse_bridge_config() {
        let toml_str = r#"
            [bridge]
            allowed_commands = ["xcodebuild", "xcrun", "adb"]
            forward_not_found = true
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.bridge.allowed_commands,
            vec!["xcodebuild", "xcrun", "adb"]
        );
        assert!(config.bridge.forward_not_found);
    }

    #[test]
    fn test_default_bridge_config() {
        let config = Config::default();
        assert!(config.bridge.allowed_commands.is_empty());
        assert!(!config.bridge.forward_not_found);
    }

    #[test]
    fn test_bridge_config_omitted() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.bridge.allowed_commands.is_empty());
        assert!(!config.bridge.forward_not_found);
        assert!(config.bridge.host_ip.is_none());
    }

    #[test]
    fn test_bridge_config_host_ip_override() {
        let toml_str = r#"
            [bridge]
            allowed_commands = ["echo"]
            host_ip = "10.0.0.1"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.bridge.host_ip, Some("10.0.0.1".to_string()));
    }

    #[test]
    fn test_parse_cli_config() {
        let toml_str = r#"
            [cli.claude]
            flags = ["--append-system-prompt", "Be careful.", "--model", "sonnet"]
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let claude_cli = config.cli.get("claude").unwrap();
        assert_eq!(
            claude_cli.flags,
            vec!["--append-system-prompt", "Be careful.", "--model", "sonnet"]
        );
    }

    #[test]
    fn test_parse_multiple_cli_configs() {
        let toml_str = r#"
            [cli.claude]
            flags = ["--model", "sonnet"]

            [cli.codex]
            flags = ["--full-auto"]
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.cli.get("claude").unwrap().flags, vec!["--model", "sonnet"]);
        assert_eq!(config.cli.get("codex").unwrap().flags, vec!["--full-auto"]);
    }

    #[test]
    fn test_default_config_has_empty_cli() {
        let config = Config::default();
        assert!(config.cli.is_empty());
    }

    #[test]
    fn test_cli_flags_helper_found() {
        let toml_str = r#"
            [cli.claude]
            flags = ["--model", "sonnet"]
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.cli_flags("claude"), &["--model", "sonnet"]);
    }

    #[test]
    fn test_cli_flags_helper_not_found() {
        let config = Config::default();
        assert!(config.cli_flags("claude").is_empty());
    }

    #[test]
    fn test_cli_config_omitted() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.cli.is_empty());
    }

    #[test]
    fn test_parse_default_agent_claude() {
        let toml_str = r#"default_agent = "claude""#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_agent.as_deref(), Some("claude"));
    }

    #[test]
    fn test_parse_default_agent_codex() {
        let toml_str = r#"default_agent = "codex""#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_agent.as_deref(), Some("codex"));
    }

    #[test]
    fn test_parse_default_agent_invalid_still_parses_as_string() {
        // Invalid values must survive TOML parsing so setup can repair them
        // interactively; validation happens in resolve_default_agent().
        let toml_str = r#"default_agent = "gemini""#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_agent.as_deref(), Some("gemini"));
    }

    #[test]
    fn test_default_agent_omitted_is_none() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.default_agent.is_none());
    }

    #[test]
    fn test_resolve_default_agent_none_falls_back_to_claude() {
        use crate::agent::CodingAgent;
        let config = Config::default();
        assert_eq!(
            config.resolve_default_agent().unwrap(),
            CodingAgent::Claude
        );
    }

    #[test]
    fn test_resolve_default_agent_claude_and_codex() {
        use crate::agent::CodingAgent;
        let mut config = Config::default();
        config.default_agent = Some("claude".into());
        assert_eq!(
            config.resolve_default_agent().unwrap(),
            CodingAgent::Claude
        );
        config.default_agent = Some("codex".into());
        assert_eq!(
            config.resolve_default_agent().unwrap(),
            CodingAgent::Codex
        );
    }

    #[test]
    fn test_resolve_default_agent_invalid_errors_with_useful_message() {
        let mut config = Config::default();
        config.default_agent = Some("gemini".into());
        let err = config.resolve_default_agent().unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("gemini"), "error message should mention the bad value; got: {msg}");
        assert!(
            msg.contains("claude") && msg.contains("codex"),
            "error message should list valid options; got: {msg}"
        );
    }
}
