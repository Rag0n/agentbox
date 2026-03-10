use std::process::Command;

/// Read git user.name and user.email from host and return
/// as environment variable pairs for the container.
pub fn git_env_vars() -> Vec<(String, String)> {
    let name = git_config("user.name");
    let email = git_config("user.email");

    match (name, email) {
        (Some(name), Some(email)) => vec![
            ("GIT_AUTHOR_NAME".into(), name.clone()),
            ("GIT_AUTHOR_EMAIL".into(), email.clone()),
            ("GIT_COMMITTER_NAME".into(), name),
            ("GIT_COMMITTER_EMAIL".into(), email),
        ],
        _ => vec![],
    }
}

fn git_config(key: &str) -> Option<String> {
    Command::new("git")
        .args(["config", "--global", key])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_env_vars_returns_four_vars() {
        let vars = git_env_vars();
        // Either 0 (not configured) or 4 (all four vars)
        assert!(vars.len() == 0 || vars.len() == 4);
    }

    #[test]
    fn test_git_env_var_names() {
        let vars = git_env_vars();
        if !vars.is_empty() {
            let keys: Vec<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
            assert!(keys.contains(&"GIT_AUTHOR_NAME"));
            assert!(keys.contains(&"GIT_AUTHOR_EMAIL"));
            assert!(keys.contains(&"GIT_COMMITTER_NAME"));
            assert!(keys.contains(&"GIT_COMMITTER_EMAIL"));
        }
    }
}
