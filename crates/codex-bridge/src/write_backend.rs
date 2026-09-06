use std::ffi::OsString;
use std::io::ErrorKind;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tungstenite::{Message, WebSocket};

pub const CODEX_BIN_ENV: &str = "CODEX_BRIDGE_CODEX_BIN";
pub const APP_SERVER_SOCKET_ENV: &str = "CODEX_BRIDGE_APP_SERVER_SOCKET";

const MAX_CAPTURED_OUTPUT_BYTES: usize = 64 * 1024;
const RPC_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct CodexCliBackend {
    program: PathBuf,
    app_server_socket: PathBuf,
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
    pub fn new(program: PathBuf, app_server_socket: PathBuf) -> Self {
        Self {
            program,
            app_server_socket,
        }
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn app_server_socket(&self) -> &Path {
        &self.app_server_socket
    }

    pub fn queue_message(
        &self,
        thread_id: &str,
        text: &str,
    ) -> Result<BackendSuccess, BackendFailure> {
        self.run(queue_invocation(thread_id, text))
    }

    pub fn steer_via_app_server(
        &self,
        thread_id: &str,
        turn_id: &str,
        text: &str,
    ) -> Result<BackendSuccess, BackendFailure> {
        let params = json!({
            "threadId": thread_id,
            "input": [{
                "type": "text",
                "text": text,
                "text_elements": []
            }],
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
        self.with_app_server(method, params)
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
        self.with_app_server(method, params)?;
        Ok(BackendSuccess {
            backend: backend_name.to_owned(),
            exit_code: 0,
            stdout: None,
            stderr: None,
        })
    }

    fn with_app_server(&self, method: &str, params: Value) -> Result<Value, BackendFailure> {
        let metadata =
            std::fs::metadata(&self.app_server_socket).map_err(|error| BackendFailure {
                code: "app_server_unavailable",
                message: format!(
                    "cannot inspect app-server socket {}: {error}",
                    self.app_server_socket.display()
                ),
            })?;
        if !metadata.file_type().is_socket() {
            return Err(BackendFailure {
                code: "app_server_unavailable",
                message: format!(
                    "app-server endpoint is not a Unix socket: {}",
                    self.app_server_socket.display()
                ),
            });
        }

        let stream =
            UnixStream::connect(&self.app_server_socket).map_err(|error| BackendFailure {
                code: "app_server_unavailable",
                message: format!(
                    "failed to connect to app-server socket {}: {error}",
                    self.app_server_socket.display()
                ),
            })?;
        stream
            .set_read_timeout(Some(RPC_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(RPC_TIMEOUT)))
            .map_err(|error| BackendFailure {
                code: "app_server_protocol_error",
                message: format!("failed to configure app-server socket timeout: {error}"),
            })?;
        let (mut websocket, _response) =
            tungstenite::client("ws://localhost/", stream).map_err(|error| match error {
                tungstenite::HandshakeError::Failure(error) => {
                    websocket_failure("upgrade app-server control socket", error)
                }
                tungstenite::HandshakeError::Interrupted(_) => BackendFailure {
                    code: "app_server_protocol_error",
                    message: "app-server WebSocket handshake was interrupted".to_owned(),
                },
            })?;

        write_rpc_message(
            &mut websocket,
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
        ensure_rpc_success(wait_for_response(&mut websocket, 0, RPC_TIMEOUT)?)?;

        write_rpc_message(&mut websocket, &json!({"method": "initialized"}))?;
        write_rpc_message(
            &mut websocket,
            &json!({
                "id": 1,
                "method": method,
                "params": params
            }),
        )?;
        let result = rpc_result(wait_for_response(&mut websocket, 1, RPC_TIMEOUT)?)?;
        let _ = websocket.close(None);
        Ok(result)
    }
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
    fn fake_codex_exercises_queue_without_a_shell() {
        let fake_codex = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fake-codex");
        let backend = CodexCliBackend::new(fake_codex, PathBuf::from("/unused/fake.sock"));

        let queue = backend.queue_message("thread-1", "hello").unwrap();
        assert_eq!(queue.backend, "codex_queue");
        assert!(queue.stdout.unwrap().contains("<--message> <hello>"));
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
            for stream in listener.incoming().take(3) {
                let mut websocket = tungstenite::accept(stream.unwrap()).unwrap();

                requests.push(read_json(&mut websocket));
                websocket
                    .send(Message::Text(
                        json!({"id": 0, "result": {"userAgent": "fake/0.1"}})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();

                requests.push(read_json(&mut websocket));
                requests.push(read_json(&mut websocket));
                websocket
                    .send(Message::Text(
                        json!({"id": 1, "result": {}}).to_string().into(),
                    ))
                    .unwrap();
            }
            requests
        });

        let backend = CodexCliBackend::new(PathBuf::from("/unused/codex"), sock_path.clone());

        let steer = backend
            .steer_via_app_server("thread-1", "turn-1", "guide")
            .unwrap();
        assert_eq!(steer.backend, "codex_app_server_steer");

        let interrupt = backend.interrupt_turn("thread-1", "turn-1").unwrap();
        assert_eq!(interrupt.backend, "codex_app_server_interrupt");

        let generic = backend
            .app_server_rpc("thread/read", json!({"threadId": "thread-1"}))
            .unwrap();
        assert_eq!(generic, json!({}));

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 9);
        assert_eq!(requests[0]["method"], "initialize");
        assert_eq!(requests[1]["method"], "initialized");
        assert_eq!(requests[2]["method"], "turn/steer");
        assert_eq!(requests[2]["params"]["threadId"], "thread-1");
        assert_eq!(requests[2]["params"]["expectedTurnId"], "turn-1");
        assert_eq!(requests[2]["params"]["input"][0]["text"], "guide");
        assert_eq!(requests[3]["method"], "initialize");
        assert_eq!(requests[4]["method"], "initialized");
        assert_eq!(requests[5]["method"], "turn/interrupt");
        assert_eq!(requests[5]["params"]["threadId"], "thread-1");
        assert_eq!(requests[5]["params"]["turnId"], "turn-1");
        assert_eq!(requests[6]["method"], "initialize");
        assert_eq!(requests[7]["method"], "initialized");
        assert_eq!(requests[8]["method"], "thread/read");
        assert_eq!(requests[8]["params"]["threadId"], "thread-1");

        std::fs::remove_file(&sock_path).unwrap();
        std::fs::remove_dir(&sock_dir).unwrap();
    }

    fn read_json(websocket: &mut WebSocket<UnixStream>) -> Value {
        let message = websocket.read().unwrap();
        serde_json::from_str(message.to_text().unwrap()).unwrap()
    }

    #[test]
    #[ignore = "requires an explicitly selected live standalone app-server socket"]
    fn live_app_server_websocket_probe() {
        let socket = std::env::var_os("CODEX_BRIDGE_TEST_APP_SERVER_SOCKET")
            .map(PathBuf::from)
            .expect("set CODEX_BRIDGE_TEST_APP_SERVER_SOCKET to an isolated/shared daemon socket");
        let backend = CodexCliBackend::new(PathBuf::from("codex"), socket);
        backend
            .run_app_server_rpc("thread/loaded/list", json!({}), "app_server_probe")
            .unwrap();
    }

    #[test]
    #[ignore = "creates and steers a new standalone Codex thread"]
    fn live_app_server_steers_new_isolated_thread() {
        assert_eq!(
            std::env::var("CODEX_BRIDGE_TEST_ALLOW_WRITE").as_deref(),
            Ok("1"),
            "set CODEX_BRIDGE_TEST_ALLOW_WRITE=1 to acknowledge creation of an isolated thread"
        );
        let socket = std::env::var_os("CODEX_BRIDGE_TEST_APP_SERVER_SOCKET")
            .map(PathBuf::from)
            .expect("set CODEX_BRIDGE_TEST_APP_SERVER_SOCKET to a standalone daemon socket");
        let sequence = SOCKET_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let cwd = std::env::temp_dir().join(format!(
            "codexapp-cli-live-steer-{}-{sequence}",
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
        let backend = CodexCliBackend::new(PathBuf::from("codex"), socket);
        backend
            .steer_via_app_server(
                &thread_id,
                &turn_id,
                "STEER_ACCEPTED: skip the sleep and finish the smoke test now.",
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
