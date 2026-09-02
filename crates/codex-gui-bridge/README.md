# codex-gui-bridge

This crate contains two registered experimental transports:

- `ws-unix-bridge` is a transparent TCP-WebSocket ↔ Unix-WebSocket adapter.
- `codex-gui-bridge` is a shared-connection broker and app-server supervisor;
  `codex-gui` is its Unix-socket client.

Both paths compile and have fake-endpoint tests. Neither has completed live
Desktop acceptance, so build/test success must not be described as proof that a
current Desktop honors `CODEX_APP_SERVER_WS_URL` or that real GUI sessions can
already be controlled.

## Transparent Path: ws-unix-bridge

`ws-unix-bridge` aims to let Codex Desktop and the official remote-control daemon share a single app-server instance. It terminates the Desktop's TCP WebSocket connection, opens another WebSocket connection to the daemon's Unix socket, and forwards frames between the two sides verbatim, without parsing or rewriting JSON-RPC.

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

The bridge forwards text, binary, ping, pong, and close frames. It doesn't send `initialize` itself: the Desktop's own handshake and subsequent app-server traffic pass through unchanged. Each Desktop connection maps to one upstream Unix-socket connection, and a disconnect on either side ends that pair.

This path only lets the GUI share the daemon with `codexctl` if the Desktop actually reads `CODEX_APP_SERVER_WS_URL` and connects to the bridge. The automated tests currently prove only transparent frame forwarding — not that any Desktop version honors this environment variable, and not that GUI threads can already be steered or interrupted.

### Launching

First, confirm whether the official remote-control daemon already holds the default socket; don't start a second instance if one is already running:

```sh
codex app-server --remote-control --listen unix://
```

Then launch the bridge from the repository:

```sh
CARGO_INCREMENTAL=0 cargo run -p codex-gui-bridge --bin ws-unix-bridge -- \
  --listen 127.0.0.1:18790 \
  --upstream-socket "$HOME/.codex/app-server-control/app-server-control.sock"
```

`--listen` defaults to `127.0.0.1:18790`. `--upstream-socket` defaults to the path shown above.

Only let the Desktop inherit the experimental transport environment variable when the user has explicitly agreed to close and restart Codex Desktop and has confirmed this won't disrupt existing work:

```sh
CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc \
  /Applications/ChatGPT.app/Contents/MacOS/ChatGPT
```

If the app bundle lives elsewhere, only replace the executable path. Don't run this during ordinary fixture debugging, and don't use existing sessions in project 585, kof96, or any other real project as the first write test.

The bridge binds only to loopback by default. It adds no authentication or authorization, so it must never listen on a non-loopback address or be exposed to an untrusted network.

### Tests

```sh
CARGO_INCREMENTAL=0 cargo test -p codex-gui-bridge --bin ws-unix-bridge
```

The tests use a temporary Unix socket, a fake WebSocket app-server, and a fake Desktop loopback connection, and only verify that frames arrive verbatim in both directions; they don't start a real app-server, daemon, or Desktop.

## Fixture-Tested Shared-Connection Broker

The registered broker uses this architecture:

```text
Desktop ──WS 127.0.0.1:18790/rpc?token=...──> broker
                                                  │ shared upstream
                                                  v
                                   app-server 127.0.0.1:18791/rpc
                                                  ^
                                                  │ injected JSON-RPC
codex-gui ──private same-UID Unix socket───────────┘
```

Desktop traffic and `send`, `steer`, and `interrupt` share one upstream
connection. Read-only `threads`, `read`, and `turns` use initialized
ephemeral connections. Injected string request IDs are consumed by the broker
and returned only to the CLI; ordinary responses, notifications, and
server-to-client approval requests continue to Desktop.

The daemon enforces the following local boundaries:

- both TCP listeners must be loopback;
- each daemon run generates a 256-bit token that must appear in the Desktop
  WebSocket URL;
- CLI writes remain disabled until the app-server accepts Desktop's
  `initialize` request;
- only one Desktop connection is accepted at a time, and disconnect cleanup
  completes before reconnect;
- the CLI socket lives under a per-user `0700` directory, has mode `0600`,
  verifies the peer UID, and replaces only an owned, proven-stale socket inode;
- shutdown kills and reaps the supervised app-server, while unexpected exits
  use bounded exponential backoff.

Current-thread tracking correlates Desktop request IDs with
`thread/start`/`thread/resume` responses, and also observes explicit
`turn/start` thread IDs. It does not treat every `thread/started`
notification as active because such notifications can describe subagents.

### Fixture tests

```sh
CARGO_INCREMENTAL=0 cargo test -p codex-gui-bridge --all-targets
```

The suite covers a fake Desktop ↔ broker ↔ fake app-server ↔ CLI write,
injected-response isolation, initialized read-only RPC, token rejection,
simultaneous-connection rejection, disconnect/reconnect, private socket
permissions and cleanup, non-socket preservation, and supervised-child
termination. It starts neither Desktop nor a real app-server.

### Experimental launch

Running this command starts a real supervised app-server, so do it only when
that experiment is explicitly authorized:

```sh
CARGO_INCREMENTAL=0 cargo run -p codex-gui-bridge --bin codex-gui-bridge
```

The daemon prints the capability-bearing `CODEX_APP_SERVER_WS_URL` and the
private CLI socket path. `codex-gui` computes the same default socket path:

```sh
CARGO_INCREMENTAL=0 cargo run -p codex-gui-bridge --bin codex-gui -- status
CARGO_INCREMENTAL=0 cargo run -p codex-gui-bridge --bin codex-gui -- current
```

Do not restart Desktop or give it the printed URL without separate permission.
The first live write must use a brand-new temporary GUI thread, not an active
project thread.

Remaining acceptance gates are real-Desktop-only: prove the current app honors
the environment variable, prove the GUI-created thread belongs to the
supervised server, and preserve notifications, approvals, final output, and
disconnect behavior through `send`/`steer`/`interrupt`. The broker also
needs a merge plan with `codexctl`/`codex-bridge` before it can be shipped
as a second public control surface.
