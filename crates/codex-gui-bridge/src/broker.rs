//! WebSocket broker: Desktop client on one side, app-server on the other.
//!
//! Architecture:
//!
//! ```text
//! Desktop ──WS:18790/rpc──> Broker ──WS:18791/rpc──> codex app-server
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

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, connect_async, MaybeTlsStream, WebSocketStream};

use crate::protocol::{self, INJECTED_ID_PREFIX};

/// Maximum number of Desktop connections kept at once (only one is expected).
const MAX_DESKTOP_CONNECTIONS: usize = 4;

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
    /// Next injected request id.
    next_inject_id: AtomicU64,
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
            next_inject_id: AtomicU64::new(1),
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
        if matches!(method, "thread/start" | "thread/resume") {
            let mut current = self.active_thread.write().await;
            *current = Some(thread_id.to_owned());
        }
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
    pub sink: Mutex<futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>,
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
) -> Result<()> {
    tracing_info(&format!(
        "broker: listening for Desktop on {} (upstream {upstream_url})",
        listener.local_addr()?
    ));
    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => accepted.context("accept desktop connection")?,
            _ = state.shutdown.changed() => {
                if *state.shutdown.borrow() {
                    tracing_info("broker: shutting down desktop listener");
                    return Ok(());
                }
                continue;
            }
        };
        tracing_info(&format!("broker: Desktop connected from {peer}"));
        let state = Arc::clone(&state);
        let upstream_url = upstream_url.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_desktop(stream, peer, &upstream_url, &state).await {
                tracing_info(&format!("broker: desktop {peer} ended: {error:#}"));
            }
        });
    }
}

async fn handle_desktop(
    stream: TcpStream,
    peer: SocketAddr,
    upstream_url: &str,
    state: &Arc<BrokerState>,
) -> Result<()> {
    let desktop_ws = accept_async(stream)
        .await
        .with_context(|| format!("websocket handshake with Desktop {peer}"))?;
    let (mut desktop_sink, mut desktop_stream) = desktop_ws.split();

    // Connect to the app-server we supervise.
    let (upstream_ws, _response) = connect_async(upstream_url)
        .await
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
            // Drop the upstream sink when Desktop goes away so the app-server
            // sees the disconnect.
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
                        if let Some(id) = value
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
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

    let _ = tokio::join!(desktop_to_upstream, upstream_to_desktop);

    // Desktop is gone: clear the shared upstream so later CLI injections fail
    // fast instead of sending into a dead connection.
    let mut registered = state.upstream.write().await;
    if let Some(current) = registered.as_ref() {
        if Arc::ptr_eq(current, &upstream) {
            *registered = None;
        }
    }
    Ok(())
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
    state: &Arc<BrokerState>,
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
async fn rpc_once(
    upstream_url: &str,
    id: u64,
    request: Value,
) -> Result<Value> {
    let (ws, _) = connect_async(upstream_url).await?;
    let (mut sink, mut stream) = ws.split();
    sink.send(Message::Text(
        serde_json::to_string(&protocol::initialize_request("codex-gui", env!("CARGO_PKG_VERSION")))?
            .into(),
    ))
    .await?;
    // Read until initialize response arrives.
    loop {
        let Some(message) = stream.next().await else {
            bail!("app-server closed during initialize");
        };
        let message = message?;
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(&text)?;
            if value.get("id").and_then(Value::as_i64) == Some(0) {
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
        let message = tokio::time::timeout(timeout, stream.next()).await.map_err(|_| {
            anyhow::anyhow!("timed out waiting for app-server response {expected}")
        })?;
        let Some(message) = message else {
            bail!("app-server closed while waiting for response {expected}");
        };
        let message = message?;
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(&text)?;
            if value.get("id").and_then(Value::as_str) == Some(expected.as_str()) {
                return Ok(value);
            }
        }
    }
}

/// Send an injected RPC over the shared Desktop upstream connection and wait
/// for its response. This is the path for turn mutations (`send`, `steer`,
/// `interrupt`): the request happens on the exact connection the app-server
/// associates with the Desktop, so turn lifecycle notifications keep flowing
/// to the GUI and there is never a second writer process.
async fn rpc_via_shared(
    state: &Arc<BrokerState>,
    request: Value,
) -> Result<Value> {
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
    upstream
        .send_text(serde_json::to_string(&request)?)
        .await
        .context("failed to send injected RPC over shared upstream")?;
    state.record("injected_request", &request).await;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
    loop {
        let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
        if timeout.is_zero() {
            bail!("timed out waiting for injected response {id}");
        }
        let response = tokio::time::timeout(timeout, rx.recv())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for injected response {id}"))?;
        let Some(response) = response else {
            bail!("injected response channel closed for {id}");
        };
        if protocol::is_injected_response(&response) {
            return Ok(response);
        }
    }
}

async fn status_json(state: &Arc<BrokerState>) -> Value {
    let active = state.active_thread.read().await.clone();
    let activity = state.recent_activity.read().await.clone();
    serde_json::json!({
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
    match args.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .with_context(|| format!("argument {key} must be a number")),
    }
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
        assert_eq!(state.active_thread.read().await.as_deref(), Some("def"));
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
