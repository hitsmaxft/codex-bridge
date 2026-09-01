# ws-unix-bridge

`ws-unix-bridge` lets Codex Desktop and the official remote-control daemon use
the same app-server instance. It terminates Desktop's TCP WebSocket connection,
opens another WebSocket connection over the daemon's Unix socket, and forwards
every WebSocket message in both directions without parsing or rewriting JSON-RPC.

```text
Codex Desktop
  CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc
          |
          | WebSocket over TCP
          v
  ws-unix-bridge (127.0.0.1:18790)
          |
          | WebSocket over Unix socket
          v
  codex app-server --remote-control --listen unix://
  ~/.codex/app-server-control/app-server-control.sock
```

The bridge forwards text, binary, ping, pong, and close messages as WebSocket
messages. It does not send `initialize` itself: Desktop's own handshake and all
later app-server traffic pass through unchanged. Each Desktop connection gets
one upstream Unix-socket connection, and either side ending the connection ends
the paired connection.

## Run

First ensure the official remote-control daemon owns its normal socket (do not
start a second copy if one is already running):

```sh
codex app-server --remote-control --listen unix://
```

Then start the bridge from this repository:

```sh
CARGO_INCREMENTAL=0 cargo run -p codex-gui-bridge --bin ws-unix-bridge -- \
  --listen 127.0.0.1:18790 \
  --upstream-socket "$HOME/.codex/app-server-control/app-server-control.sock"
```

`--listen` defaults to `127.0.0.1:18790`. `--upstream-socket` defaults to the
path shown above.

On macOS, start Codex Desktop directly so it inherits the transport variable:

```sh
CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc \
  /Applications/ChatGPT.app/Contents/MacOS/ChatGPT
```

If the application bundle is installed elsewhere, replace only the executable
path; keep the WebSocket URL unchanged.

The bridge intentionally binds loopback by default. Do not expose it on an
untrusted network: it does not add authentication or authorization.

## Test

```sh
CARGO_INCREMENTAL=0 cargo test -p codex-gui-bridge --bin ws-unix-bridge
```

The test creates a temporary Unix socket with a fake WebSocket app-server and a
loopback TCP connection with a fake Desktop. It verifies the Desktop request
arrives unchanged and the fake app-server response returns unchanged. No real
Codex app-server, daemon, or Desktop process is started.
