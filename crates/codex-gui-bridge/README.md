# codex-gui-bridge

This crate contains two registered Desktop transports:

- `ws-unix-bridge` is a transparent TCP-WebSocket ↔ Unix-WebSocket adapter.
- `codex-gui-bridge` is a shared-connection broker and app-server supervisor;
  `codex-gui` is its Unix-socket client.

The project adopted `ws-unix-bridge` plus the managed standalone daemon as
**Solution B**. That path completed live Desktop acceptance on 2026-09-04. The
broker remains fixture-tested source code, but it was not adopted and has not
completed its own live acceptance.

## Adopted Architecture: Solution B

```text
Desktop
  CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc
          |
          | WebSocket over TCP
          v
  ws-unix-bridge (127.0.0.1:18790)
          |
          | WebSocket over Unix socket
          v
  managed standalone app-server
  ~/.codex/app-server-control/app-server-control.sock
          |
          +-- local GUI and CLI control
          +-- remote control and mobile history
```

The bridge terminates the two WebSocket transports but forwards text, binary,
ping, pong, and close frames without parsing or rewriting JSON-RPC. Each
Desktop connection maps to one upstream Unix-socket connection. The Desktop
still sends `initialize` and every later request itself.

The standalone daemon is a core dependency in this deployment. It provides the
durable app-server process, remote-control endpoint, and shared thread/writer
state used by Desktop, CLI control, and the mobile client. This single-instance
property is the reason Solution B was selected over directly supervising a
second bundled app-server.

## Install

The bridge tools require the Rust toolchain. The adopted path also requires an
installed ChatGPT Desktop bundle and the managed standalone Codex package under
`~/.codex/packages/standalone`. Install all three repository binaries from a
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

`cargo install` installs only this repository's bridge/client binaries. It does
not install Desktop or the managed standalone package. On the accepted machine,
the launchd job deliberately runs the repository's release binary directly so
the deployed artifact is explicit:

```text
/Users/bhe/projects/ai/codexapp-cli/target/release/ws-unix-bridge
```

If an installed `${CARGO_HOME:-$HOME/.cargo}/bin/ws-unix-bridge` is used
instead, make the launchd `ProgramArguments` path match that choice.

## Manual Startup and Validation

Before the first manual test, save work and quit ChatGPT Desktop completely;
closing a window is not sufficient. Interposition affects only a newly started
Desktop process. It does not patch the app bundle, inject code, or bypass the
application signature.

First inspect the managed daemon and enable remote control. Use the managed
binary's explicit path when multiple `codex` versions are installed:

```sh
CODEX_STANDALONE="$HOME/.codex/packages/standalone/current/codex"
"$CODEX_STANDALONE" app-server daemon start
"$CODEX_STANDALONE" app-server daemon enable-remote-control
"$CODEX_STANDALONE" app-server daemon version
test -S "$HOME/.codex/app-server-control/app-server-control.sock"
```

`daemon version` must report `status: running` and the expected socket path.
The start command is idempotent; do not start an ad-hoc second app-server when
the managed daemon already owns the socket.

Check that the bridge port is free, then start the adapter:

```sh
lsof -nP -iTCP:18790 -sTCP:LISTEN

ws-unix-bridge \
  --listen 127.0.0.1:18790 \
  --upstream-socket "$HOME/.codex/app-server-control/app-server-control.sock"
```

Do not kill an unknown listener. Stop only a process that you knowingly
started, or choose another loopback port. The bridge has no application-layer
authentication and must never listen on a non-loopback address.

With Desktop still fully quit, launch it directly with the two local-daemon
variables used by the accepted deployment:

```sh
CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc \
CODEX_APP_SERVER_USE_LOCAL_DAEMON=1 \
  /Applications/ChatGPT.app/Contents/MacOS/ChatGPT
```

Do not use `open -a ChatGPT` for this one-process check: macOS may reuse an
existing process that did not inherit the variables. Before a write test,
verify the transport and read path:

```sh
ps eww -p "$(pgrep -x ChatGPT | tail -1)" | tr ' ' '\n' | \
  grep '^CODEX_APP_SERVER_'
lsof -nP -iTCP:18790
"$CODEX_STANDALONE" app-server daemon version
```

Desktop logs should show `transport=websocket`, `initialized=true`, and a
successful `thread/list`. Open a disposable thread and confirm that its history
loads before testing `steer` or `interrupt` on a real session.

## launchd Persistence

The accepted machine uses three user LaunchAgents. launchd does not expand
`$HOME` inside `ProgramArguments`, so every plist contains absolute paths.

| Label | Role | Important settings |
| --- | --- | --- |
| `com.lunghaa.codex-app-server` | Starts the managed daemon at login | `RunAtLoad=true`; runs `~/.codex/packages/standalone/current/codex app-server daemon start`; the command exits after the daemon's own PID manager takes over |
| `com.lunghaa.ws-unix-bridge` | Keeps the transparent adapter alive | `RunAtLoad=true`, `KeepAlive=true`; runs the release bridge with `--listen 127.0.0.1:18790` and the app-server control socket |
| `com.lunghaa.codex-app-server-env` | Injects the Desktop transport into the user launchd domain | `RunAtLoad=true`; runs a short script containing the two `launchctl setenv` commands below |

The accepted launchd user domain contained both variables. For reproducible
login persistence, the environment script should set both explicitly:

```sh
#!/bin/sh
launchctl setenv CODEX_APP_SERVER_WS_URL \
  "ws://127.0.0.1:18790/rpc"
launchctl setenv CODEX_APP_SERVER_USE_LOCAL_DAEMON "1"
```

The bridge LaunchAgent on the accepted machine has these effective arguments
and log paths:

```text
ProgramArguments:
  /Users/bhe/projects/ai/codexapp-cli/target/release/ws-unix-bridge
  --listen
  127.0.0.1:18790
  --upstream-socket
  /Users/bhe/.codex/app-server-control/app-server-control.sock
StandardOutPath: /Users/bhe/.codex/ws-unix-bridge.log
StandardErrorPath: /Users/bhe/.codex/ws-unix-bridge.log
```

After writing the plists under `~/Library/LaunchAgents`, load them once in
dependency order:

```sh
launchctl bootstrap gui/"$(id -u)" \
  "$HOME/Library/LaunchAgents/com.lunghaa.codex-app-server.plist"
launchctl bootstrap gui/"$(id -u)" \
  "$HOME/Library/LaunchAgents/com.lunghaa.ws-unix-bridge.plist"
launchctl bootstrap gui/"$(id -u)" \
  "$HOME/Library/LaunchAgents/com.lunghaa.codex-app-server-env.plist"
```

`bootstrap` is an installation-time command and reports an error if the label
is already loaded. For an existing deployment, inspect it instead of reloading
it blindly:

```sh
launchctl print gui/"$(id -u)"/com.lunghaa.codex-app-server
launchctl print gui/"$(id -u)"/com.lunghaa.ws-unix-bridge
launchctl print gui/"$(id -u)"/com.lunghaa.codex-app-server-env
launchctl getenv CODEX_APP_SERVER_WS_URL
launchctl getenv CODEX_APP_SERVER_USE_LOCAL_DAEMON
```

The environment applies only to processes launched after it is set. Fully quit
and relaunch Desktop after login-agent changes.

### Rollback

To stop interposing Desktop while retaining mobile remote control, quit Desktop,
unload only the bridge and environment jobs, remove the two variables from the
launchd user domain, and then launch Desktop normally:

```sh
launchctl bootout gui/"$(id -u)" \
  "$HOME/Library/LaunchAgents/com.lunghaa.ws-unix-bridge.plist"
launchctl bootout gui/"$(id -u)" \
  "$HOME/Library/LaunchAgents/com.lunghaa.codex-app-server-env.plist"
launchctl unsetenv CODEX_APP_SERVER_WS_URL
launchctl unsetenv CODEX_APP_SERVER_USE_LOCAL_DAEMON
```

Do not stop the standalone daemon if remote control or a mobile session still
depends on it. Disabling remote control is a separate, explicit operation.

## Live Acceptance Record: 2026-09-04

The base live-Desktop acceptance gate passed for this exact deployment:

- ChatGPT Desktop `26.831.21537` (`CFBundleVersion 7579`), with bundled
  `codex-cli 0.152.1`;
- managed standalone app-server `0.153.2`, SHA-256
  `195ace4100a634a9df39147f493e730e666b5bd87795f3c9f3251d8542400424`;
- `ws-unix-bridge` package version `0.1.0`, SHA-256
  `688f884b8d773bf8077d7fe9152c3220269f16dbd72f3ae9bc0854d463c42f92`;
- repository revision `59a5862b4b2e0a1588e4f114d681cd7034745ace`.

Observed evidence, in order:

1. Desktop PID 67425 inherited both deployment variables and connected from
   `127.0.0.1:61510` to bridge PID 68521 at `127.0.0.1:18790`.
2. The bridge held the matching TCP connection and a Unix connection to
   standalone daemon PID 50310 at
   `~/.codex/app-server-control/app-server-control.sock`.
3. After the bridge was rebuilt and relaunched, Desktop reconnected and logged
   `transport=websocket`, `initialized=true`, and successful `thread/list`.
4. GUI session history opened: repeated `thread/turns/list` requests for the
   selected thread completed with `errorCode=null`.
5. `remoteControl/enable` completed, the local remote-connection state became
   `connected`, and mobile full-history synchronization was operator-accepted.
6. A later `turn/steer` completed with `errorCode=null` through the same
   Desktop connection.

This record proves transport selection, initialization, list/read/history
loading, remote-control connection, mobile history for the observed account,
and one same-turn steer. It does not yet prove login/reboot recovery,
long-duration reconnect behavior, every approval/notification path, or future
version compatibility. Merely receiving a `thread/resume` request is not by
itself a success predicate; use the response and visible history state.

### Forwarding Regression Test

```sh
CARGO_INCREMENTAL=0 cargo test -p codex-gui-bridge --bin ws-unix-bridge
```

The test uses a temporary Unix socket, fake WebSocket app-server, and fake
Desktop loopback connection. It proves frame forwarding only; the live record
above is the separate Desktop/daemon acceptance gate.

## Alternative Not Adopted: Shared-Connection Broker

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

### Alternative launch

Running this command starts the Desktop-bundled app-server directly; it does
not use the managed standalone daemon and is not part of Solution B:

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
