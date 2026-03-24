use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::Config;

fn expand_tilde(path: &Path) -> Result<PathBuf> {
    let s = path.to_string_lossy();
    if let Some(suffix) = s.strip_prefix("~/") {
        let home = dirs::home_dir().context("cannot determine home directory")?;
        Ok(home.join(suffix))
    } else if s == "~" {
        dirs::home_dir().context("cannot determine home directory")
    } else {
        Ok(path.to_path_buf())
    }
}

pub const DEFAULT_DOCKERFILE: &str = include_str!("../resources/Dockerfile.default");
pub const ENTRYPOINT_SCRIPT: &str = include_str!("../resources/entrypoint.sh");

/// Compile-time path to the agentbox source directory.
/// Valid when built with `cargo build` or `cargo install --path .`.
/// Invalid when installed via `cargo install --git` (temp dir, cleaned up).
const PROJECT_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// Git repo URL used to fetch source when local source is unavailable.
const GIT_REPO_URL: &str = "https://github.com/Rag0n/agentbox.git";

/// Git-based builder stage, used when local source is not available.
const HOSTEXEC_GIT_BUILDER: &str = "\
FROM rust:bookworm AS hostexec-builder\n\
RUN cargo install --git ${AGENTBOX_REPO} --root /usr/local\n";

pub fn checksum(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

/// Resolve which Dockerfile to use. Returns (content, image_tag).
pub fn resolve_dockerfile(
    project_dir: &Path,
    profile: Option<&str>,
    config: &Config,
) -> Result<(String, String)> {
    let dir_name = project_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();

    // 1. Per-project Dockerfile
    let project_df = project_dir.join("agentbox.Dockerfile");
    if project_df.exists() {
        let content = std::fs::read_to_string(&project_df)
            .with_context(|| format!("failed to read {}", project_df.display()))?;
        return Ok((content, format!("agentbox:project-{}", dir_name)));
    }

    // 2. Named profile
    if let Some(name) = profile {
        if let Some(p) = config.profiles.get(name) {
            let df_path = expand_tilde(&p.dockerfile)?;
            let content = std::fs::read_to_string(&df_path).with_context(|| {
                format!(
                    "failed to read profile '{}' Dockerfile: {}",
                    name,
                    df_path.display()
                )
            })?;
            return Ok((content, format!("agentbox:profile-{}", name)));
        } else {
            anyhow::bail!("profile '{}' not found in config", name);
        }
    }

    // 3. Global default override
    if let Some(ref df) = config.dockerfile {
        let df_path = expand_tilde(df)?;
        let content = std::fs::read_to_string(&df_path)
            .with_context(|| format!("failed to read {}", df_path.display()))?;
        return Ok((content, "agentbox:default".into()));
    }

    // 4. Built-in default
    Ok((DEFAULT_DOCKERFILE.to_string(), "agentbox:default".into()))
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".cache")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("agentbox")
}

pub fn needs_build(dockerfile_content: &str, cache_key: &str, cache_path: &Path) -> bool {
    let current_hash = checksum(dockerfile_content);
    let cache_file = cache_path.join(format!("{}.sha256", cache_key));

    match std::fs::read_to_string(&cache_file) {
        Ok(cached_hash) => cached_hash.trim() != current_hash,
        Err(_) => true,
    }
}

pub fn save_cache(dockerfile_content: &str, cache_key: &str, cache_path: &Path) -> Result<()> {
    std::fs::create_dir_all(cache_path)?;
    let hash = checksum(dockerfile_content);
    let cache_file = cache_path.join(format!("{}.sha256", cache_key));
    std::fs::write(&cache_file, &hash)?;
    Ok(())
}

/// Check if a Dockerfile references `agentbox:default` as a base image.
fn references_default_base(dockerfile_content: &str) -> bool {
    dockerfile_content.lines().any(|line| {
        let trimmed = line.trim().to_lowercase();
        trimmed == "from agentbox:default" || trimmed.starts_with("from agentbox:default ")
    })
}

/// If the Dockerfile uses `FROM agentbox:default`, ensure that base image is built first.
pub fn ensure_base_image(
    dockerfile_content: &str,
    no_cache: bool,
    verbose: bool,
) -> Result<()> {
    if !references_default_base(dockerfile_content) {
        return Ok(());
    }

    let cache_key = "agentbox-default";
    if no_cache || needs_build(DEFAULT_DOCKERFILE, cache_key, &cache_dir()) {
        eprintln!("Building base image agentbox:default...");
        build("agentbox:default", DEFAULT_DOCKERFILE, no_cache, false, verbose)?;
        save_cache(DEFAULT_DOCKERFILE, cache_key, &cache_dir())?;
    }
    Ok(())
}

/// Build args for `container build`. Extracted for testability.
fn build_args(
    tag: &str,
    dockerfile_content: &str,
    dockerfile_path: &str,
    context_path: &str,
    no_cache: bool,
    pull: bool,
) -> Vec<String> {
    let mut args = vec!["build".to_string()];
    // --pull only when explicitly requested AND base image is remote.
    if pull && !references_default_base(dockerfile_content) {
        args.push("--pull".into());
    }
    args.extend([
        "-t".into(),
        tag.to_string(),
        "-f".into(),
        dockerfile_path.to_string(),
    ]);
    if no_cache {
        args.push("--no-cache".into());
    }
    args.push(context_path.to_string());
    args
}

/// Reset the buildkit builder after a crash.
/// Uses `container builder` commands (more reliable than stopping the buildkit container directly).
/// See: https://github.com/apple/container/issues/284
fn reset_buildkit(verbose: bool) {
    if verbose {
        eprintln!("[agentbox] resetting buildkit...");
    }
    let _ = Command::new("container")
        .args(["builder", "stop", "--force"])
        .output();
    let _ = Command::new("container")
        .args(["builder", "delete"])
        .output();
}

/// Check if local agentbox source is available (true for `cargo install --path .` builds).
fn local_source_available() -> bool {
    Path::new(PROJECT_DIR).join("Cargo.toml").exists()
}

/// Create a tar archive of project source files in the build context.
/// Uses tar instead of COPY to work around Apple Container buildkit bug
/// where COPY doesn't transfer files inside subdirectories.
fn create_source_tar(context_dir: &Path) -> Result<()> {
    let project = Path::new(PROJECT_DIR);
    let tar_path = context_dir.join("source.tar");

    let mut tar_args = vec![
        "cf".to_string(),
        tar_path.to_string_lossy().to_string(),
        "-C".to_string(),
        project.to_string_lossy().to_string(),
        "Cargo.toml".to_string(),
        "src".to_string(),
        "resources".to_string(),
    ];

    // Include Cargo.lock if it exists
    if project.join("Cargo.lock").exists() {
        tar_args.push("Cargo.lock".to_string());
    }

    let status = Command::new("tar")
        .args(&tar_args)
        .status()
        .context("failed to run tar")?;

    if !status.success() {
        anyhow::bail!("failed to create source tar archive");
    }

    Ok(())
}

/// Replace the local COPY-based builder stage with a `cargo install --git` stage.
/// Both variants output the binary to /usr/local/bin/agentbox, so the COPY --from line is unchanged.
fn swap_to_git_builder(dockerfile: &str) -> String {
    let git_stage = HOSTEXEC_GIT_BUILDER.replace("${AGENTBOX_REPO}", GIT_REPO_URL);
    // Replace everything from the first FROM (builder stage) up to the second FROM (final stage)
    if let Some(pos) = dockerfile.find("\nFROM debian:") {
        format!("{}\n{}", git_stage.trim(), &dockerfile[pos..])
    } else {
        dockerfile.to_string()
    }
}

/// Build an image using `container build`.
/// Automatically detects and recovers from buildkit crashes by resetting and retrying once.
pub fn build(tag: &str, dockerfile_content: &str, no_cache: bool, pull: bool, verbose: bool) -> Result<()> {
    let tmp = tempfile::tempdir().context("failed to create temp dir")?;

    // Determine effective Dockerfile: if it has a hostexec builder stage and
    // local source is unavailable, swap to git-based builder stage.
    let effective_dockerfile = if dockerfile_content.contains("hostexec-builder") {
        if local_source_available() {
            create_source_tar(tmp.path())
                .context("failed to create source tar for build context")?;
            dockerfile_content.to_string()
        } else {
            swap_to_git_builder(dockerfile_content)
        }
    } else {
        dockerfile_content.to_string()
    };

    let df_path = tmp.path().join("Dockerfile");
    std::fs::write(&df_path, &effective_dockerfile)?;
    // Write entrypoint script so Dockerfile COPY can find it
    let ep_path = tmp.path().join("entrypoint.sh");
    std::fs::write(&ep_path, ENTRYPOINT_SCRIPT)?;

    let args = build_args(
        tag,
        &effective_dockerfile,
        &df_path.to_string_lossy(),
        &tmp.path().to_string_lossy(),
        no_cache,
        pull,
    );

    if verbose {
        eprintln!("[agentbox] container {}", args.join(" "));
    }

    // Pipe stderr so we can detect buildkit crashes while still showing output in real-time.
    let mut child = Command::new("container")
        .args(&args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run 'container build'")?;

    let stderr_pipe = child.stderr.take().unwrap();
    let reader = BufReader::new(stderr_pipe);
    let mut captured_stderr = String::new();

    for line in reader.lines().flatten() {
        if verbose {
            eprintln!("{}", line);
        }
        captured_stderr.push_str(&line);
        captured_stderr.push('\n');
    }

    let status = child.wait().context("failed to wait for 'container build'")?;

    if status.success() {
        return Ok(());
    }

    // Detect buildkit crash (Apple Container framework bug) and auto-recover.
    if captured_stderr.contains("Negative count not allowed") {
        eprintln!("Detected buildkit crash, resetting builder and retrying...");
        reset_buildkit(verbose);

        let retry_status = Command::new("container")
            .args(&args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("failed to run 'container build' (retry)")?;

        if retry_status.success() {
            return Ok(());
        }
        anyhow::bail!("container build failed after buildkit reset");
    }

    anyhow::bail!("container build failed:\n{}", captured_stderr);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedded_dockerfile_not_empty() {
        assert!(!DEFAULT_DOCKERFILE.is_empty());
        assert!(DEFAULT_DOCKERFILE.contains("debian:bookworm-slim"));
    }

    #[test]
    fn test_dockerfile_checksum_deterministic() {
        let a = checksum("hello world");
        let b = checksum("hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn test_dockerfile_checksum_changes() {
        let a = checksum("version 1");
        let b = checksum("version 2");
        assert_ne!(a, b);
    }

    #[test]
    fn test_expand_tilde_home_relative() {
        let home = dirs::home_dir().unwrap();
        let expanded = expand_tilde(Path::new("~/foo/bar")).unwrap();
        assert_eq!(expanded, home.join("foo/bar"));
    }

    #[test]
    fn test_expand_tilde_absolute_unchanged() {
        let path = Path::new("/absolute/path");
        let expanded = expand_tilde(path).unwrap();
        assert_eq!(expanded, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn test_resolve_dockerfile_project_first() {
        let tmp = tempfile::tempdir().unwrap();
        let project_df = tmp.path().join("agentbox.Dockerfile");
        std::fs::write(&project_df, "FROM test:project").unwrap();

        let (content, tag) = resolve_dockerfile(tmp.path(), None, &Config::default()).unwrap();

        assert!(content.contains("FROM test:project"));
        assert!(tag.starts_with("agentbox:project-"));
    }

    #[test]
    fn test_resolve_dockerfile_falls_through_to_default() {
        let tmp = tempfile::tempdir().unwrap();

        let (content, tag) = resolve_dockerfile(tmp.path(), None, &Config::default()).unwrap();

        assert!(content.contains("debian:bookworm-slim"));
        assert_eq!(tag, "agentbox:default");
    }

    #[test]
    fn test_needs_build_no_cache() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(needs_build("test content", "default", tmp.path()));
    }

    #[test]
    fn test_references_default_base() {
        assert!(references_default_base("FROM agentbox:default"));
        assert!(references_default_base(
            "FROM agentbox:default\nRUN apt-get update"
        ));
        assert!(references_default_base("  FROM agentbox:default  "));
        assert!(references_default_base("FROM agentbox:default AS builder"));
        assert!(references_default_base("from agentbox:default as base"));
    }

    #[test]
    fn test_references_default_base_false() {
        assert!(!references_default_base("FROM debian:bookworm-slim"));
        assert!(!references_default_base("FROM agentbox:profile-ruby"));
        assert!(!references_default_base("# FROM agentbox:default"));
        assert!(!references_default_base("RUN echo agentbox:default"));
    }

    #[test]
    fn test_build_args_pull_when_requested() {
        let args = build_args(
            "agentbox:default",
            "FROM debian:bookworm-slim\nRUN echo hi",
            "/tmp/Dockerfile",
            "/tmp",
            false,
            true, // pull
        );
        assert!(args.contains(&"--pull".to_string()));
    }

    #[test]
    fn test_build_args_no_pull_when_not_requested() {
        let args = build_args(
            "agentbox:default",
            "FROM debian:bookworm-slim\nRUN echo hi",
            "/tmp/Dockerfile",
            "/tmp",
            false,
            false, // no pull
        );
        assert!(!args.contains(&"--pull".to_string()));
    }

    #[test]
    fn test_build_args_no_pull_for_local_base_even_when_requested() {
        let args = build_args(
            "agentbox:project-myapp",
            "FROM agentbox:default\nRUN apt-get install -y nodejs",
            "/tmp/Dockerfile",
            "/tmp",
            false,
            true, // pull requested, but local base overrides
        );
        assert!(!args.contains(&"--pull".to_string()));
    }

    #[test]
    fn test_build_args_no_cache_with_pull() {
        let args = build_args(
            "agentbox:default",
            "FROM debian:bookworm-slim",
            "/tmp/Dockerfile",
            "/tmp",
            true,
            true,
        );
        assert!(args.contains(&"--no-cache".to_string()));
        assert!(args.contains(&"--pull".to_string()));
    }

    #[test]
    fn test_needs_build_matching_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let content = "test dockerfile content";
        let hash = checksum(content);
        let cache_file = tmp.path().join("default.sha256");
        std::fs::write(&cache_file, &hash).unwrap();

        assert!(!needs_build(content, "default", tmp.path()));
    }

    #[test]
    fn test_local_source_available() {
        // Should be true when running tests (we have Cargo.toml at PROJECT_DIR)
        assert!(local_source_available());
    }

    #[test]
    fn test_swap_to_git_builder() {
        let local_df = "FROM rust:bookworm AS hostexec-builder\n\
                         WORKDIR /build\n\
                         COPY Cargo.toml Cargo.lock ./\n\
                         RUN cargo build --release\n\
                         \n\
                         FROM debian:bookworm-slim\n\
                         RUN echo hello\n";
        let result = swap_to_git_builder(local_df);
        assert!(result.contains("cargo install --git"));
        assert!(result.contains("FROM debian:bookworm-slim"));
        assert!(!result.contains("COPY Cargo.toml"));
    }

    #[test]
    fn test_swap_to_git_builder_preserves_final_stage() {
        let local_df = "FROM rust:bookworm AS hostexec-builder\n\
                         RUN something\n\
                         \n\
                         FROM debian:bookworm-slim\n\
                         RUN apt-get update\n\
                         COPY --from=hostexec-builder /usr/local/bin/agentbox /usr/local/bin/hostexec\n\
                         USER user\n";
        let result = swap_to_git_builder(local_df);
        assert!(result.contains("COPY --from=hostexec-builder /usr/local/bin/agentbox /usr/local/bin/hostexec"));
        assert!(result.contains("USER user"));
        assert!(result.contains("apt-get update"));
    }

    #[test]
    fn test_default_dockerfile_has_hostexec_builder() {
        assert!(DEFAULT_DOCKERFILE.contains("hostexec-builder"));
        assert!(DEFAULT_DOCKERFILE.contains("COPY --from=hostexec-builder"));
    }
}
