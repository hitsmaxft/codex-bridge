//! codex-gui: CLI client for the codex-gui-bridge daemon.
//!
//! Talks to the daemon's Unix socket and renders the JSON response.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use codex_gui_bridge::cli_api::default_socket_path;
use serde_json::{json, Value};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Control Codex Desktop sessions through the codex-gui-bridge broker"
)]
struct Args {
    /// Unix socket path of the codex-gui-bridge daemon.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show broker status (active thread, recent activity).
    Status,
    /// List threads visible through the shared app-server.
    Threads {
        /// Maximum number of threads to return.
        #[arg(long, default_value_t = 100)]
        limit: u64,
    },
    /// Show the thread the GUI currently has open.
    Current,
    /// Read a full thread.
    Read {
        /// Thread id.
        thread: String,
    },
    /// List turns of a thread.
    Turns {
        /// Thread id.
        thread: String,
        #[arg(long, default_value_t = 20)]
        limit: u64,
    },
    /// Tail the most recent turns of a thread (compact render).
    Tail {
        /// Thread id.
        thread: String,
        #[arg(long, default_value_t = 5)]
        limit: u64,
    },
    /// Send a message to a thread (starts a turn in the GUI session).
    Send {
        /// Thread id.
        thread: String,
        /// Message text.
        text: String,
    },
    /// Steer an active turn with new input.
    Steer {
        /// Thread id.
        thread: String,
        /// Active turn id.
        turn: String,
        /// Steering input text.
        text: String,
    },
    /// Interrupt an active turn.
    Interrupt {
        /// Thread id.
        thread: String,
        /// Active turn id.
        turn: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let socket = args.socket.clone().unwrap_or_else(default_socket_path);
    let request = match &args.command {
        Command::Status => json!({"cmd": "status"}),
        Command::Threads { limit } => json!({"cmd": "threads", "limit": limit}),
        Command::Current => json!({"cmd": "current"}),
        Command::Read { thread } => json!({"cmd": "read", "thread": thread}),
        Command::Turns { thread, limit } => {
            json!({"cmd": "turns", "thread": thread, "limit": limit})
        }
        Command::Tail { thread, limit } => {
            json!({"cmd": "turns", "thread": thread, "limit": limit})
        }
        Command::Send { thread, text } => {
            json!({"cmd": "send", "thread": thread, "text": text})
        }
        Command::Steer { thread, turn, text } => {
            json!({"cmd": "steer", "thread": thread, "turn": turn, "text": text})
        }
        Command::Interrupt { thread, turn } => {
            json!({"cmd": "interrupt", "thread": thread, "turn": turn})
        }
    };

    let response = request_daemon(&socket, &request).await?;
    render(&args.command, response)?;
    Ok(())
}

async fn request_daemon(socket: &PathBuf, request: &Value) -> Result<Value> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("failed to connect to {}", socket.display()))?;
    stream
        .write_all(serde_json::to_string(request)?.as_bytes())
        .await?;
    stream.write_all(b"\n").await?;
    stream.shutdown().await?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .await
        .context("read daemon response")?;
    if n == 0 {
        anyhow::bail!("daemon returned an empty response (is codex-gui-bridge running?)");
    }
    let value: Value = serde_json::from_str(&line).context("daemon returned invalid JSON")?;
    Ok(value)
}

fn render(command: &Command, response: Value) -> Result<()> {
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        let error = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown daemon error");
        anyhow::bail!("{error}");
    }
    let result = &response["result"];
    match command {
        Command::Status => render_status(result),
        Command::Threads { .. } => render_threads(result),
        Command::Current => render_current(result),
        Command::Read { .. } => render_read(result),
        Command::Turns { .. } => render_turns(result),
        Command::Tail { .. } => render_tail(result),
        Command::Send { .. } => render_send(result),
        Command::Steer { .. } => render_send(result),
        Command::Interrupt { .. } => render_send(result),
    }
    Ok(())
}

fn render_status(result: &Value) {
    let connected = result["desktopConnected"].as_bool().unwrap_or(false);
    println!("desktop connected: {connected}");
    let ready = result["desktopReady"].as_bool().unwrap_or(false);
    println!("desktop initialized: {ready}");
    let active = result["activeThread"].as_str().unwrap_or("(none)");
    println!("active thread: {active}");
    if let Some(activity) = result["recentActivity"].as_array() {
        println!("recent activity ({} shown):", activity.len());
        for item in activity {
            let method = item["method"].as_str().unwrap_or("");
            let kind = item["kind"].as_str().unwrap_or("");
            let thread = item["threadId"].as_str().unwrap_or("");
            let detail = item["detail"].as_str().unwrap_or("");
            println!("  [{kind}] {method} thread={thread} {detail}");
        }
    }
}

fn render_threads(result: &Value) {
    let data = result.get("data").and_then(Value::as_array);
    match data {
        Some(threads) => {
            println!("{} thread(s):", threads.len());
            for thread in threads {
                let id = thread["id"].as_str().unwrap_or("?");
                let title = thread["title"]
                    .as_str()
                    .or_else(|| thread["cwd"].as_str())
                    .unwrap_or("");
                println!("{id}  {title}");
            }
        }
        None => println!("{}", pretty(result)),
    }
}

fn render_current(result: &Value) {
    match result["threadId"].as_str() {
        Some(id) => println!("{id}"),
        None => println!("(no active thread observed yet)"),
    }
}

fn render_read(result: &Value) {
    let thread = result.get("thread").or_else(|| result.get("result"));
    match thread {
        Some(value) => println!("{}", pretty(value)),
        None => println!("{}", pretty(result)),
    }
}

fn render_turns(result: &Value) {
    println!("{}", pretty(result));
}

fn render_tail(result: &Value) {
    // Compact render: thread title + recent turn text.
    if let Some(data) = result.get("data").and_then(Value::as_array) {
        for turn in data {
            if let Some(role) = turn["role"].as_str() {
                let text = extract_turn_text(turn);
                println!("[{role}] {text}");
            } else {
                println!("{}", pretty(turn));
            }
        }
    } else {
        println!("{}", pretty(result));
    }
}

fn extract_turn_text(turn: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(text) = turn["text"].as_str() {
        parts.push(text.to_owned());
    }
    if let Some(items) = turn["items"].as_array() {
        for item in items {
            if let Some(text) = item["text"].as_str() {
                parts.push(text.to_owned());
            }
        }
    }
    let joined = parts.join("\n");
    let truncated: String = joined.chars().take(400).collect();
    if joined.chars().count() > 400 {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn render_send(result: &Value) {
    // Ack: print a short status rather than dumping the whole response.
    if let Some(error) = result.get("error") {
        println!("rejected: {}", error);
    } else if let Some(turn) = result
        .get("turn")
        .and_then(|t| t.get("id"))
        .and_then(Value::as_str)
    {
        println!("turn started: {turn}");
    } else {
        println!("ok: {}", pretty(result));
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}
