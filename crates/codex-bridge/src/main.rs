use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use axum::body::Bytes;
use axum::extract::ws::{Message as AxumWsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response as HttpResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use clap::Parser;
use codex_bridge::{
    default_codex_home, default_socket_path, BackendFailure, CodexCliBackend, HostExecFailure,
    HostExecutor, Request, Response, SessionStore, ThreadMessage, ThreadSummary, ThreadToolCall,
    APP_SERVER_SCHEMA_VERSION, APP_SERVER_SOCKET_ENV, CODEX_BIN_ENV, HOST_EXEC_POLICY_ENV,
    PROTOCOL_VERSION, SOCKET_ENV,
};
use futures_util::{SinkExt, StreamExt};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::mpsc;

const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_DOWNLOAD_BYTES: u64 = 16 * 1024 * 1024;
const DOWNLOAD_TICKET_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_DOWNLOAD_TICKETS: usize = 128;
const DEFAULT_WEB_UI_ADDR: &str = "127.0.0.1:18791";
const DESKTOP_CODEX_PATH: &str = "/Applications/ChatGPT.app/Contents/Resources/codex";
const WEB_INDEX: &str = include_str!("../../../web-ui/dist/index.html");
const WEB_APP_JS: &str = include_str!("../../../web-ui/dist/assets/app.js");
const WEB_APP_CSS: &str = include_str!("../../../web-ui/dist/assets/app.css");
const PINNED_THREAD_SECTION_ID: &str = "01984de2-8f74-7c91-a3b2-5c5e937cf318";
static NEXT_WORKTREE_ID: AtomicU64 = AtomicU64::new(1);

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

    /// Explicit external app-server WebSocket-over-UDS endpoint used for RPC experiments.
    /// No endpoint is selected by default; the standalone daemon is never used as a fallback.
    #[arg(long, value_name = "PATH")]
    app_server_socket: Option<PathBuf>,

    /// Maximum number of recently viewed threads kept subscribed on the app-server connection.
    #[arg(long, value_name = "COUNT", default_value_t = 3, value_parser = parse_thread_cache_limit)]
    app_server_thread_cache: usize,

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

    /// Exact HTTPS origin allowed through a trusted reverse proxy. May be repeated.
    #[arg(long, value_name = "HTTPS_ORIGIN")]
    web_ui_public_origin: Vec<String>,
}

#[derive(Clone)]
struct BridgeState {
    socket_path: Arc<PathBuf>,
    session_store: Arc<SessionStore>,
    write_backend: Arc<CodexCliBackend>,
    host_executor: Arc<HostExecutor>,
    selected_thread: Arc<RwLock<Option<String>>>,
    pending_messages: Arc<PendingMessages>,
    app_server_tools: Arc<AppServerToolCache>,
}

#[derive(Clone)]
struct WebState {
    bridge: BridgeState,
    port: u16,
    auth: Arc<WebAuth>,
    public_origins: Arc<Vec<WebOrigin>>,
    download_tickets: Arc<RwLock<HashMap<String, DownloadTicket>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WebOrigin {
    scheme: String,
    authority: String,
}

struct WebAuth {
    username: String,
    password: String,
}

#[derive(Debug, Deserialize)]
struct FileTicketRequest {
    thread_id: String,
    path: String,
}

#[derive(Debug, Deserialize)]
struct FileDownloadQuery {
    ticket: String,
}

#[derive(Debug, Clone)]
struct DownloadTicket {
    workspace: PathBuf,
    path: PathBuf,
    expires_at: Instant,
}

#[derive(Debug, PartialEq, Eq)]
enum FileDownloadFailure {
    ThreadNotFound,
    FileNotFound,
    OutsideWorkspace,
    NotAFile,
    TooLarge,
    ReadFailed,
}

#[derive(Debug, Default)]
struct AppServerToolCache {
    threads: RwLock<HashMap<String, CachedAppServerTools>>,
}

#[derive(Debug, Clone)]
struct CachedAppServerTools {
    refreshed_at: Instant,
    known_message_ids: HashSet<String>,
    tools: HashMap<String, Vec<ThreadToolCall>>,
}

#[derive(Debug, Clone, Serialize)]
struct PendingMessage {
    id: String,
    thread_id: String,
    text: String,
    action: String,
    status: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    queued_submission_id: Option<String>,
    #[serde(skip)]
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
    fn begin(&self, thread_id: &str, text: &str, action: &str, after_message_index: i64) -> String {
        let id = format!(
            "bridge-{}",
            self.next_id.fetch_add(1, Ordering::Relaxed) + 1
        );
        if let Ok(mut entries) = self.entries.write() {
            entries.push(PendingMessage {
                id: id.clone(),
                thread_id: thread_id.to_owned(),
                text: text.to_owned(),
                action: action.to_owned(),
                status: format!("{action}ing"),
                source: "bridge".to_owned(),
                queued_submission_id: None,
                after_message_index,
                error: None,
            });
        }
        id
    }

    fn finish(&self, id: &str, status: &str, error: Option<String>) {
        if let Ok(mut entries) = self.entries.write() {
            if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                entry.status = status.to_owned();
                entry.error = error;
            }
        }
    }

    fn finish_queue(&self, id: &str, queued_submission_id: String) {
        if let Ok(mut entries) = self.entries.write() {
            if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                entry.status = "queued".to_owned();
                entry.source = "app_server_queue".to_owned();
                entry.queued_submission_id = Some(queued_submission_id);
                entry.error = None;
            }
        }
    }

    fn reconcile(
        &self,
        session_store: &SessionStore,
        queued: Vec<PendingMessage>,
        thread_id: Option<&str>,
    ) -> Vec<PendingMessage> {
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
        let mut matched_queue_ids = HashSet::new();
        let mut matched_entry_ids = HashSet::new();
        for entry in entries
            .iter_mut()
            .filter(|entry| entry.action == "queue" && entry.status != "failed")
        {
            if let Some(queued_entry) = queued.iter().find(|queued_entry| {
                !matched_queue_ids.contains(&queued_entry.id)
                    && queued_entry.thread_id == entry.thread_id
                    && (entry.queued_submission_id.as_deref()
                        == queued_entry.queued_submission_id.as_deref()
                        || queued_entry.id == entry.id
                        || (entry.queued_submission_id.is_none()
                            && entry.source != "app_server_queue"
                            && queued_entry.text == entry.text))
            }) {
                entry.queued_submission_id = queued_entry.queued_submission_id.clone();
                matched_queue_ids.insert(queued_entry.id.clone());
                matched_entry_ids.insert(entry.id.clone());
            }
        }
        entries.retain(|entry| {
            let in_scope = thread_id.is_none_or(|thread_id| entry.thread_id == thread_id);
            entry.action != "queue"
                || entry.source != "app_server_queue"
                || !in_scope
                || matched_entry_ids.contains(&entry.id)
        });
        let mut combined = entries.clone();
        for queued_entry in queued {
            if !matched_queue_ids.contains(&queued_entry.id) {
                combined.push(queued_entry);
            }
        }
        combined
            .into_iter()
            .filter(|entry| thread_id.is_none_or(|thread_id| entry.thread_id == thread_id))
            .collect()
    }

    fn dismiss(&self, id: &str, thread_id: &str) {
        if let Ok(mut entries) = self.entries.write() {
            entries.retain(|entry| entry.id != id || entry.thread_id != thread_id);
        }
    }
}

fn read_codex_queue(codex_home: &Path) -> Vec<PendingMessage> {
    let path = codex_home.join("queue_1.sqlite");
    let Ok(connection) = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Vec::new();
    };
    let Ok(mut statement) = connection.prepare(
        "SELECT id, thread_id, payload_json FROM queued_items ORDER BY thread_id, queue_order",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    }) else {
        return Vec::new();
    };
    rows.filter_map(|row| row.ok())
        .filter_map(|(id, thread_id, payload)| {
            let text = queue_payload_text(&payload)?;
            Some(PendingMessage {
                queued_submission_id: Some(id.clone()),
                id,
                thread_id,
                text,
                action: "queue".to_owned(),
                status: "queued".to_owned(),
                source: "codex_queue".to_owned(),
                after_message_index: -1,
                error: None,
            })
        })
        .collect()
}

fn read_native_queue(
    write_backend: &CodexCliBackend,
    thread_id: &str,
) -> Result<Vec<PendingMessage>, BackendFailure> {
    let mut cursor = None;
    let mut queued = Vec::new();
    loop {
        let result = write_backend.app_server_rpc(
            "thread/queue/list",
            json!({"threadId": thread_id, "cursor": cursor, "limit": 100}),
        )?;
        for submission in result
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = submission.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(text) = submission
                .get("input")
                .and_then(queue_input_text)
                .filter(|text| !text.is_empty())
            else {
                continue;
            };
            let client_id = submission
                .get("clientUserMessageId")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or(id);
            queued.push(PendingMessage {
                id: client_id.to_owned(),
                thread_id: thread_id.to_owned(),
                text,
                action: "queue".to_owned(),
                status: "queued".to_owned(),
                source: "app_server_queue".to_owned(),
                queued_submission_id: Some(id.to_owned()),
                after_message_index: -1,
                error: None,
            });
        }
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            return Ok(queued);
        }
    }
}

fn queue_input_text(input: &Value) -> Option<String> {
    let text = input
        .as_array()?
        .iter()
        .filter_map(|item| {
            (item.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| item.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn queued_messages_for_thread(
    session_store: &SessionStore,
    write_backend: &CodexCliBackend,
    thread_id: Option<&str>,
) -> (Vec<PendingMessage>, &'static str) {
    if let Some(thread_id) = thread_id {
        if let Ok(messages) = read_native_queue(write_backend, thread_id) {
            return (messages, "app_server");
        }
    }
    (
        read_codex_queue(session_store.home())
            .into_iter()
            .filter(|entry| thread_id.is_none_or(|thread_id| entry.thread_id == thread_id))
            .collect(),
        "compatibility",
    )
}

fn queue_payload_text(payload: &str) -> Option<String> {
    let payload: Value = serde_json::from_str(payload).ok()?;
    let content = payload.get("UserInput")?.get("content")?.as_array()?;
    let text = content
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
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

/// Prefer the CLI shipped with Codex Desktop because launchd does not inherit
/// an interactive shell's PATH. Other installations retain the PATH lookup.
fn default_codex_program() -> PathBuf {
    let bundled = PathBuf::from(DESKTOP_CODEX_PATH);
    if bundled.is_file() {
        bundled
    } else {
        PathBuf::from("codex")
    }
}

fn app_server_version_from_user_agent(user_agent: &str) -> Option<&str> {
    user_agent
        .split(|character: char| character.is_whitespace() || character == '/')
        .find(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_digit())
                && part.chars().any(|character| character == '.')
        })
}

fn parse_thread_cache_limit(value: &str) -> std::result::Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|_| "thread cache size must be an integer".to_owned())?;
    (1..=64)
        .contains(&limit)
        .then_some(limit)
        .ok_or_else(|| "thread cache size must be between 1 and 64".to_owned())
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
        .unwrap_or_else(default_codex_program);
    let app_server_socket = args.app_server_socket.clone().or_else(|| {
        env::var_os(APP_SERVER_SOCKET_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    let session_store = Arc::new(SessionStore::new(codex_home));
    let write_backend = Arc::new(CodexCliBackend::new_with_thread_cache(
        codex_program,
        app_server_socket,
        args.app_server_thread_cache,
    ));
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
    let app_server_tools = Arc::new(AppServerToolCache::default());
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
    let web_public_origins = args
        .web_ui_public_origin
        .iter()
        .map(|origin| parse_public_web_origin(origin))
        .collect::<Result<Vec<_>>>()?;

    prepare_socket_path(&socket_path, secure_existing_parent).await?;
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to secure {}", socket_path.display()))?;
    let _socket_guard = SocketGuard::new(&socket_path)?;

    let socket_path = Arc::new(socket_path);
    let bridge_state = BridgeState {
        socket_path: Arc::clone(&socket_path),
        session_store: Arc::clone(&session_store),
        write_backend: Arc::clone(&write_backend),
        host_executor,
        selected_thread,
        pending_messages,
        app_server_tools,
    };
    let hot_cache_task = spawn_hot_session_cache(
        Arc::clone(&session_store),
        Arc::clone(&write_backend),
        args.app_server_thread_cache,
    );

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
            public_origins: Arc::new(web_public_origins),
            download_tickets: Arc::new(RwLock::new(HashMap::new())),
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
    if let Some(hot_cache_task) = hot_cache_task {
        hot_cache_task.abort();
    }

    Ok(())
}

fn spawn_hot_session_cache(
    session_store: Arc<SessionStore>,
    write_backend: Arc<CodexCliBackend>,
    pinned_limit: usize,
) -> Option<tokio::task::JoinHandle<()>> {
    let mut events = write_backend.subscribe_app_server_events()?;
    Some(tokio::spawn(async move {
        let mut refresh = tokio::time::interval(Duration::from_secs(10));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_warm = HashMap::<String, Instant>::new();
        let mut refresh_count = 0_u64;
        loop {
            tokio::select! {
                event = events.recv() => {
                    let event = match event {
                        Ok(event) => event,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    let Some(thread_id) = app_server_event_thread_id(&event) else {
                        continue;
                    };
                    let now = Instant::now();
                    if last_warm.get(&thread_id).is_some_and(|last| now.duration_since(*last) < Duration::from_millis(200)) {
                        continue;
                    }
                    last_warm.insert(thread_id.clone(), now);
                    let store = Arc::clone(&session_store);
                    let _ = tokio::task::spawn_blocking(move || store.warm_thread_messages(&thread_id)).await;
                }
                _ = refresh.tick() => {
                    refresh_count = refresh_count.wrapping_add(1);
                    let store = Arc::clone(&session_store);
                    let backend = Arc::clone(&write_backend);
                    let refresh_pins = refresh_count == 1 || refresh_count % 3 == 0;
                    let _ = tokio::task::spawn_blocking(move || {
                        let Ok(loaded_result) = backend.app_server_rpc("thread/loaded/list", json!({})) else {
                            return;
                        };
                        let loaded = loaded_thread_ids(&loaded_result);
                        let mut active_thread_ids = Vec::new();
                        let mut complete_snapshot = true;
                        for thread_id in loaded {
                            let active = match backend.app_server_rpc(
                                "thread/read",
                                json!({"threadId": thread_id, "includeTurns": false}),
                            ) {
                                Ok(result) => thread_result_is_active(&result),
                                Err(_) => {
                                    complete_snapshot = false;
                                    false
                                }
                            };
                            if active {
                                active_thread_ids.push(thread_id.clone());
                                let _ = backend.watch_thread(&thread_id);
                                let _ = store.warm_thread_messages(&thread_id);
                            }
                        }
                        if complete_snapshot {
                            backend.publish_bridge_event(json!({
                                "type": "bridge_thread_activity_snapshot",
                                "active_thread_ids": active_thread_ids,
                            }));
                        }
                        if refresh_pins {
                            for thread_id in pinned_thread_ids(&backend)
                                .unwrap_or_default()
                                .into_iter()
                                .take(pinned_limit)
                            {
                                let _ = store.warm_thread_messages(&thread_id);
                            }
                        }
                    }).await;
                    last_warm.retain(|_, last| last.elapsed() < Duration::from_secs(60));
                }
            }
        }
    }))
}

fn app_server_event_thread_id(event: &Value) -> Option<String> {
    event
        .pointer("/message/params/threadId")
        .or_else(|| event.pointer("/message/params/thread/id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn loaded_thread_ids(result: &Value) -> Vec<String> {
    result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| match entry {
            Value::String(id) => (!id.is_empty()).then(|| id.clone()),
            _ => {
                let thread = entry.get("thread").unwrap_or(entry);
                thread
                    .get("id")
                    .or_else(|| entry.get("threadId"))
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
            }
        })
        .collect()
}

fn thread_result_is_active(result: &Value) -> bool {
    let status = result
        .pointer("/thread/status")
        .or_else(|| result.get("status"));
    status.and_then(Value::as_str) == Some("active")
        || status
            .and_then(|status| status.get("type"))
            .and_then(Value::as_str)
            == Some("active")
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
                    &state.app_server_tools,
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
        .route("/assets/app.js", get(web_app_js))
        .route("/assets/app.css", get(web_app_css))
        .route("/api/auth", get(web_auth_check))
        .route("/api/file-ticket", post(web_file_ticket))
        .route("/api/file", get(web_file_download))
        .route("/api/command", post(web_command))
        .route("/api/events", get(web_events))
        .with_state(state)
}

async fn web_index(State(state): State<WebState>, headers: HeaderMap) -> HttpResponse {
    web_asset_response(&state, &headers, WEB_INDEX, "text/html; charset=utf-8")
}

async fn web_app_js(State(state): State<WebState>, headers: HeaderMap) -> HttpResponse {
    web_asset_response(
        &state,
        &headers,
        WEB_APP_JS,
        "text/javascript; charset=utf-8",
    )
}

async fn web_app_css(State(state): State<WebState>, headers: HeaderMap) -> HttpResponse {
    web_asset_response(&state, &headers, WEB_APP_CSS, "text/css; charset=utf-8")
}

async fn web_auth_check(State(state): State<WebState>, headers: HeaderMap) -> HttpResponse {
    if !basic_auth_allowed(&headers, &state.auth) {
        return basic_auth_required();
    }
    if !web_headers_allowed(&headers, state.port, false, &state.public_origins) {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

async fn web_file_download(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<FileDownloadQuery>,
) -> HttpResponse {
    if !web_headers_allowed(&headers, state.port, false, &state.public_origins) {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    let ticket = get_download_ticket(&state.download_tickets, &query.ticket);
    let Some(ticket) = ticket else {
        return (
            StatusCode::NOT_FOUND,
            "download ticket is invalid or expired",
        )
            .into_response();
    };
    let result = tokio::task::spawn_blocking(move || {
        read_workspace_download(&ticket.workspace, ticket.path.to_string_lossy().as_ref())
    })
    .await;
    let (bytes, filename) = match result {
        Ok(Ok(download)) => download,
        Ok(Err(FileDownloadFailure::ThreadNotFound | FileDownloadFailure::FileNotFound)) => {
            return (StatusCode::NOT_FOUND, "file not found").into_response();
        }
        Ok(Err(FileDownloadFailure::OutsideWorkspace)) => {
            return (
                StatusCode::FORBIDDEN,
                "file is outside the session workspace",
            )
                .into_response();
        }
        Ok(Err(FileDownloadFailure::TooLarge)) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                "file must be smaller than 16 MiB",
            )
                .into_response();
        }
        Ok(Err(FileDownloadFailure::NotAFile | FileDownloadFailure::ReadFailed)) | Err(_) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                "file cannot be downloaded",
            )
                .into_response();
        }
    };
    let mut response = bytes.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment; filename=\"download\"")),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

fn get_download_ticket(
    tickets: &RwLock<HashMap<String, DownloadTicket>>,
    token: &str,
) -> Option<DownloadTicket> {
    let mut tickets = tickets.write().ok()?;
    let now = Instant::now();
    tickets.retain(|_, ticket| ticket.expires_at > now);
    tickets.get(token).cloned()
}

async fn web_file_ticket(
    State(state): State<WebState>,
    headers: HeaderMap,
    body: Bytes,
) -> HttpResponse {
    if !basic_auth_allowed(&headers, &state.auth) {
        return basic_auth_required();
    }
    if !web_headers_allowed(&headers, state.port, true, &state.public_origins) {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    if body.len() as u64 > MAX_REQUEST_BYTES {
        return (StatusCode::PAYLOAD_TOO_LARGE, "request is too large").into_response();
    }
    let request = match serde_json::from_slice::<FileTicketRequest>(&body) {
        Ok(request) => request,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid request").into_response(),
    };
    let store = Arc::clone(&state.bridge.session_store);
    let resolved = tokio::task::spawn_blocking(move || {
        let thread = store
            .find_thread(&request.thread_id)
            .map_err(|_| FileDownloadFailure::ThreadNotFound)?
            .ok_or(FileDownloadFailure::ThreadNotFound)?;
        let (path, _, _) = resolve_workspace_download(&thread.cwd, &request.path)?;
        Ok::<_, FileDownloadFailure>((thread.cwd, path))
    })
    .await;
    let (workspace, path) = match resolved {
        Ok(Ok(resolved)) => resolved,
        Ok(Err(FileDownloadFailure::OutsideWorkspace)) => {
            return (
                StatusCode::FORBIDDEN,
                "file is outside the session workspace",
            )
                .into_response()
        }
        Ok(Err(FileDownloadFailure::TooLarge)) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                "file must be smaller than 16 MiB",
            )
                .into_response()
        }
        Ok(Err(_)) | Err(_) => return (StatusCode::NOT_FOUND, "file not found").into_response(),
    };
    let mut random = [0_u8; 32];
    if getrandom::fill(&mut random).is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to create download ticket",
        )
            .into_response();
    }
    let token = URL_SAFE_NO_PAD.encode(random);
    let Ok(mut tickets) = state.download_tickets.write() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "download ticket store is unavailable",
        )
            .into_response();
    };
    let now = Instant::now();
    tickets.retain(|_, ticket| ticket.expires_at > now);
    if tickets.len() >= MAX_DOWNLOAD_TICKETS {
        if let Some(oldest) = tickets
            .iter()
            .min_by_key(|(_, ticket)| ticket.expires_at)
            .map(|(token, _)| token.clone())
        {
            tickets.remove(&oldest);
        }
    }
    tickets.insert(
        token.clone(),
        DownloadTicket {
            workspace,
            path,
            expires_at: now + DOWNLOAD_TICKET_TTL,
        },
    );
    let mut response = Json(json!({
        "url": format!("/api/file?ticket={token}"),
        "expires_in_seconds": DOWNLOAD_TICKET_TTL.as_secs(),
    }))
    .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

fn read_workspace_download(
    workspace: &Path,
    requested: &str,
) -> std::result::Result<(Vec<u8>, String), FileDownloadFailure> {
    let (candidate, filename, _) = resolve_workspace_download(workspace, requested)?;
    let bytes = fs::read(&candidate).map_err(|_| FileDownloadFailure::ReadFailed)?;
    if bytes.len() as u64 >= MAX_DOWNLOAD_BYTES {
        return Err(FileDownloadFailure::TooLarge);
    }
    Ok((bytes, filename))
}

fn resolve_workspace_download(
    workspace: &Path,
    requested: &str,
) -> std::result::Result<(PathBuf, String, u64), FileDownloadFailure> {
    if requested.is_empty() {
        return Err(FileDownloadFailure::FileNotFound);
    }
    let workspace = fs::canonicalize(workspace).map_err(|_| FileDownloadFailure::FileNotFound)?;
    let requested = PathBuf::from(requested);
    let candidate = if requested.is_absolute() {
        requested
    } else {
        workspace.join(requested)
    };
    let candidate = fs::canonicalize(candidate).map_err(|_| FileDownloadFailure::FileNotFound)?;
    if !candidate.starts_with(&workspace) {
        return Err(FileDownloadFailure::OutsideWorkspace);
    }
    let metadata = fs::metadata(&candidate).map_err(|_| FileDownloadFailure::FileNotFound)?;
    if !metadata.is_file() {
        return Err(FileDownloadFailure::NotAFile);
    }
    if metadata.len() >= MAX_DOWNLOAD_BYTES {
        return Err(FileDownloadFailure::TooLarge);
    }
    let filename = candidate
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download")
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    Ok((candidate, filename, metadata.len()))
}

fn web_asset_response(
    state: &WebState,
    headers: &HeaderMap,
    body: &'static str,
    content_type: &'static str,
) -> HttpResponse {
    if !basic_auth_allowed(headers, &state.auth) {
        return basic_auth_required();
    }
    if !web_headers_allowed(headers, state.port, false, &state.public_origins) {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    let mut response = body.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
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
    if !web_headers_allowed(&headers, state.port, true, &state.public_origins) {
        return (
            StatusCode::FORBIDDEN,
            Json(Response::error(
                "forbidden",
                "Web UI request Host and Origin are not allowed",
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
            &bridge.app_server_tools,
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

async fn web_events(
    State(state): State<WebState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> HttpResponse {
    if !basic_auth_allowed(&headers, &state.auth) {
        return basic_auth_required();
    }
    if !web_headers_allowed(&headers, state.port, true, &state.public_origins) {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    let backend = Arc::clone(&state.bridge.write_backend);
    upgrade
        .on_upgrade(move |socket| web_event_socket(socket, backend))
        .into_response()
}

async fn web_event_socket(socket: WebSocket, backend: Arc<CodexCliBackend>) {
    let (mut sender, mut receiver) = socket.split();
    let Some(mut events) = backend.subscribe_app_server_events() else {
        let _ = sender
            .send(AxumWsMessage::Text(
                json!({
                    "type": "bridge_app_server_connection",
                    "status": "unavailable"
                })
                .to_string()
                .into(),
            ))
            .await;
        return;
    };
    if sender
        .send(AxumWsMessage::Text(
            json!({"type": "bridge_event_stream", "status": "ready"})
                .to_string()
                .into(),
        ))
        .await
        .is_err()
    {
        return;
    }
    let snapshot_backend = Arc::clone(&backend);
    if let Ok(Ok(active_thread_ids)) =
        tokio::task::spawn_blocking(move || active_loaded_thread_ids(&snapshot_backend)).await
    {
        if sender
            .send(AxumWsMessage::Text(
                json!({
                    "type": "bridge_thread_activity_snapshot",
                    "active_thread_ids": active_thread_ids,
                })
                .to_string()
                .into(),
            ))
            .await
            .is_err()
        {
            return;
        }
    }
    loop {
        tokio::select! {
            event = events.recv() => {
                let event = match event {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        json!({"type": "bridge_event_gap", "skipped": skipped})
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if sender.send(AxumWsMessage::Text(event.to_string().into())).await.is_err() {
                    break;
                }
            }
            message = receiver.next() => {
                match message {
                    Some(Ok(AxumWsMessage::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

fn active_loaded_thread_ids(
    backend: &CodexCliBackend,
) -> std::result::Result<Vec<String>, BackendFailure> {
    let loaded = loaded_thread_ids(&backend.app_server_rpc("thread/loaded/list", json!({}))?);
    let mut active = Vec::new();
    for thread_id in loaded {
        let thread = backend.app_server_rpc(
            "thread/read",
            json!({"threadId": thread_id, "includeTurns": false}),
        )?;
        if thread_result_is_active(&thread) {
            active.push(thread_id);
        }
    }
    Ok(active)
}

fn web_headers_allowed(
    headers: &HeaderMap,
    port: u16,
    require_origin: bool,
    public_origins: &[WebOrigin],
) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    if !web_authority_allowed(host, port)
        && !public_origins
            .iter()
            .any(|origin| origin.authority.eq_ignore_ascii_case(host))
    {
        return false;
    }

    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    if require_origin
        && origin.is_some_and(|origin| !web_origin_allowed(origin, port, public_origins))
    {
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

fn web_origin_allowed(origin: &str, port: u16, public_origins: &[WebOrigin]) -> bool {
    let Ok(uri) = origin.parse::<axum::http::Uri>() else {
        return false;
    };
    let local = uri.scheme_str() == Some("http")
        && uri
            .authority()
            .is_some_and(|authority| web_authority_allowed(authority.as_str(), port));
    local
        || public_origins.iter().any(|allowed| {
            uri.scheme_str()
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case(&allowed.scheme))
                && uri.authority().is_some_and(|authority| {
                    authority.as_str().eq_ignore_ascii_case(&allowed.authority)
                })
        })
}

fn parse_public_web_origin(value: &str) -> Result<WebOrigin> {
    let uri = value
        .parse::<axum::http::Uri>()
        .with_context(|| format!("invalid --web-ui-public-origin {value:?}"))?;
    let scheme = uri
        .scheme_str()
        .filter(|scheme| scheme.eq_ignore_ascii_case("https"))
        .context("--web-ui-public-origin must use https")?;
    let authority = uri
        .authority()
        .context("--web-ui-public-origin must include a hostname")?;
    if uri
        .path_and_query()
        .is_some_and(|path| path.as_str() != "/")
    {
        bail!("--web-ui-public-origin must not include a path, query, or fragment");
    }
    Ok(WebOrigin {
        scheme: scheme.to_ascii_lowercase(),
        authority: authority.as_str().to_ascii_lowercase(),
    })
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

fn weekly_usage(rate_limits: &Value) -> Option<Value> {
    let snapshot = rate_limits.get("rateLimits")?;
    let window = [snapshot.get("primary"), snapshot.get("secondary")]
        .into_iter()
        .flatten()
        .find(|window| {
            window.get("windowDurationMins").and_then(Value::as_i64) == Some(7 * 24 * 60)
        })?;
    let used_percent = window.get("usedPercent")?.as_i64()?.clamp(0, 100);
    Some(json!({
        "remaining_percent": 100 - used_percent,
        "used_percent": used_percent,
        "resets_at": window.get("resetsAt").and_then(Value::as_i64),
        "window_duration_mins": 7 * 24 * 60,
    }))
}

fn composer_model_options(response: &Value) -> Vec<Value> {
    response
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model.get("model").and_then(Value::as_str)?;
            let efforts = model
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|effort| {
                    Some(json!({
                        "id": effort.get("reasoningEffort")?.as_str()?,
                        "description": effort.get("description").and_then(Value::as_str),
                    }))
                })
                .collect::<Vec<_>>();
            (!efforts.is_empty()).then(|| {
                json!({
                    "id": id,
                    "name": model.get("displayName").and_then(Value::as_str).unwrap_or(id),
                    "description": model.get("description").and_then(Value::as_str),
                    "default_effort": model.get("defaultReasoningEffort").and_then(Value::as_str),
                    "efforts": efforts,
                })
            })
        })
        .collect()
}

fn model_supports(options: &[Value], model: &str, effort: &str) -> bool {
    options.iter().any(|option| {
        option.get("id").and_then(Value::as_str) == Some(model)
            && option
                .get("efforts")
                .and_then(Value::as_array)
                .is_some_and(|efforts| {
                    efforts
                        .iter()
                        .any(|item| item.get("id").and_then(Value::as_str) == Some(effort))
                })
    })
}

fn composer_config_settings(response: &Value) -> (Option<String>, Option<String>) {
    let config = response.get("config").unwrap_or(response);
    (
        config
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned),
        config
            .get("model_reasoning_effort")
            .and_then(Value::as_str)
            .map(str::to_owned),
    )
}

#[derive(Debug)]
struct PreparedThreadCwd {
    cwd: PathBuf,
    worktree: Option<CreatedWorktree>,
}

#[derive(Debug)]
struct CreatedWorktree {
    repository: PathBuf,
    root: PathBuf,
    allocation_dir: PathBuf,
}

#[derive(Debug)]
struct ThreadCreateFailure {
    code: &'static str,
    message: String,
}

fn prepare_thread_cwd(
    project_path: &Path,
    worktree: bool,
    bridge_home: &Path,
) -> Result<PreparedThreadCwd, ThreadCreateFailure> {
    if !project_path.is_absolute() {
        return Err(ThreadCreateFailure {
            code: "invalid_project_path",
            message: "project path must be absolute".to_owned(),
        });
    }
    let project_path = fs::canonicalize(project_path).map_err(|error| ThreadCreateFailure {
        code: "invalid_project_path",
        message: format!("cannot open project {}: {error}", project_path.display()),
    })?;
    if !project_path.is_dir() {
        return Err(ThreadCreateFailure {
            code: "invalid_project_path",
            message: format!(
                "project path is not a directory: {}",
                project_path.display()
            ),
        });
    }
    if !worktree {
        return Ok(PreparedThreadCwd {
            cwd: project_path,
            worktree: None,
        });
    }

    let repository = git_output(&project_path, &["rev-parse", "--show-toplevel"])?;
    let repository = fs::canonicalize(repository.trim()).map_err(|error| ThreadCreateFailure {
        code: "worktree_create_failed",
        message: format!("cannot resolve the Git repository root: {error}"),
    })?;
    let project_name = repository
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ThreadCreateFailure {
            code: "worktree_create_failed",
            message: "cannot derive a worktree name from the repository root".to_owned(),
        })?;
    let sequence = NEXT_WORKTREE_ID.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let worktrees_dir = bridge_home.join("worktrees");
    fs::create_dir_all(&worktrees_dir).map_err(|error| ThreadCreateFailure {
        code: "worktree_create_failed",
        message: format!(
            "cannot create bridge worktree directory {}: {error}",
            worktrees_dir.display()
        ),
    })?;
    let allocation_dir =
        worktrees_dir.join(format!("{timestamp:x}-{}-{sequence}", std::process::id()));
    let root = allocation_dir.join(project_name);
    fs::create_dir(&allocation_dir).map_err(|error| ThreadCreateFailure {
        code: "worktree_create_failed",
        message: format!(
            "cannot create worktree allocation {}: {error}",
            allocation_dir.display()
        ),
    })?;

    let output = Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&root)
        .arg("HEAD")
        .current_dir(&repository)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            let _ = fs::remove_dir_all(&allocation_dir);
            return Err(ThreadCreateFailure {
                code: "worktree_create_failed",
                message: format!("failed to run git worktree add: {error}"),
            });
        }
    };
    if !output.status.success() {
        let _ = fs::remove_dir_all(&allocation_dir);
        return Err(ThreadCreateFailure {
            code: "worktree_create_failed",
            message: command_output_message("git worktree add", &output),
        });
    }

    Ok(PreparedThreadCwd {
        cwd: root.clone(),
        worktree: Some(CreatedWorktree {
            repository,
            root,
            allocation_dir,
        }),
    })
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<String, ThreadCreateFailure> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| ThreadCreateFailure {
            code: "worktree_create_failed",
            message: format!("failed to run git {}: {error}", args.join(" ")),
        })?;
    if !output.status.success() {
        return Err(ThreadCreateFailure {
            code: "worktree_create_failed",
            message: command_output_message(&format!("git {}", args.join(" ")), &output),
        });
    }
    String::from_utf8(output.stdout).map_err(|error| ThreadCreateFailure {
        code: "worktree_create_failed",
        message: format!("git returned a non-UTF-8 repository path: {error}"),
    })
}

fn command_output_message(label: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    if detail.is_empty() {
        format!("{label} exited with {}", output.status)
    } else {
        format!("{label} failed: {detail}")
    }
}

fn remove_created_worktree(worktree: &CreatedWorktree) -> Option<String> {
    let output = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&worktree.root)
        .current_dir(&worktree.repository)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    let failure = match output {
        Ok(output) if output.status.success() => None,
        Ok(output) => Some(command_output_message("git worktree remove", &output)),
        Err(error) => Some(format!("failed to run git worktree remove: {error}")),
    };
    if failure.is_none() {
        let _ = fs::remove_dir(&worktree.allocation_dir);
    }
    failure
}

fn thread_summary_from_start(result: &Value, cwd: &Path) -> Option<Value> {
    let thread = result.get("thread")?;
    let id = thread.get("id")?.as_str()?;
    let title = thread
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| {
            thread
                .get("preview")
                .and_then(Value::as_str)
                .filter(|preview| !preview.is_empty())
        });
    let updated_at_ms = thread
        .get("updatedAt")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_mul(1_000);
    Some(json!({
        "id": id,
        "title": title,
        "cwd": thread
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or_else(|| cwd.to_str().unwrap_or("")),
        "git_branch": Value::Null,
        "updated_at_ms": updated_at_ms,
        "archived": false,
    }))
}

fn project_id_for_path(result: &Value, project_path: &Path) -> Option<String> {
    result.get("data")?.as_array()?.iter().find_map(|project| {
        let matches = project
            .get("roots")?
            .as_array()?
            .iter()
            .filter_map(|root| root.get("path").and_then(Value::as_str))
            .any(|root| fs::canonicalize(root).is_ok_and(|root| root == project_path));
        matches
            .then(|| project.get("id")?.as_str().map(str::to_owned))
            .flatten()
    })
}

fn pinned_thread_ids(write_backend: &CodexCliBackend) -> Result<Vec<String>, BackendFailure> {
    let mut ids = Vec::new();
    let mut cursor = None::<String>;
    let mut seen_cursors = HashSet::new();
    loop {
        let result = write_backend.app_server_rpc(
            "thread/list",
            json!({
                "cursor": cursor,
                "limit": 100,
                "modelProviders": [],
                "sectionId": PINNED_THREAD_SECTION_ID,
                "sortKey": "section_position",
                "sortDirection": "asc",
                "useStateDbOnly": true,
            }),
        )?;
        let data = result
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| BackendFailure {
                code: "app_server_protocol_error",
                message: "thread/list returned no pinned thread data".to_owned(),
            })?;
        ids.extend(
            data.iter()
                .filter_map(|thread| thread.get("id").and_then(Value::as_str).map(str::to_owned)),
        );
        let Some(next) = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            break;
        };
        if !seen_cursors.insert(next.clone()) {
            return Err(BackendFailure {
                code: "app_server_protocol_error",
                message: "thread/list repeated its pinned-thread cursor".to_owned(),
            });
        }
        cursor = Some(next);
    }
    Ok(ids)
}

fn typed_thread_tool(item: &Value) -> Option<ThreadToolCall> {
    let item_type = item.get("type")?.as_str()?;
    let name = match item_type {
        "commandExecution" => "exec_command".to_owned(),
        "fileChange" => "apply_patch".to_owned(),
        "mcpToolCall" => match (
            item.get("server").and_then(Value::as_str),
            item.get("tool").and_then(Value::as_str),
        ) {
            (Some(server), Some(tool)) => format!("{server}.{tool}"),
            (_, Some(tool)) => tool.to_owned(),
            _ => "mcp_tool".to_owned(),
        },
        "dynamicToolCall" => match (
            item.get("namespace").and_then(Value::as_str),
            item.get("tool").and_then(Value::as_str),
        ) {
            (Some(namespace), Some(tool)) => format!("{namespace}.{tool}"),
            (_, Some(tool)) => tool.to_owned(),
            _ => "dynamic_tool".to_owned(),
        },
        "webSearch" => "web_search".to_owned(),
        "imageView" => "view_image".to_owned(),
        "collabAgentToolCall" => item
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("collab_agent")
            .to_owned(),
        _ => return None,
    };
    let call_id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(item_type)
        .to_owned();
    let status = item
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
        .to_owned();
    let mut input = item.as_object()?.clone();
    let mut output = serde_json::Map::new();
    input.remove("id");
    input.remove("status");
    for field in [
        "aggregatedOutput",
        "exitCode",
        "durationMs",
        "result",
        "error",
        "contentItems",
        "success",
    ] {
        if let Some(value) = input.remove(field) {
            if !value.is_null() {
                output.insert(field.to_owned(), value);
            }
        }
    }
    Some(ThreadToolCall {
        call_id,
        name,
        status,
        input: Value::Object(input),
        output: (!output.is_empty()).then_some(Value::Object(output)),
    })
}

fn app_server_tools_for_messages(
    write_backend: &CodexCliBackend,
    cache: &AppServerToolCache,
    thread_id: &str,
    messages: &[ThreadMessage],
) -> Result<HashMap<String, Vec<ThreadToolCall>>, BackendFailure> {
    let wanted = messages
        .iter()
        .filter(|message| message.role == "assistant")
        .filter_map(|message| message.id.clone())
        .collect::<HashSet<_>>();
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    if let Ok(cache) = cache.threads.read() {
        if let Some(entry) = cache.get(thread_id).filter(|entry| {
            entry.refreshed_at.elapsed() <= Duration::from_secs(2)
                && wanted.is_subset(&entry.known_message_ids)
        }) {
            return Ok(wanted
                .iter()
                .filter_map(|id| {
                    entry
                        .tools
                        .get(id)
                        .cloned()
                        .map(|tools| (id.clone(), tools))
                })
                .collect());
        }
    }

    let mut tools = HashMap::<String, Vec<ThreadToolCall>>::new();
    let mut found = HashSet::<String>::new();
    let mut cursor = None::<String>;
    let mut seen_cursors = HashSet::new();
    let mut pending_tools = Vec::<ThreadToolCall>::new();
    let mut current_turn = None::<String>;
    loop {
        let result = write_backend.app_server_rpc(
            "thread/items/list",
            json!({
                "threadId": thread_id,
                "cursor": cursor,
                "limit": 100,
                "sortDirection": "desc",
            }),
        )?;
        let entries = result
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| BackendFailure {
                code: "app_server_protocol_error",
                message: "thread/items/list returned no item data".to_owned(),
            })?;
        for entry in entries {
            let turn_id = entry.get("turnId").and_then(Value::as_str);
            if current_turn
                .as_deref()
                .is_some_and(|current| Some(current) != turn_id)
            {
                pending_tools.clear();
            }
            current_turn = turn_id.map(str::to_owned);
            let Some(item) = entry.get("item") else {
                continue;
            };
            if item.get("type").and_then(Value::as_str) == Some("agentMessage") {
                if let Some(id) = item.get("id").and_then(Value::as_str) {
                    if wanted.contains(id) {
                        pending_tools.reverse();
                        tools.insert(id.to_owned(), std::mem::take(&mut pending_tools));
                        found.insert(id.to_owned());
                    } else {
                        pending_tools.clear();
                    }
                }
                continue;
            }
            if let Some(tool) = typed_thread_tool(item) {
                pending_tools.push(tool);
            }
        }
        if found.len() == wanted.len() {
            break;
        }
        let Some(next) = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            break;
        };
        if !seen_cursors.insert(next.clone()) {
            return Err(BackendFailure {
                code: "app_server_protocol_error",
                message: "thread/items/list repeated its cursor".to_owned(),
            });
        }
        cursor = Some(next);
    }
    if let Ok(mut cache) = cache.threads.write() {
        let entry = cache
            .entry(thread_id.to_owned())
            .or_insert_with(|| CachedAppServerTools {
                refreshed_at: Instant::now(),
                known_message_ids: HashSet::new(),
                tools: HashMap::new(),
            });
        entry.refreshed_at = Instant::now();
        entry.known_message_ids.extend(wanted.iter().cloned());
        entry.tools.extend(tools.clone());
    }
    Ok(tools)
}

fn overlay_app_server_tools(
    messages: &mut [ThreadMessage],
    tools: &HashMap<String, Vec<ThreadToolCall>>,
) -> usize {
    let mut replaced = 0;
    for message in messages {
        let Some(id) = message.id.as_ref() else {
            continue;
        };
        let Some(typed) = tools.get(id) else {
            continue;
        };
        message.tools.clone_from(typed);
        replaced += 1;
    }
    replaced
}

fn dispatch(
    request: Request,
    socket_path: &Path,
    session_store: &SessionStore,
    write_backend: &CodexCliBackend,
    host_executor: &HostExecutor,
    selected_thread: &RwLock<Option<String>>,
    pending_messages: &PendingMessages,
    app_server_tools: &AppServerToolCache,
) -> Response {
    match request {
        Request::Status => {
            if write_backend.app_server_socket().is_some() {
                let _ = write_backend.app_server_rpc("thread/loaded/list", json!({}));
            }
            let runtime = write_backend.app_server_runtime_info();
            let runtime_kind = if write_backend.program() == Path::new(DESKTOP_CODEX_PATH) {
                "bundled"
            } else {
                "standalone"
            };
            let app_server_mode = if write_backend.app_server_socket().is_some() {
                runtime_kind
            } else if runtime_kind == "bundled" {
                "desktop_bundled_only"
            } else {
                "standalone_unconfigured"
            };
            let runtime_version = runtime
                .as_ref()
                .and_then(|info| info.user_agent.as_deref())
                .and_then(app_server_version_from_user_agent)
                .map(|version| format!("{version}-{runtime_kind}"));
            Response::success(json!({
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
                "app_server_available": write_backend.app_server_socket().is_some_and(|socket|
                    fs::metadata(socket).is_ok_and(|metadata| metadata.file_type().is_socket())
                ),
                "app_server_mode": app_server_mode,
                "standalone_fallback": false,
                "schema_version": APP_SERVER_SCHEMA_VERSION,
                "runtime_kind": runtime_kind,
                "runtime_version": runtime_version,
                "runtime_user_agent": runtime.and_then(|info| info.user_agent),
            },
            "host_executor": host_executor.summary(),
            }))
        }
        Request::PendingMessages { thread_id } => {
            let (queued, queue_backend) =
                queued_messages_for_thread(session_store, write_backend, thread_id.as_deref());
            Response::success(json!({
                "messages": pending_messages.reconcile(session_store, queued, thread_id.as_deref()),
                "queue_backend": queue_backend,
            }))
        }
        Request::PendingMessageDelete { id, thread_id } => {
            let (queued, _) =
                queued_messages_for_thread(session_store, write_backend, Some(&thread_id));
            let entry = pending_messages
                .reconcile(session_store, queued, Some(&thread_id))
                .into_iter()
                .find(|entry| entry.id == id && entry.thread_id == thread_id);
            let Some(entry) = entry else {
                return Response::error(
                    "pending_message_not_found",
                    "the pending message no longer exists",
                );
            };
            if matches!(entry.status.as_str(), "queueing" | "steering") {
                return Response::error(
                    "pending_message_busy",
                    "wait for the pending operation to finish before deleting it",
                );
            }
            let queue_deleted = if let Some(queued_submission_id) = &entry.queued_submission_id {
                let result = match write_backend.app_server_rpc(
                    "thread/queue/delete",
                    json!({
                        "threadId": entry.thread_id,
                        "queuedSubmissionId": queued_submission_id,
                    }),
                ) {
                    Ok(result) => result,
                    Err(error) => return write_backend_error(error),
                };
                if result.get("deleted").and_then(Value::as_bool) != Some(true) {
                    return Response::error(
                        "pending_message_not_deleted",
                        "the queued message has already left the app-server queue",
                    );
                }
                true
            } else {
                false
            };
            pending_messages.dismiss(&entry.id, &entry.thread_id);
            Response::success(json!({
                "action": "pending_message_delete",
                "deleted": true,
                "queue_deleted": queue_deleted,
                "thread_id": entry.thread_id,
                "text": entry.text,
                "message_action": entry.action,
            }))
        }
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
            let pinned = if write_backend.app_server_socket().is_some_and(|socket| {
                fs::metadata(socket).is_ok_and(|metadata| metadata.file_type().is_socket())
            }) {
                pinned_thread_ids(write_backend).unwrap_or_default()
            } else {
                Vec::new()
            };
            match session_store.list_project_threads(
                &project_path,
                include_archived,
                offset as usize,
                limit as usize,
                &pinned,
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
                Ok(Some((thread, mut page))) => {
                    let tool_source = match app_server_tools_for_messages(
                        write_backend,
                        app_server_tools,
                        &thread_id,
                        &page.messages,
                    ) {
                        Ok(tools) if overlay_app_server_tools(&mut page.messages, &tools) > 0 => {
                            "app_server_items"
                        }
                        _ => "rollout_jsonl",
                    };
                    let messages = page
                        .messages
                        .iter()
                        .enumerate()
                        .map(|(offset, message)| compact_web_message(message, page.start + offset))
                        .collect::<Vec<_>>();
                    Response::success(json!({
                        "source": "rollout_jsonl",
                        "tool_source": tool_source,
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
        } => match session_store.read_message(&thread_id, message_index as usize) {
            Ok(Some(message)) => {
                let typed_tools = app_server_tools_for_messages(
                    write_backend,
                    app_server_tools,
                    &thread_id,
                    std::slice::from_ref(&message),
                )
                .ok();
                let typed_tool = message.id.as_ref().and_then(|id| {
                    typed_tools
                        .as_ref()
                        .and_then(|tools| tools.get(id))
                        .and_then(|tools| tools.get(tool_index as usize))
                        .cloned()
                });
                let tool = typed_tool.or_else(|| message.tools.get(tool_index as usize).cloned());
                let Some(tool) = tool else {
                    return Response::error(
                        "tool_content_not_found",
                        "the requested tool call was not found",
                    );
                };
                Response::success(json!({
                    "thread_id": thread_id,
                    "message_index": message_index,
                    "tool_index": tool_index,
                    "display_input": tool.input.clone(),
                    "tool": tool,
                }))
            }
            Ok(None) => Response::error(
                "tool_content_not_found",
                "the requested tool call was not found",
            ),
            Err(error) => backend_error(error),
        },
        Request::ThreadActivity { thread_id } => match session_store.thread_activity(&thread_id) {
            Ok(Some(mut activity)) => {
                if activity.phase.as_deref() == Some("model") {
                    if let Some(turn_id) = activity.active_turn_id.as_deref() {
                        if write_backend
                            .latest_item_type(&thread_id, turn_id)
                            .is_ok_and(|item_type| {
                                item_type.as_deref() == Some("contextCompaction")
                            })
                        {
                            activity.phase = Some("compacting".to_owned());
                            activity.active_tool = None;
                        }
                    }
                }
                Response::success(json!({
                    "thread_id": thread_id,
                    "activity": activity,
                }))
            }
            Ok(None) => Response::error(
                "thread_not_found",
                format!("thread {thread_id} was not found in the rollout store"),
            ),
            Err(error) => backend_error(error),
        },
        Request::ThreadWatch { thread_id } => match write_backend.watch_thread(&thread_id) {
            Ok(result) => Response::success(json!({
                "thread_id": thread_id,
                "subscribed": true,
                "thread": {
                    "id": result.pointer("/thread/id").and_then(Value::as_str),
                    "status": result.pointer("/thread/status").cloned(),
                },
            })),
            Err(error) => write_backend_error(error),
        },
        Request::ComposerStatus { thread_id } => {
            let thread = write_backend
                .app_server_rpc(
                    "thread/read",
                    json!({"threadId": thread_id, "includeTurns": false}),
                )
                .ok();
            let app_model = thread.as_ref().and_then(|thread| {
                thread
                    .pointer("/thread/model")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            let app_effort = thread.as_ref().and_then(|thread| {
                thread
                    .pointer("/thread/reasoningEffort")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            let (rollout_model, rollout_effort) = session_store
                .composer_settings(&thread_id)
                .unwrap_or_default();
            let resolved_model = app_model.or(rollout_model);
            let resolved_effort = app_effort.or(rollout_effort);
            let (config_model, config_effort) =
                if resolved_model.is_none() || resolved_effort.is_none() {
                    write_backend
                        .app_server_rpc("config/read", json!({}))
                        .ok()
                        .map(|config| composer_config_settings(&config))
                        .unwrap_or_default()
                } else {
                    (None, None)
                };
            let (weekly_usage, weekly_usage_error) =
                match write_backend.app_server_rpc("account/rateLimits/read", json!({})) {
                    Ok(rate_limits) => match weekly_usage(&rate_limits) {
                        Some(usage) => (Some(usage), None),
                        None => (
                            None,
                            Some(json!({
                                "code": "weekly_usage_unavailable",
                                "message": "app-server response did not include seven-day usage",
                            })),
                        ),
                    },
                    Err(error) => (
                        None,
                        Some(json!({
                            "code": error.code,
                            "message": error.message,
                        })),
                    ),
                };
            Response::success(json!({
                "thread_id": thread_id,
                "model": resolved_model.or(config_model),
                "reasoning_effort": resolved_effort.or(config_effort),
                "weekly_usage": weekly_usage,
                "weekly_usage_error": weekly_usage_error,
            }))
        }
        Request::ComposerOptions => {
            match write_backend
                .app_server_rpc("model/list", json!({"limit": 100, "includeHidden": false}))
            {
                Ok(result) => Response::success(json!({
                    "models": composer_model_options(&result),
                })),
                Err(error) => write_backend_error(error),
            }
        }
        Request::ThreadCreate {
            project_path,
            worktree,
            model,
        } => {
            if model
                .as_ref()
                .is_some_and(|model| model.is_empty() || model.len() > 128)
            {
                return Response::error(
                    "invalid_request",
                    "model must be omitted or contain between 1 and 128 bytes",
                );
            }
            let canonical_project = match fs::canonicalize(&project_path) {
                Ok(path) => path,
                Err(error) => {
                    return Response::error(
                        "invalid_project_path",
                        format!("cannot open project {}: {error}", project_path.display()),
                    )
                }
            };
            let known_project = match session_store.list_projects(true) {
                Ok(projects) => projects.into_iter().any(|project| {
                    fs::canonicalize(project.path).is_ok_and(|path| path == canonical_project)
                }),
                Err(error) => return backend_error(error),
            };
            if !known_project {
                return Response::error(
                    "unknown_project",
                    "new threads can only be created for a project already listed by the bridge",
                );
            }

            let bridge_home = socket_path.parent().unwrap_or_else(|| session_store.home());
            let prepared = match prepare_thread_cwd(&canonical_project, worktree, bridge_home) {
                Ok(prepared) => prepared,
                Err(error) => return Response::error(error.code, error.message),
            };
            let mut params = json!({"cwd": prepared.cwd});
            if let Some(model) = model {
                params["model"] = Value::String(model);
            }
            if let Ok(projects) = write_backend.app_server_rpc(
                "project/list",
                json!({"limit": 100, "sortKey": "recencyAt", "sortDirection": "desc"}),
            ) {
                if let Some(project_id) = project_id_for_path(&projects, &canonical_project) {
                    params["projectId"] = Value::String(project_id);
                }
            }
            let result = match write_backend.app_server_rpc("thread/start", params) {
                Ok(result) => result,
                Err(error) => {
                    let rollback_is_safe =
                        matches!(error.code, "app_server_unavailable" | "app_server_rejected");
                    let message = match (&prepared.worktree, rollback_is_safe) {
                        (Some(created), true) => match remove_created_worktree(created) {
                            Some(cleanup) => format!(
                                "{}; worktree rollback also failed: {cleanup}",
                                error.message
                            ),
                            None => error.message,
                        },
                        (Some(created), false) => format!(
                            "{}; thread creation outcome is ambiguous, so the new worktree was retained at {}",
                            error.message,
                            created.root.display()
                        ),
                        (None, _) => error.message,
                    };
                    return Response::error(error.code, message);
                }
            };
            let Some(fallback_thread) = thread_summary_from_start(&result, &prepared.cwd) else {
                let message = prepared.worktree.as_ref().map_or_else(
                    || "thread/start returned no valid thread".to_owned(),
                    |created| {
                        format!(
                            "thread/start returned no valid thread; the new worktree was retained at {} because the creation outcome is ambiguous",
                            created.root.display()
                        )
                    },
                );
                return Response::error("app_server_protocol_error", message);
            };
            let thread_id = fallback_thread["id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if let Ok(mut selected) = selected_thread.write() {
                *selected = Some(thread_id.clone());
            }

            let mut stored_thread = None;
            for attempt in 0..5 {
                session_store.invalidate_summary_cache();
                if let Ok(Some(thread)) = session_store.find_thread(&thread_id) {
                    stored_thread = serde_json::to_value(thread).ok();
                    break;
                }
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(40));
                }
            }
            Response::success(json!({
                "action": "thread_create",
                "project_path": canonical_project,
                "location": if worktree { "worktree" } else { "current_directory" },
                "worktree_path": prepared.worktree.as_ref().map(|created| &created.root),
                "thread": stored_thread.unwrap_or(fallback_thread),
            }))
        }
        Request::ThreadSettingsUpdate {
            thread_id,
            model,
            effort,
        } => {
            if model.is_empty() || model.len() > 128 || effort.is_empty() || effort.len() > 32 {
                return Response::error(
                    "invalid_request",
                    "model and effort must be non-empty and reasonably sized",
                );
            }
            let available = match write_backend
                .app_server_rpc("model/list", json!({"limit": 100, "includeHidden": false}))
            {
                Ok(result) => composer_model_options(&result),
                Err(error) => return write_backend_error(error),
            };
            if !model_supports(&available, &model, &effort) {
                return Response::error(
                    "invalid_thread_settings",
                    "the selected model and reasoning effort are not currently available",
                );
            }
            match write_backend.app_server_rpc(
                "thread/settings/update",
                json!({"threadId": thread_id, "model": model, "effort": effort}),
            ) {
                Ok(_) => Response::success(json!({
                    "action": "thread_settings_update",
                    "thread_id": thread_id,
                    "model": model,
                    "reasoning_effort": effort,
                })),
                Err(error) => write_backend_error(error),
            }
        }
        Request::ThreadRename { thread_id, name } => {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 200 {
                return Response::error(
                    "invalid_thread_name",
                    "thread name must contain between 1 and 200 characters",
                );
            }
            let resolved =
                match resolve_write_target(Some(thread_id), session_store, selected_thread) {
                    Ok(resolved) => resolved,
                    Err(response) => return response,
                };
            match write_backend.app_server_rpc(
                "thread/name/set",
                json!({"threadId": resolved.thread.id, "name": name}),
            ) {
                Ok(_) => {
                    session_store.invalidate_summary_cache();
                    Response::success(json!({
                        "action": "thread_rename",
                        "status": "renamed",
                        "thread_id": resolved.thread.id,
                        "name": name,
                        "backend": "app_server_thread_name_set",
                    }))
                }
                Err(error) => write_backend_error(error),
            }
        }
        Request::ThreadArchive { thread_id } => {
            let resolved =
                match resolve_write_target(Some(thread_id), session_store, selected_thread) {
                    Ok(resolved) => resolved,
                    Err(response) => return response,
                };
            match write_backend
                .app_server_rpc("thread/archive", json!({"threadId": resolved.thread.id}))
            {
                Ok(_) => {
                    session_store.invalidate_summary_cache();
                    if let Ok(mut selected) = selected_thread.write() {
                        if selected.as_deref() == Some(resolved.thread.id.as_str()) {
                            *selected = None;
                        }
                    }
                    Response::success(json!({
                        "action": "thread_archive",
                        "status": "archived",
                        "thread_id": resolved.thread.id,
                        "backend": "app_server_thread_archive",
                    }))
                }
                Err(error) => write_backend_error(error),
            }
        }
        Request::ThreadPins => match pinned_thread_ids(write_backend) {
            Ok(thread_ids) => {
                let mut threads = Vec::new();
                for thread_id in &thread_ids {
                    if let Ok(Some(thread)) = session_store.find_thread(thread_id) {
                        threads.push(json!({
                            "id": thread.id,
                            "title": thread.title,
                            "cwd": thread.cwd,
                            "git_branch": thread.git_branch,
                            "created_at": thread.created_at,
                            "updated_at_ms": thread.updated_at_ms,
                            "source": thread.source,
                            "archived": thread.archived,
                            "pinned": true,
                        }));
                    }
                }
                Response::success(json!({
                    "available": true,
                    "section_id": PINNED_THREAD_SECTION_ID,
                    "thread_ids": thread_ids,
                    "threads": threads,
                }))
            }
            Err(error) => write_backend_error(error),
        },
        Request::ThreadPin { thread_id, pinned } => {
            let thread = match thread_by_id(session_store, &thread_id) {
                Ok(thread) => thread,
                Err(response) => return response,
            };
            match write_backend.app_server_rpc(
                "thread/section/move",
                json!({
                    "threadId": thread.id,
                    "sectionId": if pinned {
                        Some(PINNED_THREAD_SECTION_ID)
                    } else {
                        None::<&str>
                    },
                    "beforeThreadId": Value::Null,
                }),
            ) {
                Ok(_) => Response::success(json!({
                    "action": "thread_pin",
                    "thread_id": thread.id,
                    "pinned": pinned,
                })),
                Err(error) => write_backend_error(error),
            }
        }
        Request::WorkspaceDiff { thread_id } => {
            let thread = match thread_by_id(session_store, &thread_id) {
                Ok(thread) => thread,
                Err(response) => return response,
            };
            match workspace_diff_summary(&thread.id, &thread.cwd) {
                Ok(summary) => Response::success(summary),
                Err(response) => response,
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
            let pending_id = pending_messages.begin(
                &resolved.thread.id,
                &text,
                "queue",
                latest_message_index(session_store, &resolved.thread.id),
            );
            match write_backend.queue_message(&resolved.thread.id, &text, &pending_id) {
                Ok(receipt) => {
                    pending_messages
                        .finish_queue(&pending_id, receipt.queued_submission_id.clone());
                    let status = if receipt.started_turn_id.is_some() {
                        pending_messages.finish(&pending_id, "accepted", None);
                        "accepted"
                    } else {
                        "queued"
                    };
                    Response::success(json!({
                        "action": "send",
                        "status": status,
                        "pending_id": pending_id,
                        "thread_id": resolved.thread.id,
                        "target": resolved.method,
                        "backend": receipt,
                    }))
                }
                Err(error) => {
                    let compatible_fallback = error.code == "app_server_unavailable"
                        || (error.code == "app_server_rejected"
                            && (error.message.contains("requires experimentalApi")
                                || error.message.contains("Method not found")
                                || error.message.contains("does not support thread/queue/add")));
                    if compatible_fallback {
                        match write_backend.queue_message_via_cli(&resolved.thread.id, &text) {
                            Ok(backend) => {
                                pending_messages.finish(&pending_id, "queued", None);
                                Response::success(json!({
                                    "action": "send",
                                    "status": "queued",
                                    "pending_id": pending_id,
                                    "thread_id": resolved.thread.id,
                                    "target": resolved.method,
                                    "backend": backend,
                                    "queue_backend": "codex_cli_compatibility",
                                }))
                            }
                            Err(fallback_error) => {
                                pending_messages.finish(
                                    &pending_id,
                                    "failed",
                                    Some(fallback_error.message.clone()),
                                );
                                write_backend_error(fallback_error)
                            }
                        }
                    } else {
                        pending_messages.finish(&pending_id, "failed", Some(error.message.clone()));
                        write_backend_error(error)
                    }
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
                    pending_messages.finish(&pending_id, "steered", None);
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
                    pending_messages.finish(&pending_id, "failed", Some(error.message.clone()));
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

fn workspace_diff_summary(thread_id: &str, cwd: &Path) -> Result<Value, Response> {
    let root_output = workspace_git_output(cwd, &["rev-parse", "--show-toplevel"])?;
    if !root_output.status.success() {
        return Err(Response::error(
            "not_git_repository",
            format!("{} is not inside a Git repository", cwd.display()),
        ));
    }
    let root = String::from_utf8(root_output.stdout)
        .map_err(|error| {
            Response::error(
                "git_output_invalid",
                format!("Git returned a non-UTF-8 repository path: {error}"),
            )
        })?
        .trim()
        .to_owned();
    let root = PathBuf::from(root);

    let head = workspace_git_success(&root, &["rev-parse", "HEAD"], "git rev-parse HEAD")?;
    let head_sha = String::from_utf8(head.stdout)
        .map_err(|error| {
            Response::error(
                "git_output_invalid",
                format!("Git returned a non-UTF-8 HEAD SHA: {error}"),
            )
        })?
        .trim()
        .to_owned();
    let branch = workspace_git_output(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let head_branch = branch
        .status
        .success()
        .then(|| String::from_utf8(branch.stdout).ok())
        .flatten()
        .map(|branch| branch.trim().to_owned())
        .filter(|branch| !branch.is_empty());

    let diff = workspace_git_success(
        &root,
        &[
            "diff",
            "--numstat",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
            "--",
        ],
        "git diff against HEAD",
    )?;
    let (mut additions, deletions, tracked_files) = parse_numstat(&diff.stdout);
    let untracked = workspace_git_success(
        &root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        "git ls-files",
    )?;
    let untracked_paths = untracked
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    let (untracked_additions, untracked_lines_skipped) =
        untracked_text_lines(&root, &untracked_paths);
    additions = additions.saturating_add(untracked_additions);
    let files_changed = tracked_files.saturating_add(untracked_paths.len());

    Ok(json!({
        "thread_id": thread_id,
        "repository": root,
        "base_sha": head_sha,
        "base_branch": head_branch,
        "files_changed": files_changed,
        "additions": additions,
        "deletions": deletions,
        "untracked_files": untracked_paths.len(),
        "untracked_lines_skipped": untracked_lines_skipped,
        "clean": files_changed == 0,
        "semantics": "uncommitted_worktree_vs_head",
    }))
}

fn workspace_git_output(cwd: &Path, args: &[&str]) -> Result<Output, Response> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_EXTERNAL_DIFF", "")
        .output()
        .map_err(|error| {
            Response::error(
                "git_unavailable",
                format!("failed to run git in {}: {error}", cwd.display()),
            )
        })
}

fn workspace_git_success(cwd: &Path, args: &[&str], action: &str) -> Result<Output, Response> {
    let output = workspace_git_output(cwd, args)?;
    if output.status.success() {
        return Ok(output);
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(Response::error(
        "git_command_failed",
        if detail.is_empty() {
            format!("{action} failed with status {}", output.status)
        } else {
            format!("{action} failed: {detail}")
        },
    ))
}

fn parse_numstat(output: &[u8]) -> (u64, u64, usize) {
    let mut additions = 0_u64;
    let mut deletions = 0_u64;
    let mut files = 0_usize;
    for line in String::from_utf8_lossy(output).lines() {
        let mut fields = line.splitn(3, '\t');
        additions = additions.saturating_add(
            fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0),
        );
        deletions = deletions.saturating_add(
            fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0),
        );
        files += 1;
    }
    (additions, deletions, files)
}

fn untracked_text_lines(root: &Path, paths: &[&[u8]]) -> (u64, usize) {
    const MAX_FILE_BYTES: u64 = 1024 * 1024;
    const MAX_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
    const MAX_FILES: usize = 500;

    let mut lines = 0_u64;
    let mut bytes_read = 0_u64;
    let mut skipped = 0_usize;
    for (index, path) in paths.iter().enumerate() {
        if index >= MAX_FILES || bytes_read >= MAX_TOTAL_BYTES {
            skipped += 1;
            continue;
        }
        let candidate = root.join(String::from_utf8_lossy(path).as_ref());
        let Ok(metadata) = fs::symlink_metadata(&candidate) else {
            skipped += 1;
            continue;
        };
        if !metadata.file_type().is_file() || metadata.len() > MAX_FILE_BYTES {
            skipped += 1;
            continue;
        }
        let Ok(content) = fs::read(&candidate) else {
            skipped += 1;
            continue;
        };
        if content.contains(&0) {
            skipped += 1;
            continue;
        }
        bytes_read = bytes_read.saturating_add(content.len() as u64);
        lines = lines.saturating_add(content.iter().filter(|byte| **byte == b'\n').count() as u64);
        if !content.is_empty() && !content.ends_with(b"\n") {
            lines = lines.saturating_add(1);
        }
    }
    (lines, skipped)
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
            if let Some(objective) = goal_objective(text) {
                if !objective.trim().is_empty() {
                    content.push(json!({"kind": "text", "text": objective.trim()}));
                    has_visible_text = true;
                }
                content.push(lazy_content_summary(
                    "Goal execution context",
                    item,
                    message_index,
                    content_index,
                ));
            } else if let Some(label) = transcript_block_label(text) {
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
    let changes = structured_file_changes(tool);
    let patch_stats = changes.map(|changes| {
        changes
            .iter()
            .fold((0, 0), |(additions, deletions), change| {
                let (added, deleted) = change
                    .get("diff")
                    .and_then(Value::as_str)
                    .map(patch_line_stats)
                    .unwrap_or_default();
                (additions + added, deletions + deleted)
            })
    });
    json!({
        "tool_index": tool_index,
        "name": tool.name,
        "status": tool.status,
        "preview": tool_preview(tool),
        "has_output": tool.output.is_some(),
        "bytes": serde_json::to_vec(tool).map_or(0, |encoded| encoded.len()),
        "additions": patch_stats.map(|stats| stats.0),
        "deletions": patch_stats.map(|stats| stats.1),
        "file_count": changes.map(<[Value]>::len),
    })
}

fn tool_preview(tool: &ThreadToolCall) -> String {
    if tool.name == "write_stdin" {
        return "等待输出".to_owned();
    }
    if tool.name == "exec_command" {
        return tool
            .input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("exec_command")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(140)
            .collect();
    }
    if let Some(changes) = structured_file_changes(tool) {
        return match changes {
            [file] => format!(
                "已{} {}",
                patch_action_label(
                    file.pointer("/kind/type")
                        .and_then(Value::as_str)
                        .unwrap_or("update")
                ),
                Path::new(file["path"].as_str().unwrap_or("file"))
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("file")
            ),
            changes if !changes.is_empty() => format!("已编辑 {} 个文件", changes.len()),
            _ => "应用文件补丁".to_owned(),
        };
    }
    if tool.name == "web_search" {
        return tool
            .input
            .get("query")
            .and_then(Value::as_str)
            .map(|query| format!("搜索 {query}"))
            .unwrap_or_else(|| "网页搜索".to_owned());
    }
    let preferred = tool.input.as_str().unwrap_or(&tool.name);
    let normalized = preferred.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview = if normalized.is_empty() {
        tool.name.clone()
    } else {
        normalized
    };
    preview.chars().take(140).collect()
}

fn structured_file_changes(tool: &ThreadToolCall) -> Option<&[Value]> {
    (tool.name == "apply_patch")
        .then(|| tool.input.get("changes")?.as_array().map(Vec::as_slice))?
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

fn goal_objective(text: &str) -> Option<&str> {
    const CONTEXT_START: &str = "<codex_internal_context";
    const CONTEXT_END: &str = "</codex_internal_context>";
    const OBJECTIVE_START: &str = "<objective>";
    const OBJECTIVE_END: &str = "</objective>";

    let trimmed = text.trim_start();
    let opening_end = trimmed.find('>')?;
    let opening = &trimmed[..=opening_end];
    if !opening.starts_with(CONTEXT_START)
        || !(opening.contains("source=\"goal\"") || opening.contains("source='goal'"))
    {
        return None;
    }
    let body = &trimmed[opening_end + 1..];
    let context_end = body.find(CONTEXT_END)?;
    let context = &body[..context_end];
    let objective_start = context.find(OBJECTIVE_START)? + OBJECTIVE_START.len();
    let objective_end = context[objective_start..].find(OBJECTIVE_END)? + objective_start;
    Some(&context[objective_start..objective_end])
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

    fn unique_test_dir(label: &str) -> PathBuf {
        let sequence = NEXT_WORKTREE_ID.fetch_add(1, Ordering::Relaxed);
        env::temp_dir().join(format!(
            "codex-bridge-{label}-{}-{sequence}",
            std::process::id()
        ))
    }

    #[test]
    fn workspace_download_accepts_only_small_regular_files_inside_workspace() {
        let root = unique_test_dir("workspace-download");
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.join("output")).unwrap();
        fs::write(workspace.join("output/firmware image.elf"), b"firmware").unwrap();

        let (bytes, filename) =
            read_workspace_download(&workspace, "output/firmware image.elf").unwrap();
        assert_eq!(bytes, b"firmware");
        assert_eq!(filename, "firmware_image.elf");

        let absolute = fs::canonicalize(workspace.join("output/firmware image.elf")).unwrap();
        assert_eq!(
            read_workspace_download(&workspace, absolute.to_str().unwrap())
                .unwrap()
                .0,
            b"firmware"
        );
        assert_eq!(
            read_workspace_download(&workspace, "output").unwrap_err(),
            FileDownloadFailure::NotAFile
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_download_rejects_escape_and_sixteen_mib_files() {
        let root = unique_test_dir("workspace-download-boundaries");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let outside = root.join("outside.elf");
        fs::write(&outside, b"outside").unwrap();
        assert_eq!(
            read_workspace_download(&workspace, outside.to_str().unwrap()).unwrap_err(),
            FileDownloadFailure::OutsideWorkspace
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, workspace.join("escape.elf")).unwrap();
            assert_eq!(
                read_workspace_download(&workspace, "escape.elf").unwrap_err(),
                FileDownloadFailure::OutsideWorkspace
            );
        }

        let oversized = workspace.join("sixteen-mib.bin");
        let file = fs::File::create(&oversized).unwrap();
        file.set_len(MAX_DOWNLOAD_BYTES).unwrap();
        assert_eq!(
            read_workspace_download(&workspace, oversized.to_str().unwrap()).unwrap_err(),
            FileDownloadFailure::TooLarge
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn download_tickets_are_short_lived_and_retryable() {
        assert_eq!(DOWNLOAD_TICKET_TTL, Duration::from_secs(300));
        let tickets = RwLock::new(HashMap::from([
            (
                "valid".to_owned(),
                DownloadTicket {
                    workspace: PathBuf::from("/workspace"),
                    path: PathBuf::from("/workspace/file.bin"),
                    expires_at: Instant::now() + DOWNLOAD_TICKET_TTL,
                },
            ),
            (
                "expired".to_owned(),
                DownloadTicket {
                    workspace: PathBuf::from("/workspace"),
                    path: PathBuf::from("/workspace/old.bin"),
                    expires_at: Instant::now() - Duration::from_secs(1),
                },
            ),
        ]));
        assert!(get_download_ticket(&tickets, "expired").is_none());
        assert!(get_download_ticket(&tickets, "valid").is_some());
        assert!(get_download_ticket(&tickets, "valid").is_some());
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
    fn thread_cwd_can_use_the_existing_project_directory() {
        let project = unique_test_dir("current-project");
        fs::create_dir_all(&project).unwrap();
        let prepared = prepare_thread_cwd(&project, false, &unique_test_dir("unused")).unwrap();
        assert_eq!(prepared.cwd, fs::canonicalize(&project).unwrap());
        assert!(prepared.worktree.is_none());
        fs::remove_dir(&project).unwrap();
    }

    #[test]
    fn thread_cwd_can_create_and_remove_an_isolated_git_worktree() {
        let root = unique_test_dir("worktree");
        let repository = root.join("source");
        let bridge_home = root.join("bridge");
        fs::create_dir_all(&repository).unwrap();
        assert!(Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());
        fs::write(repository.join("README.md"), "fixture\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "README.md"])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args([
                "-c",
                "user.name=Codex Bridge Test",
                "-c",
                "user.email=codex-bridge@example.invalid",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "fixture",
            ])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());

        let prepared = prepare_thread_cwd(&repository, true, &bridge_home).unwrap();
        let worktree = prepared.worktree.as_ref().unwrap();
        assert!(prepared.cwd.starts_with(bridge_home.join("worktrees")));
        assert_eq!(
            fs::read_to_string(prepared.cwd.join("README.md")).unwrap(),
            "fixture\n"
        );
        assert!(prepared.cwd.join(".git").is_file());
        assert_eq!(remove_created_worktree(worktree), None);
        assert!(!prepared.cwd.exists());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn workspace_diff_reports_only_uncommitted_changes() {
        let repository = unique_test_dir("workspace-diff");
        fs::create_dir_all(&repository).unwrap();
        assert!(Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());
        fs::write(repository.join("tracked.txt"), "old\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args([
                "-c",
                "user.name=Codex Bridge Test",
                "-c",
                "user.email=codex-bridge@example.invalid",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "baseline",
            ])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());
        fs::write(repository.join("tracked.txt"), "new\nmore\n").unwrap();
        fs::write(repository.join("untracked.txt"), "first\nsecond\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());

        let summary = workspace_diff_summary("thread-1", &repository).unwrap();
        assert_eq!(summary["semantics"], "uncommitted_worktree_vs_head");
        assert_eq!(summary["files_changed"], 2);
        assert_eq!(summary["additions"], 4);
        assert_eq!(summary["deletions"], 1);
        assert_eq!(summary["untracked_files"], 1);

        assert!(Command::new("git")
            .args([
                "-c",
                "user.name=Codex Bridge Test",
                "-c",
                "user.email=codex-bridge@example.invalid",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "changes",
            ])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());
        fs::remove_file(repository.join("untracked.txt")).unwrap();
        let clean = workspace_diff_summary("thread-1", &repository).unwrap();
        assert_eq!(clean["files_changed"], 0);
        assert_eq!(clean["clean"], true);
        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn app_server_project_assignment_matches_a_project_root() {
        let project = unique_test_dir("project-assignment");
        fs::create_dir_all(&project).unwrap();
        let canonical = fs::canonicalize(&project).unwrap();
        let projects = json!({"data":[
            {"id":"project-1","roots":[{"path":canonical}]},
            {"id":"project-2","roots":[{"path":"/missing"}]}
        ]});
        assert_eq!(
            project_id_for_path(&projects, &fs::canonicalize(&project).unwrap()).as_deref(),
            Some("project-1")
        );
        fs::remove_dir(&project).unwrap();
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
    fn app_server_version_comes_from_initialize_user_agent() {
        assert_eq!(
            app_server_version_from_user_agent("codex_cli_rs/0.152.1 (macos)"),
            Some("0.152.1")
        );
        assert_eq!(app_server_version_from_user_agent("unknown"), None);
        assert_eq!(parse_thread_cache_limit("3"), Ok(3));
        assert!(parse_thread_cache_limit("0").is_err());
        assert!(parse_thread_cache_limit("65").is_err());
    }

    #[test]
    fn native_queue_text_uses_only_text_inputs() {
        assert_eq!(
            queue_input_text(&json!([
                {"type":"text", "text":"first"},
                {"type":"localImage", "path":"/tmp/image.png"},
                {"type":"text", "text":"second"}
            ]))
            .as_deref(),
            Some("first\nsecond")
        );
    }

    #[test]
    fn web_ui_rejects_cross_origin_browser_requests() {
        let public = vec![parse_public_web_origin("https://codex.example.com").unwrap()];
        assert!(web_origin_allowed("http://127.0.0.1:47653", 47653, &public));
        assert!(web_origin_allowed(
            "http://192.168.1.20:47653",
            47653,
            &public
        ));
        assert!(web_origin_allowed(
            "https://codex.example.com",
            47653,
            &public
        ));
        assert!(!web_origin_allowed(
            "https://localhost:47653",
            47653,
            &public
        ));
        assert!(!web_origin_allowed("https://example.com", 47653, &public));
    }

    #[test]
    fn public_web_origins_are_exact_https_origins() {
        assert_eq!(
            parse_public_web_origin("https://Codex.Example.com/").unwrap(),
            WebOrigin {
                scheme: "https".into(),
                authority: "codex.example.com".into(),
            }
        );
        assert!(parse_public_web_origin("http://codex.example.com").is_err());
        assert!(parse_public_web_origin("https://codex.example.com/path").is_err());
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
        pending.finish(&id, "queued", None);
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

        assert_eq!(
            queue_payload_text(
                r#"{"UserInput":{"client_id":"client-1","content":[{"type":"text","text":"queued from Codex","text_elements":[]}]}}"#
            )
            .as_deref(),
            Some("queued from Codex")
        );
    }

    #[test]
    fn pending_queue_entries_receive_distinct_app_server_ids_and_can_be_dismissed() {
        let sequence = NEXT_WORKTREE_ID.fetch_add(1, Ordering::Relaxed);
        let home = env::temp_dir().join(format!(
            "codex-bridge-pending-queue-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&home).unwrap();
        let connection = Connection::open(home.join("queue_1.sqlite")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE queued_items (id TEXT, thread_id TEXT, payload_json TEXT, queue_order INTEGER);",
            )
            .unwrap();
        let payload = r#"{"UserInput":{"content":[{"type":"text","text":"same text"}]}}"#;
        connection
            .execute(
                "INSERT INTO queued_items VALUES (?1, 'thread-1', ?2, ?3)",
                ("queue-1", payload, 0),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO queued_items VALUES (?1, 'thread-1', ?2, ?3)",
                ("queue-2", payload, 1),
            )
            .unwrap();
        drop(connection);

        let pending = PendingMessages::default();
        let first = pending.begin("thread-1", "same text", "queue", -1);
        let second = pending.begin("thread-1", "same text", "queue", -1);
        pending.finish(&first, "queued", None);
        pending.finish(&second, "queued", None);
        let queued = read_codex_queue(&home);
        let entries = pending.reconcile(&SessionStore::new(home.clone()), queued, None);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].queued_submission_id.as_deref(), Some("queue-1"));
        assert_eq!(entries[1].queued_submission_id.as_deref(), Some("queue-2"));

        pending.dismiss(&first, "thread-1");
        assert_eq!(pending.entries.read().unwrap().len(), 1);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn app_server_queue_reconciliation_preserves_server_id_and_removes_cancelled_entries() {
        let pending = PendingMessages::default();
        let local_id = pending.begin("thread-1", "queued text", "queue", -1);
        pending.finish_queue(&local_id, "server-queue-1".to_owned());
        let queued = vec![PendingMessage {
            id: local_id.clone(),
            thread_id: "thread-1".to_owned(),
            text: "queued text".to_owned(),
            action: "queue".to_owned(),
            status: "queued".to_owned(),
            source: "app_server_queue".to_owned(),
            queued_submission_id: Some("server-queue-1".to_owned()),
            after_message_index: -1,
            error: None,
        }];
        let store = fixture_store();
        let entries = pending.reconcile(&store, queued, Some("thread-1"));
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].queued_submission_id.as_deref(),
            Some("server-queue-1")
        );

        assert!(pending
            .reconcile(&store, Vec::new(), Some("thread-1"))
            .is_empty());
        assert!(pending.entries.read().unwrap().is_empty());
    }

    #[test]
    fn weekly_usage_reports_remaining_percent_from_the_seven_day_window() {
        let limits = json!({
            "rateLimits": {
                "primary": {"usedPercent": 34, "resetsAt": 1789147537, "windowDurationMins": 10080},
                "secondary": null
            }
        });
        let usage = weekly_usage(&limits).unwrap();
        assert_eq!(usage["remaining_percent"], 66);
        assert_eq!(usage["used_percent"], 34);
        assert_eq!(usage["resets_at"], 1789147537);
        assert!(weekly_usage(
            &json!({"rateLimits":{"primary":{"usedPercent":12,"windowDurationMins":300}}})
        )
        .is_none());
    }

    #[test]
    fn composer_options_keep_only_supported_model_effort_pairs() {
        let response = json!({"data":[
            {
                "model":"gpt-test",
                "displayName":"GPT Test",
                "description":"Test model",
                "defaultReasoningEffort":"medium",
                "supportedReasoningEfforts":[
                    {"reasoningEffort":"low","description":"Fast"},
                    {"reasoningEffort":"medium","description":"Balanced"}
                ]
            },
            {"model":"gpt-empty","supportedReasoningEfforts":[]}
        ]});
        let options = composer_model_options(&response);
        assert_eq!(options.len(), 1);
        assert_eq!(options[0]["id"], "gpt-test");
        assert_eq!(options[0]["efforts"][1]["id"], "medium");
        assert!(model_supports(&options, "gpt-test", "low"));
        assert!(!model_supports(&options, "gpt-test", "high"));
        assert!(!model_supports(&options, "gpt-missing", "low"));
    }

    #[test]
    fn composer_settings_fall_back_to_config_read_shape() {
        let settings = composer_config_settings(&json!({
            "config": {
                "model": "gpt-default",
                "model_reasoning_effort": "medium"
            }
        }));
        assert_eq!(settings.0.as_deref(), Some("gpt-default"));
        assert_eq!(settings.1.as_deref(), Some("medium"));
    }

    #[test]
    fn extracts_thread_ids_from_app_server_events() {
        assert_eq!(
            app_server_event_thread_id(&json!({
                "type": "app_server",
                "message": {
                    "method": "thread/status/changed",
                    "params": {"threadId": "thread-1", "status": {"type": "active"}}
                }
            }))
            .as_deref(),
            Some("thread-1")
        );
        assert_eq!(
            app_server_event_thread_id(&json!({
                "type": "app_server",
                "message": {
                    "method": "thread/started",
                    "params": {"thread": {"id": "thread-2"}}
                }
            }))
            .as_deref(),
            Some("thread-2")
        );
    }

    #[test]
    fn loaded_thread_ids_accept_current_and_legacy_shapes() {
        let ids = loaded_thread_ids(&json!({
            "data": [
                "thread-1",
                {"id": "thread-2"},
                {"thread": {"id": "thread-3"}},
                {"threadId": "thread-4"}
            ]
        }));
        assert_eq!(ids, ["thread-1", "thread-2", "thread-3", "thread-4"]);
        assert!(thread_result_is_active(&json!({
            "thread": {"status": {"type": "active", "activeFlags": []}}
        })));
        assert!(thread_result_is_active(&json!({"status": "active"})));
        assert!(!thread_result_is_active(&json!({
            "thread": {"status": {"type": "idle"}}
        })));
    }

    #[test]
    fn vite_web_ui_uses_split_data_feeds_and_keeps_raw_protocol_access() {
        assert!(WEB_INDEX.contains("/assets/app.js"));
        assert!(WEB_INDEX.contains("/assets/app.css"));
        assert!(WEB_INDEX.contains("/assets/app.js?v="));
        assert!(WEB_INDEX.contains("/assets/app.css?v="));

        let source = concat!(
            include_str!("../../../web-ui/index.html"),
            include_str!("../../../web-ui/src/api.js"),
            include_str!("../../../web-ui/src/auth-gate.js"),
            include_str!("../../../web-ui/src/main.js"),
            include_str!("../../../web-ui/src/markdown.js"),
            include_str!("../../../web-ui/src/message-cache.js"),
            include_str!("../../../web-ui/src/i18n.js"),
            include_str!("../../../web-ui/src/composer-state.js"),
            include_str!("../../../web-ui/src/session-route.js"),
            include_str!("../../../web-ui/src/state.js"),
            include_str!("../../../web-ui/src/theme.js"),
            include_str!("../../../web-ui/src/styles.css"),
        );
        for command in [
            "projects",
            "project_threads",
            "messages",
            "message_content",
            "tool_content",
            "thread_activity",
            "thread_watch",
            "composer_status",
            "composer_options",
            "thread_create",
            "thread_settings_update",
            "thread_rename",
            "thread_archive",
            "thread_pins",
            "thread_pin",
            "workspace_diff",
            "select",
            "current",
            "status",
            "tail",
            "send",
            "steer",
            "scroll",
            "pending",
            "pending_messages",
            "pending_message_delete",
            "approve",
            "decline",
            "interrupt",
            "host_exec",
            "app_server_rpc",
            "/api/events",
        ] {
            assert!(source.contains(command), "missing {command}");
        }
        for marker in [
            "id=\"rawRequest\"",
            "Any codex-bridge Request JSON",
            "id=\"outboxTray\"",
            "id=\"createDialog\"",
            "id=\"createWorktreeBtn\"",
            "deletePending(entry, remove)",
            "删除并恢复到输入框",
            "touch-action: pan-y",
            "codex-bridge.drafts.v1",
            "正在压缩上下文…",
            "id=\"usageHealth\"",
            "服务异常 · 等待恢复",
            "Desktop bundled app-server · private transport",
            "standalone fallback 已禁用",
            "-webkit-text-size-adjust: 100%",
            "contain: inline-size",
            "id=\"modelPicker\"",
            "prefers-color-scheme: dark",
            "id=\"themeSelect\"",
            "codex-bridge.theme.v2",
            "watchSystemTheme",
            "pinnedSessions",
            "pinnedThreads",
            "--composer-height",
            "--thread-head-height",
            "ResizeObserver",
            "role=\"status\" aria-live=\"polite\"",
            "id=\"languageBtn\"",
            "id=\"settingsPanel\"",
            "github.com/hitsmaxft/codex-bridge",
            "setComposerSubmitting",
            "submit-stop",
            "stopRunConfirm",
            "interruptCurrentRun",
            "shouldOfferStop",
            "sessionIdFromHash",
            "hashchange",
            "codex-bridge.last-session.v1",
            "preserveView: true",
            "visibilitychange",
            "bridge_thread_activity_snapshot",
            "/api/file",
            "/api/file-ticket",
            "/api/auth",
            "setDeliveryState",
            "serverQueued",
            "submit-spin",
            "codex-bridge.language.v1",
            "id=\"sendModeToggle\"",
            "toggleSendModeAndKeepFocus",
            "event.detail !== 0",
            "messagePlaceholderCompact",
            "createFileDownloadTicket",
            "id=\"archiveThreadBtn\"",
            "id=\"renameThreadBtn\"",
            "message-copy",
            "SessionMessageCache",
            "tool-summary-label",
            "appendPatchDiff",
            "diff-line",
            "fileChange",
            ".outbox-item.submitting .message-body",
            "outbox-mode",
            "border: 1px dashed",
            "classList.add(\"focused\", \"input-focused\")",
            "border-width: 2px",
        ] {
            assert!(source.contains(marker), "missing {marker}");
        }
        assert!(WEB_APP_JS.contains("pending_message_delete"));
        assert!(WEB_APP_CSS.contains("touch-action:pan-y"));
        assert!(source.contains("background: var(--control-bg)"));
        assert!(source.contains("background: var(--code-bg)"));
        assert_eq!(source.matches(".message.user {").count(), 1);
        assert_eq!(source.matches(".composer-shell {").count(), 2);
        assert_eq!(source.matches(".outbox-item {").count(), 1);
        assert!(!source.contains("--mobile-code"));
        assert!(!source.contains("project-path"));
        assert!(!source.contains("max-height: min(52dvh, 480px)"));
        assert!(!source.contains("sessionStorage"));
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
                call_id: "exec-1".into(),
                name: "exec_command".into(),
                status: "completed".into(),
                input: json!({"type":"commandExecution","command":"git status --short","cwd":"/workspace","commandActions":[]}),
                output: Some(json!({"aggregatedOutput":"large private output","exitCode":0})),
            }],
        };
        let compact = compact_web_message(&message, 5);
        let encoded = serde_json::to_string(&compact).unwrap();
        assert_eq!(compact["tools"][0]["name"], "exec_command");
        assert_eq!(compact["tools"][0]["preview"], "git status --short");
        assert_eq!(compact["tools"][0]["tool_index"], 0);
        assert!(compact["tools"][0]["has_output"].as_bool().unwrap());
        assert!(!encoded.contains("large private output"));
    }

    #[test]
    fn app_server_command_item_is_kept_structured() {
        let tool = typed_thread_tool(&json!({
            "type":"commandExecution",
            "id":"exec-1",
            "command":"git status --short",
            "commandActions":[{"type":"unknown","command":"git status --short"}],
            "cwd":"/workspace",
            "status":"completed",
            "aggregatedOutput":"clean",
            "exitCode":0,
            "durationMs":12,
        }))
        .unwrap();
        assert_eq!(tool.name, "exec_command");
        assert_eq!(tool.input["command"], "git status --short");
        assert_eq!(tool.output.as_ref().unwrap()["aggregatedOutput"], "clean");
        assert_eq!(tool_preview(&tool), "git status --short");
    }

    #[test]
    fn app_server_file_change_reports_files_and_line_counts() {
        let tool = typed_thread_tool(&json!({
            "type":"fileChange",
            "id":"patch-1",
            "status":"completed",
            "changes":[{
                "path":"/tmp/src/sessions.rs",
                "kind":{"type":"update","move_path":null},
                "diff":"@@ -1 +1,2 @@\n-old\n+new\n+more"
            }]
        }))
        .unwrap();
        assert_eq!(tool_preview(&tool), "已编辑 sessions.rs");
        let compact = compact_tool_summary(&tool, 2);
        assert_eq!(compact["additions"], 2);
        assert_eq!(compact["deletions"], 1);
        assert_eq!(compact["file_count"], 1);
        assert_eq!(compact["name"], "apply_patch");
        assert_eq!(tool.input["changes"][0]["path"], "/tmp/src/sessions.rs");
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

    #[test]
    fn goal_wrapper_only_shows_the_user_objective_by_default() {
        let message = ThreadMessage {
            timestamp: None,
            id: None,
            role: "user".into(),
            phase: None,
            content: vec![json!({
                "type":"input_text",
                "text":"<codex_internal_context source=\"goal\">\nContinue working toward the active thread goal.\n\n<objective>\n实现并验证目标功能\n\n保留用户输入的换行\n</objective>\n\nBudget:\n- Tokens used: 123456\n\nCompletion audit:\nprivate injected rules\n</codex_internal_context>"
            })],
            tools: Vec::new(),
        };
        let compact = compact_web_message(&message, 11);
        let encoded = serde_json::to_string(&compact).unwrap();
        assert_eq!(compact["category"], "user");
        assert_eq!(compact["content"].as_array().unwrap().len(), 2);
        assert_eq!(
            compact["content"][0]["text"],
            "实现并验证目标功能\n\n保留用户输入的换行"
        );
        assert_eq!(compact["content"][1]["kind"], "context");
        assert_eq!(compact["content"][1]["label"], "Goal execution context");
        assert_eq!(compact["content"][1]["message_index"], 11);
        assert_eq!(compact["content"][1]["content_index"], 0);
        assert!(!encoded.contains("123456"));
        assert!(!encoded.contains("private injected rules"));
    }

    #[test]
    fn ordinary_internal_context_is_not_misclassified_as_a_goal() {
        assert_eq!(
            goal_objective(
                "<codex_internal_context source=\"other\"><objective>text</objective></codex_internal_context>"
            ),
            None
        );
        assert_eq!(
            goal_objective(
                "<codex_internal_context source=\"goal\"></codex_internal_context><objective>outside</objective>"
            ),
            None
        );
    }
}
