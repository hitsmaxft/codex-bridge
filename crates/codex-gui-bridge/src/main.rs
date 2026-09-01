//! codex-gui-bridge daemon.
//!
//! Architecture:
//!
//! ```text
//! Desktop ──WS:18790/rpc──> Broker ──WS:18791/rpc──> codex app-server
//!                               ^
//!                           CLI API (Unix socket)
//! ```
//!
//! Launch Codex Desktop with `CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc`
//! (see README) so its app-server transport points at this broker instead of a
//! private stdio pipe. The broker owns the real app-server instance and lets
//! both the Desktop and the CLI speak to it over the same upstream connection.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::net::TcpListener;
use tokio::sync::watch;

use codex_gui_bridge::broker::{serve_desktop, BrokerState};
use codex_gui_bridge::cli_api;
use codex_gui_bridge::supervisor::Supervisor;

const DEFAULT_BROKER_ADDR: &str = "127.0.0.1:18790";
const DEFAULT_APP_SERVER_ADDR: &str = "127.0.0.1:18791";
const DEFAULT_CLI_SOCKET: &str = "/tmp/codex-gui.sock";

#[derive(Debug, Parser)]
#[command(version, about = "Broker + app-server supervisor for Codex Desktop remote control")]
struct Args {
    /// Address the broker listens on for the Desktop WebSocket connection.
    #[arg(long, default_value = DEFAULT_BROKER_ADDR)]
    broker_addr: String,

    /// Address the supervised app-server listens on.
    #[arg(long, default_value = DEFAULT_APP_SERVER_ADDR)]
    app_server_addr: String,

    /// `codex` binary used to run the app-server. Defaults to the binary
    /// bundled with Codex Desktop so the protocol schema matches.
    #[arg(long, value_name = "PATH")]
    codex_bin: Option<PathBuf>,

    /// Unix socket path for the CLI API.
    #[arg(long, default_value = DEFAULT_CLI_SOCKET)]
    cli_socket: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let codex_bin = match args.codex_bin {
        Some(path) => path,
        None => default_desktop_codex()?,
    };

    let app_server_url = format!("ws://{}/rpc", args.app_server_addr);
    let broker_listen_url = format!("ws://{}/rpc", args.broker_addr);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Supervisor: owns the app-server process.
    let supervisor = Supervisor::new(codex_bin, app_server_url.clone());
    let supervisor_handle = supervisor.handle();
    let supervisor_task = tokio::spawn(async move {
        if let Err(error) = supervisor.run(shutdown_rx.clone()).await {
            eprintln!("[codex-gui-bridge] supervisor failed: {error:#}");
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
        async move {
            if let Err(error) = serve_desktop(broker_listener, url, state).await {
                eprintln!("[codex-gui-bridge] broker failed: {error:#}");
            }
        }
    });

    // CLI API: Unix socket for `codex-gui`.
    let cli_task = tokio::spawn({
        let state = Arc::clone(&state);
        let socket = args.cli_socket.clone();
        let url = app_server_url.clone();
        async move {
            if let Err(error) =
                cli_api::serve_cli(&socket, &state, &url, shutdown_rx.clone()).await
            {
                eprintln!("[codex-gui-bridge] cli-api failed: {error:#}");
            }
        }
    });

    eprintln!(
        "[codex-gui-bridge] started: broker={} app-server={} cli-socket={}",
        broker_listen_url,
        app_server_url,
        args.cli_socket.display()
    );

    // Wait for Ctrl-C.
    tokio::signal::ctrl_c().await.context("failed to listen for ctrl-c")?;
    eprintln!("[codex-gui-bridge] shutting down");
    shutdown_tx.send_replace(true);

    let _ = tokio::join!(supervisor_task, broker_task, cli_task);
    let _ = supervisor_handle;
    Ok(())
}

/// Locate the `codex` binary bundled inside Codex Desktop, falling back to the
/// PATH `codex` if the app bundle is not found.
fn default_desktop_codex() -> Result<PathBuf> {
    let bundled = PathBuf::from(
        "/Applications/ChatGPT.app/Contents/Resources/codex",
    );
    if bundled.exists() {
        return Ok(bundled);
    }
    Ok(PathBuf::from("codex"))
}
