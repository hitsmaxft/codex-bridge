use std::env;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::Parser;
use codex_bridge::{default_socket_path, Request, Response, PROTOCOL_VERSION, SOCKET_ENV};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const MAX_REQUEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Parser)]
#[command(version, about = "Local bridge daemon for Codex Desktop")]
struct Args {
    /// Override the control socket path.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let secure_existing_parent = args.socket.is_none()
        && env::var_os(SOCKET_ENV)
            .filter(|value| !value.is_empty())
            .is_none();
    let socket_path = match args.socket {
        Some(path) => path,
        None => default_socket_path()?,
    };

    prepare_socket_path(&socket_path, secure_existing_parent).await?;
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to secure {}", socket_path.display()))?;
    let _socket_guard = SocketGuard::new(&socket_path)?;

    println!("codex-bridge listening on {}", socket_path.display());

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    let socket_path = Arc::new(socket_path);

    loop {
        tokio::select! {
            signal = &mut shutdown => {
                signal.context("failed to listen for shutdown signal")?;
                break;
            }
            connection = listener.accept() => {
                let (stream, _) = connection.context("failed to accept control connection")?;
                let socket_path = Arc::clone(&socket_path);
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, &socket_path).await {
                        eprintln!("control connection failed: {error:#}");
                    }
                });
            }
        }
    }

    Ok(())
}

async fn prepare_socket_path(path: &Path, secure_existing_parent: bool) -> Result<()> {
    let parent = path
        .parent()
        .context("control socket path has no parent directory")?;
    let parent_existed = parent.exists();
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    if !parent_existed || secure_existing_parent {
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to secure {}", parent.display()))?;
    }

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };

    if !metadata.file_type().is_socket() {
        bail!("refusing to replace non-socket path at {}", path.display());
    }

    match UnixStream::connect(path).await {
        Ok(_) => bail!("codex-bridge is already listening on {}", path.display()),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::ConnectionRefused | ErrorKind::NotFound
            ) =>
        {
            fs::remove_file(path)
                .with_context(|| format!("failed to remove stale socket {}", path.display()))?;
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("could not verify existing socket {}", path.display()));
        }
    }

    Ok(())
}

async fn handle_connection(stream: UnixStream, socket_path: &Path) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader).take(MAX_REQUEST_BYTES + 1);
    let mut line = String::new();
    let bytes_read = reader
        .read_line(&mut line)
        .await
        .context("failed to read request")?;

    let response = if bytes_read == 0 {
        Response::error("empty_request", "request body is empty")
    } else if bytes_read as u64 > MAX_REQUEST_BYTES {
        Response::error("request_too_large", "request exceeds 1 MiB")
    } else {
        match serde_json::from_str::<Request>(&line) {
            Ok(request) => dispatch(request, socket_path),
            Err(error) => Response::error("invalid_request", error.to_string()),
        }
    };

    let mut encoded = serde_json::to_vec(&response).context("failed to encode response")?;
    encoded.push(b'\n');
    writer
        .write_all(&encoded)
        .await
        .context("failed to write response")?;
    Ok(())
}

fn dispatch(request: Request, socket_path: &Path) -> Response {
    match request {
        Request::Status => Response::success(json!({
            "service": "codex-bridge",
            "status": "ready",
            "protocol_version": PROTOCOL_VERSION,
            "socket": socket_path,
        })),
        request => Response::error(
            "not_implemented",
            format!(
                "{} is part of the CLI protocol but has no backend yet",
                request.name()
            ),
        ),
    }
}

struct SocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl SocketGuard {
    fn new(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect bound socket {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.dev() == self.device && metadata.ino() == self.inode {
            let _ = fs::remove_file(&self.path);
        }
    }
}
