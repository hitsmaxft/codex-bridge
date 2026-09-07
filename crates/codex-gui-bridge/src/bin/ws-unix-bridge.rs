use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, client_async, WebSocketStream};

const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:18790";
const UPSTREAM_WEBSOCKET_URI: &str = "ws://localhost/rpc";

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Bridge a TCP WebSocket endpoint to app-server's WebSocket-over-Unix-socket endpoint"
)]
struct Args {
    /// TCP address on which Desktop can open its WebSocket connection.
    #[arg(long, default_value = DEFAULT_LISTEN_ADDR)]
    listen: SocketAddr,

    /// Unix socket served by the ChatGPT.app-bundled `codex app-server`.
    #[arg(long, value_name = "PATH")]
    upstream_socket: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let upstream_socket = match args.upstream_socket {
        Some(path) => path,
        None => default_upstream_socket()?,
    };
    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("failed to bind TCP listener on {}", args.listen))?;

    eprintln!(
        "ws-unix-bridge: listening on ws://{}/rpc and forwarding to {}",
        listener.local_addr()?,
        upstream_socket.display()
    );

    tokio::select! {
        result = serve(listener, upstream_socket) => result,
        result = tokio::signal::ctrl_c() => {
            result.context("failed to listen for Ctrl-C")?;
            Ok(())
        }
    }
}

fn default_upstream_socket() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .context("HOME is unset; pass --upstream-socket with the bundled app-server socket path")?;
    Ok(PathBuf::from(home)
        .join(".codex-bridge")
        .join("bundled-app-server.sock"))
}

async fn serve(listener: TcpListener, upstream_socket: PathBuf) -> Result<()> {
    loop {
        let (stream, peer) = listener
            .accept()
            .await
            .context("failed to accept Desktop TCP connection")?;
        let upstream_socket = upstream_socket.clone();
        tokio::spawn(async move {
            if let Err(error) = bridge_connection(stream, &upstream_socket).await {
                eprintln!("ws-unix-bridge: connection from {peer} ended: {error:#}");
            }
        });
    }
}

async fn bridge_connection(desktop_tcp: TcpStream, upstream_socket: &Path) -> Result<()> {
    let desktop_peer = desktop_tcp.peer_addr().ok();
    let desktop = accept_async(desktop_tcp)
        .await
        .with_context(|| match desktop_peer {
            Some(peer) => format!("failed WebSocket handshake with Desktop at {peer}"),
            None => "failed WebSocket handshake with Desktop".to_owned(),
        })?;

    let upstream_unix = UnixStream::connect(upstream_socket)
        .await
        .with_context(|| format!("failed to connect to {}", upstream_socket.display()))?;
    let (upstream, _) = client_async(UPSTREAM_WEBSOCKET_URI, upstream_unix)
        .await
        .with_context(|| {
            format!(
                "failed WebSocket handshake over {}",
                upstream_socket.display()
            )
        })?;

    relay(desktop, upstream).await
}

async fn relay(
    mut desktop: WebSocketStream<TcpStream>,
    mut upstream: WebSocketStream<UnixStream>,
) -> Result<()> {
    loop {
        tokio::select! {
            message = desktop.next() => {
                match message {
                    Some(Ok(Message::Close(frame))) => {
                        let _ = upstream.send(Message::Close(frame.clone())).await;
                        let _ = desktop.close(frame).await;
                        return Ok(());
                    }
                    Some(Ok(message)) => {
                        if let Err(error) = upstream.send(message).await {
                            let _ = desktop.close(None).await;
                            return Err(error).context("failed to forward Desktop frame to app-server");
                        }
                    }
                    Some(Err(error)) => {
                        let _ = upstream.close(None).await;
                        return Err(error).context("failed to read Desktop WebSocket frame");
                    }
                    None => {
                        let _ = upstream.close(None).await;
                        return Ok(());
                    }
                }
            }
            message = upstream.next() => {
                match message {
                    Some(Ok(Message::Close(frame))) => {
                        let _ = desktop.send(Message::Close(frame.clone())).await;
                        let _ = upstream.close(frame).await;
                        return Ok(());
                    }
                    Some(Ok(message)) => {
                        if let Err(error) = desktop.send(message).await {
                            let _ = upstream.close(None).await;
                            return Err(error).context("failed to forward app-server frame to Desktop");
                        }
                    }
                    Some(Err(error)) => {
                        let _ = desktop.close(None).await;
                        return Err(error).context("failed to read app-server WebSocket frame");
                    }
                    None => {
                        let _ = desktop.close(None).await;
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use anyhow::{bail, Result};
    use tokio::net::UnixListener;
    use tokio::time::timeout;
    use tokio_tungstenite::connect_async;

    use super::*;

    static NEXT_SOCKET_ID: AtomicU64 = AtomicU64::new(0);
    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    struct TestSocketPath(PathBuf);

    impl TestSocketPath {
        fn new() -> Self {
            let id = NEXT_SOCKET_ID.fetch_add(1, Ordering::Relaxed);
            Self(
                std::env::temp_dir()
                    .join(format!("ws-unix-bridge-{}-{id}.sock", std::process::id())),
            )
        }
    }

    impl Drop for TestSocketPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn forwards_text_frames_in_both_directions() -> Result<()> {
        const REQUEST: &str = r#"{"id":7,"method":"thread/list","params":{"limit":1}}"#;
        const RESPONSE: &str = r#"{"id":7,"result":{"echo":"thread/list"}}"#;

        let socket_path = TestSocketPath::new();
        let fake_app_server = UnixListener::bind(&socket_path.0)?;
        let bridge_listener = TcpListener::bind("127.0.0.1:0").await?;
        let bridge_addr = bridge_listener.local_addr()?;

        let app_server_task = tokio::spawn(async move {
            let (stream, _) = fake_app_server.accept().await?;
            let mut websocket = accept_async(stream).await?;

            let received = match websocket.next().await {
                Some(Ok(Message::Text(text))) => text.to_string(),
                Some(Ok(message)) => {
                    bail!("fake app-server received unexpected frame: {message:?}")
                }
                Some(Err(error)) => return Err(error.into()),
                None => bail!("bridge disconnected before forwarding Desktop request"),
            };
            websocket.send(Message::text(RESPONSE)).await?;

            match websocket.next().await {
                Some(Ok(Message::Close(_))) | None => {}
                Some(Ok(message)) => {
                    bail!("fake app-server received unexpected frame: {message:?}")
                }
                Some(Err(error)) => return Err(error.into()),
            }
            Ok::<_, anyhow::Error>(received)
        });

        let bridge_socket = socket_path.0.clone();
        let bridge_task = tokio::spawn(async move {
            let (stream, _) = bridge_listener.accept().await?;
            bridge_connection(stream, &bridge_socket).await
        });

        let (mut desktop, _) = connect_async(format!("ws://{bridge_addr}/rpc")).await?;
        desktop.send(Message::text(REQUEST)).await?;
        let response = timeout(TEST_TIMEOUT, desktop.next())
            .await
            .context("timed out waiting for fake app-server response")?;
        match response {
            Some(Ok(Message::Text(text))) => assert_eq!(text, RESPONSE),
            Some(Ok(message)) => bail!("fake Desktop received unexpected frame: {message:?}"),
            Some(Err(error)) => return Err(error.into()),
            None => bail!("bridge disconnected before returning app-server response"),
        }
        desktop.close(None).await?;

        let forwarded_request = timeout(TEST_TIMEOUT, app_server_task)
            .await
            .context("fake app-server task timed out")???;
        assert_eq!(forwarded_request, REQUEST);
        timeout(TEST_TIMEOUT, bridge_task)
            .await
            .context("bridge task timed out")???;

        Ok(())
    }
}
