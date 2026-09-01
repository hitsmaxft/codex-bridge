# codex-gui-bridge

This crate currently contains two layers of implementation at different levels of maturity:

- `ws-unix-bridge` is the only binary registered in `Cargo.toml`. It already has fake WebSocket/Unix-socket round-trip tests and can serve as an experimental transparent transport adapter.
- `src/main.rs`, `src/lib.rs`, `broker.rs`, `cli_api.rs`, `protocol.rs`, `supervisor.rs`, and `src/bin/codex-gui.rs` are broker/supervisor drafts in the workspace. The crate currently sets `autolib = false` and `autobins = false`, so these targets aren't registered yet and lack the corresponding dependency configuration; a plain `cargo build --workspace` won't compile or verify them.

So the "transparent bridge" section below describes the currently buildable path, while the "shared-connection broker draft" only records design and TODOs and can't be taken as evidence that the feature is complete.

## Currently Buildable Path: ws-unix-bridge

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

## Not Yet Wired Into the Build: Shared-Connection Broker Draft

The draft source describes this target architecture:

```text
Desktop ──WS 127.0.0.1:18790/rpc──> broker
                                        │ shared upstream connection
                                        v
                         supervised app-server 127.0.0.1:18791/rpc
                                        ^
                                        │ injected JSON-RPC
codex-gui ──Unix socket /tmp/codex-gui.sock──┘
```

The design intent is for GUI traffic and `send`, `steer`, and `interrupt` to share the same app-server upstream, while the read-only `threads`, `read`, and `turns` use ephemeral connections. The draft CLI also defines `status`, `current`, and `tail`.

Before this can become a working feature, at least the following must be completed:

1. Explicitly register the library, daemon, and `codex-gui` binary in Cargo, add the serde and Tokio process/time/sync dependencies, and pass build, Clippy, and tests.
2. Put the CLI socket in a user-private directory with `0600` permissions, and use inode/ownership checks to handle stale sockets; don't unconditionally delete the fixed `/tmp/codex-gui.sock`.
3. Add a local authorization boundary for the loopback broker, or explicitly prove that only a trusted Desktop can connect; as it stands, any local process could try to inject RPC.
4. Properly terminate the app-server child started by the supervisor, limit crash-loops, and verify that upstream connections don't cross when the Desktop disconnects, reconnects, or opens multiple connections.
5. Reliably track the GUI's current thread from app-server responses/notifications; watching only Desktop requests that carry `threadId` isn't enough to cover newly created threads.
6. Add end-to-end tests covering fake Desktop ↔ broker ↔ fake app-server ↔ CLI, then do real GUI acceptance on a fresh temporary thread authorized by the user.
7. Clarify how it merges with the existing `codexctl`/`codex-bridge`, to avoid maintaining a second CLI protocol and output format long-term.

Until these gates are met, don't run or ship the draft daemon, and don't describe the existence of the source as proof that GUI control is complete.
