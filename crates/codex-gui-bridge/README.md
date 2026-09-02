# codex-gui-bridge

This crate contains two registered experimental transports:

- `ws-unix-bridge` is a transparent TCP-WebSocket ↔ Unix-WebSocket adapter.
- `codex-gui-bridge` is a shared-connection broker and app-server supervisor;
  `codex-gui` is its Unix-socket client.

Both paths compile and have fake-endpoint tests. Neither has completed live
Desktop acceptance, so build/test success must not be described as proof that a
current Desktop honors `CODEX_APP_SERVER_WS_URL` or that real GUI sessions can
already be controlled.

## Install

The tools require the Rust toolchain and, for the recommended live path, an
installed ChatGPT Desktop bundle. A separately installed/managed standalone
Codex daemon is **not** required. Install all three repository binaries from a
checkout:

```sh
git clone https://github.com/hitsmaxft/codexapp-cli.git
cd codexapp-cli
CARGO_TARGET_DIR="$PWD/target" CARGO_INCREMENTAL=0 \
  cargo install --locked --path crates/codex-gui-bridge --bins
```

Cargo installs `codex-gui-bridge`, `codex-gui`, and `ws-unix-bridge` into
`${CARGO_HOME:-$HOME/.cargo}/bin`. Ensure that directory is on `PATH`, then
confirm the installed commands without starting a daemon:

```sh
codex-gui-bridge --version
codex-gui --version
ws-unix-bridge --version
```

During repository development, the equivalent commands can be run without
installation by replacing each binary name with, for example,
`CARGO_INCREMENTAL=0 cargo run -p codex-gui-bridge --bin codex-gui-bridge --`.

### Runtime choice, verified 2026-09-02

Use the app-server executable shipped inside the installed Desktop bundle for
the broker path:

```text
new ChatGPT Desktop process
  -> codex-gui-bridge
  -> /Applications/ChatGPT.app/Contents/Resources/codex app-server --listen ws://127.0.0.1:18791
```

This is a directly supervised child process, not `codex app-server daemon` and
not the managed standalone package under `~/.codex/packages/standalone`.
`cargo install` above installs only this repository's three bridge/client
binaries; it does not install another app-server.

The choice is based on a real local protocol probe, not just `--help` output.
After correcting the listener URL so `--listen` receives `ws://IP:PORT` while
clients connect to `ws://IP:PORT/rpc`, the following two binaries both
completed `initialize` followed by `thread/list`:

- Desktop bundle: `codex-cli 0.151.0-alpha.7.2`, SHA-256
  `a6042937174f72112dbd2d554a4af36936422e0c5ac69e353dc68994458996e9`;
- PATH CLI: `codex-cli 0.152.1`, SHA-256
  `8194ea3181f330e63023b234b0b231855e5874e0331c5ef7cbc490591497a7bf`.

That probe proves basic direct app-server startup and read-only RPC for these
exact binaries. It does not prove full Desktop compatibility for the different
PATH version. The bundled binary is therefore the default because it minimizes
protocol-version skew with the installed GUI. Use `--codex-bin PATH` only for
an explicit compatibility experiment.

The managed daemon reported version `0.152.1`, backend `pid`, and control
socket `~/.codex/app-server-control/app-server-control.sock` on the same
machine. Only its status, version, and socket presence were confirmed in this
pass; it was not needed for the successful direct probes and has not passed the
broker's live-Desktop acceptance gates. It remains an optional transparent-path
experiment below, not the installation recommendation.

## Interpose the Desktop Transport

Here, “interpose” (or “hijack”) means launching a **new** Desktop process with
`CODEX_APP_SERVER_WS_URL` pointed at a loopback bridge. It is process-local
configuration: no app bundle is modified, no code is injected, and the app's
signature is not bypassed.

The shared-connection broker is the useful path when `codex-gui` must observe
or control the same app-server connection as Desktop. Before the first live
attempt, save any work and quit ChatGPT Desktop completely; closing a window is
not sufficient. Check that neither required loopback port is already owned by
another process:

```sh
lsof -nP -iTCP:18790 -sTCP:LISTEN
lsof -nP -iTCP:18791 -sTCP:LISTEN
```

Do not kill an unknown listener. Either stop the process that you knowingly
started or select unused loopback ports with `--broker-addr` and
`--app-server-addr`.

Start the broker in its own terminal:

```sh
codex-gui-bridge \
  --codex-bin /Applications/ChatGPT.app/Contents/Resources/codex
```

The explicit path makes the runtime choice visible and reproducible. Omitting
`--codex-bin` currently selects the same bundled binary when it exists and
falls back to `codex` on `PATH` otherwise. Keep this terminal open and copy the
complete `broker=` URL from the startup line, including its random
`?token=...` query. Treat that capability URL as a secret for the life of the
broker and do not publish it in logs or issue reports.

With Desktop still fully quit, launch the bundle executable directly from a
second terminal, substituting the exact URL printed by the broker:

```sh
CODEX_APP_SERVER_WS_URL='ws://127.0.0.1:18790/rpc?token=REPLACE_WITH_PRINTED_TOKEN' \
  /Applications/ChatGPT.app/Contents/MacOS/ChatGPT
```

Do not use `open -a ChatGPT` for this check: macOS may reuse an existing
process, in which case the process will not inherit the shell assignment. Do
not set the variable globally with `launchctl setenv`; keeping it on this one
command makes rollback deterministic.

Once the GUI is visible, verify the transport before sending anything:

```sh
codex-gui status
codex-gui current
codex-gui threads --limit 10
```

`status` must report both `desktop connected: true` and
`desktop initialized: true`. A false value means the interposition has not
been proved; common causes are a reused Desktop process, an ignored environment
variable, a wrong/expired token, or a protocol mismatch. Create a brand-new
temporary GUI thread for the first read/write test. Do not use an active
project thread.

To roll back, quit that Desktop instance normally, press Ctrl-C in the broker
terminal, and launch Desktop normally without `CODEX_APP_SERVER_WS_URL`. The
broker stops and reaps its supervised app-server; it does not leave a patched
Desktop installation behind.

## Optional, Unaccepted Transparent Path: ws-unix-bridge

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
  codex app-server daemon (remote control enabled)
  ~/.codex/app-server-control/app-server-control.sock
```

The bridge forwards text, binary, ping, pong, and close frames. It doesn't send `initialize` itself: the Desktop's own handshake and subsequent app-server traffic pass through unchanged. Each Desktop connection maps to one upstream Unix-socket connection, and a disconnect on either side ends that pair.

This path only lets the GUI share the daemon with `codexctl` if the Desktop actually reads `CODEX_APP_SERVER_WS_URL` and connects to the bridge. The automated tests currently prove only transparent frame forwarding — not that any Desktop version honors this environment variable, and not that GUI threads can already be steered or interrupted.

### Launching

This section is not part of the recommended installation. First inspect the
managed daemon instead of installing or restarting it blindly:

```sh
codex app-server daemon version
test -S "$HOME/.codex/app-server-control/app-server-control.sock"
```

Only when deliberately testing this transparent path and no managed daemon is
running, start it and enable remote control. These commands replace the older,
unsupported `codex app-server --remote-control --listen unix://` spelling:

```sh
codex app-server daemon start
codex app-server daemon enable-remote-control
test -S "$HOME/.codex/app-server-control/app-server-control.sock"
```

Then launch the installed bridge:

```sh
ws-unix-bridge \
  --listen 127.0.0.1:18790 \
  --upstream-socket "$HOME/.codex/app-server-control/app-server-control.sock"
```

`--listen` defaults to `127.0.0.1:18790`. `--upstream-socket` defaults to the path shown above.

Only let a fully quit Desktop inherit the experimental transport variable:

```sh
CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc \
  /Applications/ChatGPT.app/Contents/MacOS/ChatGPT
```

If the app bundle lives elsewhere, only replace the executable path. Don't run this during ordinary fixture debugging, and don't use existing sessions in project 585, kof96, or any other real project as the first write test.

This transparent path has no capability token and exposes no `codex-gui` CLI
socket. Keep it on loopback, verify it only with a temporary thread, and use
the same quit/Ctrl-C/normal-launch rollback described above.

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

Running this command starts the Desktop-bundled app-server directly; it does
not use the managed standalone daemon. The full safe launch and rollback
sequence is in [Interpose the Desktop Transport](#interpose-the-desktop-transport):

```sh
codex-gui-bridge \
  --codex-bin /Applications/ChatGPT.app/Contents/Resources/codex
```

The daemon prints the capability-bearing `CODEX_APP_SERVER_WS_URL` and the
private CLI socket path. `codex-gui` computes the same default socket path:

```sh
codex-gui status
codex-gui current
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
