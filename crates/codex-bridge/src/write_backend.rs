use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc as std_mpsc, Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tungstenite::{Message, WebSocket};

use crate::RealtimeAudioChunk;

pub const CODEX_BIN_ENV: &str = "CODEX_BRIDGE_CODEX_BIN";
pub const APP_SERVER_SOCKET_ENV: &str = "CODEX_BRIDGE_APP_SERVER_SOCKET";

const MAX_CAPTURED_OUTPUT_BYTES: usize = 64 * 1024;
const RPC_TIMEOUT: Duration = Duration::from_secs(10);
const APP_SERVER_IDLE_POLL: Duration = Duration::from_millis(200);
const APP_SERVER_EVENT_CAPACITY: usize = 512;
const APP_SERVER_MAX_MESSAGE_BYTES: usize = 64 << 20;
const REALTIME_TRANSCRIPTION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct CodexCliBackend {
    program: PathBuf,
    app_server_socket: Option<PathBuf>,
    app_server: Option<Arc<AppServerSession>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NativeQueueReceipt {
    pub backend: String,
    pub queued_submission_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_turn_id: Option<String>,
}

#[derive(Debug)]
enum AppServerCommand {
    Rpc {
        method: String,
        params: Value,
        response: std_mpsc::SyncSender<Result<Value, BackendFailure>>,
    },
    WatchThread {
        thread_id: String,
        response: std_mpsc::SyncSender<Result<Value, BackendFailure>>,
    },
}

#[derive(Debug)]
struct AppServerSession {
    commands: std_mpsc::Sender<AppServerCommand>,
    events: broadcast::Sender<Value>,
    runtime: Arc<RwLock<AppServerRuntimeInfo>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppServerRuntimeInfo {
    pub connected: bool,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BackendSuccess {
    pub backend: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendFailure {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Invocation {
    backend: &'static str,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
}

impl CodexCliBackend {
    pub fn new(program: PathBuf, app_server_socket: Option<PathBuf>) -> Self {
        Self::new_with_thread_cache(program, app_server_socket, 3)
    }

    pub fn new_with_thread_cache(
        program: PathBuf,
        app_server_socket: Option<PathBuf>,
        thread_cache_limit: usize,
    ) -> Self {
        let app_server = app_server_socket.as_ref().map(|socket| {
            Arc::new(AppServerSession::spawn(
                socket.clone(),
                thread_cache_limit.max(1),
            ))
        });
        Self {
            program,
            app_server_socket,
            app_server,
        }
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn app_server_socket(&self) -> Option<&Path> {
        self.app_server_socket.as_deref()
    }

    pub fn queue_message(
        &self,
        thread_id: &str,
        input: &[Value],
        client_user_message_id: &str,
    ) -> Result<NativeQueueReceipt, BackendFailure> {
        let result = self.app_server_rpc(
            "thread/queue/add",
            json!({
                "threadId": thread_id,
                "input": input,
                "clientUserMessageId": client_user_message_id,
            }),
        )?;
        let queued_submission_id = result
            .pointer("/queuedSubmission/id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| BackendFailure {
                code: "app_server_protocol_error",
                message: "thread/queue/add response has no queuedSubmission.id".to_owned(),
            })?;
        let started_turn_id = self
            .app_server_rpc(
                "thread/read",
                json!({"threadId": thread_id, "includeTurns": false}),
            )
            .ok()
            .filter(|thread| !app_server_thread_is_active(thread))
            .and_then(|_| {
                self.app_server_rpc(
                    "thread/queue/start",
                    json!({
                        "threadId": thread_id,
                        "queuedSubmissionId": queued_submission_id,
                    }),
                )
                .ok()
            })
            .and_then(|result| {
                result
                    .pointer("/turn/id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
            });
        Ok(NativeQueueReceipt {
            backend: "app_server_queue".to_owned(),
            queued_submission_id: queued_submission_id.to_owned(),
            started_turn_id,
        })
    }

    pub fn queue_message_via_cli(
        &self,
        thread_id: &str,
        text: &str,
    ) -> Result<BackendSuccess, BackendFailure> {
        self.run(queue_invocation(thread_id, text))
    }

    pub fn watch_thread(&self, thread_id: &str) -> Result<Value, BackendFailure> {
        let app_server = self
            .app_server
            .as_ref()
            .ok_or_else(app_server_unavailable)?;
        app_server.watch_thread(thread_id)
    }

    pub fn subscribe_app_server_events(&self) -> Option<broadcast::Receiver<Value>> {
        self.app_server
            .as_ref()
            .map(|session| session.events.subscribe())
    }

    pub fn publish_bridge_event(&self, event: Value) -> bool {
        self.app_server
            .as_ref()
            .is_some_and(|session| session.events.send(event).is_ok())
    }

    pub fn app_server_runtime_info(&self) -> Option<AppServerRuntimeInfo> {
        self.app_server
            .as_ref()
            .and_then(|session| session.runtime.read().ok().map(|info| info.clone()))
    }

    pub fn steer_via_app_server(
        &self,
        thread_id: &str,
        turn_id: &str,
        input: &[Value],
    ) -> Result<BackendSuccess, BackendFailure> {
        let params = json!({
            "threadId": thread_id,
            "input": input,
            "expectedTurnId": turn_id
        });
        self.run_app_server_rpc("turn/steer", params, "codex_app_server_steer")
    }

    pub fn interrupt_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<BackendSuccess, BackendFailure> {
        let params = json!({"threadId": thread_id, "turnId": turn_id});
        self.run_app_server_rpc("turn/interrupt", params, "codex_app_server_interrupt")
    }

    pub fn app_server_rpc(&self, method: &str, params: Value) -> Result<Value, BackendFailure> {
        let app_server = self
            .app_server
            .as_ref()
            .ok_or_else(app_server_unavailable)?;
        app_server.call(method, params)
    }

    pub fn transcribe_audio(
        &self,
        thread_id: &str,
        audio: &RealtimeAudioChunk,
    ) -> Result<String, BackendFailure> {
        let mut events = self
            .subscribe_app_server_events()
            .ok_or_else(app_server_unavailable)?;
        self.app_server_rpc(
            "thread/realtime/start",
            json!({
                "threadId": thread_id,
                "version": "v2",
                "outputModality": "text",
                "clientManagedHandoffs": true,
                "includeStartupContext": false,
                "flushTranscriptTailOnSessionEnd": false,
            }),
        )?;
        if let Err(error) =
            wait_for_realtime_event(&mut events, thread_id, "thread/realtime/started")
        {
            let _ = self.app_server_rpc("thread/realtime/stop", json!({"threadId": thread_id}));
            return Err(error);
        }
        self.app_server_rpc(
            "thread/realtime/appendAudio",
            json!({
                "threadId": thread_id,
                "audio": {
                    "data": audio.data,
                    "sampleRate": audio.sample_rate,
                    "numChannels": audio.num_channels,
                    "samplesPerChannel": audio.samples_per_channel,
                }
            }),
        )?;
        let transcript = wait_for_realtime_transcript(&mut events, thread_id);
        let stop = self.app_server_rpc("thread/realtime/stop", json!({"threadId": thread_id}));
        match (transcript, stop) {
            (Ok(text), _) => Ok(text),
            (Err(error), Ok(_)) => Err(error),
            (Err(_), Err(error)) => Err(error),
        }
    }

    pub fn latest_item_type(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<Option<String>, BackendFailure> {
        let result = self.app_server_rpc(
            "thread/items/list",
            serde_json::json!({
                "threadId": thread_id,
                "turnId": turn_id,
                "limit": 1,
                "sortDirection": "desc",
            }),
        )?;
        Ok(latest_item_type_from_response(&result, turn_id))
    }

    fn run(&self, invocation: Invocation) -> Result<BackendSuccess, BackendFailure> {
        let mut command = Command::new(&self.program);
        command.args(&invocation.args).stdin(Stdio::null());
        if let Some(cwd) = &invocation.cwd {
            command.current_dir(cwd);
        }

        let output = command.output().map_err(|error| BackendFailure {
            code: "codex_cli_unavailable",
            message: format!(
                "failed to execute {} using {}: {error}",
                invocation.backend,
                self.program.display()
            ),
        })?;
        let stdout = captured_text(&output.stdout);
        let stderr = captured_text(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);
        if !output.status.success() {
            return Err(BackendFailure {
                code: "codex_cli_failed",
                message: command_failure_message(invocation.backend, exit_code, &stdout, &stderr),
            });
        }

        Ok(BackendSuccess {
            backend: invocation.backend.to_owned(),
            exit_code,
            stdout,
            stderr,
        })
    }

    fn run_app_server_rpc(
        &self,
        method: &str,
        params: Value,
        backend_name: &'static str,
    ) -> Result<BackendSuccess, BackendFailure> {
        self.app_server_rpc(method, params)?;
        Ok(BackendSuccess {
            backend: backend_name.to_owned(),
            exit_code: 0,
            stdout: None,
            stderr: None,
        })
    }
}

fn app_server_thread_is_active(result: &Value) -> bool {
    let status = result
        .pointer("/thread/status")
        .or_else(|| result.get("status"));
    status.and_then(Value::as_str) == Some("active")
        || status
            .and_then(|status| status.get("type"))
            .and_then(Value::as_str)
            == Some("active")
}

fn app_server_unavailable() -> BackendFailure {
    BackendFailure {
        code: "app_server_unavailable",
        message: "Desktop bundled app-server uses a private stdio connection; no external app-server endpoint is configured and standalone fallback is disabled".to_owned(),
    }
}

fn app_server_event<'a>(event: &'a Value, thread_id: &str) -> Option<(&'a str, &'a Value)> {
    let message = event.get("message")?;
    let method = message.get("method")?.as_str()?;
    let params = message.get("params")?;
    (params.get("threadId")?.as_str()? == thread_id).then_some((method, params))
}

fn wait_for_realtime_event(
    events: &mut broadcast::Receiver<Value>,
    thread_id: &str,
    expected_method: &str,
) -> Result<(), BackendFailure> {
    let deadline = Instant::now() + REALTIME_TRANSCRIPTION_TIMEOUT;
    loop {
        match events.try_recv() {
            Ok(event) => {
                if let Some((method, params)) = app_server_event(&event, thread_id) {
                    if method == expected_method {
                        return Ok(());
                    }
                    if method == "thread/realtime/error" {
                        let message = params
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("app-server realtime transcription failed");
                        return Err(BackendFailure {
                            code: if message.contains("requires API key auth") {
                                "audio_transcription_auth_required"
                            } else {
                                "audio_transcription_failed"
                            },
                            message: message.to_owned(),
                        });
                    }
                }
            }
            Err(broadcast::error::TryRecvError::Empty) => {
                if Instant::now() >= deadline {
                    return Err(BackendFailure {
                        code: "audio_transcription_timeout",
                        message: format!("timed out waiting for {expected_method}"),
                    });
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(broadcast::error::TryRecvError::Closed) => return Err(app_server_unavailable()),
        }
    }
}

fn wait_for_realtime_transcript(
    events: &mut broadcast::Receiver<Value>,
    thread_id: &str,
) -> Result<String, BackendFailure> {
    let deadline = Instant::now() + REALTIME_TRANSCRIPTION_TIMEOUT;
    loop {
        match events.try_recv() {
            Ok(event) => {
                if let Some((method, params)) = app_server_event(&event, thread_id) {
                    if method == "thread/realtime/transcript/done"
                        && params.get("role").and_then(Value::as_str) == Some("user")
                    {
                        return Ok(params
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned());
                    }
                    if method == "thread/realtime/error" {
                        let message = params
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("app-server realtime transcription failed");
                        return Err(BackendFailure {
                            code: if message.contains("requires API key auth") {
                                "audio_transcription_auth_required"
                            } else {
                                "audio_transcription_failed"
                            },
                            message: message.to_owned(),
                        });
                    }
                }
            }
            Err(broadcast::error::TryRecvError::Empty) => {
                if Instant::now() >= deadline {
                    return Err(BackendFailure {
                        code: "audio_transcription_timeout",
                        message: "timed out waiting for speech transcription".to_owned(),
                    });
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(broadcast::error::TryRecvError::Closed) => return Err(app_server_unavailable()),
        }
    }
}

impl AppServerSession {
    fn spawn(socket: PathBuf, thread_cache_limit: usize) -> Self {
        let (command_tx, command_rx) = std_mpsc::channel();
        let (events, _) = broadcast::channel(APP_SERVER_EVENT_CAPACITY);
        let worker_events = events.clone();
        let runtime = Arc::new(RwLock::new(AppServerRuntimeInfo {
            connected: false,
            user_agent: None,
        }));
        let worker_runtime = Arc::clone(&runtime);
        thread::Builder::new()
            .name("codex-app-server".to_owned())
            .spawn(move || {
                app_server_worker(
                    socket,
                    thread_cache_limit,
                    command_rx,
                    worker_events,
                    worker_runtime,
                )
            })
            .expect("failed to start app-server connection worker");
        Self {
            commands: command_tx,
            events,
            runtime,
        }
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, BackendFailure> {
        let (response_tx, response_rx) = std_mpsc::sync_channel(1);
        self.commands
            .send(AppServerCommand::Rpc {
                method: method.to_owned(),
                params,
                response: response_tx,
            })
            .map_err(|_| BackendFailure {
                code: "app_server_unavailable",
                message: "app-server connection worker stopped".to_owned(),
            })?;
        response_rx
            .recv_timeout(RPC_TIMEOUT + Duration::from_secs(2))
            .map_err(|error| BackendFailure {
                code: "app_server_timeout",
                message: format!("failed waiting for app-server connection worker: {error}"),
            })?
    }

    fn watch_thread(&self, thread_id: &str) -> Result<Value, BackendFailure> {
        let (response_tx, response_rx) = std_mpsc::sync_channel(1);
        self.commands
            .send(AppServerCommand::WatchThread {
                thread_id: thread_id.to_owned(),
                response: response_tx,
            })
            .map_err(|_| BackendFailure {
                code: "app_server_unavailable",
                message: "app-server connection worker stopped".to_owned(),
            })?;
        response_rx
            .recv_timeout(RPC_TIMEOUT + Duration::from_secs(2))
            .map_err(|error| BackendFailure {
                code: "app_server_timeout",
                message: format!("failed waiting for app-server thread subscription: {error}"),
            })?
    }
}

fn app_server_worker(
    socket: PathBuf,
    thread_cache_limit: usize,
    commands: std_mpsc::Receiver<AppServerCommand>,
    events: broadcast::Sender<Value>,
    runtime: Arc<RwLock<AppServerRuntimeInfo>>,
) {
    let mut websocket = None;
    let mut next_request_id = 1_i64;
    let mut watched_threads = VecDeque::<String>::new();
    loop {
        match commands.recv_timeout(APP_SERVER_IDLE_POLL) {
            Ok(command) => {
                if let Err(error) = ensure_app_server_connection(
                    &socket,
                    &mut websocket,
                    &mut next_request_id,
                    &watched_threads,
                    &events,
                    &runtime,
                ) {
                    match command {
                        AppServerCommand::Rpc { response, .. }
                        | AppServerCommand::WatchThread { response, .. } => {
                            let _ = response.send(Err(error));
                        }
                    }
                    continue;
                }
                match command {
                    AppServerCommand::Rpc {
                        method,
                        params,
                        response,
                    } => {
                        let result = session_rpc(
                            websocket.as_mut().expect("connection ensured"),
                            &mut next_request_id,
                            &method,
                            params,
                            &events,
                        );
                        if result.is_err() {
                            websocket = None;
                            emit_connection_event(&events, &runtime, "disconnected", None, None);
                        }
                        let _ = response.send(result.clone());
                    }
                    AppServerCommand::WatchThread {
                        thread_id,
                        response,
                    } => {
                        if let Some(index) = watched_threads.iter().position(|id| id == &thread_id)
                        {
                            watched_threads.remove(index);
                            watched_threads.push_back(thread_id.clone());
                            let result = Ok(json!({"threadId": thread_id, "cached": true}));
                            let _ = response.send(result.clone());
                        } else {
                            let result = session_rpc(
                                websocket.as_mut().expect("connection ensured"),
                                &mut next_request_id,
                                "thread/resume",
                                json!({"threadId": thread_id, "excludeTurns": true}),
                                &events,
                            );
                            if result.is_ok() {
                                watched_threads.push_back(thread_id.clone());
                                if watched_threads.len() > thread_cache_limit {
                                    if let Some(evicted) = watched_threads.pop_front() {
                                        let _ = session_rpc(
                                            websocket.as_mut().expect("connection ensured"),
                                            &mut next_request_id,
                                            "thread/unsubscribe",
                                            json!({"threadId": evicted}),
                                            &events,
                                        );
                                    }
                                }
                            } else {
                                websocket = None;
                                emit_connection_event(
                                    &events,
                                    &runtime,
                                    "disconnected",
                                    None,
                                    None,
                                );
                            }
                            let _ = response.send(result.clone());
                        }
                    }
                }
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                if let Some(active) = websocket.as_mut() {
                    match read_app_server_message(active, &events) {
                        Ok(_) => {}
                        Err(error) if error.code == "app_server_timeout" => {}
                        Err(error) => {
                            websocket = None;
                            emit_connection_event(
                                &events,
                                &runtime,
                                "disconnected",
                                None,
                                Some(&error.message),
                            );
                        }
                    }
                }
            }
        }
    }
}

fn ensure_app_server_connection(
    app_server_socket: &Path,
    websocket: &mut Option<WebSocket<UnixStream>>,
    next_request_id: &mut i64,
    watched_threads: &VecDeque<String>,
    events: &broadcast::Sender<Value>,
    runtime: &Arc<RwLock<AppServerRuntimeInfo>>,
) -> Result<(), BackendFailure> {
    if websocket.is_some() {
        return Ok(());
    }
    let metadata = std::fs::metadata(app_server_socket).map_err(|error| BackendFailure {
        code: "app_server_unavailable",
        message: format!(
            "cannot inspect app-server socket {}: {error}",
            app_server_socket.display()
        ),
    })?;
    if !metadata.file_type().is_socket() {
        return Err(BackendFailure {
            code: "app_server_unavailable",
            message: format!(
                "app-server endpoint is not a Unix socket: {}",
                app_server_socket.display()
            ),
        });
    }

    let stream = UnixStream::connect(app_server_socket).map_err(|error| BackendFailure {
        code: "app_server_unavailable",
        message: format!(
            "failed to connect to app-server socket {}: {error}",
            app_server_socket.display()
        ),
    })?;
    stream
        .set_read_timeout(Some(RPC_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(RPC_TIMEOUT)))
        .map_err(|error| BackendFailure {
            code: "app_server_protocol_error",
            message: format!("failed to configure app-server socket timeout: {error}"),
        })?;
    let websocket_config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(APP_SERVER_MAX_MESSAGE_BYTES))
        .max_frame_size(Some(APP_SERVER_MAX_MESSAGE_BYTES));
    let (mut connected, _response) =
        tungstenite::client::client_with_config("ws://localhost/", stream, Some(websocket_config))
            .map_err(|error| match error {
                tungstenite::HandshakeError::Failure(error) => {
                    websocket_failure("upgrade app-server control socket", error)
                }
                tungstenite::HandshakeError::Interrupted(_) => BackendFailure {
                    code: "app_server_protocol_error",
                    message: "app-server WebSocket handshake was interrupted".to_owned(),
                },
            })?;

    write_rpc_message(
        &mut connected,
        &json!({
            "id": 0,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "codex-bridge",
                    "title": "codex-bridge",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {"experimentalApi": true}
            }
        }),
    )?;
    let initialize = rpc_result(wait_for_response(&mut connected, 0, RPC_TIMEOUT)?)?;
    write_rpc_message(
        &mut connected,
        &json!({"method": "initialized", "params": {}}),
    )?;
    connected
        .get_mut()
        .set_read_timeout(Some(APP_SERVER_IDLE_POLL))
        .map_err(|error| BackendFailure {
            code: "app_server_protocol_error",
            message: format!("failed to configure app-server event timeout: {error}"),
        })?;
    let user_agent = initialize
        .get("userAgent")
        .and_then(Value::as_str)
        .map(str::to_owned);
    emit_connection_event(events, runtime, "connected", user_agent.as_deref(), None);
    for thread_id in watched_threads {
        session_rpc(
            &mut connected,
            next_request_id,
            "thread/resume",
            json!({"threadId": thread_id, "excludeTurns": true}),
            events,
        )?;
    }
    *websocket = Some(connected);
    Ok(())
}

fn session_rpc(
    websocket: &mut WebSocket<UnixStream>,
    next_request_id: &mut i64,
    method: &str,
    params: Value,
    events: &broadcast::Sender<Value>,
) -> Result<Value, BackendFailure> {
    let id = *next_request_id;
    *next_request_id = next_request_id.saturating_add(1);
    websocket
        .get_mut()
        .set_read_timeout(Some(RPC_TIMEOUT))
        .map_err(|error| BackendFailure {
            code: "app_server_protocol_error",
            message: format!("failed to configure app-server response timeout: {error}"),
        })?;
    write_rpc_message(
        websocket,
        &json!({"id": id, "method": method, "params": params}),
    )?;
    let deadline = Instant::now() + RPC_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(BackendFailure {
                code: "app_server_timeout",
                message: format!("timed out waiting for app-server response {id}"),
            });
        }
        websocket
            .get_mut()
            .set_read_timeout(Some(remaining))
            .map_err(|error| BackendFailure {
                code: "app_server_protocol_error",
                message: format!("failed to update app-server response timeout: {error}"),
            })?;
        if let Some(response) = read_app_server_message(websocket, events)? {
            if response.get("id").and_then(Value::as_i64) == Some(id) {
                websocket
                    .get_mut()
                    .set_read_timeout(Some(APP_SERVER_IDLE_POLL))
                    .ok();
                return rpc_result(response);
            }
        }
    }
}

fn read_app_server_message(
    websocket: &mut WebSocket<UnixStream>,
    events: &broadcast::Sender<Value>,
) -> Result<Option<Value>, BackendFailure> {
    let message = websocket
        .read()
        .map_err(|error| websocket_failure("read app-server message", error))?;
    let Message::Text(payload) = message else {
        return Ok(None);
    };
    let value = serde_json::from_str::<Value>(&payload).map_err(|error| BackendFailure {
        code: "app_server_protocol_error",
        message: format!("app-server returned invalid JSON: {error}"),
    })?;
    if value.get("method").is_some() {
        let event_type = if value.get("id").is_some() {
            "app_server_request"
        } else {
            "app_server"
        };
        let _ = events.send(json!({"type": event_type, "message": value}));
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

fn emit_connection_event(
    events: &broadcast::Sender<Value>,
    runtime: &Arc<RwLock<AppServerRuntimeInfo>>,
    status: &str,
    user_agent: Option<&str>,
    error: Option<&str>,
) {
    if let Ok(mut info) = runtime.write() {
        info.connected = status == "connected";
        if user_agent.is_some() {
            info.user_agent = user_agent.map(str::to_owned);
        }
    }
    let _ = events.send(json!({
        "type": "bridge_app_server_connection",
        "status": status,
        "user_agent": user_agent,
        "error": error,
    }));
}

fn latest_item_type_from_response(response: &Value, expected_turn_id: &str) -> Option<String> {
    let entry = response.get("data")?.as_array()?.first()?;
    (entry.get("turnId")?.as_str()? == expected_turn_id)
        .then(|| entry.pointer("/item/type")?.as_str().map(str::to_owned))?
}

fn queue_invocation(thread_id: &str, text: &str) -> Invocation {
    Invocation {
        backend: "codex_queue",
        args: vec![
            "queue".into(),
            "--thread".into(),
            thread_id.into(),
            "--message".into(),
            text.into(),
        ],
        cwd: None,
    }
}

fn write_rpc_message(
    websocket: &mut WebSocket<UnixStream>,
    value: &Value,
) -> Result<(), BackendFailure> {
    let payload = serde_json::to_string(value).map_err(|error| BackendFailure {
        code: "app_server_protocol_error",
        message: format!("failed to encode app-server request: {error}"),
    })?;
    websocket
        .send(Message::Text(payload.into()))
        .map_err(|error| websocket_failure("send app-server request", error))
}

fn wait_for_response(
    websocket: &mut WebSocket<UnixStream>,
    id: i64,
    timeout: Duration,
) -> Result<Value, BackendFailure> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(BackendFailure {
                code: "app_server_timeout",
                message: format!("timed out waiting for app-server response {id}"),
            });
        }
        websocket
            .get_mut()
            .set_read_timeout(Some(remaining))
            .map_err(|error| BackendFailure {
                code: "app_server_protocol_error",
                message: format!("failed to update app-server response timeout: {error}"),
            })?;
        let message = websocket
            .read()
            .map_err(|error| websocket_failure("read app-server response", error))?;
        let Message::Text(payload) = message else {
            continue;
        };
        let value = serde_json::from_str::<Value>(&payload).map_err(|error| BackendFailure {
            code: "app_server_protocol_error",
            message: format!("app-server returned invalid JSON: {error}"),
        })?;
        if value.get("id").and_then(Value::as_i64) == Some(id) {
            return Ok(value);
        }
    }
}

fn websocket_failure(context: &str, error: tungstenite::Error) -> BackendFailure {
    let code = match &error {
        tungstenite::Error::Io(error)
            if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) =>
        {
            "app_server_timeout"
        }
        _ => "app_server_protocol_error",
    };
    BackendFailure {
        code,
        message: format!("failed to {context}: {error}"),
    }
}

#[cfg(test)]
fn ensure_rpc_success(response: Value) -> Result<(), BackendFailure> {
    rpc_result(response).map(|_| ())
}

fn rpc_result(response: Value) -> Result<Value, BackendFailure> {
    if let Some(error) = response.get("error") {
        let code = error.get("code").map(Value::to_string).unwrap_or_default();
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown app-server error");
        return Err(BackendFailure {
            code: "app_server_rejected",
            message: format!("app-server error {code}: {message}"),
        });
    }
    match response.get("result") {
        Some(result) => Ok(result.clone()),
        None => Err(BackendFailure {
            code: "app_server_protocol_error",
            message: "app-server response has neither result nor error".to_owned(),
        }),
    }
}

fn captured_text(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let truncated = bytes.len() > MAX_CAPTURED_OUTPUT_BYTES;
    let bytes = &bytes[..bytes.len().min(MAX_CAPTURED_OUTPUT_BYTES)];
    let mut text = String::from_utf8_lossy(bytes).trim().to_owned();
    if truncated {
        text.push_str("\n[output truncated]");
    }
    (!text.is_empty()).then_some(text)
}

fn command_failure_message(
    backend: &str,
    exit_code: i32,
    stdout: &Option<String>,
    stderr: &Option<String>,
) -> String {
    let mut message = format!("{backend} exited with status {exit_code}");
    if let Some(stderr) = stderr {
        message.push_str(": ");
        message.push_str(stderr);
    } else if let Some(stdout) = stdout {
        message.push_str(": ");
        message.push_str(stdout);
    }
    message
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use super::*;

    static SOCKET_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn args(invocation: &Invocation) -> Vec<String> {
        invocation
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn queue_passes_message_as_one_argument_without_a_shell() {
        let invocation = queue_invocation("thread-1", "hello; $(touch nope)");
        assert_eq!(
            args(&invocation),
            [
                "queue",
                "--thread",
                "thread-1",
                "--message",
                "hello; $(touch nope)"
            ]
        );
        assert_eq!(invocation.cwd, None);
    }

    #[test]
    fn rpc_error_is_preserved_as_a_backend_failure() {
        let failure = ensure_rpc_success(json!({
            "id": 1,
            "error": {"code": -32602, "message": "turn id mismatch"}
        }))
        .unwrap_err();
        assert_eq!(failure.code, "app_server_rejected");
        assert!(failure.message.contains("turn id mismatch"));
    }

    #[test]
    fn reads_latest_item_type_only_for_the_expected_turn() {
        let response = json!({
            "data": [{
                "turnId": "turn-active",
                "item": {"id": "item-1", "type": "contextCompaction"}
            }]
        });
        assert_eq!(
            latest_item_type_from_response(&response, "turn-active").as_deref(),
            Some("contextCompaction")
        );
        assert_eq!(
            latest_item_type_from_response(&response, "turn-other"),
            None
        );
        assert_eq!(
            latest_item_type_from_response(&json!({"data": []}), "turn-active"),
            None
        );
    }

    #[test]
    fn fake_codex_exercises_queue_without_a_shell() {
        let fake_codex = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fake-codex");
        let backend = CodexCliBackend::new(fake_codex, None);

        let queue = backend.queue_message_via_cli("thread-1", "hello").unwrap();
        assert_eq!(queue.backend, "codex_queue");
        assert!(queue.stdout.unwrap().contains("<--message> <hello>"));
    }

    #[test]
    fn missing_external_endpoint_never_falls_back_to_the_standalone_socket() {
        let backend = CodexCliBackend::new(PathBuf::from("codex"), None);
        assert_eq!(backend.app_server_socket(), None);
        let failure = backend
            .app_server_rpc("thread/read", json!({}))
            .unwrap_err();
        assert_eq!(failure.code, "app_server_unavailable");
        assert!(failure.message.contains("standalone fallback is disabled"));
    }

    #[test]
    fn steer_interrupt_and_generic_rpc_use_websocket_uds() {
        let sequence = SOCKET_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let sock_dir = std::env::temp_dir().join(format!(
            "codexctl-websocket-test-{}-{sequence}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&sock_dir).unwrap();
        let sock_path = sock_dir.join("bridge.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            let stream = listener.incoming().next().unwrap().unwrap();
            let mut websocket = tungstenite::accept(stream).unwrap();
            requests.push(read_json(&mut websocket));
            websocket
                .send(Message::Text(
                    json!({"id": 0, "result": {"userAgent": "fake/0.1"}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            requests.push(read_json(&mut websocket));
            for index in 0..3 {
                requests.push(read_json(&mut websocket));
                let id = requests.last().unwrap()["id"].as_i64().unwrap();
                if index == 0 {
                    websocket
                        .send(Message::Text(
                            json!({
                                "method": "thread/status/changed",
                                "params": {"threadId": "thread-1", "status": {"type": "active", "activeFlags": []}}
                            })
                            .to_string()
                            .into(),
                        ))
                        .unwrap();
                }
                websocket
                    .send(Message::Text(
                        json!({"id": id, "result": {}}).to_string().into(),
                    ))
                    .unwrap();
            }
            requests
        });

        let backend = CodexCliBackend::new(PathBuf::from("/unused/codex"), Some(sock_path.clone()));
        let mut events = backend.subscribe_app_server_events().unwrap();

        let steer = backend
            .steer_via_app_server(
                "thread-1",
                "turn-1",
                &[json!({"type":"text", "text":"guide", "text_elements":[]})],
            )
            .unwrap();
        assert_eq!(steer.backend, "codex_app_server_steer");

        let interrupt = backend.interrupt_turn("thread-1", "turn-1").unwrap();
        assert_eq!(interrupt.backend, "codex_app_server_interrupt");

        let generic = backend
            .app_server_rpc("thread/read", json!({"threadId": "thread-1"}))
            .unwrap();
        assert_eq!(generic, json!({}));
        let received_status = (0..3).any(|_| {
            events.try_recv().ok().is_some_and(|event| {
                event.pointer("/message/method") == Some(&json!("thread/status/changed"))
            })
        });
        assert!(received_status);

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 5);
        assert_eq!(requests[0]["method"], "initialize");
        assert_eq!(requests[1]["method"], "initialized");
        assert_eq!(requests[2]["method"], "turn/steer");
        assert_eq!(requests[2]["params"]["threadId"], "thread-1");
        assert_eq!(requests[2]["params"]["expectedTurnId"], "turn-1");
        assert_eq!(requests[2]["params"]["input"][0]["text"], "guide");
        assert_eq!(requests[3]["method"], "turn/interrupt");
        assert_eq!(requests[3]["params"]["threadId"], "thread-1");
        assert_eq!(requests[3]["params"]["turnId"], "turn-1");
        assert_eq!(requests[4]["method"], "thread/read");
        assert_eq!(requests[4]["params"]["threadId"], "thread-1");

        std::fs::remove_file(&sock_path).unwrap();
        std::fs::remove_dir(&sock_dir).unwrap();
    }

    #[test]
    fn bridge_events_share_the_persistent_app_server_broadcast() {
        let (commands, _command_rx) = std_mpsc::channel();
        let (events, _) = broadcast::channel(4);
        let session = Arc::new(AppServerSession {
            commands,
            events,
            runtime: Arc::new(RwLock::new(AppServerRuntimeInfo {
                connected: true,
                user_agent: None,
            })),
        });
        let backend = CodexCliBackend {
            program: PathBuf::from("/unused/codex"),
            app_server_socket: Some(PathBuf::from("/unused/app-server.sock")),
            app_server: Some(session),
        };
        let mut receiver = backend.subscribe_app_server_events().unwrap();
        assert!(backend.publish_bridge_event(json!({
            "type": "bridge_thread_activity_snapshot",
            "active_thread_ids": ["thread-1"]
        })));
        assert_eq!(
            receiver.try_recv().unwrap()["active_thread_ids"][0],
            "thread-1"
        );
    }

    #[test]
    fn watched_threads_use_one_connection_and_evict_the_oldest_subscription() {
        let sequence = SOCKET_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let sock_dir = std::env::temp_dir().join(format!(
            "codex-thread-cache-test-{}-{sequence}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&sock_dir).unwrap();
        let sock_path = sock_dir.join("bridge.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        let server = thread::spawn(move || {
            let stream = listener.incoming().next().unwrap().unwrap();
            let mut websocket = tungstenite::accept(stream).unwrap();
            let initialize = read_json(&mut websocket);
            websocket
                .send(Message::Text(
                    json!({"id": initialize["id"], "result": {"userAgent": "fake/0.1"}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            let initialized = read_json(&mut websocket);
            let mut requests = Vec::new();
            for _ in 0..5 {
                let request = read_json(&mut websocket);
                let id = request["id"].clone();
                let result = if request["method"] == "thread/resume" {
                    json!({"thread": {"id": request["params"]["threadId"], "status": {"type": "idle"}}})
                } else {
                    json!({})
                };
                websocket
                    .send(Message::Text(
                        json!({"id": id, "result": result}).to_string().into(),
                    ))
                    .unwrap();
                requests.push(request);
            }
            (initialize, initialized, requests)
        });
        let backend = CodexCliBackend::new_with_thread_cache(
            PathBuf::from("/unused/codex"),
            Some(sock_path.clone()),
            3,
        );
        for thread_id in ["thread-1", "thread-2", "thread-3", "thread-4"] {
            backend.watch_thread(thread_id).unwrap();
        }
        let (initialize, initialized, requests) = server.join().unwrap();
        assert_eq!(initialize["method"], "initialize");
        assert_eq!(initialized["method"], "initialized");
        assert_eq!(
            requests
                .iter()
                .map(|request| request["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "thread/resume",
                "thread/resume",
                "thread/resume",
                "thread/resume",
                "thread/unsubscribe"
            ]
        );
        assert_eq!(requests[4]["params"]["threadId"], "thread-1");
        std::fs::remove_file(&sock_path).unwrap();
        std::fs::remove_dir(&sock_dir).unwrap();
    }

    #[test]
    fn native_queue_uses_stable_client_and_server_submission_ids() {
        let sequence = SOCKET_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let sock_dir = std::env::temp_dir().join(format!(
            "codex-native-queue-test-{}-{sequence}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&sock_dir).unwrap();
        let sock_path = sock_dir.join("bridge.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        let server = thread::spawn(move || {
            let stream = listener.incoming().next().unwrap().unwrap();
            let mut websocket = tungstenite::accept(stream).unwrap();
            let initialize = read_json(&mut websocket);
            websocket
                .send(Message::Text(
                    json!({"id": initialize["id"], "result": {"userAgent": "fake/0.1"}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            let _initialized = read_json(&mut websocket);
            let request = read_json(&mut websocket);
            websocket
                .send(Message::Text(
                    json!({
                        "id": request["id"],
                        "result": {"queuedSubmission": {
                            "id": "server-queue-1",
                            "clientUserMessageId": request["params"]["clientUserMessageId"],
                            "input": request["params"]["input"]
                        }}
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            let read = read_json(&mut websocket);
            websocket
                .send(Message::Text(
                    json!({"id": read["id"], "result": {"thread": {"status": {"type": "idle"}}}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            let start = read_json(&mut websocket);
            websocket
                .send(Message::Text(
                    json!({"id": start["id"], "result": {"turn": {"id": "turn-1"}}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            (request, read, start)
        });
        let backend = CodexCliBackend::new(PathBuf::from("/unused/codex"), Some(sock_path.clone()));
        let receipt = backend
            .queue_message(
                "thread-1",
                &[json!({"type":"text", "text":"queued text", "text_elements":[]})],
                "browser-message-1",
            )
            .unwrap();
        assert_eq!(receipt.backend, "app_server_queue");
        assert_eq!(receipt.queued_submission_id, "server-queue-1");
        assert_eq!(receipt.started_turn_id.as_deref(), Some("turn-1"));
        let (request, read, start) = server.join().unwrap();
        assert_eq!(request["method"], "thread/queue/add");
        assert_eq!(request["params"]["threadId"], "thread-1");
        assert_eq!(
            request["params"]["clientUserMessageId"],
            "browser-message-1"
        );
        assert_eq!(request["params"]["input"][0]["text"], "queued text");
        assert_eq!(read["method"], "thread/read");
        assert_eq!(read["params"]["includeTurns"], false);
        assert_eq!(start["method"], "thread/queue/start");
        assert_eq!(start["params"]["queuedSubmissionId"], "server-queue-1");
        std::fs::remove_file(&sock_path).unwrap();
        std::fs::remove_dir(&sock_dir).unwrap();
    }

    #[test]
    fn native_queue_activity_detection_accepts_current_status_shapes() {
        assert!(app_server_thread_is_active(
            &json!({"thread": {"status": {"type": "active"}}})
        ));
        assert!(app_server_thread_is_active(&json!({"status": "active"})));
        assert!(!app_server_thread_is_active(
            &json!({"thread": {"status": {"type": "idle"}}})
        ));
    }

    fn read_json(websocket: &mut WebSocket<UnixStream>) -> Value {
        let message = websocket.read().unwrap();
        serde_json::from_str(message.to_text().unwrap()).unwrap()
    }

    #[test]
    #[ignore = "requires an explicitly selected live app-server socket"]
    fn live_app_server_websocket_probe() {
        let socket = std::env::var_os("CODEX_BRIDGE_TEST_APP_SERVER_SOCKET")
            .map(PathBuf::from)
            .expect(
                "set CODEX_BRIDGE_TEST_APP_SERVER_SOCKET to an isolated/shared app-server socket",
            );
        let backend = CodexCliBackend::new(PathBuf::from("codex"), Some(socket));
        backend
            .run_app_server_rpc("thread/loaded/list", json!({}), "app_server_probe")
            .unwrap();
    }

    #[test]
    #[ignore = "creates and steers a new live Codex thread"]
    fn live_app_server_steers_new_isolated_thread() {
        assert_eq!(
            std::env::var("CODEX_BRIDGE_TEST_ALLOW_WRITE").as_deref(),
            Ok("1"),
            "set CODEX_BRIDGE_TEST_ALLOW_WRITE=1 to acknowledge creation of an isolated thread"
        );
        let socket = std::env::var_os("CODEX_BRIDGE_TEST_APP_SERVER_SOCKET")
            .map(PathBuf::from)
            .expect(
                "set CODEX_BRIDGE_TEST_APP_SERVER_SOCKET to an isolated/shared app-server socket",
            );
        let sequence = SOCKET_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let cwd = std::env::temp_dir().join(format!(
            "codex-bridge-live-steer-{}-{sequence}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&cwd).unwrap();

        let stream = UnixStream::connect(&socket).unwrap();
        stream.set_read_timeout(Some(RPC_TIMEOUT)).unwrap();
        stream.set_write_timeout(Some(RPC_TIMEOUT)).unwrap();
        let (mut websocket, _) = tungstenite::client("ws://localhost/", stream).unwrap();
        write_rpc_message(
            &mut websocket,
            &json!({
                "id": 0,
                "method": "initialize",
                "params": {
                    "clientInfo": {
                        "name": "codex-bridge-live-test",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {"experimentalApi": true}
                }
            }),
        )
        .unwrap();
        ensure_rpc_success(wait_for_response(&mut websocket, 0, RPC_TIMEOUT).unwrap()).unwrap();
        write_rpc_message(&mut websocket, &json!({"method": "initialized"})).unwrap();

        write_rpc_message(
            &mut websocket,
            &json!({
                "id": 10,
                "method": "thread/start",
                "params": {
                    "cwd": cwd,
                    "approvalPolicy": "never",
                    "sandbox": "workspace-write"
                }
            }),
        )
        .unwrap();
        let thread_response = wait_for_response(&mut websocket, 10, RPC_TIMEOUT).unwrap();
        ensure_rpc_success(thread_response.clone()).unwrap();
        let thread_id = thread_response["result"]["thread"]["id"]
            .as_str()
            .unwrap()
            .to_owned();

        write_rpc_message(
            &mut websocket,
            &json!({
                "id": 11,
                "method": "turn/start",
                "params": {
                    "threadId": thread_id,
                    "input": [{
                        "type": "text",
                        "text": "This is an isolated transport smoke test. Run `sleep 20`, then reply READY.",
                        "text_elements": []
                    }]
                }
            }),
        )
        .unwrap();
        let turn_response = wait_for_response(&mut websocket, 11, RPC_TIMEOUT).unwrap();
        ensure_rpc_success(turn_response.clone()).unwrap();
        let turn_id = turn_response["result"]["turn"]["id"]
            .as_str()
            .unwrap()
            .to_owned();

        eprintln!(
            "live steer target: thread={thread_id} turn={turn_id} cwd={}",
            cwd.display()
        );
        let backend = CodexCliBackend::new(PathBuf::from("codex"), Some(socket));
        backend
            .steer_via_app_server(
                &thread_id,
                &turn_id,
                &[json!({
                    "type":"text",
                    "text":"STEER_ACCEPTED: skip the sleep and finish the smoke test now.",
                    "text_elements":[]
                })],
            )
            .unwrap();
        match backend.interrupt_turn(&thread_id, &turn_id) {
            Ok(_) => {}
            Err(error)
                if error.code == "app_server_rejected"
                    && error.message.contains("no active turn to interrupt") => {}
            Err(error) => panic!("failed to clean up live smoke-test turn: {error:?}"),
        }

        let _ = websocket.close(None);
        std::fs::remove_dir(&cwd).unwrap();
    }
}
