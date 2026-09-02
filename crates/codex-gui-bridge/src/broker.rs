//! WebSocket broker: Desktop client on one side, app-server on the other.
//!
//! Architecture:
//!
//! ```text
//! Desktop ──WS:18790/rpc?token=...──> Broker ──WS:18791/rpc──> codex app-server
//!                               ^
//!                           CLI IPC (Unix socket)
//! ```
//!
//! The broker owns the upstream connection to the app-server. Desktop's
//! initialize/initialized handshake is forwarded so the app-server treats the
//! Desktop as its client. CLI-injected requests reuse the *same* upstream
//! connection, which is what makes `turn/steer` / `turn/interrupt` work on GUI
//! threads: the turn is active inside the one app-server instance that owns it.
//!
//! Frame rules:
//! - Desktop request -> forwarded upstream; recorded by the recorder.
//! - Upstream response to a Desktop request -> forwarded back to Desktop.
//! - Upstream response to an injected request (`__bridge_*` id) -> consumed by
//!   the broker, correlated back to the waiting CLI caller; never forwarded.
//! - Upstream notification -> forwarded to Desktop (and mirrored to recorder).
//! - Server -> client requests (e.g. approvals) -> forwarded to Desktop so the
//!   GUI can approve them normally.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch, Mutex};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_hdr_async, connect_async, MaybeTlsStream, WebSocketStream};

use crate::protocol::{self, INJECTED_ID_PREFIX};

/// Pending injected RPC: id -> response channel.
type PendingInjection = mpsc::UnboundedSender<Value>;

/// Shared broker state.
pub struct BrokerState {
    /// Active thread observed from Desktop traffic (thread/start, thread/resume).
    pub active_thread: tokio::sync::RwLock<Option<String>>,
    /// Recent RPC activity recorded from Desktop traffic (ring buffer).
    pub recent_activity: tokio::sync::RwLock<Vec<ActivityRecord>>,
    /// The upstream connection currently shared with Desktop. CLI-injected
    /// turn mutations ride this same connection (single app-server instance,
    /// single writer per thread — no cross-process writer fights).
    pub upstream: tokio::sync::RwLock<Option<Arc<Upstream>>>,
    /// Injected request correlator: `__bridge_<n>` -> channel to CLI caller.
    pending: tokio::sync::RwLock<std::collections::HashMap<String, PendingInjection>>,
    /// Desktop request id -> method, used to recognize the thread returned by
    /// thread/start and thread/resume without treating unrelated
    /// thread/started notifications (for example subagents) as the active GUI
    /// thread.
    desktop_requests: tokio::sync::RwLock<std::collections::HashMap<String, String>>,
    /// Next injected request id.
    next_inject_id: AtomicU64,
    /// The broker deliberately supports one Desktop/upstream pair. Allowing a
    /// second pair would make the meaning of "current" and injected writes
    /// ambiguous.
    desktop_connected: AtomicBool,
    /// True only after the app-server has accepted Desktop's initialize
    /// request on the shared upstream.
    desktop_ready: AtomicBool,
    /// Shutdown signal (shared with main).
    pub shutdown: watch::Receiver<bool>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ActivityRecord {
    pub at: u64,
    pub kind: &'static str,
    pub method: String,
    pub thread_id: Option<String>,
    pub detail: String,
}

const ACTIVITY_LIMIT: usize = 200;

impl BrokerState {
    pub fn new(shutdown: watch::Receiver<bool>) -> Arc<Self> {
        Arc::new(Self {
            active_thread: tokio::sync::RwLock::new(None),
            recent_activity: tokio::sync::RwLock::new(Vec::new()),
            upstream: tokio::sync::RwLock::new(None),
            pending: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            desktop_requests: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            next_inject_id: AtomicU64::new(1),
            desktop_connected: AtomicBool::new(false),
            desktop_ready: AtomicBool::new(false),
            shutdown,
        })
    }

    pub fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    async fn record(&self, kind: &'static str, message: &Value) {
        let method = protocol::method_of(message).unwrap_or("").to_owned();
        let thread_id = protocol::thread_id_of(message);
        let detail = summarize(message);
        let mut activity = self.recent_activity.write().await;
        activity.push(ActivityRecord {
            at: self.now_ms(),
            kind,
            method,
            thread_id,
            detail,
        });
        if activity.len() > ACTIVITY_LIMIT {
            let excess = activity.len() - ACTIVITY_LIMIT;
            activity.drain(..excess);
        }
    }

    /// Note a thread that Desktop started or resumed.
    pub async fn note_thread_activity(&self, method: &str, thread_id: &str) {
        if matches!(method, "thread/start" | "thread/resume" | "turn/start") {
            self.set_active_thread(thread_id).await;
        }
    }

    pub async fn set_active_thread(&self, thread_id: &str) {
        let mut current = self.active_thread.write().await;
        *current = Some(thread_id.to_owned());
    }

    /// Register an injected request and hand back the response receiver.
    pub async fn register_injection(&self, id: u64) -> mpsc::UnboundedReceiver<Value> {
        let key = format!("{INJECTED_ID_PREFIX}{id:06}");
        self.register_injection_by_key(&key).await
    }

    /// Register an injected request by its full id string.
    pub async fn register_injection_by_key(&self, key: &str) -> mpsc::UnboundedReceiver<Value> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.pending.write().await.insert(key.to_owned(), tx);
        rx
    }

    /// Resolve an injected response to its waiting CLI caller.
    pub async fn resolve_injection(&self, id: &str) -> Option<PendingInjection> {
        self.pending.write().await.remove(id)
    }

    async fn remember_desktop_request(&self, message: &Value, method: &str) {
        if let Some(id) = protocol::request_id_key(message) {
            self.desktop_requests
                .write()
                .await
                .insert(id, method.to_owned());
        }
    }

    async fn take_desktop_request_method(&self, message: &Value) -> Option<String> {
        let id = protocol::request_id_key(message)?;
        self.desktop_requests.write().await.remove(&id)
    }

    async fn clear_connection_state(&self) {
        self.pending.write().await.clear();
        self.desktop_requests.write().await.clear();
        *self.active_thread.write().await = None;
        self.desktop_ready.store(false, Ordering::Release);
    }

    pub fn next_inject_id(&self) -> u64 {
        self.next_inject_id.fetch_add(1, Ordering::Relaxed)
    }
}

fn summarize(message: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(params) = message.get("params") {
        if let Some(text) = params.get("text").and_then(Value::as_str) {
            parts.push(format!("text={:?}", truncate(text, 60)));
        }
        if let Some(input) = params.get("input").and_then(Value::as_array) {
            parts.push(format!("input={} item(s)", input.len()));
            if let Some(text) = input
                .iter()
                .find_map(|item| item.get("text").and_then(Value::as_str))
            {
                parts.push(format!("text={:?}", truncate(text, 60)));
            }
        }
        if let Some(turn) = params.get("turnId").and_then(Value::as_str) {
            parts.push(format!("turn={}", truncate(turn, 16)));
        }
    }
    if let Some(result) = message.get("result") {
        parts.push(format!("result={}", truncate(&result.to_string(), 40)));
    }
    if let Some(error) = message.get("error") {
        parts.push(format!("error={}", truncate(&error.to_string(), 60)));
    }
    parts.join("; ")
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut s: String = text.chars().take(max).collect();
        s.push('…');
        s
    }
}

/// Upstream connection used both by Desktop traffic and injected RPC.
pub struct Upstream {
    pub sink:
        Mutex<futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>,
}

impl Upstream {
    /// Send one text frame through the shared upstream connection.
    pub async fn send_text(&self, text: String) -> Result<()> {
        self.sink.lock().await.send(Message::text(text)).await?;
        Ok(())
    }
}

/// Accept Desktop connections on the broker listen address.
pub async fn serve_desktop(
    listener: TcpListener,
    upstream_url: String,
    state: Arc<BrokerState>,
    desktop_token: String,
) -> Result<()> {
    ensure_loopback_ws_url(&upstream_url)?;
    let local_addr = listener.local_addr()?;
    if !local_addr.ip().is_loopback() {
        bail!("Desktop broker must listen on loopback, got {local_addr}");
    }
    tracing_info(&format!(
        "broker: listening for Desktop on {} (upstream {upstream_url})",
        local_addr
    ));
    let mut shutdown = state.shutdown.clone();
    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => accepted.context("accept desktop connection")?,
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    tracing_info("broker: shutting down desktop listener");
                    return Ok(());
                }
                continue;
            }
        };
        tracing_info(&format!("broker: Desktop connected from {peer}"));
        let state = Arc::clone(&state);
        let upstream_url = upstream_url.clone();
        let desktop_token = desktop_token.clone();
        tokio::spawn(async move {
            if let Err(error) =
                handle_desktop(stream, peer, &upstream_url, &desktop_token, &state).await
            {
                tracing_info(&format!("broker: desktop {peer} ended: {error:#}"));
            }
        });
    }
}

// tungstenite requires the handshake callback to return its full HTTP
// ErrorResponse type; boxing that error would not match the callback API.
#[allow(clippy::result_large_err)]
async fn handle_desktop(
    stream: TcpStream,
    peer: SocketAddr,
    upstream_url: &str,
    desktop_token: &str,
    state: &Arc<BrokerState>,
) -> Result<()> {
    if state
        .desktop_connected
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        bail!("another Desktop connection is already active");
    }
    let _connection_guard = DesktopConnectionGuard(&state.desktop_connected);
    state.desktop_ready.store(false, Ordering::Release);

    let expected_token = desktop_token.to_owned();
    let desktop_ws = accept_hdr_async(stream, move |request: &Request, response: Response| {
        if authorized_desktop_request(request, &expected_token) {
            Ok(response)
        } else {
            let mut response = ErrorResponse::new(Some("unauthorized Desktop connection".into()));
            *response.status_mut() = StatusCode::UNAUTHORIZED;
            Err(response)
        }
    })
    .await
    .with_context(|| format!("websocket handshake with Desktop {peer}"))?;
    let (mut desktop_sink, mut desktop_stream) = desktop_ws.split();

    // Connect to the app-server we supervise.
    let (upstream_ws, _response) = tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        connect_async(upstream_url),
    )
    .await
    .context("timed out connecting to supervised app-server")?
    .with_context(|| format!("connect upstream {upstream_url}"))?;
    let (upstream_sink, mut upstream_stream) = upstream_ws.split();
    let upstream = Arc::new(Upstream {
        sink: Mutex::new(upstream_sink),
    });
    // Register the shared upstream so CLI-injected RPC rides this connection.
    *state.upstream.write().await = Some(Arc::clone(&upstream));

    tracing_info(&format!(
        "broker: upstream {upstream_url} connected for Desktop {peer}"
    ));

    // Desktop -> upstream. Record requests; forward everything.
    let desktop_to_upstream = {
        let upstream = Arc::clone(&upstream);
        let state = Arc::clone(state);
        tokio::spawn(async move {
            while let Some(message) = desktop_stream.next().await {
                let message = match message {
                    Ok(message) => message,
                    Err(error) => {
                        tracing_info(&format!("broker: desktop->upstream read error: {error}"));
                        break;
                    }
                };
                let text = match message {
                    Message::Text(text) => text,
                    Message::Close(_) => break,
                    Message::Ping(payload) => {
                        // Forward pings as pings; tungstenite auto-responds to
                        // pings it receives on the read side, so only forward
                        // the payload to keep Desktop's keepalive working.
                        let mut sink = upstream.sink.lock().await;
                        if sink.send(Message::Ping(payload)).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    _ => continue,
                };
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    if let Some(method) = protocol::method_of(&value) {
                        state.record("desktop_request", &value).await;
                        state.remember_desktop_request(&value, method).await;
                        if let Some(thread_id) = protocol::thread_id_of(&value) {
                            state.note_thread_activity(method, &thread_id).await;
                        }
                    }
                }
                let mut sink = upstream.sink.lock().await;
                if sink.send(Message::Text(text)).await.is_err() {
                    tracing_info("broker: upstream closed while forwarding desktop->upstream");
                    break;
                }
            }
            // Release this task's writer reference. The coordinator clears
            // the registered upstream after either direction ends.
            drop(upstream);
        })
    };

    // Upstream -> Desktop. Consume injected responses; forward everything else.
    let upstream_to_desktop = {
        let state = Arc::clone(state);
        tokio::spawn(async move {
            while let Some(message) = upstream_stream.next().await {
                let message = match message {
                    Ok(message) => message,
                    Err(error) => {
                        tracing_info(&format!("broker: upstream->desktop read error: {error}"));
                        break;
                    }
                };
                let text = match message {
                    Message::Text(text) => text,
                    Message::Close(_) => break,
                    Message::Ping(payload) => {
                        if desktop_sink.send(Message::Ping(payload)).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    _ => continue,
                };
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    // Injected response -> give to the waiting CLI caller.
                    if protocol::is_injected_response(&value) {
                        if let Some(id) = value.get("id").and_then(Value::as_str).map(str::to_owned)
                        {
                            if let Some(tx) = state.resolve_injection(&id).await {
                                let _ = tx.send(value.clone());
                            } else {
                                tracing_info(&format!(
                                    "broker: injected response for unknown id {id} dropped"
                                ));
                            }
                        }
                        state.record("injected_response", &value).await;
                        continue;
                    }
                    if protocol::method_of(&value).is_none() {
                        match state.take_desktop_request_method(&value).await.as_deref() {
                            Some("initialize") if value.get("result").is_some() => {
                                state.desktop_ready.store(true, Ordering::Release);
                            }
                            Some("thread/start" | "thread/resume") => {
                                if let Some(thread_id) = protocol::observed_thread_id(&value) {
                                    state.set_active_thread(&thread_id).await;
                                }
                            }
                            _ => {}
                        }
                    }
                    state.record("upstream_message", &value).await;
                }
                if desktop_sink.send(Message::text(text)).await.is_err() {
                    tracing_info("broker: Desktop closed while forwarding upstream->desktop");
                    break;
                }
            }
            let _ = desktop_sink.close().await;
        })
    };

    let mut desktop_to_upstream = desktop_to_upstream;
    let mut upstream_to_desktop = upstream_to_desktop;
    let mut connection_shutdown = state.shutdown.clone();
    tokio::select! {
        _ = &mut desktop_to_upstream => {
            upstream_to_desktop.abort();
            let _ = upstream_to_desktop.await;
        },
        _ = &mut upstream_to_desktop => {
            desktop_to_upstream.abort();
            let _ = desktop_to_upstream.await;
        },
        _ = wait_for_shutdown(&mut connection_shutdown) => {
            desktop_to_upstream.abort();
            upstream_to_desktop.abort();
            let _ = tokio::join!(desktop_to_upstream, upstream_to_desktop);
        },
    }

    // Desktop is gone: clear the shared upstream so later CLI injections fail
    // fast instead of sending into a dead connection.
    let mut registered = state.upstream.write().await;
    if let Some(current) = registered.as_ref() {
        if Arc::ptr_eq(current, &upstream) {
            *registered = None;
        }
    }
    drop(registered);
    state.clear_connection_state().await;
    Ok(())
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

struct DesktopConnectionGuard<'a>(&'a AtomicBool);

impl Drop for DesktopConnectionGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn authorized_desktop_request(request: &Request, expected_token: &str) -> bool {
    if request.uri().path() != "/rpc" {
        return false;
    }
    request
        .uri()
        .query()
        .and_then(|query| {
            query.split('&').find_map(|part| {
                let (name, value) = part.split_once('=')?;
                (name == "token").then_some(value)
            })
        })
        .is_some_and(|token| constant_time_eq(token.as_bytes(), expected_token.as_bytes()))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

/// Types of results the CLI API can return.
pub enum CliCall {
    /// A plain JSON value.
    Json(Value),
    /// A stream of notifications (e.g. tail while a turn is running).
    Notifications(mpsc::UnboundedReceiver<Value>),
}

/// Execute a CLI command against the broker state and upstream.
pub async fn execute_cli(
    state: &BrokerState,
    upstream_url: &str,
    cmd: &str,
    args: &serde_json::Map<String, Value>,
) -> Result<CliCall> {
    match cmd {
        "status" => Ok(CliCall::Json(status_json(state).await)),
        "current" => {
            let current = state.active_thread.read().await.clone();
            Ok(CliCall::Json(serde_json::json!({ "threadId": current })))
        }
        "threads" => {
            let id = state.next_inject_id();
            let request = protocol::thread_list_request(id, arg_u64(args, "limit", 100)?);
            let response = rpc_once(upstream_url, id, request).await?;
            Ok(CliCall::Json(response))
        }
        "read" => {
            let thread_id = arg_str(args, "thread")?;
            let id = state.next_inject_id();
            let request = protocol::thread_read_request(id, &thread_id);
            let response = rpc_once(upstream_url, id, request).await?;
            Ok(CliCall::Json(response))
        }
        "turns" => {
            let thread_id = arg_str(args, "thread")?;
            let limit = arg_u64(args, "limit", 20)?;
            let id = state.next_inject_id();
            let request = protocol::thread_turns_list_request(id, &thread_id, limit);
            let response = rpc_once(upstream_url, id, request).await?;
            Ok(CliCall::Json(response))
        }
        "send" => {
            let thread_id = arg_str(args, "thread")?;
            let text = arg_str(args, "text")?;
            let id = state.next_inject_id();
            let request = protocol::turn_start_request(id, &thread_id, &text);
            let response = rpc_via_shared(state, request).await?;
            Ok(CliCall::Json(response))
        }
        "interrupt" => {
            let thread_id = arg_str(args, "thread")?;
            let turn_id = arg_str(args, "turn")?;
            let id = state.next_inject_id();
            let request = protocol::turn_interrupt_request(id, &thread_id, &turn_id);
            let response = rpc_via_shared(state, request).await?;
            Ok(CliCall::Json(response))
        }
        "steer" => {
            let thread_id = arg_str(args, "thread")?;
            let turn_id = arg_str(args, "turn")?;
            let text = arg_str(args, "text")?;
            let id = state.next_inject_id();
            let request = protocol::turn_steer_request(id, &thread_id, &turn_id, &text);
            let response = rpc_via_shared(state, request).await?;
            Ok(CliCall::Json(response))
        }
        other => bail!("unknown command: {other}"),
    }
}

/// Run one injected RPC over a temporary upstream connection and wait for its
/// response. Used for read-only CLI commands that do not need to ride the
/// Desktop connection.
async fn rpc_once(upstream_url: &str, id: u64, request: Value) -> Result<Value> {
    ensure_loopback_ws_url(upstream_url)?;
    let (ws, _) = tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        connect_async(upstream_url),
    )
    .await
    .context("timed out connecting to app-server")??;
    let (mut sink, mut stream) = ws.split();
    sink.send(Message::Text(
        serde_json::to_string(&protocol::initialize_request(
            "codex-gui",
            env!("CARGO_PKG_VERSION"),
        ))?
        .into(),
    ))
    .await?;
    // Read until initialize response arrives.
    let initialize_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
    loop {
        let remaining = initialize_deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            bail!("timed out waiting for app-server initialize response");
        }
        let message = tokio::time::timeout(remaining, stream.next())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for app-server initialize response"))?;
        let Some(message) = message else {
            bail!("app-server closed during initialize");
        };
        let message = message?;
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(&text)?;
            if value.get("id").and_then(Value::as_i64) == Some(0) {
                rpc_result(value)?;
                break;
            }
        }
    }
    sink.send(Message::Text(
        serde_json::to_string(&protocol::initialized_notification())?.into(),
    ))
    .await?;
    sink.send(Message::Text(serde_json::to_string(&request)?.into()))
        .await?;
    // Wait for the injected id response.
    let expected = format!("{INJECTED_ID_PREFIX}{id:06}");
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
    loop {
        let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
        let message = tokio::time::timeout(timeout, stream.next())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for app-server response {expected}"))?;
        let Some(message) = message else {
            bail!("app-server closed while waiting for response {expected}");
        };
        let message = message?;
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(&text)?;
            if value.get("id").and_then(Value::as_str) == Some(expected.as_str()) {
                return rpc_result(value);
            }
        }
    }
}

fn ensure_loopback_ws_url(url: &str) -> Result<()> {
    let uri: tokio_tungstenite::tungstenite::http::Uri = url
        .parse()
        .with_context(|| format!("invalid app-server URL: {url}"))?;
    if uri.scheme_str() != Some("ws") {
        bail!("app-server URL must use ws://");
    }
    let host = uri
        .host()
        .context("app-server URL must contain an IP host")?;
    let host_without_brackets = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let ip: IpAddr = host_without_brackets.parse().with_context(|| {
        format!("app-server host must be an IP literal: {host_without_brackets}")
    })?;
    if !ip.is_loopback() {
        bail!("app-server URL must be loopback, got {ip}");
    }
    Ok(())
}

/// Send an injected RPC over the shared Desktop upstream connection and wait
/// for its response. This is the path for turn mutations (`send`, `steer`,
/// `interrupt`): the request happens on the exact connection the app-server
/// associates with the Desktop, so turn lifecycle notifications keep flowing
/// to the GUI and there is never a second writer process.
async fn rpc_via_shared(state: &BrokerState, request: Value) -> Result<Value> {
    if !state.desktop_ready.load(Ordering::Acquire) {
        bail!("Desktop connection has not completed app-server initialization");
    }
    let upstream = {
        let guard = state.upstream.read().await;
        match guard.as_ref() {
            Some(upstream) => Arc::clone(upstream),
            None => {
                bail!(
                    "no Desktop connection: the GUI must be running and connected \
                     to the broker before send/steer/interrupt (read-only commands \
                     still work)"
                )
            }
        }
    };
    let id = request
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("injected request is missing an id"))?;
    let mut rx = state.register_injection_by_key(&id).await;
    match tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        upstream.send_text(serde_json::to_string(&request)?),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            state.resolve_injection(&id).await;
            return Err(error).context("failed to send injected RPC over shared upstream");
        }
        Err(_) => {
            state.resolve_injection(&id).await;
            bail!("timed out sending injected RPC over shared upstream");
        }
    }
    state.record("injected_request", &request).await;
    let response = match tokio::time::timeout(tokio::time::Duration::from_secs(30), rx.recv()).await
    {
        Ok(Some(response)) => response,
        Ok(None) => bail!("injected response channel closed for {id}"),
        Err(_) => {
            state.resolve_injection(&id).await;
            bail!("timed out waiting for injected response {id}");
        }
    };
    rpc_result(response)
}

fn rpc_result(response: Value) -> Result<Value> {
    if response.get("error").is_some() {
        bail!("{}", protocol::error_message(&response));
    }
    response
        .get("result")
        .cloned()
        .context("app-server response has neither result nor error")
}

async fn status_json(state: &BrokerState) -> Value {
    let active = state.active_thread.read().await.clone();
    let desktop_connected = state.upstream.read().await.is_some();
    let desktop_ready = state.desktop_ready.load(Ordering::Acquire);
    let activity = state.recent_activity.read().await.clone();
    serde_json::json!({
        "desktopConnected": desktop_connected,
        "desktopReady": desktop_ready,
        "activeThread": active,
        "recentActivityCount": activity.len(),
        "recentActivity": activity.iter().rev().take(10).collect::<Vec<_>>(),
    })
}

fn arg_str(args: &serde_json::Map<String, Value>, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("missing string argument: {key}"))
}

fn arg_u64(args: &serde_json::Map<String, Value>, key: &str, default: u64) -> Result<u64> {
    let value = match args.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .with_context(|| format!("argument {key} must be a number")),
    }?;
    if value > u32::MAX.into() {
        bail!("argument {key} exceeds the app-server uint32 limit");
    }
    Ok(value)
}

pub fn tracing_info(message: &str) {
    eprintln!("[codex-gui-bridge] {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn injection_register_and_resolve() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let state = BrokerState::new(shutdown_rx);
        let id = state.next_inject_id();
        assert_eq!(id, 1);
        let rx = state.register_injection(id).await;
        let key = format!("{INJECTED_ID_PREFIX}{id:06}");
        let resolved = state.resolve_injection(&key).await;
        assert!(resolved.is_some());
        drop(rx);
        drop(shutdown_tx);
    }

    #[tokio::test]
    async fn active_thread_tracking() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let state = BrokerState::new(shutdown_rx);
        state.note_thread_activity("thread/start", "abc").await;
        assert_eq!(state.active_thread.read().await.as_deref(), Some("abc"));
        state.note_thread_activity("thread/resume", "def").await;
        assert_eq!(state.active_thread.read().await.as_deref(), Some("def"));
        state.note_thread_activity("turn/start", "xyz").await;
        assert_eq!(state.active_thread.read().await.as_deref(), Some("xyz"));
        drop(shutdown_tx);
    }

    #[test]
    fn desktop_handshake_requires_exact_path_and_token() {
        let accepted = Request::builder()
            .uri("/rpc?token=abcdefghijklmnopqrstuvwxyz123456")
            .body(())
            .unwrap();
        assert!(authorized_desktop_request(
            &accepted,
            "abcdefghijklmnopqrstuvwxyz123456"
        ));

        let wrong_token = Request::builder().uri("/rpc?token=wrong").body(()).unwrap();
        assert!(!authorized_desktop_request(
            &wrong_token,
            "abcdefghijklmnopqrstuvwxyz123456"
        ));

        let wrong_path = Request::builder()
            .uri("/other?token=abcdefghijklmnopqrstuvwxyz123456")
            .body(())
            .unwrap();
        assert!(!authorized_desktop_request(
            &wrong_path,
            "abcdefghijklmnopqrstuvwxyz123456"
        ));

        assert!(ensure_loopback_ws_url("ws://127.0.0.1:18791/rpc").is_ok());
        assert!(ensure_loopback_ws_url("ws://[::1]:18791/rpc").is_ok());
        assert!(ensure_loopback_ws_url("ws://192.0.2.1:18791/rpc").is_err());
        assert!(ensure_loopback_ws_url("wss://127.0.0.1:18791/rpc").is_err());
        assert!(ensure_loopback_ws_url("ws://localhost:18791/rpc").is_err());
    }

    #[tokio::test]
    async fn fake_desktop_and_cli_share_one_upstream() {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream_listener.accept().await.unwrap();
            let mut websocket = tokio_tungstenite::accept_async(stream).await.unwrap();

            let initialize = websocket.next().await.unwrap().unwrap();
            let Message::Text(initialize) = initialize else {
                panic!("expected Desktop text request");
            };
            let initialize: Value = serde_json::from_str(&initialize).unwrap();
            assert_eq!(initialize["method"], "initialize");
            websocket
                .send(Message::text(
                    serde_json::json!({
                        "id": initialize["id"],
                        "result": {}
                    })
                    .to_string(),
                ))
                .await
                .unwrap();

            let initialized = websocket.next().await.unwrap().unwrap();
            let Message::Text(initialized) = initialized else {
                panic!("expected Desktop initialized notification");
            };
            let initialized: Value = serde_json::from_str(&initialized).unwrap();
            assert_eq!(initialized["method"], "initialized");

            let desktop_request = websocket.next().await.unwrap().unwrap();
            let Message::Text(desktop_request) = desktop_request else {
                panic!("expected Desktop thread request");
            };
            let desktop_request: Value = serde_json::from_str(&desktop_request).unwrap();
            assert_eq!(desktop_request["method"], "thread/start");
            websocket
                .send(Message::text(
                    serde_json::json!({
                        "id": desktop_request["id"],
                        "result": {"thread": {"id": "gui-thread"}}
                    })
                    .to_string(),
                ))
                .await
                .unwrap();

            let injected_request = websocket.next().await.unwrap().unwrap();
            let Message::Text(injected_request) = injected_request else {
                panic!("expected injected text request");
            };
            let injected_request: Value = serde_json::from_str(&injected_request).unwrap();
            assert_eq!(injected_request["method"], "turn/start");
            assert_eq!(injected_request["params"]["threadId"], "gui-thread");
            websocket
                .send(Message::text(
                    serde_json::json!({
                        "id": injected_request["id"],
                        "result": {"turn": {"id": "turn-1"}}
                    })
                    .to_string(),
                ))
                .await
                .unwrap();

            let _ = websocket.next().await;

            let (stream, _) = upstream_listener.accept().await.unwrap();
            let mut websocket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = websocket.next().await;
        });

        let broker_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let broker_addr = broker_listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let state = BrokerState::new(shutdown_rx);
        let token = "abcdefghijklmnopqrstuvwxyz123456".to_owned();
        let broker_task = tokio::spawn(serve_desktop(
            broker_listener,
            format!("ws://{upstream_addr}/rpc"),
            Arc::clone(&state),
            token.clone(),
        ));

        let unauthorized =
            connect_async(format!("ws://{broker_addr}/rpc?token=not-the-token")).await;
        assert!(unauthorized.is_err());

        let (mut desktop, _) = connect_async(format!("ws://{broker_addr}/rpc?token={token}"))
            .await
            .unwrap();
        let simultaneous = connect_async(format!("ws://{broker_addr}/rpc?token={token}")).await;
        assert!(
            simultaneous.is_err(),
            "a second simultaneous Desktop connection was accepted"
        );

        let early_arguments = serde_json::json!({
            "thread": "gui-thread",
            "text": "too early"
        })
        .as_object()
        .unwrap()
        .clone();
        let early_error = match execute_cli(
            &state,
            &format!("ws://{upstream_addr}/rpc"),
            "send",
            &early_arguments,
        )
        .await
        {
            Err(error) => error,
            Ok(_) => panic!("CLI write succeeded before Desktop initialization"),
        };
        assert!(early_error
            .to_string()
            .contains("has not completed app-server initialization"));

        desktop
            .send(Message::text(
                serde_json::json!({
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "clientInfo": {
                            "name": "fake-desktop",
                            "title": "fake-desktop",
                            "version": "test"
                        },
                        "capabilities": {}
                    }
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let initialize_response = desktop.next().await.unwrap().unwrap();
        let Message::Text(initialize_response) = initialize_response else {
            panic!("expected initialize response");
        };
        let initialize_response: Value = serde_json::from_str(&initialize_response).unwrap();
        assert_eq!(initialize_response["id"], 1);
        assert!(state.desktop_ready.load(Ordering::Acquire));
        desktop
            .send(Message::text(
                serde_json::json!({"method": "initialized"}).to_string(),
            ))
            .await
            .unwrap();

        desktop
            .send(Message::text(
                serde_json::json!({
                    "id": 7,
                    "method": "thread/start",
                    "params": {}
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let response = desktop.next().await.unwrap().unwrap();
        let Message::Text(response) = response else {
            panic!("expected thread/start response");
        };
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["result"]["thread"]["id"], "gui-thread");

        tokio::time::timeout(tokio::time::Duration::from_secs(1), async {
            loop {
                if state.active_thread.read().await.as_deref() == Some("gui-thread") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let arguments = serde_json::json!({
            "thread": "gui-thread",
            "text": "continue"
        })
        .as_object()
        .unwrap()
        .clone();
        let result = execute_cli(
            &state,
            &format!("ws://{upstream_addr}/rpc"),
            "send",
            &arguments,
        )
        .await
        .unwrap();
        let CliCall::Json(result) = result else {
            panic!("expected one JSON response");
        };
        assert_eq!(result["turn"]["id"], "turn-1");
        assert!(
            tokio::time::timeout(tokio::time::Duration::from_millis(50), desktop.next())
                .await
                .is_err(),
            "injected response leaked to Desktop"
        );

        desktop.close(None).await.unwrap();
        tokio::time::timeout(tokio::time::Duration::from_secs(1), async {
            loop {
                if state.upstream.read().await.is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!state.desktop_ready.load(Ordering::Acquire));
        assert!(state.active_thread.read().await.is_none());

        let (mut reconnected, _) = connect_async(format!("ws://{broker_addr}/rpc?token={token}"))
            .await
            .unwrap();
        tokio::time::timeout(tokio::time::Duration::from_secs(1), async {
            loop {
                if state.upstream.read().await.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        reconnected.close(None).await.unwrap();
        tokio::time::timeout(tokio::time::Duration::from_secs(1), async {
            loop {
                if state.upstream.read().await.is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        shutdown_tx.send_replace(true);
        broker_task.await.unwrap().unwrap();
        tokio::time::timeout(tokio::time::Duration::from_secs(1), upstream_task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn read_only_cli_uses_an_initialized_ephemeral_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = tokio_tungstenite::accept_async(stream).await.unwrap();

            let initialize = websocket.next().await.unwrap().unwrap();
            let Message::Text(initialize) = initialize else {
                panic!("expected initialize request");
            };
            let initialize: Value = serde_json::from_str(&initialize).unwrap();
            assert_eq!(initialize["method"], "initialize");
            websocket
                .send(Message::text(
                    serde_json::json!({"id": 0, "result": {}}).to_string(),
                ))
                .await
                .unwrap();

            let initialized = websocket.next().await.unwrap().unwrap();
            let Message::Text(initialized) = initialized else {
                panic!("expected initialized notification");
            };
            let initialized: Value = serde_json::from_str(&initialized).unwrap();
            assert_eq!(initialized["method"], "initialized");

            let request = websocket.next().await.unwrap().unwrap();
            let Message::Text(request) = request else {
                panic!("expected thread/list request");
            };
            let request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["method"], "thread/list");
            websocket
                .send(Message::text(
                    serde_json::json!({
                        "id": request["id"],
                        "result": {"data": [{"id": "thread-1"}]}
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
        });

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let state = BrokerState::new(shutdown_rx);
        let arguments = serde_json::json!({"limit": 10})
            .as_object()
            .unwrap()
            .clone();
        let result = execute_cli(
            &state,
            &format!("ws://{address}/rpc"),
            "threads",
            &arguments,
        )
        .await
        .unwrap();
        let CliCall::Json(result) = result else {
            panic!("expected one JSON response");
        };
        assert_eq!(result["data"][0]["id"], "thread-1");

        server.await.unwrap();
        drop(shutdown_tx);
    }

    #[test]
    fn summarize_captures_text() {
        let message = serde_json::json!({
            "id": 1,
            "method": "turn/start",
            "params": {"threadId": "t1", "input": [{"type": "text", "text": "hello world"}]}
        });
        let summary = summarize(&message);
        assert!(summary.contains("hello world"));
    }
}
