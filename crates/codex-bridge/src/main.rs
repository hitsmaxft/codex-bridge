use std::env;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{bail, Context, Result};
use clap::Parser;
use codex_bridge::{
    default_codex_home, default_socket_path, BackendFailure, CodexCliBackend, HostExecFailure,
    HostExecutor, Request, Response, SessionStore, ThreadSummary, APP_SERVER_SOCKET_ENV,
    CODEX_BIN_ENV, HOST_EXEC_POLICY_ENV, PROTOCOL_VERSION, SOCKET_ENV,
};
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

    /// Override the read-only Codex state directory.
    #[arg(long, value_name = "PATH")]
    codex_home: Option<PathBuf>,

    /// Codex CLI executable used by write backends.
    #[arg(long, value_name = "PATH")]
    codex_bin: Option<PathBuf>,

    /// Shared app-server WebSocket-over-UDS endpoint used for steer and interrupt.
    #[arg(long, value_name = "PATH")]
    app_server_socket: Option<PathBuf>,

    /// JSON policy replacing the built-in host-exec allowlist.
    #[arg(long, value_name = "PATH")]
    host_exec_policy: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let codex_home = match args.codex_home.clone() {
        Some(path) => path,
        None => default_codex_home()?,
    };
    let codex_program = args
        .codex_bin
        .clone()
        .or_else(|| {
            env::var_os(CODEX_BIN_ENV)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from("codex"));
    let app_server_socket = args
        .app_server_socket
        .clone()
        .or_else(|| {
            env::var_os(APP_SERVER_SOCKET_ENV)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| {
            codex_home
                .join("app-server-control")
                .join("app-server-control.sock")
        });
    let session_store = Arc::new(SessionStore::new(codex_home));
    let write_backend = Arc::new(CodexCliBackend::new(codex_program, app_server_socket));
    let host_exec_policy = args.host_exec_policy.or_else(|| {
        env::var_os(HOST_EXEC_POLICY_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    let host_executor = Arc::new(
        HostExecutor::load(host_exec_policy.as_deref())
            .map_err(|error| anyhow::anyhow!("{}: {}", error.code, error.message))?,
    );
    let selected_thread = Arc::new(RwLock::new(None));
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
                let session_store = Arc::clone(&session_store);
                let write_backend = Arc::clone(&write_backend);
                let host_executor = Arc::clone(&host_executor);
                let selected_thread = Arc::clone(&selected_thread);
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(
                        stream,
                        &socket_path,
                        session_store,
                        write_backend,
                        host_executor,
                        selected_thread,
                    ).await {
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

async fn handle_connection(
    stream: UnixStream,
    socket_path: &Path,
    session_store: Arc<SessionStore>,
    write_backend: Arc<CodexCliBackend>,
    host_executor: Arc<HostExecutor>,
    selected_thread: Arc<RwLock<Option<String>>>,
) -> Result<()> {
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
            Ok(request) => {
                let socket_path = socket_path.to_path_buf();
                tokio::task::spawn_blocking(move || {
                    dispatch(
                        request,
                        &socket_path,
                        &session_store,
                        &write_backend,
                        &host_executor,
                        &selected_thread,
                    )
                })
                .await
                .context("bridge worker failed")?
            }
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

fn dispatch(
    request: Request,
    socket_path: &Path,
    session_store: &SessionStore,
    write_backend: &CodexCliBackend,
    host_executor: &HostExecutor,
    selected_thread: &RwLock<Option<String>>,
) -> Response {
    match request {
        Request::Status => Response::success(json!({
            "service": "codex-bridge",
            "status": "ready",
            "protocol_version": PROTOCOL_VERSION,
            "socket": socket_path,
            "rollout_store": {
                "available": session_store.is_available(),
                "codex_home": session_store.home(),
                "read_only": true,
            },
            "selected_thread_id": selected_thread.read().ok().and_then(|selected| selected.clone()),
            "write_backend": {
                "codex_program": write_backend.program(),
                "app_server_socket": write_backend.app_server_socket(),
                "app_server_available": fs::metadata(write_backend.app_server_socket())
                    .is_ok_and(|metadata| metadata.file_type().is_socket()),
            },
            "host_executor": host_executor.summary(),
        })),
        Request::Ls {
            limit,
            include_archived,
        } => {
            if !(1..=1_000).contains(&limit) {
                return Response::error("invalid_request", "ls limit must be between 1 and 1000");
            }
            match session_store.list_threads_limited(include_archived, limit as usize) {
                Ok((threads, available)) => {
                    let returned = threads.len();
                    Response::success(json!({
                        "source": "rollout_jsonl",
                        "threads": threads,
                        "returned": returned,
                        "available": available,
                        "include_archived": include_archived,
                    }))
                }
                Err(error) => backend_error(error),
            }
        }
        Request::Select { thread_id } => {
            let thread = match thread_by_id(session_store, &thread_id) {
                Ok(thread) => thread,
                Err(response) => return response,
            };
            let mut selected = match selected_thread.write() {
                Ok(selected) => selected,
                Err(_) => return selection_state_error(),
            };
            *selected = Some(thread.id.clone());
            Response::success(json!({
                "selected": true,
                "thread": thread,
            }))
        }
        Request::Current => match resolve_read_target(None, session_store, selected_thread) {
            Ok(resolved) => Response::success(json!({
                "source": "rollout_jsonl",
                "thread": resolved.thread,
                "selection": {
                    "method": resolved.method,
                    "authoritative": resolved.authoritative,
                    "note": if resolved.authoritative {
                        "The bridge selected this thread explicitly."
                    } else {
                        "This does not prove which Codex Desktop window is focused."
                    }
                }
            })),
            Err(response) => response,
        },
        Request::Show { thread_id, last } => {
            if last == Some(0) || last.is_some_and(|last| last > 10_000) {
                return Response::error("invalid_request", "show last must be between 1 and 10000");
            }
            let resolved = match resolve_read_target(thread_id, session_store, selected_thread) {
                Ok(resolved) => resolved,
                Err(response) => return response,
            };

            match session_store.read_thread(&resolved.thread.id) {
                Ok(Some(mut snapshot)) => {
                    let messages_total = snapshot.messages.len();
                    if let Some(last) = last {
                        let keep_from = messages_total.saturating_sub(last as usize);
                        snapshot.messages = snapshot.messages.split_off(keep_from);
                    }
                    let messages_returned = snapshot.messages.len();
                    Response::success(json!({
                        "source": "rollout_jsonl",
                        "selection": {
                            "method": resolved.method,
                            "authoritative": resolved.authoritative,
                        },
                        "thread": snapshot.thread,
                        "messages": snapshot.messages,
                        "messages_returned": messages_returned,
                        "messages_total": messages_total,
                    }))
                }
                Ok(None) => Response::error(
                    "thread_not_found",
                    format!(
                        "thread {} was not found in the rollout store",
                        resolved.thread.id
                    ),
                ),
                Err(error) => backend_error(error),
            }
        }
        Request::Send { thread_id, text } => {
            if text.trim().is_empty() {
                return Response::error("invalid_request", "send message must not be empty");
            }
            let resolved = match resolve_write_target(thread_id, session_store, selected_thread) {
                Ok(resolved) => resolved,
                Err(response) => return response,
            };
            match write_backend.queue_message(&resolved.thread.id, &text) {
                Ok(backend) => Response::success(json!({
                    "action": "send",
                    "status": "queued",
                    "thread_id": resolved.thread.id,
                    "target": resolved.method,
                    "backend": backend,
                })),
                Err(error) => write_backend_error(error),
            }
        }
        Request::Steer { thread_id, text } => {
            if text.trim().is_empty() {
                return Response::error("invalid_request", "steer message must not be empty");
            }
            let resolved = match resolve_write_target(thread_id, session_store, selected_thread) {
                Ok(resolved) => resolved,
                Err(response) => return response,
            };
            let turn_id = match session_store.active_turn_id(&resolved.thread.id) {
                Ok(Some(turn_id)) => turn_id,
                Ok(None) => {
                    return Response::error(
                        "no_active_turn",
                        format!(
                            "thread {} has no active turn; use send to queue a message instead",
                            resolved.thread.id
                        ),
                    );
                }
                Err(error) => return backend_error(error),
            };
            match write_backend.steer_via_app_server(&resolved.thread.id, &turn_id, &text) {
                Ok(backend) => Response::success(json!({
                    "action": "steer",
                    "status": "steered",
                    "thread_id": resolved.thread.id,
                    "target": resolved.method,
                    "semantics": "app_server_steer",
                    "backend": backend,
                })),
                Err(error) => write_backend_error(error),
            }
        }
        Request::Interrupt { thread_id } => {
            let resolved = match resolve_write_target(thread_id, session_store, selected_thread) {
                Ok(resolved) => resolved,
                Err(response) => return response,
            };
            let turn_id = match session_store.active_turn_id(&resolved.thread.id) {
                Ok(Some(turn_id)) => turn_id,
                Ok(None) => {
                    return Response::error(
                        "no_active_turn",
                        format!(
                            "thread {} has no active turn in its rollout",
                            resolved.thread.id
                        ),
                    );
                }
                Err(error) => return backend_error(error),
            };
            match write_backend.interrupt_turn(&resolved.thread.id, &turn_id) {
                Ok(backend) => Response::success(json!({
                    "action": "interrupt",
                    "status": "interrupted",
                    "thread_id": resolved.thread.id,
                    "turn_id": turn_id,
                    "target": resolved.method,
                    "backend": backend,
                })),
                Err(error) => write_backend_error(error),
            }
        }
        Request::HostExec {
            thread_id,
            argv,
            timeout_seconds,
        } => {
            let resolved = match resolve_write_target(thread_id, session_store, selected_thread) {
                Ok(resolved) => resolved,
                Err(response) => return response,
            };
            match host_executor.execute(&resolved.thread.cwd, &argv, timeout_seconds) {
                Ok(execution) => Response::success(json!({
                    "action": "host_exec",
                    "status": if execution.timed_out { "timed_out" } else { "exited" },
                    "thread_id": resolved.thread.id,
                    "target": resolved.method,
                    "execution": execution,
                })),
                Err(error) => host_exec_error(error),
            }
        }
        request => Response::error(
            "not_implemented",
            format!(
                "{} is part of the CLI protocol but has no backend yet",
                request.name()
            ),
        ),
    }
}

#[derive(Debug)]
struct ResolvedThread {
    thread: ThreadSummary,
    method: &'static str,
    authoritative: bool,
}

fn resolve_read_target(
    requested_thread_id: Option<String>,
    session_store: &SessionStore,
    selected_thread: &RwLock<Option<String>>,
) -> Result<ResolvedThread, Response> {
    if let Some(thread_id) = requested_thread_id {
        return Ok(ResolvedThread {
            thread: thread_by_id(session_store, &thread_id)?,
            method: "explicit_thread_id",
            authoritative: true,
        });
    }
    if let Some(thread_id) = selected_thread_id(selected_thread)? {
        return Ok(ResolvedThread {
            thread: thread_by_id(session_store, &thread_id)?,
            method: "selected_thread",
            authoritative: true,
        });
    }

    match session_store.current_thread() {
        Ok(Some(thread)) => Ok(ResolvedThread {
            thread,
            method: "latest_rollout_mtime",
            authoritative: false,
        }),
        Ok(None) => Err(Response::error(
            "thread_not_found",
            "no unarchived rollout was found",
        )),
        Err(error) => Err(backend_error(error)),
    }
}

fn resolve_write_target(
    requested_thread_id: Option<String>,
    session_store: &SessionStore,
    selected_thread: &RwLock<Option<String>>,
) -> Result<ResolvedThread, Response> {
    let resolved = if let Some(thread_id) = requested_thread_id {
        ResolvedThread {
            thread: thread_by_id(session_store, &thread_id)?,
            method: "explicit_thread_id",
            authoritative: true,
        }
    } else if let Some(thread_id) = selected_thread_id(selected_thread)? {
        ResolvedThread {
            thread: thread_by_id(session_store, &thread_id)?,
            method: "selected_thread",
            authoritative: true,
        }
    } else {
        return Err(Response::error(
            "thread_not_selected",
            "write commands require --thread <THREAD_ID> or codexctl select <THREAD_ID>",
        ));
    };

    if resolved.thread.archived {
        return Err(Response::error(
            "thread_archived",
            format!("thread {} is archived", resolved.thread.id),
        ));
    }
    Ok(resolved)
}

fn thread_by_id(session_store: &SessionStore, thread_id: &str) -> Result<ThreadSummary, Response> {
    match session_store.find_thread(thread_id) {
        Ok(Some(thread)) => Ok(thread),
        Ok(None) => Err(Response::error(
            "thread_not_found",
            format!("thread {thread_id} was not found in the rollout store"),
        )),
        Err(error) => Err(backend_error(error)),
    }
}

fn selected_thread_id(
    selected_thread: &RwLock<Option<String>>,
) -> Result<Option<String>, Response> {
    selected_thread
        .read()
        .map(|selected| selected.clone())
        .map_err(|_| selection_state_error())
}

fn selection_state_error() -> Response {
    Response::error(
        "selection_state_error",
        "selected thread state is unavailable",
    )
}

fn backend_error(error: anyhow::Error) -> Response {
    Response::error("rollout_store_error", format!("{error:#}"))
}

fn write_backend_error(error: BackendFailure) -> Response {
    Response::error(error.code, error.message)
}

fn host_exec_error(error: HostExecFailure) -> Response {
    Response::error(error.code, error.message)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_store() -> SessionStore {
        SessionStore::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/codex-home"))
    }

    #[test]
    fn writes_never_fall_back_to_latest_rollout() {
        let store = fixture_store();
        let selected = RwLock::new(None);
        let response = resolve_write_target(None, &store, &selected).unwrap_err();
        assert_eq!(
            response.error.unwrap().code,
            "thread_not_selected".to_owned()
        );
    }

    #[test]
    fn selected_thread_is_an_authoritative_write_target() {
        let store = fixture_store();
        let selected = RwLock::new(Some("00000000-0000-7000-8000-000000000001".to_owned()));
        let resolved = resolve_write_target(None, &store, &selected).unwrap();

        assert_eq!(resolved.method, "selected_thread");
        assert!(resolved.authoritative);
        assert_eq!(resolved.thread.cwd, Path::new("/tmp"));
    }
}
