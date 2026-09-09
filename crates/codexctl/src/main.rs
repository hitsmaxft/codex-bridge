use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use codex_bridge::{default_socket_path, Request, Response, ScrollDirection};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Control a running Codex Desktop through codex-bridge"
)]
struct Cli {
    /// Override the bridge control socket path.
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<PathBuf>,

    /// Target thread for show/send/steer/interrupt/host-exec, overriding daemon selection.
    #[arg(long, global = true, value_name = "THREAD_ID")]
    thread: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List known Codex threads.
    Ls {
        /// Maximum number of threads to return.
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=1000))]
        limit: u32,

        /// Include archived rollouts in addition to unarchived ones.
        #[arg(long)]
        include_archived: bool,

        /// Emit structured JSON.
        #[arg(long)]
        json: bool,
    },
    /// Select the default thread used by subsequent commands.
    Select {
        /// Thread UUID returned by `codexctl ls`.
        #[arg(value_name = "THREAD_ID")]
        thread_id: String,

        /// Emit structured JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show the selected thread or most recently modified unarchived rollout.
    Current {
        /// Emit structured JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show bridge and rollout-store status.
    Status {
        /// Emit structured JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show messages from --thread, the selected thread, or the latest rollout.
    Show {
        /// Legacy positional form of --thread.
        #[arg(value_name = "THREAD_ID")]
        thread_id: Option<String>,

        /// Return only the last N user/assistant messages.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=10000))]
        last: Option<u32>,

        /// Emit structured JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stream new items from the current thread.
    Tail,
    /// Queue a new turn through `codex queue` without taking writer ownership.
    Send {
        #[arg(required = true, value_name = "TEXT")]
        text: Vec<String>,

        /// Emit structured JSON.
        #[arg(long)]
        json: bool,
    },
    /// Inject guidance into the active turn through the shared app-server.
    Steer {
        #[arg(required = true, value_name = "TEXT")]
        text: Vec<String>,

        /// Emit structured JSON.
        #[arg(long)]
        json: bool,
    },
    /// Scroll the Codex conversation UI.
    Scroll(ScrollArgs),
    /// List pending approval requests.
    Pending,
    /// Approve a pending request.
    Approve { id: u64 },
    /// Decline a pending request.
    Decline { id: u64 },
    /// Interrupt the active turn through the shared app-server.
    Interrupt {
        /// Emit structured JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run an allowlisted command on the host in the target thread workspace.
    HostExec {
        /// Kill the command after this many seconds (daemon default: 300).
        #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout: Option<u64>,

        /// Emit structured JSON.
        #[arg(long)]
        json: bool,

        /// Command and arguments. Use `--` before values that start with `-`.
        #[arg(
            required = true,
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "COMMAND"
        )]
        argv: Vec<String>,
    },
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("mode")
        .required(true)
        .multiple(false)
        .args(["direction", "pixels", "target", "message_id"])
))]
struct ScrollArgs {
    #[arg(value_enum)]
    direction: Option<DirectionArg>,

    /// Dispatch a wheel event by a signed pixel distance.
    #[arg(long, allow_hyphen_values = true)]
    pixels: Option<i64>,

    /// Scroll to a semantic target, such as "bottom".
    #[arg(long = "to", value_name = "TARGET")]
    target: Option<String>,

    /// Scroll to a message by its structured identifier.
    #[arg(long = "to-message", value_name = "ID")]
    message_id: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DirectionArg {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy)]
enum OutputKind {
    Generic,
    Threads,
    Selection,
    Current,
    Messages,
    Write,
    HostExec,
}

impl From<DirectionArg> for ScrollDirection {
    fn from(value: DirectionArg) -> Self {
        match value {
            DirectionArg::Up => Self::Up,
            DirectionArg::Down => Self::Down,
        }
    }
}

impl Command {
    fn into_request(self, target_thread: Option<String>) -> Result<(Request, bool, OutputKind)> {
        match self {
            Self::Ls {
                limit,
                include_archived,
                json,
            } => {
                reject_target(&target_thread, "ls")?;
                Ok((
                    Request::Ls {
                        limit,
                        include_archived,
                    },
                    json,
                    OutputKind::Threads,
                ))
            }
            Self::Select { thread_id, json } => {
                reject_target(&target_thread, "select")?;
                Ok((Request::Select { thread_id }, json, OutputKind::Selection))
            }
            Self::Current { json } => {
                reject_target(&target_thread, "current")?;
                Ok((Request::Current, json, OutputKind::Current))
            }
            Self::Status { json } => {
                reject_target(&target_thread, "status")?;
                Ok((Request::Status, json, OutputKind::Generic))
            }
            Self::Show {
                thread_id,
                last,
                json,
            } => Ok((
                Request::Show {
                    thread_id: merge_target(target_thread, thread_id)?,
                    last,
                },
                json,
                OutputKind::Messages,
            )),
            Self::Tail => {
                reject_target(&target_thread, "tail")?;
                Ok((Request::Tail, false, OutputKind::Generic))
            }
            Self::Send { text, json } => Ok((
                Request::Send {
                    thread_id: target_thread,
                    text: text.join(" "),
                    attachments: Vec::new(),
                },
                json,
                OutputKind::Write,
            )),
            Self::Steer { text, json } => Ok((
                Request::Steer {
                    thread_id: target_thread,
                    text: text.join(" "),
                    attachments: Vec::new(),
                },
                json,
                OutputKind::Write,
            )),
            Self::Scroll(args) => {
                reject_target(&target_thread, "scroll")?;
                Ok((
                    Request::Scroll {
                        direction: args.direction.map(Into::into),
                        pixels: args.pixels,
                        target: args.target,
                        message_id: args.message_id,
                    },
                    false,
                    OutputKind::Generic,
                ))
            }
            Self::Pending => {
                reject_target(&target_thread, "pending")?;
                Ok((Request::Pending, false, OutputKind::Generic))
            }
            Self::Approve { id } => {
                reject_target(&target_thread, "approve")?;
                Ok((Request::Approve { id }, false, OutputKind::Generic))
            }
            Self::Decline { id } => {
                reject_target(&target_thread, "decline")?;
                Ok((Request::Decline { id }, false, OutputKind::Generic))
            }
            Self::Interrupt { json } => Ok((
                Request::Interrupt {
                    thread_id: target_thread,
                    turn_id: None,
                },
                json,
                OutputKind::Write,
            )),
            Self::HostExec {
                timeout,
                json,
                argv,
            } => Ok((
                Request::HostExec {
                    thread_id: target_thread,
                    argv,
                    timeout_seconds: timeout,
                },
                json,
                OutputKind::HostExec,
            )),
        }
    }
}

fn merge_target(
    target_thread: Option<String>,
    positional_thread: Option<String>,
) -> Result<Option<String>> {
    match (target_thread, positional_thread) {
        (Some(_), Some(_)) => bail!("pass either --thread or positional THREAD_ID, not both"),
        (Some(thread), None) | (None, Some(thread)) => Ok(Some(thread)),
        (None, None) => Ok(None),
    }
}

fn reject_target(target_thread: &Option<String>, command: &str) -> Result<()> {
    if target_thread.is_some() {
        bail!("--thread is not supported by {command}");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket_path = match cli.socket {
        Some(path) => path,
        None => default_socket_path()?,
    };
    let (request, json_output, output_kind) = cli.command.into_request(cli.thread)?;
    let response = send_request(&socket_path, &request).await?;

    if !response.ok {
        let error = response
            .error
            .context("bridge returned an unsuccessful response without an error")?;
        bail!("{}: {}", error.code, error.message);
    }

    let result = response.result.unwrap_or(serde_json::Value::Null);
    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print_human(&result, output_kind)?;
    }
    if let Some(message) = host_exec_failure(&result, output_kind) {
        bail!(message);
    }
    Ok(())
}

async fn send_request(socket_path: &Path, request: &Request) -> Result<Response> {
    let mut stream = UnixStream::connect(socket_path).await.with_context(|| {
        format!(
            "cannot connect to codex-bridge at {}",
            socket_path.display()
        )
    })?;
    let mut encoded = serde_json::to_vec(request).context("failed to encode request")?;
    encoded.push(b'\n');
    stream
        .write_all(&encoded)
        .await
        .context("failed to send request")?;

    let mut reader = BufReader::new(stream).take(MAX_RESPONSE_BYTES + 1);
    let mut line = String::new();
    let bytes_read = reader
        .read_line(&mut line)
        .await
        .context("failed to read bridge response")?;
    if bytes_read == 0 {
        bail!("codex-bridge closed the connection without a response");
    }
    if bytes_read as u64 > MAX_RESPONSE_BYTES {
        bail!("codex-bridge response exceeds 16 MiB");
    }

    serde_json::from_str(&line).context("codex-bridge returned invalid JSON")
}

fn print_human(value: &serde_json::Value, output_kind: OutputKind) -> Result<()> {
    match output_kind {
        OutputKind::Threads => return print_threads(value),
        OutputKind::Selection => return print_selection(value),
        OutputKind::Current => return print_current(value),
        OutputKind::Messages => return print_messages(value),
        OutputKind::Write => return print_write_result(value),
        OutputKind::HostExec => return print_host_exec_result(value),
        OutputKind::Generic => {}
    }

    match value {
        serde_json::Value::Null => {}
        serde_json::Value::String(text) => println!("{text}"),
        serde_json::Value::Object(fields) => {
            for (name, value) in fields {
                match value {
                    serde_json::Value::String(text) => println!("{name}: {text}"),
                    value => println!("{name}: {value}"),
                }
            }
        }
        value => println!("{}", serde_json::to_string_pretty(value)?),
    }
    Ok(())
}

fn print_threads(value: &serde_json::Value) -> Result<()> {
    let threads = value
        .get("threads")
        .and_then(serde_json::Value::as_array)
        .context("bridge response has no threads array")?;
    for thread in threads {
        let id = string_value(thread, "id").unwrap_or("<unknown>");
        let title = string_value(thread, "title").unwrap_or("<untitled>");
        let (cwd, git_branch) = thread_location(thread);
        let archived = thread
            .get("archived")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let marker = if archived { "archived" } else { "unarchived" };
        println!("{id}\t{marker}\t{title}\t{cwd}\t{git_branch}");
    }

    let returned = value.get("returned").and_then(serde_json::Value::as_u64);
    let available = value.get("available").and_then(serde_json::Value::as_u64);
    if let (Some(returned), Some(available)) = (returned, available) {
        if returned < available {
            eprintln!("showing {returned} of {available} threads; increase --limit to see more");
        }
    }
    Ok(())
}

fn print_current(value: &serde_json::Value) -> Result<()> {
    let thread = value
        .get("thread")
        .context("bridge response has no thread")?;
    println!("id: {}", string_value(thread, "id").unwrap_or("<unknown>"));
    println!(
        "title: {}",
        string_value(thread, "title").unwrap_or("<untitled>")
    );
    print_thread_location(thread);
    let selection = value
        .get("selection")
        .and_then(|selection| selection.get("method"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    println!("selection: {selection}");
    Ok(())
}

fn print_selection(value: &serde_json::Value) -> Result<()> {
    let thread = value
        .get("thread")
        .context("bridge response has no selected thread")?;
    println!(
        "selected: {} ({})",
        string_value(thread, "title").unwrap_or("<untitled>"),
        string_value(thread, "id").unwrap_or("<unknown>")
    );
    print_thread_location(thread);
    Ok(())
}

fn print_write_result(value: &serde_json::Value) -> Result<()> {
    let action = string_value(value, "action").unwrap_or("write");
    let status = string_value(value, "status").unwrap_or("completed");
    let thread_id = string_value(value, "thread_id").unwrap_or("<unknown>");
    let backend = value
        .get("backend")
        .and_then(|backend| backend.get("backend"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<unknown>");
    println!("{action}: {status}");
    println!("thread: {thread_id}");
    println!("backend: {backend}");
    if let Some(stdout) = value
        .get("backend")
        .and_then(|backend| backend.get("stdout"))
        .and_then(serde_json::Value::as_str)
    {
        println!("{stdout}");
    }
    Ok(())
}

fn print_host_exec_result(value: &serde_json::Value) -> Result<()> {
    let execution = value
        .get("execution")
        .context("bridge response has no host execution result")?;
    let status = string_value(value, "status").unwrap_or("unknown");
    let exit_code = execution
        .get("exit_code")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(-1);
    let duration_ms = execution
        .get("duration_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    println!("host-exec: {status} (exit {exit_code}, {duration_ms} ms)");
    println!(
        "cwd: {}",
        string_value(execution, "cwd").unwrap_or("<unknown>")
    );
    println!(
        "policy: {}",
        string_value(execution, "policy_rule").unwrap_or("<unknown>")
    );
    if let Some(stdout) = string_value(execution, "stdout") {
        print!("{stdout}");
        if !stdout.ends_with('\n') {
            println!();
        }
    }
    if execution
        .get("stdout_truncated")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        println!("[stdout truncated]");
    }
    if let Some(stderr) = string_value(execution, "stderr") {
        eprint!("{stderr}");
        if !stderr.ends_with('\n') {
            eprintln!();
        }
    }
    if execution
        .get("stderr_truncated")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        eprintln!("[stderr truncated]");
    }
    Ok(())
}

fn host_exec_failure(value: &serde_json::Value, output_kind: OutputKind) -> Option<String> {
    if !matches!(output_kind, OutputKind::HostExec) {
        return None;
    }
    let execution = value.get("execution")?;
    if execution
        .get("timed_out")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Some("host command timed out".to_owned());
    }
    let exit_code = execution.get("exit_code")?.as_i64()?;
    (exit_code != 0).then(|| format!("host command exited with status {exit_code}"))
}

fn print_messages(value: &serde_json::Value) -> Result<()> {
    let thread = value
        .get("thread")
        .context("bridge response has no thread")?;
    println!(
        "thread: {} ({})",
        string_value(thread, "title").unwrap_or("<untitled>"),
        string_value(thread, "id").unwrap_or("<unknown>")
    );
    print_thread_location(thread);

    let messages = value
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .context("bridge response has no messages array")?;
    for message in messages {
        let role = string_value(message, "role").unwrap_or("unknown");
        let phase = string_value(message, "phase")
            .map(|phase| format!("/{phase}"))
            .unwrap_or_default();
        println!("\n[{role}{phase}]");
        let content = message
            .get("content")
            .and_then(serde_json::Value::as_array)
            .context("message has no content array")?;
        for item in content {
            if let Some(text) = item.get("text").and_then(serde_json::Value::as_str) {
                println!("{text}");
            } else {
                println!("{}", serde_json::to_string(item)?);
            }
        }
    }
    Ok(())
}

fn string_value<'a>(value: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(serde_json::Value::as_str)
}

fn thread_location(thread: &serde_json::Value) -> (&str, &str) {
    (
        string_value(thread, "cwd").unwrap_or("<unknown>"),
        string_value(thread, "git_branch").unwrap_or("<unknown>"),
    )
}

fn print_thread_location(thread: &serde_json::Value) {
    let (cwd, git_branch) = thread_location(thread);
    println!("cwd: {cwd}");
    println!("git branch: {git_branch}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_joins_unquoted_message_words() {
        let cli = Cli::try_parse_from([
            "codexctl", "--thread", "thread-1", "send", "keep", "going", "--json",
        ])
        .unwrap();
        let (request, json, _) = cli.command.into_request(cli.thread).unwrap();

        assert!(json);
        assert_eq!(
            request,
            Request::Send {
                thread_id: Some("thread-1".into()),
                text: "keep going".into(),
                attachments: Vec::new(),
            }
        );
    }

    #[test]
    fn scroll_requires_exactly_one_mode() {
        assert!(Cli::try_parse_from(["codexctl", "scroll"]).is_err());
        assert!(Cli::try_parse_from(["codexctl", "scroll", "down", "--to", "bottom"]).is_err());
        assert!(Cli::try_parse_from(["codexctl", "scroll", "--to", "bottom"]).is_ok());
    }

    #[test]
    fn show_accepts_an_explicit_thread_id() {
        let cli =
            Cli::try_parse_from(["codexctl", "show", "thread-1", "--last", "5", "--json"]).unwrap();
        let (request, json, _) = cli.command.into_request(cli.thread).unwrap();

        assert!(json);
        assert_eq!(
            request,
            Request::Show {
                thread_id: Some("thread-1".into()),
                last: Some(5),
            }
        );
    }

    #[test]
    fn select_and_interrupt_use_explicit_targeting() {
        let cli = Cli::try_parse_from(["codexctl", "select", "thread-1", "--json"]).unwrap();
        let (request, json, _) = cli.command.into_request(cli.thread).unwrap();
        assert!(json);
        assert_eq!(
            request,
            Request::Select {
                thread_id: "thread-1".into()
            }
        );

        let cli = Cli::try_parse_from(["codexctl", "interrupt", "--thread", "thread-2", "--json"])
            .unwrap();
        let (request, json, _) = cli.command.into_request(cli.thread).unwrap();
        assert!(json);
        assert_eq!(
            request,
            Request::Interrupt {
                thread_id: Some("thread-2".into()),
                turn_id: None,
            }
        );
    }

    #[test]
    fn show_rejects_two_thread_targets() {
        let cli =
            Cli::try_parse_from(["codexctl", "--thread", "thread-1", "show", "thread-2"]).unwrap();
        assert!(cli.command.into_request(cli.thread).is_err());
    }

    #[test]
    fn session_location_formats_branch_and_null_as_unknown() {
        let thread = serde_json::json!({"cwd": "/tmp/project", "git_branch": "main"});
        assert_eq!(thread_location(&thread), ("/tmp/project", "main"));

        let thread = serde_json::json!({"cwd": "/tmp/deleted", "git_branch": null});
        assert_eq!(thread_location(&thread), ("/tmp/deleted", "<unknown>"));
    }

    #[test]
    fn host_exec_preserves_argv_and_accepts_hyphenated_arguments() {
        let cli = Cli::try_parse_from([
            "codexctl",
            "--thread",
            "thread-1",
            "host-exec",
            "--timeout",
            "45",
            "--json",
            "--",
            "wlink",
            "--probe",
            "1",
        ])
        .unwrap();
        let (request, json, output) = cli.command.into_request(cli.thread).unwrap();

        assert!(json);
        assert!(matches!(output, OutputKind::HostExec));
        assert_eq!(
            request,
            Request::HostExec {
                thread_id: Some("thread-1".into()),
                argv: vec!["wlink".into(), "--probe".into(), "1".into()],
                timeout_seconds: Some(45),
            }
        );
    }

    #[test]
    fn host_exec_failure_detects_timeout_and_nonzero_exit() {
        let timed_out = serde_json::json!({
            "execution": {"timed_out": true, "exit_code": -1}
        });
        assert_eq!(
            host_exec_failure(&timed_out, OutputKind::HostExec).as_deref(),
            Some("host command timed out")
        );

        let failed = serde_json::json!({
            "execution": {"timed_out": false, "exit_code": 7}
        });
        assert_eq!(
            host_exec_failure(&failed, OutputKind::HostExec).as_deref(),
            Some("host command exited with status 7")
        );
    }
}
