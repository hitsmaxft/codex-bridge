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
use tokio::time::{sleep, Instant};

const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(10);
const HEALTHY_RUN: Duration = Duration::from_secs(30);

/// Handle returned by [`Supervisor::handle`] used to observe child state.
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
            if *shutdown.borrow() {
                tracing_info("supervisor: shutdown requested");
                return Ok(());
            }
            match self.spawn_and_wait(&mut shutdown).await? {
                ChildOutcome::Shutdown => {
                    tracing_info("supervisor: app-server terminated for shutdown");
                    return Ok(());
                }
                ChildOutcome::Exited(exit_status, runtime) => {
                    if runtime >= HEALTHY_RUN {
                        restarts = 0;
                    }
                    restarts = restarts.saturating_add(1);
                    tracing_info(&format!(
                        "supervisor: app-server exited after {runtime:?} \
                         (status {exit_status}); restart {restarts}"
                    ));
                    let shift = restarts.saturating_sub(1).min(5);
                    let backoff = BACKOFF_BASE.saturating_mul(1u32 << shift).min(BACKOFF_MAX);
                    tracing_info(&format!(
                        "supervisor: backing off {backoff:?} before restart"
                    ));
                    tokio::select! {
                        _ = sleep(backoff) => {}
                        changed = shutdown.changed() => {
                            if changed.is_err() || *shutdown.borrow() {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
    }

    async fn spawn_and_wait(
        &mut self,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<ChildOutcome> {
        tracing_info(&format!(
            "supervisor: starting {} app-server --listen {}",
            self.codex_bin.display(),
            self.listen_url
        ));
        let mut command = Command::new(&self.codex_bin);
        command
            .args(["app-server", "--listen", &self.listen_url])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // Preserve startup/schema diagnostics in the broker's log. A
            // rejected --listen URL otherwise looks like an unexplained
            // restart loop with status 2.
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child: Child = command
            .spawn()
            .with_context(|| format!("failed to spawn {}", self.codex_bin.display()))?;
        self.running_tx.send_replace(true);
        let started = Instant::now();
        let outcome: Result<ChildOutcome> = loop {
            tokio::select! {
                status = child.wait() => {
                    break status
                        .context("failed to wait on app-server")
                        .map(|status| ChildOutcome::Exited(status, started.elapsed()));
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        if let Err(error) = child.start_kill() {
                            break Err(error).context("failed to terminate app-server");
                        }
                        let _ = child.wait().await;
                        break Ok(ChildOutcome::Shutdown);
                    }
                }
            }
        };
        self.running_tx.send_replace(false);
        outcome
    }
}

enum ChildOutcome {
    Exited(std::process::ExitStatus, Duration),
    Shutdown,
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
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_PROCESS: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn handle_starts_stopped() {
        let supervisor = Supervisor::new(
            PathBuf::from("/bin/true"),
            "ws://127.0.0.1:18791".to_owned(),
        );
        let handle = supervisor.handle();
        assert!(!*handle.running.borrow());
        assert_eq!(handle.listen_url, "ws://127.0.0.1:18791");
    }

    #[tokio::test]
    async fn shutdown_terminates_the_supervised_child() {
        let directory = std::env::temp_dir().join(format!(
            "codex-gui-supervisor-test-{}-{}",
            std::process::id(),
            NEXT_TEST_PROCESS.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let executable = directory.join("fake-codex");
        std::fs::write(&executable, "#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();

        let supervisor = Supervisor::new(executable.clone(), "ws://127.0.0.1:18791".to_owned());
        let mut handle = supervisor.handle();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(supervisor.run(shutdown_rx));

        tokio::time::timeout(Duration::from_secs(1), handle.running.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(*handle.running.borrow());

        shutdown_tx.send_replace(true);
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!*handle.running.borrow());

        std::fs::remove_file(executable).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
