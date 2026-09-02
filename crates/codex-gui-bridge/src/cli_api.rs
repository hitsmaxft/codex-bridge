//! Unix socket CLI API for the broker.
//!
//! The `codex-gui` client connects to this socket and sends one JSON object per
//! request:
//!
//! ```json
//! {"cmd": "send", "thread": "01a0...", "text": "继续"}
//! ```
//!
//! The broker replies with one JSON object:
//!
//! ```json
//! {"ok": true, "result": { ... }}
//! {"ok": false, "error": "..."}
//! ```

use std::fs;
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::broker::{execute_cli, BrokerState, CliCall};

const MAX_REQUEST_BYTES: u64 = 1024 * 1024;

/// Default to a per-user directory rather than a fixed name directly under
/// /tmp. The daemon verifies the directory owner and mode before binding.
pub fn default_socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!("codex-gui-{}", effective_uid()))
        .join("bridge.sock")
}

/// Serve CLI requests on `socket_path` until `shutdown` fires.
pub async fn serve_cli(
    socket_path: &Path,
    state: Arc<BrokerState>,
    upstream_url: &str,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    prepare_private_parent(socket_path)?;
    remove_owned_stale_socket(socket_path).await?;
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to chmod 0600 {}", socket_path.display()))?;
    let socket_guard = BoundSocket::new(socket_path)?;
    tracing_info(&format!(
        "cli-api: listening on {} (upstream {upstream_url})",
        socket_path.display()
    ));
    loop {
        let (stream, _addr) = tokio::select! {
            accepted = listener.accept() => accepted.context("accept cli connection")?,
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    drop(socket_guard);
                    return Ok(());
                }
                continue;
            }
        };
        if let Err(error) = verify_peer_owner(&stream) {
            tracing_info(&format!("cli-api: rejected peer: {error:#}"));
            continue;
        }
        let state = Arc::clone(&state);
        let upstream_url = upstream_url.to_owned();
        tokio::spawn(async move {
            if let Err(error) = handle_cli(stream, &state, &upstream_url).await {
                tracing_info(&format!("cli-api: request failed: {error:#}"));
            }
        });
    }
}

fn prepare_private_parent(socket_path: &Path) -> Result<()> {
    let parent = socket_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .context("CLI socket path must have a parent directory")?;
    if !parent.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(parent)
            .with_context(|| format!("failed to create private directory {}", parent.display()))?;
    }
    let metadata = fs::symlink_metadata(parent)
        .with_context(|| format!("failed to inspect {}", parent.display()))?;
    anyhow::ensure!(
        metadata.file_type().is_dir(),
        "{} is not a directory",
        parent.display()
    );
    anyhow::ensure!(
        metadata.uid() == effective_uid(),
        "{} is not owned by the current user",
        parent.display()
    );
    anyhow::ensure!(
        metadata.mode() & 0o077 == 0,
        "{} must not be accessible by group or other users (mode {:o})",
        parent.display(),
        metadata.mode() & 0o777
    );
    Ok(())
}

async fn remove_owned_stale_socket(socket_path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(socket_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect {}", socket_path.display()))
        }
    };
    anyhow::ensure!(
        metadata.file_type().is_socket(),
        "refusing to replace non-socket path {}",
        socket_path.display()
    );
    anyhow::ensure!(
        metadata.uid() == effective_uid(),
        "refusing to replace socket not owned by the current user: {}",
        socket_path.display()
    );
    match UnixStream::connect(socket_path).await {
        Ok(_) => anyhow::bail!("CLI socket is already in use: {}", socket_path.display()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => fs::remove_file(socket_path)
            .with_context(|| format!("failed to remove stale socket {}", socket_path.display())),
        Err(error) => Err(error).with_context(|| {
            format!(
                "could not prove existing socket is stale: {}",
                socket_path.display()
            )
        }),
    }
}

struct BoundSocket {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl BoundSocket {
    fn new(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect bound socket {}", path.display()))?;
        anyhow::ensure!(
            metadata.file_type().is_socket(),
            "bound CLI path is not a socket"
        );
        anyhow::ensure!(
            metadata.uid() == effective_uid(),
            "bound CLI socket owner changed"
        );
        anyhow::ensure!(
            metadata.mode() & 0o777 == 0o600,
            "bound CLI socket mode is not 0600"
        );
        Ok(Self {
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.uid() == effective_uid()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and no side effects.
    unsafe { libc::geteuid() }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn verify_peer_owner(stream: &UnixStream) -> Result<()> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: both output pointers are valid for writes and the file
    // descriptor belongs to a connected Unix-domain socket.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("getpeereid failed");
    }
    anyhow::ensure!(
        uid == effective_uid(),
        "peer uid {uid} does not match daemon owner"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_peer_owner(stream: &UnixStream) -> Result<()> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the credential buffer and length pointer are valid, and the file
    // descriptor belongs to a connected Unix-domain socket.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("SO_PEERCRED failed");
    }
    anyhow::ensure!(
        credentials.uid == effective_uid(),
        "peer uid {} does not match daemon owner",
        credentials.uid
    );
    Ok(())
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly",
    target_os = "linux"
)))]
fn verify_peer_owner(_stream: &UnixStream) -> Result<()> {
    Ok(())
}

async fn handle_cli(stream: UnixStream, state: &BrokerState, upstream_url: &str) -> Result<()> {
    let reader = BufReader::new(stream);
    let mut limited = reader.take(MAX_REQUEST_BYTES + 1);
    let mut line = String::new();
    let n = limited
        .read_line(&mut line)
        .await
        .context("read cli request")?;
    if n == 0 {
        return Ok(());
    }
    let response = match parse_request(&line) {
        Ok((cmd, args)) => match execute_cli(state, upstream_url, &cmd, &args).await {
            Ok(CliCall::Json(value)) => json!({"ok": true, "result": value}),
            Ok(CliCall::Notifications(mut rx)) => {
                // First response is the acknowledgement; subsequent lines are
                // notifications. For now we only support the acknowledgement.
                let first = rx.recv().await.unwrap_or(json!(null));
                json!({"ok": true, "result": first})
            }
            Err(error) => json!({"ok": false, "error": format!("{error:#}")}),
        },
        Err(error) => json!({"ok": false, "error": format!("{error:#}")}),
    };
    let mut writer = limited.into_inner().into_inner();
    writer
        .write_all(serde_json::to_string(&response)?.as_bytes())
        .await?;
    writer.write_all(b"\n").await?;
    writer.shutdown().await?;
    Ok(())
}

fn parse_request(line: &str) -> Result<(String, serde_json::Map<String, Value>)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        anyhow::bail!("empty request");
    }
    if trimmed.len() as u64 > MAX_REQUEST_BYTES {
        anyhow::bail!("request too large");
    }
    let value: Value = serde_json::from_str(trimmed).context("request must be a JSON object")?;
    let object = value.as_object().context("request must be a JSON object")?;
    let cmd = object
        .get("cmd")
        .and_then(Value::as_str)
        .context("request is missing string field \"cmd\"")?
        .to_owned();
    let mut args = object.clone();
    args.remove("cmd");
    Ok((cmd, args))
}

pub fn tracing_info(message: &str) {
    eprintln!("[codex-gui-bridge] {message}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    fn private_test_directory() -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "codex-gui-bridge-test-{}-{}",
            std::process::id(),
            NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).create(&directory).unwrap();
        directory
    }

    #[test]
    fn parse_simple_request() {
        let (cmd, args) =
            parse_request(r#"{"cmd": "send", "thread": "t1", "text": "hello"}"#).unwrap();
        assert_eq!(cmd, "send");
        assert_eq!(args["thread"], "t1");
        assert_eq!(args["text"], "hello");
    }

    #[test]
    fn parse_rejects_bad_json() {
        assert!(parse_request("not json").is_err());
        assert!(parse_request(r#"{"cmd": 42}"#).is_err());
    }

    #[tokio::test]
    async fn private_socket_serves_status_and_cleans_up() {
        let directory = private_test_directory();
        let socket = directory.join("bridge.sock");
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let state = BrokerState::new(shutdown_rx.clone());
        let socket_for_server = socket.clone();
        let server = tokio::spawn(async move {
            serve_cli(
                &socket_for_server,
                state,
                "ws://127.0.0.1:9/rpc",
                shutdown_rx,
            )
            .await
        });

        tokio::time::timeout(tokio::time::Duration::from_secs(1), async {
            loop {
                if fs::symlink_metadata(&socket)
                    .is_ok_and(|metadata| metadata.mode() & 0o777 == 0o600)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let mut stream = UnixStream::connect(&socket).await.unwrap();
        stream.write_all(b"{\"cmd\":\"status\"}\n").await.unwrap();
        stream.shutdown().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["ok"], true);

        shutdown_tx.send_replace(true);
        server.await.unwrap().unwrap();
        assert!(!socket.exists());
        fs::remove_dir(directory).unwrap();
    }

    #[tokio::test]
    async fn stale_cleanup_never_replaces_a_regular_file() {
        let directory = private_test_directory();
        let path = directory.join("bridge.sock");
        fs::write(&path, b"keep me").unwrap();

        let error = remove_owned_stale_socket(&path).await.unwrap_err();
        assert!(error.to_string().contains("non-socket"));
        assert_eq!(fs::read(&path).unwrap(), b"keep me");

        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
