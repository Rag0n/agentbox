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
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    let hash = format!("{:x}", Sha256::digest(path.as_bytes()));
    let short_hash = &hash[..6];
    format!("agentbox-{}-{}", dir_name, short_hash)
}

#[derive(Debug, Clone)]
pub enum RunMode {
    Agent {
        agent: crate::agent::CodingAgent,
        task: Option<String>,
        cli_flags: Vec<String>,
    },
    Shell {
        cmd: Vec<String>,
    },
}

impl RunMode {
    pub fn is_interactive(&self) -> bool {
        match self {
            RunMode::Agent { task, .. } => task.is_none(),
            RunMode::Shell { cmd } => cmd.is_empty(),
        }
    }
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
    pub mode: RunMode,
}

impl RunOpts {
    pub fn to_run_args(&self) -> Vec<String> {
        let mut args = vec!["run".to_string()];

        args.extend(["--name".into(), self.name.clone()]);
        args.extend(["--cpus".into(), self.cpus.to_string()]);
        args.extend(["--memory".into(), self.memory.clone()]);
        args.extend(["--workdir".into(), self.workdir.clone()]);
        args.push("--init".into());

        if self.mode.is_interactive() {
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

        match &self.mode {
            RunMode::Agent { agent, task, cli_flags } => {
                args.push(agent.entrypoint_arg().to_string());
                args.extend(agent.invocation(cli_flags, task.as_deref()));
            }
            RunMode::Shell { cmd } => {
                args.push("--shell".into());
                for token in cmd {
                    args.push(token.clone());
                }
            }
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

/// Check container status using `container inspect`.
pub fn status(name: &str) -> Result<ContainerStatus> {
    let output = Command::new("container")
        .args(["inspect", name])
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
        .pointer("/0/status")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match status_str {
        "running" => ContainerStatus::Running,
        "stopped" => ContainerStatus::Stopped,
        _ => ContainerStatus::NotFound,
    }
}

/// Returns true if a `ps -eo pid,args` row represents a `container exec`
/// or `container run` invocation that references the given container name.
/// The always-on `container-runtime-linux` process does not match because
/// it does not contain `container exec` or `container run`.
fn matches_session(line: &str, container_name: &str) -> Option<u32> {
    let trimmed = line.trim();
    let (pid_str, args) = trimmed.split_once(char::is_whitespace)?;
    let pid: u32 = pid_str.trim().parse().ok()?;
    let is_session = (args.contains("container exec") || args.contains("container run"))
        && args.contains(container_name);
    if is_session {
        Some(pid)
    } else {
        None
    }
}

/// Check if other processes are using the same container.
/// Parses `ps -eo pid,args` output, looking for `container exec` or
/// `container run` rows that reference the given container name,
/// excluding our own PID.
pub fn has_other_sessions(ps_output: &str, container_name: &str, our_pid: u32) -> bool {
    ps_output
        .lines()
        .filter_map(|line| matches_session(line, container_name))
        .any(|pid| pid != our_pid)
}

/// Count attached sessions for a container by parsing `ps -eo pid,args`.
/// Counts every `container exec` / `container run` row that references the
/// container name. Used by `agentbox status` to populate the SESSIONS column.
pub fn count_sessions(ps_output: &str, container_name: &str) -> usize {
    ps_output
        .lines()
        .filter_map(|line| matches_session(line, container_name))
        .count()
}

/// Stop the container if no other agentbox sessions are attached to it.
/// Called after the blocking exec/run call returns.
/// Errors are intentionally ignored — this is best-effort cleanup.
pub fn maybe_stop_container(name: &str, verbose: bool) {
    let our_pid = std::process::id();

    let output = match Command::new("ps")
        .args(["-eo", "pid,args"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return, // Can't check — don't stop
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    if has_other_sessions(&stdout, name, our_pid) {
        return;
    }

    if verbose {
        eprintln!("[agentbox] no other sessions, stopping container {}...", name);
    }
    let _ = stop(name, false);
}

/// Parse container list JSON, returning (name, state) pairs for agentbox containers.
fn parse_container_list(json_str: &str) -> Vec<(String, String)> {
    let containers: Vec<serde_json::Value> = serde_json::from_str(json_str).unwrap_or_default();
    let mut result = Vec::new();
    for json in &containers {
        let name = json
            .pointer("/configuration/id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if name.starts_with("agentbox-") {
            let state = json
                .pointer("/status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            result.push((name.to_string(), state.to_string()));
        }
    }
    result
}

/// Run a container with the given options. Returns the process exit code.
pub fn run(opts: &RunOpts, verbose: bool) -> Result<i32> {
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

    Ok(status.code().unwrap_or(1))
}

/// Returns the HOSTEXEC bash setup prefix that runs before the main command.
/// Mirrors the logic that the entrypoint runs at cold-start, applied here for
/// the exec path which bypasses the entrypoint.
fn build_setup_prefix(env_vars: &[(String, String)]) -> String {
    let mut prefix = String::new();
    if env_vars.iter().any(|(k, _)| k == "HOSTEXEC_COMMANDS") {
        prefix.push_str(
            "if [ -n \"$HOSTEXEC_COMMANDS\" ]; then \
             mkdir -p ~/.local/bin; \
             for c in $HOSTEXEC_COMMANDS; do \
             ln -sf /usr/local/bin/hostexec ~/.local/bin/$c; \
             done; fi; ",
        );
    }
    if env_vars
        .iter()
        .any(|(k, v)| k == "HOSTEXEC_FORWARD_NOT_FOUND" && v == "true")
    {
        prefix.push_str(
            "if ! grep -q command_not_found_handle /etc/bash.bashrc 2>/dev/null; then \
             echo 'command_not_found_handle() { /usr/local/bin/hostexec \"$@\"; }' \
             | sudo tee -a /etc/bash.bashrc > /dev/null; fi; ",
        );
    }
    prefix
}

/// Build the argument list for `container exec`.
fn build_exec_args(name: &str, mode: &RunMode, env_vars: &[(String, String)]) -> Vec<String> {
    let mut args = vec!["exec".to_string()];
    if mode.is_interactive() {
        args.push("--interactive".into());
        args.push("--tty".into());
    }
    for (key, val) in env_vars {
        args.extend(["--env".into(), format!("{}={}", key, val)]);
    }
    args.extend(["--user".into(), "user".to_string()]);
    args.push(name.to_string());
    // Use login shell so PATH includes ~/.local/bin where claude is installed
    args.push("bash".into());
    args.extend(["-lc".into()]);

    let setup = build_setup_prefix(env_vars);

    match mode {
        RunMode::Agent { agent, task, cli_flags } => {
            let mut cmd = setup;
            cmd.push_str("exec ");
            cmd.push_str(agent.binary());
            for tok in agent.invocation(cli_flags, task.as_deref()) {
                cmd.push_str(&format!(" '{}'", tok.replace('\'', "'\\''")));
            }
            args.push(cmd);
        }
        RunMode::Shell { cmd: shell_cmd } => {
            let mut cmd = setup;
            if shell_cmd.is_empty() {
                cmd.push_str("exec bash -l");
                args.push(cmd);
            } else {
                // Pass user tokens as bash positional args: the script execs
                // "$@" and tokens follow a $0 placeholder. Preserves arg
                // boundaries without string-level shell escaping.
                cmd.push_str("exec \"$@\"");
                args.push(cmd);
                args.push("bash".into());
                args.extend(shell_cmd.iter().cloned());
            }
        }
    }
    args
}

/// Exec into a running container. Returns the process exit code.
pub fn exec(
    name: &str,
    mode: &RunMode,
    env_vars: &[(String, String)],
    verbose: bool,
) -> Result<i32> {
    let args = build_exec_args(name, mode, env_vars);

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

    Ok(status.code().unwrap_or(1))
}

/// Start a stopped container.
pub fn start(name: &str, verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("[agentbox] container start {}", name);
    }
    let output = Command::new("container")
        .args(["start", name])
        .output()
        .context("failed to run 'container start'")?;

    if !output.status.success() {
        bail!("container start failed");
    }
    Ok(())
}

/// Stop a running container.
pub fn stop(name: &str, verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("[agentbox] container stop {}", name);
    }
    let output = Command::new("container")
        .args(["stop", name])
        .output()
        .context("failed to run 'container stop'")?;

    if !output.status.success() {
        bail!("container stop failed");
    }
    Ok(())
}

/// Remove a container.
pub fn rm(name: &str, verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("[agentbox] container rm -f {}", name);
    }
    let output = Command::new("container")
        .args(["rm", "-f", name])
        .output()
        .context("failed to run 'container rm'")?;

    if !output.status.success() {
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


#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::CodingAgent;

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
            env_vars: vec![("GH_TOKEN".into(), "tok123".into())],
            volumes: vec!["/Users/alex/Dev/myapp:/Users/alex/Dev/myapp".into()],
            mode: RunMode::Agent { agent: CodingAgent::Claude, task: None, cli_flags: vec![] },
        };
        let args = opts.to_run_args();
        assert!(args.contains(&"--name".to_string()));
        assert!(args.contains(&"agentbox-myapp-abc123".to_string()));
        assert!(args.contains(&"--cpus".to_string()));
        assert!(args.contains(&"4".to_string()));
        assert!(args.contains(&"--memory".to_string()));
        assert!(args.contains(&"8G".to_string()));
        assert!(args.contains(&"--init".to_string()));
        assert!(args.contains(&"--interactive".to_string()));
        assert!(args.contains(&"--tty".to_string()));
    }

    #[test]
    fn test_parse_status_running() {
        let json: serde_json::Value = serde_json::json!([{"status": "running"}]);
        assert_eq!(parse_status(&json), ContainerStatus::Running);
    }

    #[test]
    fn test_parse_status_stopped() {
        let json: serde_json::Value = serde_json::json!([{"status": "stopped"}]);
        assert_eq!(parse_status(&json), ContainerStatus::Stopped);
    }

    #[test]
    fn test_parse_status_missing() {
        let json: serde_json::Value = serde_json::json!([{}]);
        assert_eq!(parse_status(&json), ContainerStatus::NotFound);
    }

    #[test]
    fn test_parse_status_empty_array() {
        let json: serde_json::Value = serde_json::json!([]);
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
        assert_eq!(
            result[0],
            ("agentbox-myapp-abc123".into(), "stopped".into())
        );
        assert_eq!(
            result[1],
            ("agentbox-other-def456".into(), "running".into())
        );
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
    fn test_exec_args_with_env_vars() {
        let env_vars = vec![
            ("GH_TOKEN".to_string(), "tok123".to_string()),
            ("TERM".to_string(), "xterm".to_string()),
        ];
        let mode = RunMode::Agent {
            agent: CodingAgent::Claude,
            task: Some("fix tests".into()),
            cli_flags: vec![],
        };
        let args = build_exec_args("mycontainer", &mode, &env_vars);
        assert!(args.contains(&"--env".to_string()));
        assert!(args.contains(&"GH_TOKEN=tok123".to_string()));
        assert!(args.contains(&"TERM=xterm".to_string()));
        assert!(!args.contains(&"--interactive".to_string()));
        // Task passed inside bash -lc command string
        assert!(args.contains(&"bash".to_string()));
        let cmd = args.last().unwrap();
        // After the refactor, all tokens are uniformly single-quoted
        assert!(cmd.contains("exec claude"));
        assert!(cmd.contains("'-p'"));
        assert!(cmd.contains("'fix tests'"));
    }

    #[test]
    fn test_exec_args_interactive_no_task() {
        let mode = RunMode::Agent { agent: CodingAgent::Claude, task: None, cli_flags: vec![] };
        let args = build_exec_args("mycontainer", &mode, &[]);
        assert!(args.contains(&"--interactive".to_string()));
        assert!(args.contains(&"--tty".to_string()));
        // No -p flag in command string when no task
        let cmd = args.last().unwrap();
        assert_eq!(cmd, "exec claude");
    }

    #[test]
    fn test_exec_args_with_hostexec_env() {
        let env_vars = vec![
            ("HOSTEXEC_HOST".to_string(), "192.168.64.1".to_string()),
            ("HOSTEXEC_PORT".to_string(), "12345".to_string()),
            ("HOSTEXEC_TOKEN".to_string(), "tok".to_string()),
            ("HOSTEXEC_COMMANDS".to_string(), "xcodebuild xcrun".to_string()),
        ];
        let mode = RunMode::Agent { agent: CodingAgent::Claude, task: None, cli_flags: vec![] };
        let args = build_exec_args("mycontainer", &mode, &env_vars);
        let cmd = args.last().unwrap();
        assert!(cmd.contains("HOSTEXEC_COMMANDS"), "should set up symlinks");
        assert!(cmd.contains("ln -sf /usr/local/bin/hostexec"));
        assert!(cmd.contains("exec claude"));
    }

    #[test]
    fn test_exec_args_with_forward_not_found() {
        let env_vars = vec![
            ("HOSTEXEC_COMMANDS".to_string(), "xcodebuild".to_string()),
            (
                "HOSTEXEC_FORWARD_NOT_FOUND".to_string(),
                "true".to_string(),
            ),
        ];
        let mode = RunMode::Agent { agent: CodingAgent::Claude, task: None, cli_flags: vec![] };
        let args = build_exec_args("mycontainer", &mode, &env_vars);
        let cmd = args.last().unwrap();
        assert!(cmd.contains("command_not_found_handle"));
    }

    #[test]
    fn test_exec_args_no_hostexec_without_env() {
        let mode = RunMode::Agent { agent: CodingAgent::Claude, task: None, cli_flags: vec![] };
        let args = build_exec_args("mycontainer", &mode, &[]);
        let cmd = args.last().unwrap();
        assert!(!cmd.contains("HOSTEXEC"), "no bridge setup without env vars");
        assert_eq!(cmd, "exec claude");
    }

    #[test]
    fn test_has_other_sessions_no_matches() {
        let ps_output = "  PID ARGS\n  100 /bin/bash\n  200 vim main.rs\n";
        assert!(!has_other_sessions(ps_output, "agentbox-myapp-abc123", 999));
    }

    #[test]
    fn test_has_other_sessions_ignores_runtime_process() {
        // Apple Container keeps a VM runtime process running — must not match
        let ps_output = "  PID ARGS\n  100 /usr/local/libexec/container/plugins/container-runtime-linux/bin/container-runtime-linux start --root /Users/alex/Library/Application Support/com.apple.container/containers/agentbox-myapp-abc123 --uuid agentbox-myapp-abc123\n";
        assert!(!has_other_sessions(ps_output, "agentbox-myapp-abc123", 999));
    }

    #[test]
    fn test_has_other_sessions_own_pid_excluded() {
        let ps_output = "  PID ARGS\n  100 container exec --tty agentbox-myapp-abc123 bash\n";
        assert!(!has_other_sessions(ps_output, "agentbox-myapp-abc123", 100));
    }

    #[test]
    fn test_has_other_sessions_other_pid_found() {
        let ps_output = "  PID ARGS\n  100 container exec --tty agentbox-myapp-abc123 bash\n  200 vim\n";
        assert!(has_other_sessions(ps_output, "agentbox-myapp-abc123", 999));
    }

    #[test]
    fn test_has_other_sessions_different_container() {
        let ps_output = "  PID ARGS\n  100 container exec --tty agentbox-other-def456 bash\n";
        assert!(!has_other_sessions(ps_output, "agentbox-myapp-abc123", 999));
    }

    #[test]
    fn test_has_other_sessions_multiple_sessions() {
        let ps_output = "  PID ARGS\n  100 container exec --tty agentbox-myapp-abc123 bash\n  200 container exec agentbox-myapp-abc123 bash -lc claude\n";
        assert!(has_other_sessions(ps_output, "agentbox-myapp-abc123", 999));
    }

    #[test]
    fn test_has_other_sessions_run_command() {
        let ps_output = "  PID ARGS\n  100 container run --name agentbox-myapp-abc123 --cpus 4 agentbox:default\n";
        assert!(has_other_sessions(ps_output, "agentbox-myapp-abc123", 999));
    }

    #[test]
    fn test_count_sessions_zero() {
        let ps_output = "  PID ARGS\n  100 vim main.rs\n";
        assert_eq!(count_sessions(ps_output, "agentbox-myapp-abc123"), 0);
    }

    #[test]
    fn test_count_sessions_one() {
        let ps_output = "  PID ARGS\n  100 container exec --tty agentbox-myapp-abc123 bash\n";
        assert_eq!(count_sessions(ps_output, "agentbox-myapp-abc123"), 1);
    }

    #[test]
    fn test_count_sessions_multiple() {
        let ps_output = "  PID ARGS\n  100 container exec --tty agentbox-myapp-abc123 bash\n  200 container exec agentbox-myapp-abc123 bash -lc claude\n  300 container run --name agentbox-myapp-abc123 --cpus 4 agentbox:default\n";
        assert_eq!(count_sessions(ps_output, "agentbox-myapp-abc123"), 3);
    }

    #[test]
    fn test_count_sessions_ignores_runtime_process() {
        let ps_output = "  PID ARGS\n  100 /usr/local/libexec/container/plugins/container-runtime-linux/bin/container-runtime-linux start --root /Users/alex/Library/Application Support/com.apple.container/containers/agentbox-myapp-abc123 --uuid agentbox-myapp-abc123\n";
        assert_eq!(count_sessions(ps_output, "agentbox-myapp-abc123"), 0);
    }

    #[test]
    fn test_count_sessions_different_container() {
        let ps_output = "  PID ARGS\n  100 container exec --tty agentbox-other-def456 bash\n";
        assert_eq!(count_sessions(ps_output, "agentbox-myapp-abc123"), 0);
    }

    #[test]
    fn test_exec_args_with_cli_flags() {
        let cli_flags = vec![
            "--append-system-prompt".to_string(),
            "Be careful.".to_string(),
            "--model".to_string(),
            "sonnet".to_string(),
        ];
        let mode = RunMode::Agent {
            agent: CodingAgent::Claude,
            task: None,
            cli_flags: cli_flags.clone(),
        };
        let args = build_exec_args("mycontainer", &mode, &[]);
        let cmd = args.last().unwrap();
        assert!(cmd.contains("exec claude"));
        assert!(cmd.contains("'--append-system-prompt' 'Be careful.'"));
        assert!(cmd.contains("'--model' 'sonnet'"));
    }

    #[test]
    fn test_exec_args_cli_flags_before_task() {
        let cli_flags = vec!["--model".to_string(), "sonnet".to_string()];
        let mode = RunMode::Agent {
            agent: CodingAgent::Claude,
            task: Some("fix tests".into()),
            cli_flags: cli_flags.clone(),
        };
        let args = build_exec_args("mycontainer", &mode, &[]);
        let cmd = args.last().unwrap();
        // Flags should appear before -p (task marker)
        let model_pos = cmd.find("'--model'").unwrap();
        let task_pos = cmd.find("'-p'").unwrap();
        assert!(model_pos < task_pos);
    }

    #[test]
    fn test_exec_args_cli_flags_empty() {
        let mode = RunMode::Agent { agent: CodingAgent::Claude, task: None, cli_flags: vec![] };
        let args = build_exec_args("mycontainer", &mode, &[]);
        let cmd = args.last().unwrap();
        assert_eq!(cmd, "exec claude");
    }

    #[test]
    fn test_exec_args_cli_flags_with_single_quotes() {
        let cli_flags = vec![
            "--append-system-prompt".to_string(),
            "Don't break things".to_string(),
        ];
        let mode = RunMode::Agent {
            agent: CodingAgent::Claude,
            task: None,
            cli_flags: cli_flags.clone(),
        };
        let args = build_exec_args("mycontainer", &mode, &[]);
        let cmd = args.last().unwrap();
        // Single quotes in values must be escaped
        assert!(cmd.contains("Don'\\''t break things"));
    }

    #[test]
    fn test_run_args_with_cli_flags() {
        let opts = RunOpts {
            name: "agentbox-myapp-abc123".into(),
            image: "agentbox:default".into(),
            workdir: "/Users/alex/Dev/myapp".into(),
            cpus: 2,
            memory: "4G".into(),
            env_vars: vec![],
            volumes: vec![],
            mode: RunMode::Agent { agent: CodingAgent::Claude, task: Some("fix tests".into()), cli_flags: vec!["--model".into(), "sonnet".into()] },
        };
        let args = opts.to_run_args();
        let image_pos = args.iter().position(|a| a == "agentbox:default").unwrap();
        let model_pos = args.iter().position(|a| a == "--model").unwrap();
        let p_pos = args.iter().position(|a| a == "-p").unwrap();
        // cli_flags come after image, before -p
        assert!(image_pos < model_pos);
        assert!(model_pos < p_pos);
    }

    #[test]
    fn test_run_args_cli_flags_empty() {
        let opts = RunOpts {
            name: "test".into(),
            image: "agentbox:default".into(),
            workdir: "/tmp".into(),
            cpus: 1,
            memory: "4G".into(),
            env_vars: vec![],
            volumes: vec![],
            mode: RunMode::Agent { agent: CodingAgent::Claude, task: None, cli_flags: vec![] },
        };
        let args = opts.to_run_args();
        // Last arg should be --claude (entrypoint_arg) since no task and no cli_flags
        assert_eq!(args.last().unwrap(), "--claude");
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
            mode: RunMode::Agent { agent: CodingAgent::Claude, task: Some("fix the tests".into()), cli_flags: vec![] },
        };
        let args = opts.to_run_args();
        assert!(!args.contains(&"--interactive".to_string()));
        assert!(!args.contains(&"--tty".to_string()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"fix the tests".to_string()));
    }

    #[test]
    fn test_run_args_shell_interactive_no_cmd() {
        let opts = RunOpts {
            name: "agentbox-myapp-abc123".into(),
            image: "agentbox:default".into(),
            workdir: "/Users/alex/Dev/myapp".into(),
            cpus: 2,
            memory: "4G".into(),
            env_vars: vec![],
            volumes: vec![],
            mode: RunMode::Shell { cmd: vec![] },
        };
        let args = opts.to_run_args();
        let image_pos = args.iter().position(|a| a == "agentbox:default").unwrap();
        let after_image: Vec<&String> = args.iter().skip(image_pos + 1).collect();
        assert_eq!(after_image, vec![&"--shell".to_string()]);
        // Interactive: TTY flags present
        assert!(args.contains(&"--interactive".to_string()));
        assert!(args.contains(&"--tty".to_string()));
    }

    #[test]
    fn test_run_args_shell_with_cmd() {
        let opts = RunOpts {
            name: "agentbox-myapp-abc123".into(),
            image: "agentbox:default".into(),
            workdir: "/Users/alex/Dev/myapp".into(),
            cpus: 2,
            memory: "4G".into(),
            env_vars: vec![],
            volumes: vec![],
            mode: RunMode::Shell {
                cmd: vec!["ls".into(), "-la".into(), "/workspace".into()],
            },
        };
        let args = opts.to_run_args();
        let image_pos = args.iter().position(|a| a == "agentbox:default").unwrap();
        let after_image: Vec<&String> = args.iter().skip(image_pos + 1).collect();
        assert_eq!(
            after_image,
            vec![
                &"--shell".to_string(),
                &"ls".to_string(),
                &"-la".to_string(),
                &"/workspace".to_string(),
            ]
        );
        // Headless: no TTY flags
        assert!(!args.contains(&"--tty".to_string()));
    }

    #[test]
    fn test_run_mode_is_interactive() {
        assert!(RunMode::Agent { agent: CodingAgent::Claude, task: None, cli_flags: vec![] }.is_interactive());
        assert!(!RunMode::Agent { agent: CodingAgent::Claude, task: Some("t".into()), cli_flags: vec![] }.is_interactive());
        assert!(RunMode::Shell { cmd: vec![] }.is_interactive());
        assert!(!RunMode::Shell { cmd: vec!["ls".into()] }.is_interactive());
    }

    #[test]
    fn test_exec_args_shell_interactive_no_cmd() {
        let mode = RunMode::Shell { cmd: vec![] };
        let args = build_exec_args("mycontainer", &mode, &[]);
        assert!(args.contains(&"--interactive".to_string()));
        assert!(args.contains(&"--tty".to_string()));
        let lc_pos = args.iter().position(|a| a == "-lc").unwrap();
        let script = &args[lc_pos + 1];
        assert!(script.ends_with("exec bash -l"), "got: {}", script);
        assert!(!script.contains("claude"));
    }

    #[test]
    fn test_exec_args_shell_with_cmd() {
        let mode = RunMode::Shell {
            cmd: vec!["ls".into(), "-la".into(), "/workspace".into()],
        };
        let args = build_exec_args("mycontainer", &mode, &[]);
        assert!(!args.contains(&"--tty".to_string()));
        let lc_pos = args.iter().position(|a| a == "-lc").unwrap();
        let script = &args[lc_pos + 1];
        assert!(script.ends_with("exec \"$@\""), "got: {}", script);
        assert!(!script.contains("claude"));
        assert_eq!(&args[lc_pos + 2], "bash");
        assert_eq!(&args[lc_pos + 3], "ls");
        assert_eq!(&args[lc_pos + 4], "-la");
        assert_eq!(&args[lc_pos + 5], "/workspace");
    }

    #[test]
    fn test_exec_args_shell_cmd_preserves_quotes_via_positional_args() {
        let mode = RunMode::Shell {
            cmd: vec!["echo".into(), "Don't".into()],
        };
        let args = build_exec_args("mycontainer", &mode, &[]);
        let lc_pos = args.iter().position(|a| a == "-lc").unwrap();
        assert_eq!(&args[lc_pos + 3], "echo");
        assert_eq!(&args[lc_pos + 4], "Don't");
    }

    #[test]
    fn test_exec_args_shell_with_hostexec_env() {
        let env_vars = vec![
            ("HOSTEXEC_HOST".to_string(), "192.168.64.1".to_string()),
            ("HOSTEXEC_PORT".to_string(), "12345".to_string()),
            ("HOSTEXEC_TOKEN".to_string(), "tok".to_string()),
            ("HOSTEXEC_COMMANDS".to_string(), "xcodebuild xcrun".to_string()),
        ];
        let mode = RunMode::Shell { cmd: vec![] };
        let args = build_exec_args("mycontainer", &mode, &env_vars);
        let lc_pos = args.iter().position(|a| a == "-lc").unwrap();
        let script = &args[lc_pos + 1];
        assert!(script.contains("HOSTEXEC_COMMANDS"));
        assert!(script.contains("ln -sf /usr/local/bin/hostexec"));
        assert!(script.contains("exec bash -l"));
        let setup_pos = script.find("ln -sf").unwrap();
        let bash_pos = script.find("exec bash").unwrap();
        assert!(setup_pos < bash_pos);
    }

    #[test]
    fn test_run_args_agent_codex_interactive_no_task() {
        let opts = RunOpts {
            name: "agentbox-app-abc123".into(),
            image: "agentbox:default".into(),
            workdir: "/Users/alex/Dev/app".into(),
            cpus: 4,
            memory: "8G".into(),
            env_vars: vec![],
            volumes: vec![],
            mode: RunMode::Agent {
                agent: CodingAgent::Codex,
                task: None,
                cli_flags: vec!["--dangerously-bypass-approvals-and-sandbox".into()],
            },
        };
        let args = opts.to_run_args();
        let image_idx = args.iter().position(|a| a == "agentbox:default").unwrap();
        // After the image: --codex, then flags. No "exec".
        assert_eq!(args[image_idx + 1], "--codex");
        assert_eq!(args[image_idx + 2], "--dangerously-bypass-approvals-and-sandbox");
        assert!(!args[image_idx + 1..].contains(&"exec".to_string()));
        assert!(args.contains(&"--interactive".to_string()));
        assert!(args.contains(&"--tty".to_string()));
    }

    #[test]
    fn test_run_args_agent_codex_headless_puts_exec_before_flags() {
        let opts = RunOpts {
            name: "agentbox-app-abc123".into(),
            image: "agentbox:default".into(),
            workdir: "/Users/alex/Dev/app".into(),
            cpus: 4,
            memory: "8G".into(),
            env_vars: vec![],
            volumes: vec![],
            mode: RunMode::Agent {
                agent: CodingAgent::Codex,
                task: Some("fix tests".into()),
                cli_flags: vec!["--dangerously-bypass-approvals-and-sandbox".into()],
            },
        };
        let args = opts.to_run_args();
        let image_idx = args.iter().position(|a| a == "agentbox:default").unwrap();
        assert_eq!(args[image_idx + 1], "--codex");
        assert_eq!(args[image_idx + 2], "exec");
        assert_eq!(
            args[image_idx + 3],
            "--dangerously-bypass-approvals-and-sandbox"
        );
        assert_eq!(args[image_idx + 4], "fix tests");
        // Headless: no TTY
        assert!(!args.contains(&"--tty".to_string()));
    }

    #[test]
    fn test_run_args_agent_claude_headless_preserves_legacy_ordering() {
        let opts = RunOpts {
            name: "agentbox-app-abc123".into(),
            image: "agentbox:default".into(),
            workdir: "/Users/alex/Dev/app".into(),
            cpus: 4,
            memory: "8G".into(),
            env_vars: vec![],
            volumes: vec![],
            mode: RunMode::Agent {
                agent: CodingAgent::Claude,
                task: Some("fix tests".into()),
                cli_flags: vec!["--dangerously-skip-permissions".into()],
            },
        };
        let args = opts.to_run_args();
        let image_idx = args.iter().position(|a| a == "agentbox:default").unwrap();
        assert_eq!(args[image_idx + 1], "--claude");
        assert_eq!(args[image_idx + 2], "--dangerously-skip-permissions");
        assert_eq!(args[image_idx + 3], "-p");
        assert_eq!(args[image_idx + 4], "fix tests");
    }

    #[test]
    fn test_exec_args_agent_codex_headless() {
        let env_vars: Vec<(String, String)> = vec![];
        let mode = RunMode::Agent {
            agent: CodingAgent::Codex,
            task: Some("fix tests".into()),
            cli_flags: vec!["--dangerously-bypass-approvals-and-sandbox".into()],
        };
        let args = build_exec_args("mycontainer", &mode, &env_vars);
        let cmd = args.last().unwrap();
        assert!(cmd.contains("codex"));
        // `exec` subcommand must come BEFORE flags
        let exec_pos = cmd.find("'exec'").expect("expected 'exec' token in bash cmd");
        let flag_pos = cmd
            .find("'--dangerously-bypass-approvals-and-sandbox'")
            .expect("expected bypass flag");
        assert!(
            exec_pos < flag_pos,
            "exec should precede flags in codex headless; got: {cmd}"
        );
        assert!(cmd.contains("'fix tests'"));
        // Must NOT contain '-p' token (that's claude's headless syntax).
        // The quoted form matches how real argv tokens appear in the payload.
        assert!(!cmd.contains("'-p'"));
    }

    #[test]
    fn test_exec_args_agent_codex_interactive() {
        let env_vars: Vec<(String, String)> = vec![];
        let mode = RunMode::Agent {
            agent: CodingAgent::Codex,
            task: None,
            cli_flags: vec![],
        };
        let args = build_exec_args("mycontainer", &mode, &env_vars);
        let cmd = args.last().unwrap();
        assert!(cmd.contains("codex"));
        assert!(!cmd.contains("'exec'"));
        assert!(args.contains(&"--interactive".to_string()));
        assert!(args.contains(&"--tty".to_string()));
    }

    #[test]
    fn test_exec_args_shell_with_forward_not_found() {
        let env_vars = vec![
            ("HOSTEXEC_COMMANDS".to_string(), "xcodebuild".to_string()),
            ("HOSTEXEC_FORWARD_NOT_FOUND".to_string(), "true".to_string()),
        ];
        let mode = RunMode::Shell { cmd: vec![] };
        let args = build_exec_args("mycontainer", &mode, &env_vars);
        let lc_pos = args.iter().position(|a| a == "-lc").unwrap();
        let script = &args[lc_pos + 1];
        assert!(script.contains("command_not_found_handle"));
        assert!(script.contains("exec bash -l"));
    }

    #[test]
    fn test_exec_args_shell_forward_not_found_standalone() {
        let env_vars = vec![
            ("HOSTEXEC_FORWARD_NOT_FOUND".to_string(), "true".to_string()),
        ];
        let mode = RunMode::Shell { cmd: vec![] };
        let args = build_exec_args("mycontainer", &mode, &env_vars);
        let lc_pos = args.iter().position(|a| a == "-lc").unwrap();
        let script = &args[lc_pos + 1];
        assert!(script.contains("command_not_found_handle"));
        assert!(!script.contains("ln -sf /usr/local/bin/hostexec"));
        assert!(script.contains("exec bash -l"));
    }

    #[test]
    fn test_exec_args_shell_with_cmd_includes_setup_prefix() {
        let env_vars = vec![
            ("HOSTEXEC_COMMANDS".to_string(), "xcodebuild".to_string()),
        ];
        let mode = RunMode::Shell {
            cmd: vec!["xcodebuild".into(), "-version".into()],
        };
        let args = build_exec_args("mycontainer", &mode, &env_vars);
        let lc_pos = args.iter().position(|a| a == "-lc").unwrap();
        let script = &args[lc_pos + 1];
        assert!(script.contains("ln -sf /usr/local/bin/hostexec"));
        assert!(script.ends_with("exec \"$@\""));
    }
}
