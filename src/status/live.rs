//! Live-mode implementation for `agentbox status`.
//!
//! Contains the tokio-based polling loop, terminal-mode RAII guard, and
//! the subprocess helper that races stdout/stderr reads against a
//! shutdown watch channel.

use anyhow::{bail, Context, Result};
use std::io::{self, IsTerminal, Write};
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::watch;

use crossterm::{cursor, terminal, ExecutableCommand};

/// Spawn a subprocess, drain stdout+stderr concurrently, and race the
/// drain against a shutdown signal.
///
/// - Both stdout and stderr are piped (not inherited) — child diagnostics
///   never reach the alt-screen UI.
/// - On non-zero exit, returns `Err` with captured stderr included.
/// - On shutdown, the child is killed (SIGKILL via `start_kill`) and
///   reaped, and the function returns an error.
pub async fn fetch_once(
    program: &str,
    args: &[&str],
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Vec<u8>> {
    // Synchronous check: is shutdown already set? `borrow()` returns the
    // current value independent of this receiver's version-tracking, so
    // this catches the "already shut down" case that a later
    // `changed()` call would miss (since `changed()` only fires on a
    // *new* change relative to the receiver's last observed version).
    if *shutdown.borrow() {
        bail!("shutdown requested");
    }

    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)   // critical: if the future is cancelled
                              // (e.g. outer select picks a different arm),
                              // Tokio must SIGKILL the child. Without this,
                              // dropping a Child leaves an orphan process.
        .spawn()
        .with_context(|| format!("failed to spawn `{} {}`", program, args.join(" ")))?;
    let mut stdout = child.stdout.take().expect("stdout was requested");
    let mut stderr = child.stderr.take().expect("stderr was requested");
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();

    let drain = async {
        tokio::try_join!(
            stdout.read_to_end(&mut out_buf),
            stderr.read_to_end(&mut err_buf),
        )
    };

    tokio::select! {
        res = drain => {
            res.with_context(|| format!("failed reading output of `{}`", program))?;
            let status = child.wait().await
                .with_context(|| format!("failed waiting on `{}`", program))?;
            if !status.success() {
                bail!(
                    "`{} {}` exited with {}: {}",
                    program,
                    args.join(" "),
                    status,
                    String::from_utf8_lossy(&err_buf).trim(),
                );
            }
            Ok(out_buf)
        }
        _ = shutdown.changed() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            bail!("shutdown requested");
        }
    }
}

/// RAII guard for TUI terminal state. On construction (when stdout is a
/// TTY) it enters the alternate screen, hides the cursor, and enables
/// raw mode. On drop (including panic unwind) it reverses all three.
///
/// The guard is also paired with a process-global panic hook that runs
/// the same restoration inline, so a panic reaches a sane terminal
/// before the default panic handler prints.
pub struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    /// Construct a guard. If stdout is not a TTY, returns an inactive
    /// guard that does nothing on drop (live mode should not be invoked
    /// off-TTY, but this avoids surprising callers).
    pub fn new_if_tty() -> Self {
        if !io::stdout().is_terminal() {
            return Self { active: false };
        }
        install_panic_hook_once();
        let mut stdout = io::stdout();
        let _ = stdout.execute(terminal::EnterAlternateScreen);
        let _ = stdout.execute(cursor::Hide);
        let _ = terminal::enable_raw_mode();
        let _ = stdout.flush();
        Self { active: true }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        restore_terminal();
    }
}

fn restore_terminal() {
    let mut stdout = io::stdout();
    let _ = terminal::disable_raw_mode();
    let _ = stdout.execute(cursor::Show);
    let _ = stdout.execute(terminal::LeaveAlternateScreen);
    let _ = stdout.flush();
}

fn install_panic_hook_once() {
    use std::sync::OnceLock;
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            default(info);
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fetch_once_success_returns_stdout() {
        let (_tx, mut rx) = watch::channel(false);
        let out = fetch_once("printf", &["hello"], &mut rx).await.unwrap();
        assert_eq!(out, b"hello");
    }

    #[tokio::test]
    async fn test_fetch_once_nonzero_exit_is_error() {
        let (_tx, mut rx) = watch::channel(false);
        // `false` always exits non-zero
        let err = fetch_once("false", &[], &mut rx).await.unwrap_err();
        assert!(err.to_string().contains("exited with"));
    }

    #[tokio::test]
    async fn test_fetch_once_shutdown_kills_child() {
        let (tx, mut rx) = watch::channel(false);
        // sleep 30s in the background; shutdown within 100ms.
        let handle = tokio::spawn(async move {
            fetch_once("sleep", &["30"], &mut rx).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        tx.send(true).unwrap();
        let start = std::time::Instant::now();
        let res = handle.await.unwrap();
        assert!(res.is_err());
        // Should return in well under the sleep duration.
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[tokio::test]
    async fn test_fetch_once_already_shutdown_bails_synchronously() {
        // Regression test: previously, a receiver that had already
        // observed the flip wouldn't see it again via `changed()`, so
        // a second subprocess call after an interrupted one could
        // silently run to completion. The sync `borrow()` check at
        // the top of fetch_once prevents that.
        let (tx, mut rx) = watch::channel(false);
        tx.send(true).unwrap();
        // Mark rx as having seen the change by calling changed once.
        rx.changed().await.unwrap();

        let start = std::time::Instant::now();
        let res = fetch_once("sleep", &["10"], &mut rx).await;
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("shutdown"));
        // Must bail before spawning — well under any subprocess start time.
        assert!(start.elapsed() < std::time::Duration::from_millis(500));
    }

    #[test]
    fn test_terminal_guard_construction_is_safe_without_tty() {
        // In CI / test environments stdout is usually not a TTY. The guard
        // should gracefully skip the real terminal-mode switches in that
        // case instead of panicking. This just verifies no panic / no
        // process death.
        let _ = TerminalGuard::new_if_tty();
    }
}
