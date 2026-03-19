use std::collections::HashSet;

use tokio::io::AsyncReadExt;
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;

use crate::bridge::protocol::ServerMessage;

/// Check if a command is allowed by the allowlist.
/// Matches cmd[0] exactly against the allowed set.
pub fn is_command_allowed(cmd: &[String], allowed: &HashSet<String>) -> bool {
    cmd.first().is_some_and(|c| allowed.contains(c.as_str()))
}

/// Parse a signal name (e.g., "SIGINT") to a nix signal number.
pub fn parse_signal(name: &str) -> Option<i32> {
    match name {
        "SIGINT" => Some(2),
        "SIGHUP" => Some(1),
        "SIGTERM" => Some(15),
        "SIGQUIT" => Some(3),
        "SIGKILL" => Some(9),
        _ => None,
    }
}

/// Spawn a command and stream its I/O over the provided channel.
/// Returns the child's PID and a stdin writer handle.
/// The child is waited on internally; an Exit message is sent when it completes.
pub async fn spawn_and_stream(
    id: String,
    cmd: &[String],
    cwd: Option<&str>,
    default_cwd: &str,
    tx: mpsc::UnboundedSender<ServerMessage>,
) -> Result<(u32, tokio::process::ChildStdin), String> {
    let program = &cmd[0];
    let args = &cmd[1..];

    let work_dir = cwd.unwrap_or(default_cwd);
    let work_dir = if std::path::Path::new(work_dir).exists() {
        work_dir
    } else {
        default_cwd
    };

    let mut child = TokioCommand::new(program)
        .args(args)
        .current_dir(work_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {}", program, e))?;

    let pid = child.id().unwrap_or(0);
    let stdin = child.stdin.take().unwrap();

    // Spawn stdout reader — chunk-based, not line-based
    let stdout_handle = if let Some(stdout) = child.stdout.take() {
        let tx = tx.clone();
        let id = id.clone();
        Some(tokio::spawn(async move {
            let mut reader = stdout;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&buf[..n]).to_string();
                        let _ = tx.send(ServerMessage::Stdout {
                            id: id.clone(),
                            data,
                        });
                    }
                    Err(_) => break,
                }
            }
        }))
    } else {
        None
    };

    // Spawn stderr reader — same chunk-based approach
    let stderr_handle = if let Some(stderr) = child.stderr.take() {
        let tx = tx.clone();
        let id = id.clone();
        Some(tokio::spawn(async move {
            let mut reader = stderr;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&buf[..n]).to_string();
                        let _ = tx.send(ServerMessage::Stderr {
                            id: id.clone(),
                            data,
                        });
                    }
                    Err(_) => break,
                }
            }
        }))
    } else {
        None
    };

    // Spawn a task to wait on the child and send the Exit message.
    // Must await stdout/stderr readers first so all output is forwarded before Exit.
    {
        let tx = tx.clone();
        let id = id.clone();
        tokio::spawn(async move {
            let exit_status = child.wait().await;
            let code = exit_status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
            // Ensure all output is flushed before sending Exit
            if let Some(h) = stdout_handle {
                let _ = h.await;
            }
            if let Some(h) = stderr_handle {
                let _ = h.await;
            }
            let _ = tx.send(ServerMessage::Exit { id, code });
        });
    }

    Ok((pid, stdin))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_command() {
        let allowed: HashSet<String> = ["xcodebuild", "adb"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let cmd = vec![
            "xcodebuild".to_string(),
            "-project".to_string(),
            "Foo.xcodeproj".to_string(),
        ];
        assert!(is_command_allowed(&cmd, &allowed));
    }

    #[test]
    fn test_disallowed_command() {
        let allowed: HashSet<String> = ["xcodebuild".to_string()].into_iter().collect();
        let cmd = vec!["rm".to_string(), "-rf".to_string(), "/".to_string()];
        assert!(!is_command_allowed(&cmd, &allowed));
    }

    #[test]
    fn test_empty_cmd() {
        let allowed: HashSet<String> = ["xcodebuild".to_string()].into_iter().collect();
        let cmd: Vec<String> = vec![];
        assert!(!is_command_allowed(&cmd, &allowed));
    }

    #[test]
    fn test_exact_match_not_prefix() {
        let allowed: HashSet<String> = ["xc".to_string()].into_iter().collect();
        let cmd = vec!["xcodebuild".to_string()];
        assert!(!is_command_allowed(&cmd, &allowed));
    }

    #[test]
    fn test_parse_known_signals() {
        assert_eq!(parse_signal("SIGINT"), Some(2));
        assert_eq!(parse_signal("SIGTERM"), Some(15));
        assert_eq!(parse_signal("SIGKILL"), Some(9));
        assert_eq!(parse_signal("SIGHUP"), Some(1));
        assert_eq!(parse_signal("SIGQUIT"), Some(3));
    }

    #[test]
    fn test_parse_unknown_signal() {
        assert_eq!(parse_signal("SIGFOO"), None);
    }
}
