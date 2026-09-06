use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use anyhow::{bail, Context, Result};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response as HttpResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use clap::Parser;
use codex_bridge::{
    default_codex_home, default_socket_path, BackendFailure, CodexCliBackend, HostExecFailure,
    HostExecutor, Request, Response, SessionStore, ThreadMessage, ThreadSummary, ThreadToolCall,
    APP_SERVER_SOCKET_ENV, CODEX_BIN_ENV, HOST_EXEC_POLICY_ENV, PROTOCOL_VERSION, SOCKET_ENV,
};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::mpsc;

const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const DEFAULT_WEB_UI_ADDR: &str = "127.0.0.1:18791";

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

    /// Start the HTTP Web UI. HTTP Basic Auth is required.
    #[arg(long)]
    web_ui: bool,

    /// Address for the optional Web UI.
    #[arg(long, value_name = "IP:PORT", default_value = DEFAULT_WEB_UI_ADDR)]
    web_ui_listen: SocketAddr,

    /// Username for HTTP Basic Auth.
    #[arg(long, value_name = "USER", default_value = "codex")]
    web_ui_user: String,

    /// Read the HTTP Basic Auth password from a same-user mode-0600 file.
    #[arg(long, value_name = "PATH")]
    web_ui_password_file: Option<PathBuf>,
}

#[derive(Clone)]
struct BridgeState {
    socket_path: Arc<PathBuf>,
    session_store: Arc<SessionStore>,
    write_backend: Arc<CodexCliBackend>,
    host_executor: Arc<HostExecutor>,
    selected_thread: Arc<RwLock<Option<String>>>,
    pending_messages: Arc<PendingMessages>,
}

#[derive(Clone)]
struct WebState {
    bridge: BridgeState,
    port: u16,
    auth: Arc<WebAuth>,
}

struct WebAuth {
    username: String,
    password: String,
}

#[derive(Debug, Clone, Serialize)]
struct PendingMessage {
    id: u64,
    thread_id: String,
    text: String,
    action: String,
    status: String,
    after_message_index: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Default)]
struct PendingMessages {
    next_id: AtomicU64,
    entries: RwLock<Vec<PendingMessage>>,
}

impl PendingMessages {
    fn begin(&self, thread_id: &str, text: &str, action: &str, after_message_index: i64) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut entries) = self.entries.write() {
            entries.push(PendingMessage {
                id,
                thread_id: thread_id.to_owned(),
                text: text.to_owned(),
                action: action.to_owned(),
                status: format!("{action}ing"),
                after_message_index,
                error: None,
            });
        }
        id
    }

    fn finish(&self, id: u64, status: &str, error: Option<String>) {
        if let Ok(mut entries) = self.entries.write() {
            if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                entry.status = status.to_owned();
                entry.error = error;
            }
        }
    }

    fn reconcile(&self, session_store: &SessionStore) -> Vec<PendingMessage> {
        let Ok(mut entries) = self.entries.write() else {
            return Vec::new();
        };
        let mut messages_by_thread = HashMap::new();
        let mut landed = HashSet::new();
        entries.retain(|entry| {
            if entry.status == "failed" {
                return true;
            }
            let messages = messages_by_thread
                .entry(entry.thread_id.clone())
                .or_insert_with(|| {
                    session_store
                        .read_thread(&entry.thread_id)
                        .ok()
                        .flatten()
                        .map(|snapshot| snapshot.messages)
                        .unwrap_or_default()
                });
            let is_landed = pending_message_landed(entry, messages, &mut landed);
            !is_landed
        });
        entries.clone()
    }
}

fn pending_message_landed(
    entry: &PendingMessage,
    messages: &[ThreadMessage],
    landed: &mut HashSet<(String, usize, String)>,
) -> bool {
    messages
        .iter()
        .enumerate()
        .skip((entry.after_message_index + 1).max(0) as usize)
        .filter(|(_, message)| message.role == "user")
        .flat_map(|(message_index, message)| {
            message.content.iter().filter_map(move |content| {
                content
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| (message_index, text))
            })
        })
        .any(|(message_index, text)| {
            let key = (entry.thread_id.clone(), message_index, text.to_owned());
            text == entry.text && landed.insert(key)
        })
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
    let pending_messages = Arc::new(PendingMessages::default());
    let secure_existing_parent = args.socket.is_none()
        && env::var_os(SOCKET_ENV)
            .filter(|value| !value.is_empty())
            .is_none();
    let socket_path = match args.socket {
        Some(path) => path,
        None => default_socket_path()?,
    };

    let web_auth = args
        .web_ui
        .then(|| load_web_auth(&args.web_ui_user, args.web_ui_password_file.as_deref()))
        .transpose()?;

    prepare_socket_path(&socket_path, secure_existing_parent).await?;
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to secure {}", socket_path.display()))?;
    let _socket_guard = SocketGuard::new(&socket_path)?;

    let socket_path = Arc::new(socket_path);
    let bridge_state = BridgeState {
        socket_path: Arc::clone(&socket_path),
        session_store,
        write_backend,
        host_executor,
        selected_thread,
        pending_messages,
    };

    println!("codex-bridge listening on {}", socket_path.display());

    let (web_failure_tx, mut web_failure_rx) = mpsc::channel::<String>(1);
    let web_task = if args.web_ui {
        let web_listener = TcpListener::bind(args.web_ui_listen)
            .await
            .with_context(|| format!("failed to bind Web UI at {}", args.web_ui_listen))?;
        let web_addr = web_listener
            .local_addr()
            .context("failed to inspect Web UI listener")?;
        let web_state = WebState {
            bridge: bridge_state.clone(),
            port: web_addr.port(),
            auth: Arc::new(web_auth.context("Web UI authentication is unavailable")?),
        };
        println!("codex-bridge Web UI listening on http://{web_addr}/");
        Some(tokio::spawn(async move {
            let result = axum::serve(web_listener, web_router(web_state)).await;
            let message = match result {
                Ok(()) => "Web UI server stopped unexpectedly".to_owned(),
                Err(error) => format!("Web UI server failed: {error}"),
            };
            let _ = web_failure_tx.send(message).await;
        }))
    } else {
        None
    };

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            signal = &mut shutdown => {
                signal.context("failed to listen for shutdown signal")?;
                break;
            }
            Some(error) = web_failure_rx.recv(), if web_task.is_some() => {
                bail!(error);
            }
            connection = listener.accept() => {
                let (stream, _) = connection.context("failed to accept control connection")?;
                let state = bridge_state.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(
                        stream,
                        state,
                    ).await {
                        eprintln!("control connection failed: {error:#}");
                    }
                });
            }
        }
    }

    if let Some(web_task) = web_task {
        web_task.abort();
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

fn load_web_auth(username: &str, password_file: Option<&Path>) -> Result<WebAuth> {
    if username.is_empty() || username.contains(':') {
        bail!("Web UI username must be non-empty and must not contain ':'");
    }
    let password_file = password_file.context("--web-ui requires --web-ui-password-file")?;
    let metadata = fs::symlink_metadata(password_file)
        .with_context(|| format!("failed to inspect {}", password_file.display()))?;
    if !metadata.file_type().is_file() {
        bail!(
            "Web UI password path must be a regular file, not {}",
            password_file.display()
        );
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        bail!("Web UI password file must be owned by the current user");
    }
    if metadata.mode() & 0o077 != 0 {
        bail!("Web UI password file must not be accessible by group or other users");
    }
    if metadata.len() > 1024 {
        bail!("Web UI password file is unexpectedly large");
    }
    let password = fs::read_to_string(password_file)
        .with_context(|| format!("failed to read {}", password_file.display()))?
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    if password.is_empty() {
        bail!("Web UI password must not be empty");
    }
    Ok(WebAuth {
        username: username.to_owned(),
        password,
    })
}

async fn handle_connection(stream: UnixStream, state: BridgeState) -> Result<()> {
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
            Ok(request) => tokio::task::spawn_blocking(move || {
                dispatch(
                    request,
                    &state.socket_path,
                    &state.session_store,
                    &state.write_backend,
                    &state.host_executor,
                    &state.selected_thread,
                    &state.pending_messages,
                )
            })
            .await
            .context("bridge worker failed")?,
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

fn web_router(state: WebState) -> Router {
    Router::new()
        .route("/", get(web_index))
        .route("/api/command", post(web_command))
        .with_state(state)
}

async fn web_index(State(state): State<WebState>, headers: HeaderMap) -> HttpResponse {
    if !basic_auth_allowed(&headers, &state.auth) {
        return basic_auth_required();
    }
    if !web_headers_allowed(&headers, state.port, false) {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    let mut response = Html(include_str!("web_ui.html")).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    response
}

async fn web_command(
    State(state): State<WebState>,
    headers: HeaderMap,
    body: Bytes,
) -> HttpResponse {
    if !basic_auth_allowed(&headers, &state.auth) {
        return basic_auth_required();
    }
    if !web_headers_allowed(&headers, state.port, true) {
        return (
            StatusCode::FORBIDDEN,
            Json(Response::error(
                "forbidden",
                "Web UI requests must come from this loopback origin",
            )),
        )
            .into_response();
    }
    if body.len() as u64 > MAX_REQUEST_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(Response::error(
                "request_too_large",
                "request exceeds 1 MiB",
            )),
        )
            .into_response();
    }
    let Some(content_type) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(Response::error(
                "invalid_request",
                "Content-Type must be application/json",
            )),
        )
            .into_response();
    };
    if !content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
    {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(Response::error(
                "invalid_request",
                "Content-Type must be application/json",
            )),
        )
            .into_response();
    }

    let request = match serde_json::from_slice::<Request>(&body) {
        Ok(request) => request,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(Response::error("invalid_request", error.to_string())),
            )
                .into_response()
        }
    };
    let bridge = state.bridge;
    match tokio::task::spawn_blocking(move || {
        dispatch(
            request,
            &bridge.socket_path,
            &bridge.session_store,
            &bridge.write_backend,
            &bridge.host_executor,
            &bridge.selected_thread,
            &bridge.pending_messages,
        )
    })
    .await
    {
        Ok(response) => Json(response).into_response(),
        Err(error) => {
            Json(Response::error("bridge_worker_error", error.to_string())).into_response()
        }
    }
}

fn web_headers_allowed(headers: &HeaderMap, port: u16, require_origin: bool) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    if !web_authority_allowed(host, port) {
        return false;
    }

    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    if require_origin && origin.is_some_and(|origin| !web_origin_allowed(origin, port)) {
        return false;
    }
    true
}

fn web_authority_allowed(authority: &str, port: u16) -> bool {
    let Ok(authority) = authority.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    if authority.port_u16() != Some(port) {
        return false;
    }
    let host = authority.host();
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok()
}

fn web_origin_allowed(origin: &str, port: u16) -> bool {
    let Ok(uri) = origin.parse::<axum::http::Uri>() else {
        return false;
    };
    uri.scheme_str() == Some("http")
        && uri
            .authority()
            .is_some_and(|authority| web_authority_allowed(authority.as_str(), port))
}

fn basic_auth_allowed(headers: &HeaderMap, auth: &WebAuth) -> bool {
    let Some(encoded) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
    else {
        return false;
    };
    let Ok(decoded) = BASE64_STANDARD.decode(encoded) else {
        return false;
    };
    let Ok(credentials) = String::from_utf8(decoded) else {
        return false;
    };
    let Some((username, password)) = credentials.split_once(':') else {
        return false;
    };
    constant_time_eq(username.as_bytes(), auth.username.as_bytes())
        & constant_time_eq(password.as_bytes(), auth.password.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

fn basic_auth_required() -> HttpResponse {
    let mut response = (StatusCode::UNAUTHORIZED, "authentication required").into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"Codex Bridge\", charset=\"UTF-8\""),
    );
    response
}

fn dispatch(
    request: Request,
    socket_path: &Path,
    session_store: &SessionStore,
    write_backend: &CodexCliBackend,
    host_executor: &HostExecutor,
    selected_thread: &RwLock<Option<String>>,
    pending_messages: &PendingMessages,
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
        Request::PendingMessages => Response::success(json!({
            "messages": pending_messages.reconcile(session_store),
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
        Request::Projects { include_archived } => {
            match session_store.list_projects(include_archived) {
                Ok(projects) => Response::success(json!({
                    "source": "rollout_jsonl",
                    "projects": projects,
                    "include_archived": include_archived,
                })),
                Err(error) => backend_error(error),
            }
        }
        Request::ProjectThreads {
            project_path,
            include_archived,
            offset,
            limit,
        } => {
            if !(1..=200).contains(&limit) {
                return Response::error(
                    "invalid_request",
                    "project_threads limit must be between 1 and 200",
                );
            }
            match session_store.list_project_threads(
                &project_path,
                include_archived,
                offset as usize,
                limit as usize,
            ) {
                Ok((threads, available)) => {
                    let returned = threads.len();
                    Response::success(json!({
                        "source": "rollout_jsonl",
                        "project_path": project_path,
                        "threads": threads,
                        "offset": offset,
                        "returned": returned,
                        "available": available,
                    }))
                }
                Err(error) => backend_error(error),
            }
        }
        Request::Messages {
            thread_id,
            before,
            limit,
        } => {
            if !(1..=200).contains(&limit) {
                return Response::error(
                    "invalid_request",
                    "messages limit must be between 1 and 200",
                );
            }
            match session_store.read_message_page(
                &thread_id,
                before.map(|value| value as usize),
                limit as usize,
            ) {
                Ok(Some((thread, page))) => {
                    let messages = page
                        .messages
                        .iter()
                        .enumerate()
                        .map(|(offset, message)| compact_web_message(message, page.start + offset))
                        .collect::<Vec<_>>();
                    Response::success(json!({
                        "source": "rollout_jsonl",
                        "thread": {
                            "id": thread.id,
                            "title": thread.title,
                            "cwd": thread.cwd,
                            "git_branch": thread.git_branch,
                            "created_at": thread.created_at,
                            "updated_at_ms": thread.updated_at_ms,
                            "source": thread.source,
                            "archived": thread.archived,
                        },
                        "messages": messages,
                        "page": {
                            "start": page.start,
                            "end": page.end,
                            "total": page.total,
                            "has_more": page.has_more,
                            "before": page.start,
                        }
                    }))
                }
                Ok(None) => Response::error(
                    "thread_not_found",
                    format!("thread {thread_id} was not found in the rollout store"),
                ),
                Err(error) => backend_error(error),
            }
        }
        Request::MessageContent {
            thread_id,
            message_index,
            content_index,
            content_end,
        } => match session_store.read_message_content(
            &thread_id,
            message_index as usize,
            content_index as usize,
            content_end.map(|value| value as usize),
        ) {
            Ok(Some(content)) => Response::success(json!({
                "thread_id": thread_id,
                "message_index": message_index,
                "content_index": content_index,
                "content_end": content_end,
                "content": content,
            })),
            Ok(None) => Response::error(
                "message_content_not_found",
                "the requested message content was not found",
            ),
            Err(error) => backend_error(error),
        },
        Request::ToolContent {
            thread_id,
            message_index,
            tool_index,
        } => match session_store.read_tool_content(
            &thread_id,
            message_index as usize,
            tool_index as usize,
        ) {
            Ok(Some(tool)) => {
                let display_input = parsed_tool_input(&tool).unwrap_or_else(|| tool.input.clone());
                Response::success(json!({
                    "thread_id": thread_id,
                    "message_index": message_index,
                    "tool_index": tool_index,
                    "tool": tool,
                    "display_input": display_input,
                }))
            }
            Ok(None) => Response::error(
                "tool_content_not_found",
                "the requested tool call was not found",
            ),
            Err(error) => backend_error(error),
        },
        Request::ThreadActivity { thread_id } => match session_store.thread_activity(&thread_id) {
            Ok(Some(activity)) => Response::success(json!({
                "thread_id": thread_id,
                "activity": activity,
            })),
            Ok(None) => Response::error(
                "thread_not_found",
                format!("thread {thread_id} was not found in the rollout store"),
            ),
            Err(error) => backend_error(error),
        },
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
            let pending_id = pending_messages.begin(
                &resolved.thread.id,
                &text,
                "queue",
                latest_message_index(session_store, &resolved.thread.id),
            );
            match write_backend.queue_message(&resolved.thread.id, &text) {
                Ok(backend) => {
                    pending_messages.finish(pending_id, "queued", None);
                    Response::success(json!({
                        "action": "send",
                        "status": "queued",
                        "pending_id": pending_id,
                        "thread_id": resolved.thread.id,
                        "target": resolved.method,
                        "backend": backend,
                    }))
                }
                Err(error) => {
                    pending_messages.finish(pending_id, "failed", Some(error.message.clone()));
                    write_backend_error(error)
                }
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
            let pending_id = pending_messages.begin(
                &resolved.thread.id,
                &text,
                "steer",
                latest_message_index(session_store, &resolved.thread.id),
            );
            match write_backend.steer_via_app_server(&resolved.thread.id, &turn_id, &text) {
                Ok(backend) => {
                    pending_messages.finish(pending_id, "steered", None);
                    Response::success(json!({
                        "action": "steer",
                        "status": "steered",
                        "pending_id": pending_id,
                        "thread_id": resolved.thread.id,
                        "target": resolved.method,
                        "semantics": "app_server_steer",
                        "backend": backend,
                    }))
                }
                Err(error) => {
                    pending_messages.finish(pending_id, "failed", Some(error.message.clone()));
                    write_backend_error(error)
                }
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
        Request::AppServerRpc { method, params } => {
            if method.is_empty() || method.len() > 256 {
                return Response::error(
                    "invalid_request",
                    "app-server method must contain between 1 and 256 bytes",
                );
            }
            if matches!(method.as_str(), "initialize" | "initialized") {
                return Response::error(
                    "invalid_request",
                    "the bridge manages app-server initialization automatically",
                );
            }
            match write_backend.app_server_rpc(&method, params) {
                Ok(result) => Response::success(json!({
                    "action": "app_server_rpc",
                    "method": method,
                    "result": result,
                })),
                Err(error) => write_backend_error(error),
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

fn latest_message_index(session_store: &SessionStore, thread_id: &str) -> i64 {
    session_store
        .read_thread(thread_id)
        .ok()
        .flatten()
        .map(|snapshot| snapshot.messages.len() as i64 - 1)
        .unwrap_or(-1)
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

fn compact_web_message(message: &ThreadMessage, message_index: usize) -> Value {
    let mut content = Vec::new();
    let mut has_visible_text = false;
    let mut content_index = 0;
    while content_index < message.content.len() {
        let item = &message.content[content_index];
        let item_type = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("content");
        let text = item
            .get("text")
            .or_else(|| item.get("output_text"))
            .and_then(Value::as_str);

        if let Some(text) = text {
            if let Some(label) = transcript_block_label(text) {
                let content_end = transcript_block_end(&message.content, content_index);
                content.push(lazy_content_range_summary(
                    label,
                    &message.content[content_index..content_end],
                    message_index,
                    content_index,
                    content_end,
                ));
                content_index = content_end;
                continue;
            } else if let Some(request) = user_request_from_file_wrapper(text) {
                let content_end = attachment_block_end(&message.content, content_index);
                let label = if message.content[content_index..content_end]
                    .iter()
                    .any(|item| item.get("type").and_then(Value::as_str) == Some("input_image"))
                {
                    "Image attachment"
                } else {
                    "Attached-file context"
                };
                content.push(lazy_content_range_summary(
                    label,
                    &message.content[content_index..content_end],
                    message_index,
                    content_index,
                    content_end,
                ));
                if !request.trim().is_empty() {
                    content.push(json!({"kind": "text", "text": request}));
                    has_visible_text = true;
                }
                content_index = content_end;
                continue;
            } else if let Some((before, citation, after)) = memory_citation_parts(text) {
                if !before.trim().is_empty() {
                    content.push(json!({"kind": "text", "text": before.trim()}));
                    has_visible_text = true;
                }
                content.push(json!({
                    "kind": "context",
                    "label": "Memory citations",
                    "bytes": citation.len(),
                    "text": citation,
                }));
                if !after.trim().is_empty() {
                    content.push(json!({"kind": "text", "text": after.trim()}));
                    has_visible_text = true;
                }
            } else if let Some(label) = injected_text_label(text) {
                content.push(lazy_content_summary(
                    label,
                    item,
                    message_index,
                    content_index,
                ));
            } else {
                content.push(json!({"kind": "text", "text": text}));
                has_visible_text = true;
            }
        } else {
            let label = match item_type {
                "input_image" => "Image attachment",
                "input_audio" => "Audio attachment",
                _ => "Structured context",
            };
            content.push(lazy_content_summary(
                label,
                item,
                message_index,
                content_index,
            ));
        }
        content_index += 1;
    }

    let category = match message.role.as_str() {
        "user" if has_visible_text => "user",
        "assistant" => "assistant",
        _ => "context",
    };
    json!({
        "timestamp": message.timestamp,
        "id": message.id,
        "role": message.role,
        "phase": message.phase,
        "category": category,
        "message_index": message_index,
        "content": content,
        "tools": message
            .tools
            .iter()
            .enumerate()
            .map(|(tool_index, tool)| compact_tool_summary(tool, tool_index))
            .collect::<Vec<_>>(),
    })
}

fn compact_tool_summary(tool: &ThreadToolCall, tool_index: usize) -> Value {
    let patch_stats = tool_patch(tool).map(|patch| patch_line_stats(&patch));
    json!({
        "tool_index": tool_index,
        "name": display_tool_name(tool),
        "status": tool.status,
        "preview": tool_preview(tool),
        "has_output": tool.output.is_some(),
        "bytes": serde_json::to_vec(tool).map_or(0, |encoded| encoded.len()),
        "additions": patch_stats.map(|stats| stats.0),
        "deletions": patch_stats.map(|stats| stats.1),
    })
}

fn tool_preview(tool: &ThreadToolCall) -> String {
    let raw = tool.input.as_str().unwrap_or("");
    if let Some(patch) = tool_patch(tool) {
        let files = patch_file_actions(&patch);
        return match files.as_slice() {
            [file] => format!(
                "已{} {}",
                patch_action_label(file["action"].as_str().unwrap_or("update")),
                Path::new(file["path"].as_str().unwrap_or("file"))
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("file")
            ),
            files if !files.is_empty() => format!("已编辑 {} 个文件", files.len()),
            _ => "应用文件补丁".to_owned(),
        };
    }
    let preferred = if display_tool_name(tool) == "exec_command" {
        source_string_field(raw, "cmd").unwrap_or_else(|| raw.to_owned())
    } else if tool.name.ends_with(".js") {
        source_string_field(raw, "title")
            .or_else(|| source_string_field(raw, "code"))
            .unwrap_or_else(|| raw.to_owned())
    } else {
        raw.to_owned()
    };
    let normalized = preferred.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview = if normalized.is_empty() {
        tool.name.clone()
    } else {
        normalized
    };
    preview.chars().take(140).collect()
}

fn parsed_tool_input(tool: &ThreadToolCall) -> Option<Value> {
    let source = tool.input.as_str()?;
    if let Some(patch) = tool_patch(tool) {
        return Some(json!({
            "operation": "apply_patch",
            "files": patch_file_actions(&patch),
            "patch": patch,
        }));
    }
    if display_tool_name(tool) != "exec_command" {
        return None;
    }
    let mut parsed = serde_json::Map::new();
    for field in ["cmd", "workdir", "justification"] {
        if let Some(value) = source_string_field(source, field) {
            parsed.insert(field.to_owned(), Value::String(value));
        }
    }
    for field in ["yield_time_ms", "max_output_tokens"] {
        if let Some(value) = source_u64_field(source, field) {
            parsed.insert(field.to_owned(), Value::from(value));
        }
    }
    (!parsed.is_empty()).then_some(Value::Object(parsed))
}

fn display_tool_name(tool: &ThreadToolCall) -> &str {
    let source = tool.input.as_str().unwrap_or("");
    if tool.name == "apply_patch" || source.contains("tools.apply_patch") {
        "apply_patch"
    } else if tool.name == "exec" && source.contains("tools.exec_command") {
        "exec_command"
    } else if tool.name == "exec" && source.contains("tools.write_stdin") {
        "write_stdin"
    } else {
        &tool.name
    }
}

fn tool_patch(tool: &ThreadToolCall) -> Option<String> {
    let source = tool.input.as_str()?;
    if tool.name == "apply_patch" && source.trim_start().starts_with("*** Begin Patch") {
        return Some(source.to_owned());
    }
    for marker in ["const patch =", "let patch =", "var patch ="] {
        if let Some(rest) = source.split_once(marker).map(|(_, rest)| rest.trim_start()) {
            let mut stream = serde_json::Deserializer::from_str(rest).into_iter::<String>();
            if let Some(Ok(patch)) = stream.next() {
                return Some(patch);
            }
        }
    }
    let rest = source.split_once("tools.apply_patch(")?.1.trim_start();
    let mut stream = serde_json::Deserializer::from_str(rest).into_iter::<String>();
    stream.next()?.ok()
}

fn patch_file_actions(patch: &str) -> Vec<Value> {
    patch
        .lines()
        .filter_map(|line| {
            [
                ("*** Add File: ", "add"),
                ("*** Update File: ", "update"),
                ("*** Delete File: ", "delete"),
                ("*** Move to: ", "move"),
            ]
            .into_iter()
            .find_map(|(prefix, action)| {
                line.strip_prefix(prefix)
                    .map(|path| json!({"action": action, "path": path}))
            })
        })
        .collect()
}

fn patch_action_label(action: &str) -> &'static str {
    match action {
        "add" => "新建",
        "delete" => "删除",
        "move" => "移动",
        _ => "编辑",
    }
}

fn patch_line_stats(patch: &str) -> (usize, usize) {
    patch.lines().fold((0, 0), |(additions, deletions), line| {
        if line.starts_with('+') && !line.starts_with("+++") {
            (additions + 1, deletions)
        } else if line.starts_with('-') && !line.starts_with("---") {
            (additions, deletions + 1)
        } else {
            (additions, deletions)
        }
    })
}

fn source_field_tail<'a>(source: &'a str, field: &str) -> Option<&'a str> {
    for marker in [format!("\"{field}\""), field.to_owned()] {
        let mut search_from = 0;
        while let Some(relative) = source[search_from..].find(&marker) {
            let start = search_from + relative;
            let before = source[..start].chars().next_back();
            let after_name = start + marker.len();
            let boundary_ok = marker.starts_with('"')
                || before.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
            if boundary_ok {
                let tail = source[after_name..].trim_start();
                if let Some(tail) = tail.strip_prefix(':') {
                    return Some(tail.trim_start());
                }
            }
            search_from = after_name;
        }
    }
    None
}

fn source_string_field(source: &str, field: &str) -> Option<String> {
    let rest = source_field_tail(source, field)?;
    let mut stream = serde_json::Deserializer::from_str(rest).into_iter::<String>();
    stream.next()?.ok()
}

fn source_u64_field(source: &str, field: &str) -> Option<u64> {
    let rest = source_field_tail(source, field)?;
    let end = rest
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(rest.len());
    rest.get(..end)?.parse().ok()
}

fn lazy_content_range_summary(
    label: &str,
    items: &[Value],
    message_index: usize,
    content_index: usize,
    content_end: usize,
) -> Value {
    json!({
        "kind": "context",
        "label": label,
        "bytes": items
            .iter()
            .map(|item| serde_json::to_vec(item).map_or(0, |encoded| encoded.len()))
            .sum::<usize>(),
        "message_index": message_index,
        "content_index": content_index,
        "content_end": content_end,
    })
}

fn transcript_block_label(text: &str) -> Option<&'static str> {
    let trimmed = text.trim_start();
    if trimmed.starts_with(">>> TRANSCRIPT DELTA START") {
        Some("Transcript delta")
    } else if trimmed.starts_with(">>> TRANSCRIPT START")
        || trimmed.starts_with(
            "The following is the Codex agent history whose request action you are assessing.",
        )
    {
        Some("Agent transcript context")
    } else {
        None
    }
}

fn transcript_block_end(items: &[Value], start: usize) -> usize {
    if items[start]
        .get("text")
        .and_then(Value::as_str)
        .is_some_and(|text| {
            text.contains(">>> TRANSCRIPT END") || text.contains(">>> TRANSCRIPT DELTA END")
        })
    {
        return start + 1;
    }
    items
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(index, item)| {
            item.get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| {
                    text.contains(">>> TRANSCRIPT END") || text.contains(">>> TRANSCRIPT DELTA END")
                })
                .then_some(index + 1)
        })
        .unwrap_or(items.len())
}

fn lazy_content_summary(
    label: &str,
    item: &Value,
    message_index: usize,
    content_index: usize,
) -> Value {
    json!({
        "kind": "context",
        "label": label,
        "bytes": serde_json::to_vec(item).map_or(0, |encoded| encoded.len()),
        "message_index": message_index,
        "content_index": content_index,
    })
}

fn attachment_block_end(items: &[Value], start: usize) -> usize {
    let mut end = start + 1;
    while let Some(item) = items.get(end) {
        let item_type = item.get("type").and_then(Value::as_str);
        let text = item
            .get("text")
            .or_else(|| item.get("output_text"))
            .and_then(Value::as_str)
            .map(str::trim);
        let belongs_to_attachment = matches!(item_type, Some("input_image" | "input_audio"))
            || text.is_some_and(|text| text.starts_with("<image name=") || text == "</image>");
        if !belongs_to_attachment {
            break;
        }
        end += 1;
    }
    end
}

fn memory_citation_parts(text: &str) -> Option<(&str, &str, &str)> {
    const START: &str = "<oai-mem-citation>";
    const END: &str = "</oai-mem-citation>";
    let start = text.find(START)?;
    let relative_end = text[start..].find(END)?;
    let end = start + relative_end + END.len();
    Some((&text[..start], &text[start..end], &text[end..]))
}

fn injected_text_label(text: &str) -> Option<&'static str> {
    let trimmed = text.trim_start();
    if trimmed.starts_with(">>> TRANSCRIPT DELTA START") {
        Some("Transcript delta")
    } else if trimmed.starts_with(">>> TRANSCRIPT START")
        || trimmed.starts_with(
            "The following is the Codex agent history whose request action you are assessing.",
        )
    {
        Some("Agent transcript context")
    } else if trimmed.starts_with("<recommended_plugins>") {
        Some("Recommended plugins")
    } else if trimmed.starts_with("# AGENTS.md instructions") {
        Some("AGENTS.md instructions")
    } else if trimmed.starts_with("<environment_context>") {
        Some("Environment context")
    } else if trimmed.starts_with("<permissions instructions>") {
        Some("Permission context")
    } else if trimmed.starts_with("<skills_instructions>") {
        Some("Skills context")
    } else if trimmed.starts_with("<collaboration_mode>")
        || trimmed.starts_with("<multi_agent_mode>")
    {
        Some("Collaboration context")
    } else if trimmed.starts_with("<image name=") || trimmed == "</image>" {
        Some("Image reference")
    } else {
        None
    }
}

fn user_request_from_file_wrapper(text: &str) -> Option<&str> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("# Files mentioned by the user:") {
        return None;
    }
    text.split_once("## My request:")
        .map(|(_, request)| request.trim())
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

    #[test]
    fn web_ui_accepts_ip_authorities_only_on_its_port() {
        assert!(web_authority_allowed("127.0.0.1:47653", 47653));
        assert!(web_authority_allowed("192.168.1.20:47653", 47653));
        assert!(web_authority_allowed("[::1]:47653", 47653));
        assert!(!web_authority_allowed("127.0.0.1:9000", 47653));
        assert!(!web_authority_allowed("codex.example:47653", 47653));
    }

    #[test]
    fn web_ui_rejects_cross_origin_browser_requests() {
        assert!(web_origin_allowed("http://127.0.0.1:47653", 47653));
        assert!(web_origin_allowed("http://192.168.1.20:47653", 47653));
        assert!(!web_origin_allowed("https://localhost:47653", 47653));
        assert!(!web_origin_allowed("https://example.com", 47653));
    }

    #[test]
    fn web_ui_requires_exact_basic_auth_credentials() {
        let auth = WebAuth {
            username: "codex".to_owned(),
            password: "test-password".to_owned(),
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic Y29kZXg6dGVzdC1wYXNzd29yZA=="),
        );
        assert!(basic_auth_allowed(&headers, &auth));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic Y29kZXg6d3Jvbmc="),
        );
        assert!(!basic_auth_allowed(&headers, &auth));
    }

    #[test]
    fn bridge_pending_state_tracks_status_and_new_message_position() {
        let pending = PendingMessages::default();
        let id = pending.begin("thread-1", "same text", "queue", 1);
        pending.finish(id, "queued", None);
        let entry = pending.entries.read().unwrap()[0].clone();
        assert_eq!(entry.status, "queued");

        let message = |role: &str, text: &str| ThreadMessage {
            timestamp: None,
            id: None,
            role: role.to_owned(),
            phase: None,
            content: vec![json!({"type":"input_text","text":text})],
            tools: Vec::new(),
        };
        let messages = vec![
            message("user", "same text"),
            message("assistant", "working"),
            message("user", "same text"),
        ];
        let mut landed = HashSet::new();
        assert!(pending_message_landed(&entry, &messages, &mut landed));
        assert!(!pending_message_landed(&entry, &messages, &mut landed));

        let after_new_message = PendingMessage {
            after_message_index: 2,
            ..entry
        };
        assert!(!pending_message_landed(
            &after_new_message,
            &messages,
            &mut HashSet::new()
        ));
    }

    #[test]
    fn embedded_web_ui_uses_split_data_feeds_and_keeps_raw_protocol_access() {
        let html = include_str!("web_ui.html");
        for command in [
            "projects",
            "project_threads",
            "messages",
            "message_content",
            "tool_content",
            "thread_activity",
            "select",
            "current",
            "status",
            "tail",
            "send",
            "steer",
            "scroll",
            "pending",
            "pending_messages",
            "approve",
            "decline",
            "interrupt",
            "host_exec",
            "app_server_rpc",
        ] {
            assert!(html.contains(command), "missing {command}");
        }
        assert!(html.contains("id=\"rawRequest\""));
        assert!(html.contains("Any codex-bridge Request JSON"));
        assert!(html.contains("appendToolValue(body,r.tool.output)"));
        assert!(html.contains(".tools select option{background:#fff;color:#151515}"));
        assert!(html.contains("id=\"outboxTray\""));
        assert!(html.contains("await refreshPending()"));
        assert!(!html.contains("sessionStorage"));
    }

    #[test]
    fn web_message_summary_folds_injected_and_binary_content() {
        let message = ThreadMessage {
            timestamp: None,
            id: Some("m1".into()),
            role: "user".into(),
            phase: None,
            content: vec![
                json!({"type":"input_text","text":">>> TRANSCRIPT DELTA START\nprivate injected text\n>>> TRANSCRIPT DELTA END"}),
                json!({"type":"input_image","image_url":"data:image/jpeg;base64,very-large-payload"}),
            ],
            tools: Vec::new(),
        };
        let compact = compact_web_message(&message, 7);
        let encoded = serde_json::to_string(&compact).unwrap();
        assert_eq!(compact["category"], "context");
        assert_eq!(compact["content"][0]["label"], "Transcript delta");
        assert_eq!(compact["content"][1]["label"], "Image attachment");
        assert_eq!(compact["content"][0]["message_index"], 7);
        assert!(!encoded.contains("private injected text"));
        assert!(!encoded.contains("very-large-payload"));
    }

    #[test]
    fn web_tool_summary_keeps_arguments_and_output_lazy() {
        let message = ThreadMessage {
            timestamp: None,
            id: Some("m1".into()),
            role: "assistant".into(),
            phase: Some("commentary".into()),
            content: vec![json!({"type":"output_text","text":"Checking."})],
            tools: vec![ThreadToolCall {
                call_id: "call-1".into(),
                name: "exec".into(),
                status: "completed".into(),
                input: Value::String(
                    r#"const r = await tools.exec_command({"cmd":"git status --short"});"#.into(),
                ),
                output: Some(Value::String("large private output".into())),
            }],
        };
        let compact = compact_web_message(&message, 5);
        let encoded = serde_json::to_string(&compact).unwrap();
        assert_eq!(compact["tools"][0]["name"], "exec_command");
        assert_eq!(compact["tools"][0]["preview"], "git status --short");
        assert_eq!(compact["tools"][0]["tool_index"], 0);
        assert!(compact["tools"][0]["has_output"].as_bool().unwrap());
        assert!(!encoded.contains("large private output"));
        assert!(!encoded.contains("tools.exec_command"));
    }

    #[test]
    fn exec_wrapper_with_unquoted_properties_is_parsed_for_display() {
        let tool = ThreadToolCall {
            call_id: "call-2".into(),
            name: "exec".into(),
            status: "completed".into(),
            input: Value::String(
                r#"const r = await tools.exec_command({cmd:"CARGO_INCREMENTAL=0 cargo build --release --locked && launchctl kickstart -k gui/501/com.lunghaa.codex-bridge",workdir:"/workspace/codexapp-cli",yield_time_ms:30000,max_output_tokens:20000}); text(JSON.stringify(r))"#.into(),
            ),
            output: None,
        };
        assert_eq!(
            tool_preview(&tool),
            "CARGO_INCREMENTAL=0 cargo build --release --locked && launchctl kickstart -k gui/501/com.lunghaa.codex-bridge"
        );
        assert_eq!(
            parsed_tool_input(&tool).unwrap(),
            json!({
                "cmd": "CARGO_INCREMENTAL=0 cargo build --release --locked && launchctl kickstart -k gui/501/com.lunghaa.codex-bridge",
                "workdir": "/workspace/codexapp-cli",
                "yield_time_ms": 30000,
                "max_output_tokens": 20000,
            })
        );
    }

    #[test]
    fn apply_patch_wrapper_reports_files_and_line_counts() {
        let tool = ThreadToolCall {
            call_id: "call-patch".into(),
            name: "exec".into(),
            status: "completed".into(),
            input: Value::String(
                r#"const patch = "*** Begin Patch\n*** Update File: /tmp/src/sessions.rs\n@@\n-old\n+new\n+more\n*** End Patch"; const r = await tools.apply_patch(patch);"#.into(),
            ),
            output: None,
        };
        assert_eq!(display_tool_name(&tool), "apply_patch");
        assert_eq!(tool_preview(&tool), "已编辑 sessions.rs");
        let compact = compact_tool_summary(&tool, 2);
        assert_eq!(compact["additions"], 2);
        assert_eq!(compact["deletions"], 1);
        assert_eq!(compact["name"], "apply_patch");
        let parsed = parsed_tool_input(&tool).unwrap();
        assert_eq!(parsed["operation"], "apply_patch");
        assert_eq!(parsed["files"][0]["path"], "/tmp/src/sessions.rs");
        assert!(parsed["patch"].as_str().unwrap().contains("+more"));
    }

    #[test]
    fn file_wrapper_keeps_the_actual_request_visible() {
        let message = ThreadMessage {
            timestamp: None,
            id: None,
            role: "user".into(),
            phase: None,
            content: vec![json!({
                "type":"input_text",
                "text":"# Files mentioned by the user:\n\n## image.jpg\n\n## My request:\n参考 Codex 的风格"
            })],
            tools: Vec::new(),
        };
        let compact = compact_web_message(&message, 0);
        assert_eq!(compact["category"], "user");
        assert_eq!(compact["content"][0]["label"], "Attached-file context");
        assert_eq!(compact["content"][1]["text"], "参考 Codex 的风格");
    }

    #[test]
    fn file_wrapper_and_image_parts_are_one_attachment() {
        let message = ThreadMessage {
            timestamp: None,
            id: None,
            role: "user".into(),
            phase: None,
            content: vec![
                json!({"type":"input_text","text":"# Files mentioned by the user:\n\n## image.jpg: /tmp/image.jpg\n\n## My request:\n输入框样式异常"}),
                json!({"type":"input_text","text":"<image name=[Image #1] path=\"/tmp/image.jpg\">"}),
                json!({"type":"input_image","image_url":"data:image/jpeg;base64,large-payload"}),
                json!({"type":"input_text","text":"</image>"}),
            ],
            tools: Vec::new(),
        };
        let compact = compact_web_message(&message, 3);
        let encoded = serde_json::to_string(&compact).unwrap();
        assert_eq!(compact["category"], "user");
        assert_eq!(compact["content"].as_array().unwrap().len(), 2);
        assert_eq!(compact["content"][0]["label"], "Image attachment");
        assert_eq!(compact["content"][0]["content_index"], 0);
        assert_eq!(compact["content"][0]["content_end"], 4);
        assert_eq!(compact["content"][1]["text"], "输入框样式异常");
        assert!(!encoded.contains("large-payload"));
    }

    #[test]
    fn memory_citation_is_folded_without_hiding_surrounding_markdown() {
        let message = ThreadMessage {
            timestamp: None,
            id: None,
            role: "assistant".into(),
            phase: None,
            content: vec![json!({
                "type":"output_text",
                "text":"## 完成\n\n正文\n<oai-mem-citation>private</oai-mem-citation>\n后续"
            })],
            tools: Vec::new(),
        };
        let compact = compact_web_message(&message, 0);
        assert_eq!(compact["content"].as_array().unwrap().len(), 3);
        assert_eq!(compact["content"][0]["text"], "## 完成\n\n正文");
        assert_eq!(compact["content"][1]["label"], "Memory citations");
        assert_eq!(compact["content"][2]["text"], "后续");
    }
}
