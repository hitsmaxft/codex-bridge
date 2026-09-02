//! codex-gui-bridge daemon.
//!
//! Architecture:
//!
//! ```text
//! Desktop ──WS:18790/rpc?token=...──> Broker ──WS:18791/rpc──> codex app-server
//!                               ^
//!                           CLI API (Unix socket)
//! ```
//!
//! With explicit authorization, launch Codex Desktop using the capability URL
//! printed by this daemon as `CODEX_APP_SERVER_WS_URL` (see README). The
//! broker owns the real app-server instance and lets both Desktop and the CLI
//! speak to it over the same upstream connection.

use std::io::Read;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::Parser;
use tokio::net::TcpListener;
use tokio::sync::watch;

use codex_gui_bridge::broker::{serve_desktop, BrokerState};
use codex_gui_bridge::cli_api;
use codex_gui_bridge::supervisor::Supervisor;

const DEFAULT_BROKER_ADDR: &str = "127.0.0.1:18790";
const DEFAULT_APP_SERVER_ADDR: &str = "127.0.0.1:18791";

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Broker + app-server supervisor for Codex Desktop remote control"
)]
struct Args {
    /// Address the broker listens on for the Desktop WebSocket connection.
    #[arg(long, default_value = DEFAULT_BROKER_ADDR)]
    broker_addr: SocketAddr,

    /// Address the supervised app-server listens on.
    #[arg(long, default_value = DEFAULT_APP_SERVER_ADDR)]
    app_server_addr: SocketAddr,

    /// `codex` binary used to run the app-server. Defaults to the binary
    /// bundled with Codex Desktop so the protocol schema matches.
    #[arg(long, value_name = "PATH")]
    codex_bin: Option<PathBuf>,

    /// Unix socket path for the CLI API.
    #[arg(long)]
    cli_socket: Option<PathBuf>,

    /// Capability token required in the Desktop WebSocket URL. When omitted,
    /// the daemon generates a fresh 256-bit token.
    #[arg(long)]
    desktop_token: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if !args.broker_addr.ip().is_loopback() {
        bail!("broker address must be loopback: {}", args.broker_addr);
    }
    if !args.app_server_addr.ip().is_loopback() {
        bail!(
            "supervised app-server address must be loopback: {}",
            args.app_server_addr
        );
    }
    let cli_socket = args.cli_socket.unwrap_or_else(cli_api::default_socket_path);
    let desktop_token = match args.desktop_token {
        Some(token) => validate_desktop_token(token)?,
        None => generate_desktop_token()?,
    };

    let codex_bin = match args.codex_bin {
        Some(path) => path,
        None => default_desktop_codex()?,
    };

    let (app_server_listen_url, app_server_url) = app_server_urls(args.app_server_addr);
    let broker_listen_url = format!("ws://{}/rpc?token={desktop_token}", args.broker_addr);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Supervisor: owns the app-server process.
    let supervisor = Supervisor::new(codex_bin, app_server_listen_url);
    let supervisor_shutdown = shutdown_rx.clone();
    let supervisor_failure = shutdown_tx.clone();
    let supervisor_task = tokio::spawn(async move {
        if let Err(error) = supervisor.run(supervisor_shutdown).await {
            eprintln!("[codex-gui-bridge] supervisor failed: {error:#}");
            supervisor_failure.send_replace(true);
        }
    });

    // Broker state shared across desktop connections and CLI calls.
    let state = BrokerState::new(shutdown_rx.clone());

    // Broker: accept Desktop connections and proxy to the app-server.
    let broker_listener = TcpListener::bind(&args.broker_addr)
        .await
        .with_context(|| format!("failed to bind broker on {}", args.broker_addr))?;
    let broker_task = tokio::spawn({
        let state = Arc::clone(&state);
        let url = app_server_url.clone();
        let desktop_token = desktop_token.clone();
        let broker_failure = shutdown_tx.clone();
        async move {
            if let Err(error) = serve_desktop(broker_listener, url, state, desktop_token).await {
                eprintln!("[codex-gui-bridge] broker failed: {error:#}");
                broker_failure.send_replace(true);
            }
        }
    });

    // CLI API: Unix socket for `codex-gui`.
    let cli_task = tokio::spawn({
        let state = Arc::clone(&state);
        let socket = cli_socket.clone();
        let url = app_server_url.clone();
        let cli_shutdown = shutdown_rx.clone();
        let cli_failure = shutdown_tx.clone();
        async move {
            if let Err(error) = cli_api::serve_cli(&socket, state, &url, cli_shutdown).await {
                eprintln!("[codex-gui-bridge] cli-api failed: {error:#}");
                cli_failure.send_replace(true);
            }
        }
    });

    eprintln!(
        "[codex-gui-bridge] started: broker={} app-server={} cli-socket={}",
        broker_listen_url,
        app_server_url,
        cli_socket.display()
    );

    // Wait for Ctrl-C or a fatal component error.
    let mut failure_rx = shutdown_rx;
    let component_failed = tokio::select! {
        result = tokio::signal::ctrl_c() => {
            result.context("failed to listen for ctrl-c")?;
            false
        }
        changed = failure_rx.changed() => {
            changed.context("shutdown channel closed unexpectedly")?;
            *failure_rx.borrow()
        }
    };
    eprintln!("[codex-gui-bridge] shutting down");
    shutdown_tx.send_replace(true);

    let _ = tokio::join!(supervisor_task, broker_task, cli_task);
    if component_failed {
        bail!("a bridge component failed; see diagnostics above");
    }
    Ok(())
}

fn validate_desktop_token(token: String) -> Result<String> {
    if token.len() < 32
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("desktop token must be at least 32 URL-safe ASCII characters");
    }
    Ok(token)
}

fn generate_desktop_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .context("failed to open operating-system random source")?
        .read_exact(&mut bytes)
        .context("failed to read operating-system random source")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// `codex app-server --listen` accepts only `ws://IP:PORT`, while WebSocket
/// clients connect to its `/rpc` resource.
fn app_server_urls(address: SocketAddr) -> (String, String) {
    let listen_url = format!("ws://{address}");
    let rpc_url = format!("{listen_url}/rpc");
    (listen_url, rpc_url)
}

/// Locate the `codex` binary bundled inside Codex Desktop, falling back to the
/// PATH `codex` if the app bundle is not found.
fn default_desktop_codex() -> Result<PathBuf> {
    let bundled = PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex");
    if bundled.exists() {
        return Ok(bundled);
    }
    Ok(PathBuf::from("codex"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_server_listen_url_has_no_rpc_path() {
        let address: SocketAddr = "127.0.0.1:18791".parse().unwrap();
        let (listen_url, rpc_url) = app_server_urls(address);
        assert_eq!(listen_url, "ws://127.0.0.1:18791");
        assert_eq!(rpc_url, "ws://127.0.0.1:18791/rpc");
    }
}
