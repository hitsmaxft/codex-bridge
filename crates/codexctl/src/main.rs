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

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List known Codex threads.
    Ls,
    /// Show the thread associated with the active Codex window.
    Current,
    /// Show bridge and active-thread status.
    Status,
    /// Show the current thread's messages.
    Show {
        /// Emit structured JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stream new items from the current thread.
    Tail,
    /// Send a new message through the best available write path.
    Send {
        #[arg(required = true, value_name = "TEXT")]
        text: Vec<String>,
    },
    /// Steer the active turn without starting a separate request.
    Steer {
        #[arg(required = true, value_name = "TEXT")]
        text: Vec<String>,
    },
    /// Scroll the Codex conversation UI.
    Scroll(ScrollArgs),
    /// List pending approval requests.
    Pending,
    /// Approve a pending request.
    Approve { id: u64 },
    /// Decline a pending request.
    Decline { id: u64 },
    /// Interrupt the active turn.
    Interrupt,
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

impl From<DirectionArg> for ScrollDirection {
    fn from(value: DirectionArg) -> Self {
        match value {
            DirectionArg::Up => Self::Up,
            DirectionArg::Down => Self::Down,
        }
    }
}

impl Command {
    fn into_request(self) -> (Request, bool) {
        match self {
            Self::Ls => (Request::Ls, false),
            Self::Current => (Request::Current, false),
            Self::Status => (Request::Status, false),
            Self::Show { json } => (Request::Show { json }, json),
            Self::Tail => (Request::Tail, false),
            Self::Send { text } => (
                Request::Send {
                    text: text.join(" "),
                },
                false,
            ),
            Self::Steer { text } => (
                Request::Steer {
                    text: text.join(" "),
                },
                false,
            ),
            Self::Scroll(args) => (
                Request::Scroll {
                    direction: args.direction.map(Into::into),
                    pixels: args.pixels,
                    target: args.target,
                    message_id: args.message_id,
                },
                false,
            ),
            Self::Pending => (Request::Pending, false),
            Self::Approve { id } => (Request::Approve { id }, false),
            Self::Decline { id } => (Request::Decline { id }, false),
            Self::Interrupt => (Request::Interrupt, false),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket_path = match cli.socket {
        Some(path) => path,
        None => default_socket_path()?,
    };
    let (request, json_output) = cli.command.into_request();
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
        print_human(&result)?;
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

fn print_human(value: &serde_json::Value) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_joins_unquoted_message_words() {
        let cli = Cli::try_parse_from(["codexctl", "send", "keep", "going"]).unwrap();
        let (request, _) = cli.command.into_request();

        assert_eq!(
            request,
            Request::Send {
                text: "keep going".into()
            }
        );
    }

    #[test]
    fn scroll_requires_exactly_one_mode() {
        assert!(Cli::try_parse_from(["codexctl", "scroll"]).is_err());
        assert!(Cli::try_parse_from(["codexctl", "scroll", "down", "--to", "bottom"]).is_err());
        assert!(Cli::try_parse_from(["codexctl", "scroll", "--to", "bottom"]).is_ok());
    }
}
