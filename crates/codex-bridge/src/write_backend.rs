use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

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

    pub fn steer_with_resume(
        &self,
        thread_id: &str,
        cwd: &Path,
        text: &str,
    ) -> Result<BackendSuccess, BackendFailure> {
        self.run(steer_invocation(thread_id, cwd, text))
    }

    pub fn interrupt_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<BackendSuccess, BackendFailure> {
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

        self.run_interrupt_proxy(thread_id, turn_id)
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

    fn run_interrupt_proxy(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<BackendSuccess, BackendFailure> {
        let mut child = Command::new(&self.program)
            .args([
                OsString::from("app-server"),
                OsString::from("proxy"),
                OsString::from("--sock"),
                self.app_server_socket.as_os_str().to_owned(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| BackendFailure {
                code: "codex_cli_unavailable",
                message: format!(
                    "failed to execute app-server proxy using {}: {error}",
                    self.program.display()
                ),
            })?;

        let mut stdin = child.stdin.take().expect("piped child stdin");
        let stdout = child.stdout.take().expect("piped child stdout");
        let stderr = child.stderr.take().expect("piped child stderr");

        let (line_sender, line_receiver) = mpsc::channel();
        let stdout_reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if line_sender.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr_reader = thread::spawn(move || {
            let mut reader = stderr;
            let mut bytes = Vec::new();
            let _ = reader.read_to_end(&mut bytes);
            bytes
        });

        let attempt = (|| {
            write_rpc_line(
                &mut stdin,
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
            ensure_rpc_success(wait_for_response(&line_receiver, 0, RPC_TIMEOUT)?)?;

            write_rpc_line(&mut stdin, &json!({"method": "initialized"}))?;
            write_rpc_line(
                &mut stdin,
                &json!({
                    "id": 1,
                    "method": "turn/interrupt",
                    "params": {"threadId": thread_id, "turnId": turn_id}
                }),
            )?;
            ensure_rpc_success(wait_for_response(&line_receiver, 1, RPC_TIMEOUT)?)
        })();

        drop(stdin);
        let _ = child.kill();
        let status = child.wait().ok();
        let _ = stdout_reader.join();
        let stderr = stderr_reader.join().unwrap_or_default();
        let stderr = captured_text(&stderr);

        attempt.map_err(|mut error| {
            if let Some(stderr) = &stderr {
                error.message.push_str("; proxy stderr: ");
                error.message.push_str(stderr);
            }
            error
        })?;

        Ok(BackendSuccess {
            backend: "codex_app_server_interrupt".to_owned(),
            exit_code: status.and_then(|status| status.code()).unwrap_or(0),
            stdout: None,
            stderr,
        })
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

fn steer_invocation(thread_id: &str, cwd: &Path, text: &str) -> Invocation {
    Invocation {
        backend: "codex_exec_resume",
        args: vec![
            "exec".into(),
            "resume".into(),
            thread_id.into(),
            text.into(),
        ],
        cwd: Some(cwd.to_path_buf()),
    }
}

fn write_rpc_line(writer: &mut impl Write, value: &Value) -> Result<(), BackendFailure> {
    serde_json::to_writer(&mut *writer, value).map_err(|error| BackendFailure {
        code: "app_server_protocol_error",
        message: format!("failed to encode app-server request: {error}"),
    })?;
    writer.write_all(b"\n").map_err(|error| BackendFailure {
        code: "app_server_protocol_error",
        message: format!("failed to write app-server request: {error}"),
    })?;
    writer.flush().map_err(|error| BackendFailure {
        code: "app_server_protocol_error",
        message: format!("failed to flush app-server request: {error}"),
    })
}

fn wait_for_response(
    receiver: &Receiver<std::io::Result<String>>,
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
        let line = match receiver.recv_timeout(remaining) {
            Ok(Ok(line)) => line,
            Ok(Err(error)) => {
                return Err(BackendFailure {
                    code: "app_server_protocol_error",
                    message: format!("failed to read app-server response: {error}"),
                });
            }
            Err(RecvTimeoutError::Timeout) => {
                return Err(BackendFailure {
                    code: "app_server_timeout",
                    message: format!("timed out waiting for app-server response {id}"),
                });
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(BackendFailure {
                    code: "app_server_protocol_error",
                    message: format!("app-server proxy closed before response {id}"),
                });
            }
        };
        let value = serde_json::from_str::<Value>(&line).map_err(|error| BackendFailure {
            code: "app_server_protocol_error",
            message: format!("app-server proxy returned invalid JSON: {error}"),
        })?;
        if value.get("id").and_then(Value::as_i64) == Some(id) {
            return Ok(value);
        }
    }
}

fn ensure_rpc_success(response: Value) -> Result<(), BackendFailure> {
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
    if response.get("result").is_none() {
        return Err(BackendFailure {
            code: "app_server_protocol_error",
            message: "app-server response has neither result nor error".to_owned(),
        });
    }
    Ok(())
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
    use super::*;

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
    fn steer_uses_exec_resume_in_the_thread_workspace() {
        let invocation = steer_invocation("thread-1", Path::new("/tmp/project"), "guide it");
        assert_eq!(
            args(&invocation),
            ["exec", "resume", "thread-1", "guide it"]
        );
        assert_eq!(invocation.cwd.as_deref(), Some(Path::new("/tmp/project")));
    }

    #[test]
    fn interrupt_rpc_requires_both_thread_and_turn_ids() {
        let mut bytes = Vec::new();
        write_rpc_line(
            &mut bytes,
            &json!({
                "id": 1,
                "method": "turn/interrupt",
                "params": {"threadId": "thread-1", "turnId": "turn-1"}
            }),
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["method"], "turn/interrupt");
        assert_eq!(value["params"]["threadId"], "thread-1");
        assert_eq!(value["params"]["turnId"], "turn-1");
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
    fn fake_codex_exercises_queue_resume_and_interrupt_processes() {
        let fake_codex = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fake-codex");
        let backend = CodexCliBackend::new(fake_codex, PathBuf::from("/unused/fake.sock"));

        let queue = backend.queue_message("thread-1", "hello").unwrap();
        assert_eq!(queue.backend, "codex_queue");
        assert!(queue.stdout.unwrap().contains("<--message> <hello>"));

        let steer = backend
            .steer_with_resume("thread-1", Path::new("/tmp"), "guide")
            .unwrap();
        assert_eq!(steer.backend, "codex_exec_resume");
        assert!(steer
            .stdout
            .unwrap()
            .contains("<resume> <thread-1> <guide>"));

        let interrupt = backend.run_interrupt_proxy("thread-1", "turn-1").unwrap();
        assert_eq!(interrupt.backend, "codex_app_server_interrupt");
    }
}
