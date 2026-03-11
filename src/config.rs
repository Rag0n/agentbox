use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub cpus: Option<usize>,
    pub memory: String,
    pub dockerfile: Option<PathBuf>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub profiles: HashMap<String, Profile>,
    #[serde(default)]
    pub volumes: Vec<String>,
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
            env: HashMap::new(),
            profiles: HashMap::new(),
            volumes: Vec::new(),
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

    pub fn init_template() -> &'static str {
        r#"# agentbox configuration

# Resources (auto-detected from host if not set)
# cpus = 4          # default: half of host cores
# memory = "8G"     # default: 8G

# Additional volumes to mount into containers
# volumes = [
#   "~/.config/worktrunk",              # tilde = home-relative mapping
#   "/opt/shared-libs",                  # absolute = same path in container
#   "/source/path:/dest/path",          # explicit source:dest mapping
# ]

# Override the default Dockerfile for all projects
# dockerfile = "~/.config/agentbox/Dockerfile.custom"

# Environment variables to pass into container
# [env]
# GH_TOKEN = ""           # empty = inherit from host env
# LINEAR_API_KEY = "abc"  # literal value

# Named profiles with custom Dockerfiles
# [profiles.mystack]
# dockerfile = "/path/to/mystack.Dockerfile"
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
        assert_eq!(config.dockerfile, Some(PathBuf::from("/path/to/Dockerfile")));
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
    }
}
