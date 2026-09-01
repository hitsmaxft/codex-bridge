//! Supervisor for the upstream `codex app-server` process.
//!
//! The broker owns the app-server instance that Desktop ultimately talks to.
//! We spawn it with the Desktop-bundled (or user-selected) `codex` binary using
//! `app-server --listen ws://127.0.0.1:PORT` and restart it on unexpected exit.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::time::sleep;

const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(10);
const MAX_RESTARTS_BEFORE_IDLE: u32 = 5;

/// Handle returned by [`Supervisor::start`] used to observe child state.
#[derive(Debug, Clone)]
pub struct SupervisorHandle {
    /// True while the app-server child process is running.
    pub running: watch::Receiver<bool>,
    /// The listen URL we passed to the app-server.
    pub listen_url: String,
}

/// Long-running supervisor task. Spawns and re-spawns the app-server until the
/// shutdown signal fires.
pub struct Supervisor {
    codex_bin: PathBuf,
    listen_url: String,
    running_tx: watch::Sender<bool>,
}

impl Supervisor {
    pub fn new(codex_bin: PathBuf, listen_url: String) -> Self {
        let (running_tx, _) = watch::channel(false);
        Self {
            codex_bin,
            listen_url,
            running_tx,
        }
    }

    pub fn handle(&self) -> SupervisorHandle {
        SupervisorHandle {
            running: self.running_tx.subscribe(),
            listen_url: self.listen_url.clone(),
        }
    }

    /// Run the supervision loop until `shutdown` resolves.
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        let mut restarts = 0u32;
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing_info("supervisor: shutdown requested");
                        return Ok(());
                    }
                }
                result = self.spawn_and_wait() => {
                    match result {
                        Ok(exit_status) => {
                            restarts += 1;
                            tracing_info(&format!(
                                "supervisor: app-server exited (status {exit_status}); restart {restarts}"
                            ));
                        }
                        Err(error) => {
                            restarts += 1;
                            tracing_info(&format!("supervisor: app-server failed to start: {error:#}"));
                        }
                    }
                    // After a burst of restarts, back off to avoid a crash loop.
                    if restarts > MAX_RESTARTS_BEFORE_IDLE {
                        let backoff = BACKOFF_BASE
                            .saturating_mul(restarts.saturating_sub(MAX_RESTARTS_BEFORE_IDLE) as u32)
                            .min(BACKOFF_MAX);
                        tracing_info(&format!("supervisor: backing off {backoff:?} before restart"));
                        tokio::select! {
                            _ = sleep(backoff) => {}
                            _ = shutdown.changed() => {
                                if *shutdown.borrow() {
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    async fn spawn_and_wait(&mut self) -> Result<std::process::ExitStatus> {
        tracing_info(&format!(
            "supervisor: starting {} app-server --listen {}",
            self.codex_bin.display(),
            self.listen_url
        ));
        let mut child: Child = Command::new(&self.codex_bin)
            .args(["app-server", "--listen", &self.listen_url])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to spawn {}", self.codex_bin.display()))?;
        self.running_tx.send_replace(true);
        let status = child.wait().await.context("failed to wait on app-server")?;
        self.running_tx.send_replace(false);
        Ok(status)
    }
}

fn tracing_info(message: &str) {
    // Keep the daemon quiet on stdout (it is the broker's log channel); use
    // stderr for diagnostics so the CLI never confuses app-server logs with
    // its own output.
    eprintln!("[codex-gui-bridge] {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_starts_stopped() {
        let supervisor = Supervisor::new(
            PathBuf::from("/bin/true"),
            "ws://127.0.0.1:18791/rpc".to_owned(),
        );
        let handle = supervisor.handle();
        assert!(!*handle.running.borrow());
        assert_eq!(handle.listen_url, "ws://127.0.0.1:18791/rpc");
    }
}
