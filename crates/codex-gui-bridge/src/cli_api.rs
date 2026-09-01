//! Unix socket CLI API for the broker.
//!
//! The `codex-gui` client connects to this socket and sends one JSON object per
//! request:
//!
//! ```json
//! {"cmd": "send", "thread": "01a0...", "text": "继续"}
//! ```
//!
//! The broker replies with one JSON object:
//!
//! ```json
//! {"ok": true, "result": { ... }}
//! {"ok": false, "error": "..."}
//! ```

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::broker::{execute_cli, CliCall, BrokerState};

const MAX_REQUEST_BYTES: u64 = 1024 * 1024;

/// Serve CLI requests on `socket_path` until `shutdown` fires.
pub async fn serve_cli(
    socket_path: &Path,
    state: &BrokerState,
    upstream_url: &str,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    if socket_path.exists() {
        // Best effort: remove a stale socket left by a previous daemon. If the
        // path is a live socket, bind will fail below and we report it.
        let _ = std::fs::remove_file(socket_path);
    }
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    tracing_info(&format!(
        "cli-api: listening on {} (upstream {upstream_url})",
        socket_path.display()
    ));
    loop {
        let (stream, _addr) = tokio::select! {
            accepted = listener.accept() => accepted.context("accept cli connection")?,
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return Ok(());
                }
                continue;
            }
        };
        let state = state.clone();
        let upstream_url = upstream_url.to_owned();
        tokio::spawn(async move {
            if let Err(error) = handle_cli(stream, &state, &upstream_url).await {
                tracing_info(&format!("cli-api: request failed: {error:#}"));
            }
        });
    }
}

async fn handle_cli(
    stream: UnixStream,
    state: &BrokerState,
    upstream_url: &str,
) -> Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .await
        .context("read cli request")?;
    if n == 0 {
        return Ok(());
    }
    let response = match parse_request(&line) {
        Ok((cmd, args)) => match execute_cli(state, upstream_url, &cmd, &args).await {
            Ok(CliCall::Json(value)) => json!({"ok": true, "result": value}),
            Ok(CliCall::Notifications(mut rx)) => {
                // First response is the acknowledgement; subsequent lines are
                // notifications. For now we only support the acknowledgement.
                let first = rx.recv().await.unwrap_or(json!(null));
                json!({"ok": true, "result": first})
            }
            Err(error) => json!({"ok": false, "error": format!("{error:#}")}),
        },
        Err(error) => json!({"ok": false, "error": format!("{error:#}")}),
    };
    let mut writer = reader.into_inner();
    writer
        .write_all(serde_json::to_string(&response)?.as_bytes())
        .await?;
    writer.write_all(b"\n").await?;
    writer.shutdown().await?;
    Ok(())
}

fn parse_request(line: &str) -> Result<(String, serde_json::Map<String, Value>)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        anyhow::bail!("empty request");
    }
    if trimmed.len() as u64 > MAX_REQUEST_BYTES {
        anyhow::bail!("request too large");
    }
    let value: Value = serde_json::from_str(trimmed).context("request must be a JSON object")?;
    let object = value.as_object().context("request must be a JSON object")?;
    let cmd = object
        .get("cmd")
        .and_then(Value::as_str)
        .context("request is missing string field \"cmd\"")?
        .to_owned();
    let mut args = object.clone();
    args.remove("cmd");
    Ok((cmd, args))
}

pub fn tracing_info(message: &str) {
    eprintln!("[codex-gui-bridge] {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_request() {
        let (cmd, args) =
            parse_request(r#"{"cmd": "send", "thread": "t1", "text": "hello"}"#).unwrap();
        assert_eq!(cmd, "send");
        assert_eq!(args["thread"], "t1");
        assert_eq!(args["text"], "hello");
    }

    #[test]
    fn parse_rejects_bad_json() {
        assert!(parse_request("not json").is_err());
        assert!(parse_request(r#"{"cmd": 42}"#).is_err());
    }
}
