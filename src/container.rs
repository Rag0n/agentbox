use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::process::Command;

/// Generate a deterministic container name from a project path.
pub fn container_name(path: &str) -> String {
    let dir_name = std::path::Path::new(path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>();
    let hash = format!("{:x}", Sha256::digest(path.as_bytes()));
    let short_hash = &hash[..6];
    format!("agentbox-{}-{}", dir_name, short_hash)
}

#[derive(Debug)]
pub struct RunOpts {
    pub name: String,
    pub image: String,
    pub workdir: String,
    pub cpus: usize,
    pub memory: String,
    pub env_vars: Vec<(String, String)>,
    pub volumes: Vec<String>,
    pub interactive: bool,
    pub task: Option<String>,
}

impl RunOpts {
    pub fn to_run_args(&self) -> Vec<String> {
        let mut args = vec!["run".to_string()];

        args.extend(["--name".into(), self.name.clone()]);
        args.extend(["--cpus".into(), self.cpus.to_string()]);
        args.extend(["--memory".into(), self.memory.clone()]);
        args.extend(["--workdir".into(), self.workdir.clone()]);

        if self.interactive {
            args.push("--interactive".into());
            args.push("--tty".into());
        }

        for (key, val) in &self.env_vars {
            args.extend(["--env".into(), format!("{}={}", key, val)]);
        }

        for vol in &self.volumes {
            args.extend(["--volume".into(), vol.clone()]);
        }

        args.push(self.image.clone());

        // Append task args after image (passed to entrypoint)
        if let Some(task) = &self.task {
            args.extend(["-p".into(), task.clone()]);
        }

        args
    }
}

#[derive(Debug, PartialEq)]
pub enum ContainerStatus {
    Running,
    Stopped,
    NotFound,
}

/// Check container status using `container inspect --format json`.
pub fn status(name: &str) -> Result<ContainerStatus> {
    let output = Command::new("container")
        .args(["inspect", "--format", "json", name])
        .output()
        .context("failed to run 'container inspect'")?;

    if !output.status.success() {
        return Ok(ContainerStatus::NotFound);
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("failed to parse container inspect output")?;

    Ok(parse_status(&json))
}

/// Parse container status from inspect JSON.
fn parse_status(json: &serde_json::Value) -> ContainerStatus {
    let status_str = json
        .pointer("/status")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match status_str {
        "running" => ContainerStatus::Running,
        "stopped" => ContainerStatus::Stopped,
        _ => ContainerStatus::NotFound,
    }
}

/// Parse container list JSON, returning (name, state) pairs for agentbox containers.
fn parse_container_list(json_str: &str) -> Vec<(String, String)> {
    let containers: Vec<serde_json::Value> = serde_json::from_str(json_str)
        .unwrap_or_default();
    let mut result = Vec::new();
    for json in &containers {
        let name = json.pointer("/configuration/id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if name.starts_with("agentbox-") {
            let state = json.pointer("/status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            result.push((name.to_string(), state.to_string()));
        }
    }
    result
}

/// Run a container with the given options.
pub fn run(opts: &RunOpts, verbose: bool) -> Result<()> {
    let args = opts.to_run_args();
    if verbose {
        eprintln!("[agentbox] container {}", args.join(" "));
    }
    let status = Command::new("container")
        .args(&args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .context("failed to run 'container run'")?;

    if !status.success() {
        bail!("container exited with status {}", status);
    }
    Ok(())
}

/// Exec into a running container.
pub fn exec(name: &str, task: Option<&str>, verbose: bool) -> Result<()> {
    let mut args = vec!["exec".to_string()];
    if task.is_none() {
        args.push("--interactive".into());
        args.push("--tty".into());
    }
    args.extend(["--user".into(), "user".to_string()]);
    args.push(name.to_string());
    args.push("claude".into());
    args.push("--dangerously-skip-permissions".into());
    if let Some(t) = task {
        args.extend(["-p".into(), t.to_string()]);
    }

    if verbose {
        eprintln!("[agentbox] container {}", args.join(" "));
    }
    let status = Command::new("container")
        .args(&args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .context("failed to run 'container exec'")?;

    if !status.success() {
        bail!("container exec exited with status {}", status);
    }
    Ok(())
}

/// Start a stopped container.
pub fn start(name: &str, verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("[agentbox] container start {}", name);
    }
    let status = Command::new("container")
        .args(["start", name])
        .status()
        .context("failed to run 'container start'")?;

    if !status.success() {
        bail!("container start failed");
    }
    Ok(())
}

/// Stop a running container.
pub fn stop(name: &str, verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("[agentbox] container stop {}", name);
    }
    let status = Command::new("container")
        .args(["stop", name])
        .status()
        .context("failed to run 'container stop'")?;

    if !status.success() {
        bail!("container stop failed");
    }
    Ok(())
}

/// Remove a container.
pub fn rm(name: &str, verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("[agentbox] container rm -f {}", name);
    }
    let status = Command::new("container")
        .args(["rm", "-f", name])
        .status()
        .context("failed to run 'container rm'")?;

    if !status.success() {
        bail!("container rm failed");
    }
    Ok(())
}

/// Return names of all agentbox containers.
pub fn list_names(verbose: bool) -> Result<Vec<String>> {
    if verbose {
        eprintln!("[agentbox] container ls --all --format json");
    }
    let output = Command::new("container")
        .args(["ls", "--all", "--format", "json"])
        .output()
        .context("failed to run 'container ls'")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let containers = parse_container_list(&stdout);
    Ok(containers.into_iter().map(|(name, _)| name).collect())
}

/// List all agentbox containers.
pub fn list(verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("[agentbox] container ls --all --format json");
    }
    let output = Command::new("container")
        .args(["ls", "--all", "--format", "json"])
        .output()
        .context("failed to run 'container ls'")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let containers = parse_container_list(&stdout);
    if containers.is_empty() {
        println!("No agentbox containers found.");
    } else {
        for (name, state) in &containers {
            println!("{}\t{}", name, state);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_container_name_from_path() {
        let name = container_name("/Users/alex/Dev/myapp");
        assert!(name.starts_with("agentbox-myapp-"));
        assert_eq!(name.len(), "agentbox-myapp-".len() + 6);
    }

    #[test]
    fn test_container_name_uniqueness() {
        let a = container_name("/Users/alex/work/myapp");
        let b = container_name("/Users/alex/personal/myapp");
        assert_ne!(a, b);
    }

    #[test]
    fn test_container_name_stability() {
        let a = container_name("/Users/alex/Dev/myapp");
        let b = container_name("/Users/alex/Dev/myapp");
        assert_eq!(a, b);
    }

    #[test]
    fn test_build_run_args() {
        let opts = RunOpts {
            name: "agentbox-myapp-abc123".into(),
            image: "agentbox:default".into(),
            workdir: "/Users/alex/Dev/myapp".into(),
            cpus: 4,
            memory: "8G".into(),
            env_vars: vec![
                ("GH_TOKEN".into(), "tok123".into()),
            ],
            volumes: vec![
                "/Users/alex/Dev/myapp:/Users/alex/Dev/myapp".into(),
            ],
            interactive: true,
            task: None,
        };
        let args = opts.to_run_args();
        assert!(args.contains(&"--name".to_string()));
        assert!(args.contains(&"agentbox-myapp-abc123".to_string()));
        assert!(args.contains(&"--cpus".to_string()));
        assert!(args.contains(&"4".to_string()));
        assert!(args.contains(&"--memory".to_string()));
        assert!(args.contains(&"8G".to_string()));
        assert!(args.contains(&"--interactive".to_string()));
        assert!(args.contains(&"--tty".to_string()));
    }

    #[test]
    fn test_parse_status_running() {
        let json: serde_json::Value = serde_json::json!({"status": "running"});
        assert_eq!(parse_status(&json), ContainerStatus::Running);
    }

    #[test]
    fn test_parse_status_stopped() {
        let json: serde_json::Value = serde_json::json!({"status": "stopped"});
        assert_eq!(parse_status(&json), ContainerStatus::Stopped);
    }

    #[test]
    fn test_parse_status_missing() {
        let json: serde_json::Value = serde_json::json!({});
        assert_eq!(parse_status(&json), ContainerStatus::NotFound);
    }

    #[test]
    fn test_parse_container_list_filters_agentbox() {
        let json = serde_json::json!([
            {
                "status": "stopped",
                "configuration": {"id": "agentbox-myapp-abc123"}
            },
            {
                "status": "running",
                "configuration": {"id": "buildkit"}
            },
            {
                "status": "running",
                "configuration": {"id": "agentbox-other-def456"}
            }
        ]);
        let result = parse_container_list(&json.to_string());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], ("agentbox-myapp-abc123".into(), "stopped".into()));
        assert_eq!(result[1], ("agentbox-other-def456".into(), "running".into()));
    }

    #[test]
    fn test_parse_container_list_empty() {
        assert!(parse_container_list("[]").is_empty());
    }

    #[test]
    fn test_parse_container_list_invalid_json() {
        assert!(parse_container_list("not json").is_empty());
    }

    #[test]
    fn test_build_run_args_headless() {
        let opts = RunOpts {
            name: "agentbox-myapp-abc123".into(),
            image: "agentbox:default".into(),
            workdir: "/Users/alex/Dev/myapp".into(),
            cpus: 2,
            memory: "4G".into(),
            env_vars: vec![],
            volumes: vec![],
            interactive: false,
            task: Some("fix the tests".into()),
        };
        let args = opts.to_run_args();
        assert!(!args.contains(&"--interactive".to_string()));
        assert!(!args.contains(&"--tty".to_string()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"fix the tests".to_string()));
    }
}
