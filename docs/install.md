# Installation and service setup

This guide installs `codex-bridge` as a global Cargo service with its production Web UI embedded
in the executable. It then covers the two runtime topologies:

| Environment                     | App-server                       | Processes to run                     | `ws-unix-bridge` |
| ------------------------------- | -------------------------------- | ------------------------------------ | ---------------- |
| macOS with Codex Desktop        | CLI bundled in `ChatGPT.app`     | app-server, adapter, bridge, Desktop | Required         |
| Linux, or macOS without Desktop | Open-source standalone Codex CLI | app-server and bridge                | Not used         |

The app-server supports stdio, Unix-socket, and WebSocket transports. This project uses a local
Unix socket for bridge RPC. See the
[official Codex App Server documentation](https://learn.chatgpt.com/docs/app-server) for protocol
and transport details.

## 1. Build and install the global commands

Requirements:

- a current stable Rust toolchain with Cargo;
- Node.js and npm, used once to build the Vite frontend;
- a Codex runtime: `ChatGPT.app` for the macOS Desktop topology, or a recent standalone `codex`
  executable whose `codex app-server --help` lists `unix://` transport support.

Clone the repository, build the production frontend first, then install the daemon and CLI:

```sh
git clone https://github.com/hitsmaxft/codex-bridge.git
cd codex-bridge

npm --prefix web-ui ci
npm --prefix web-ui run build

CARGO_TARGET_DIR="$PWD/target" CARGO_INCREMENTAL=0 \
  cargo install --locked --path crates/codex-bridge
CARGO_TARGET_DIR="$PWD/target" CARGO_INCREMENTAL=0 \
  cargo install --locked --path crates/codexctl
```

When using Codex Desktop on macOS, also install its transport adapter:

```sh
CARGO_TARGET_DIR="$PWD/target" CARGO_INCREMENTAL=0 \
  cargo install --locked --path crates/codex-gui-bridge --bin ws-unix-bridge
```

Reusing the checkout's `target` directory avoids separate Cargo build caches for each package.
Cargo installs the executables into `${CARGO_HOME:-$HOME/.cargo}/bin`; put that directory on `PATH`
for interactive use and use its absolute path in service-manager configuration.

The files in `web-ui/dist` are compiled into `codex-bridge` with `include_str!`. A globally
installed daemon therefore serves the frontend without a source checkout, Node.js, or a runtime
assets directory. Rebuild `web-ui/dist` before `cargo install` whenever frontend source changed.

Confirm the installed artifacts without starting a service:

```sh
codex-bridge --version
codexctl --version

# macOS Desktop topology only
ws-unix-bridge --version
```

`cargo install` does not install Codex Desktop or the standalone Codex CLI. It installs only this
repository's bridge processes.

For an update, pull the desired revision, rebuild the frontend, and repeat the applicable install
commands with `--force`.

## 2. Prepare private runtime state

Both topologies use a private directory for the daemon socket and app-server socket:

```sh
install -d -m 700 "$HOME/.codex-bridge"
```

The optional Web UI requires a regular password file owned by the current user and inaccessible to
group and other users:

```sh
umask 077
printf '%s\n' 'replace-with-a-long-random-password' > \
  "$HOME/.codex-bridge/web-ui-password"
chmod 600 "$HOME/.codex-bridge/web-ui-password"
```

Do not enable `--web-ui` until this file exists.

## 3. macOS with Codex Desktop

### Why Desktop needs transport interposition

Desktop normally owns a private app-server connection. To let Desktop and `codex-bridge` address
the same app-server instance, start the Codex executable bundled in `ChatGPT.app` on a Unix socket,
forward a loopback TCP WebSocket to that socket, and launch Desktop with:

```text
CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc
```

`CODEX_APP_SERVER_USE_LOCAL_DAEMON` must be absent. This changes only the startup environment and
transport route; it does not modify the signed app bundle.

Fully quit ChatGPT before changing this route. Closing a window is not enough because an existing
process will retain its old environment.

### Verify the topology manually

Use three terminals. First start the bundled app-server, explicitly removing the variables meant
for Desktop so the server cannot recursively connect to its own adapter:

```sh
env -u CODEX_APP_SERVER_WS_URL -u CODEX_APP_SERVER_USE_LOCAL_DAEMON \
  /Applications/ChatGPT.app/Contents/Resources/codex app-server \
  --listen "unix://$HOME/.codex-bridge/bundled-app-server.sock"
```

Start the transport adapter:

```sh
ws-unix-bridge \
  --listen 127.0.0.1:18790 \
  --upstream-socket "$HOME/.codex-bridge/bundled-app-server.sock"
```

Start the bridge daemon:

```sh
codex-bridge \
  --codex-bin /Applications/ChatGPT.app/Contents/Resources/codex \
  --app-server-socket "$HOME/.codex-bridge/bundled-app-server.sock"
```

Finally, start a new Desktop process with the intercepted WebSocket URL:

```sh
env -u CODEX_APP_SERVER_USE_LOCAL_DAEMON \
  CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc \
  /Applications/ChatGPT.app/Contents/MacOS/ChatGPT
```

Do not use `open -a ChatGPT` for this manual test: macOS may reuse an already-running process that
did not inherit the variable.

### Keep the services running with launchd

Use user LaunchAgents under `~/Library/LaunchAgents`. `launchd` does not expand `$HOME` inside
`ProgramArguments`, so replace every example with an absolute path. Keep logs under a directory the
user owns, such as `~/.codex`.

The persistent setup contains four jobs:

- `local.codex-bridge.app-server` runs a wrapper around the bundled app-server and owns
  `/Users/YOU/.codex-bridge/bundled-app-server.sock`.
- `local.codex-bridge.ws-adapter` runs the installed `ws-unix-bridge`, listening only on
  `127.0.0.1:18790` and forwarding to the bundled Unix socket.
- `local.codex-bridge.daemon` runs the installed `codex-bridge`, with both `--codex-bin` and
  `--app-server-socket` set explicitly. This job also owns the optional Web UI.
- `local.codex-bridge.desktop-env` runs a one-shot script that injects the WebSocket route into the
  launchd user environment for newly launched Desktop processes.

The app-server wrapper is important because user-domain launchd environment variables are
inherited by services. Create a script such as `~/.local/bin/codex-bundled-app-server`:

```sh
#!/bin/sh
unset CODEX_APP_SERVER_WS_URL
unset CODEX_APP_SERVER_USE_LOCAL_DAEMON
exec /Applications/ChatGPT.app/Contents/Resources/codex app-server \
  --listen "unix://$HOME/.codex-bridge/bundled-app-server.sock"
```

Make both the app-server wrapper and Desktop environment script executable with `chmod 700`.

The one-shot Desktop environment script should contain:

```sh
#!/bin/sh
launchctl setenv CODEX_APP_SERVER_WS_URL "ws://127.0.0.1:18790/rpc"
launchctl unsetenv CODEX_APP_SERVER_USE_LOCAL_DAEMON
```

Each long-running plist should set `RunAtLoad` and `KeepAlive` to `true`. The environment job uses
`RunAtLoad=true` and `KeepAlive=false`. A minimal daemon plist has this shape; replace `YOU` and add
the optional Web UI arguments described below when needed:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>local.codex-bridge.daemon</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/YOU/.cargo/bin/codex-bridge</string>
    <string>--codex-bin</string>
    <string>/Applications/ChatGPT.app/Contents/Resources/codex</string>
    <string>--app-server-socket</string>
    <string>/Users/YOU/.codex-bridge/bundled-app-server.sock</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key>
  <string>/Users/YOU/.codex/codex-bridge.log</string>
  <key>StandardErrorPath</key>
  <string>/Users/YOU/.codex/codex-bridge.log</string>
</dict>
</plist>
```

Use the same plist structure for the app-server wrapper and `ws-unix-bridge`, substituting the
absolute executables and arguments from the manual startup commands. The environment job runs its
script with `KeepAlive=false`. Validate each file before loading it:

```sh
plutil -lint "$HOME/Library/LaunchAgents/local.codex-bridge.daemon.plist"
launchctl bootstrap gui/"$(id -u)" \
  "$HOME/Library/LaunchAgents/local.codex-bridge.daemon.plist"
```

Bootstrap all four jobs in app-server, adapter, bridge, environment order. `bootstrap` is a
one-time load operation and fails if the label is already loaded. Inspect or restart an installed
job with `launchctl print`, `kickstart`, and `bootout` rather than bootstrapping duplicates.

After the environment job has run, fully quit and relaunch Desktop. Confirm that the new process
received the route:

```sh
launchctl getenv CODEX_APP_SERVER_WS_URL
launchctl getenv CODEX_APP_SERVER_USE_LOCAL_DAEMON
test -S "$HOME/.codex-bridge/bundled-app-server.sock"
lsof -nP -iTCP:18790 -sTCP:LISTEN
ps eww -p "$(pgrep -x ChatGPT | tail -1)" | tr ' ' '\n' | \
  grep '^CODEX_APP_SERVER_'
codexctl status
```

The URL should be present, the local-daemon flag should print nothing, the Unix socket and loopback
listener should exist, and `codexctl status` should succeed.

### Enable the Web UI in the launchd daemon

Add these strings to the daemon plist's `ProgramArguments` array:

```xml
<string>--web-ui</string>
<string>--web-ui-listen</string>
<string>127.0.0.1:18791</string>
<string>--web-ui-user</string>
<string>codex</string>
<string>--web-ui-password-file</string>
<string>/Users/YOU/.codex-bridge/web-ui-password</string>
```

For a trusted LAN, replace the listen address with the host's LAN address or `0.0.0.0:PORT`. For an
HTTPS reverse proxy, also repeat `--web-ui-public-origin` with each exact external origin. Public
origins must use HTTPS and cannot contain a path, query, or fragment.

### Disable Desktop interposition

Quit Desktop, boot out the adapter and environment jobs, clear the variables, and then launch
Desktop normally:

```sh
launchctl bootout gui/"$(id -u)" \
  "$HOME/Library/LaunchAgents/local.codex-bridge.ws-adapter.plist"
launchctl bootout gui/"$(id -u)" \
  "$HOME/Library/LaunchAgents/local.codex-bridge.desktop-env.plist"
launchctl unsetenv CODEX_APP_SERVER_WS_URL
launchctl unsetenv CODEX_APP_SERVER_USE_LOCAL_DAEMON
```

Stopping the adapter does not imply that the app-server or bridge should be stopped; another local
client or Web UI may still be using them.

## 4. Linux or macOS without Desktop

When Desktop is not part of the topology, run the open-source standalone app-server directly on a
Unix socket and connect `codex-bridge` to it. There is no Desktop TCP WebSocket connection, no
`CODEX_APP_SERVER_WS_URL`, and no reason to run `ws-unix-bridge`.

Manual startup:

```sh
install -d -m 700 "$HOME/.codex-bridge"

codex app-server \
  --listen "unix://$HOME/.codex-bridge/app-server.sock"
```

In another terminal:

```sh
codex-bridge \
  --codex-bin "$(command -v codex)" \
  --app-server-socket "$HOME/.codex-bridge/app-server.sock"
```

Add the Web UI flags from the previous section to the `codex-bridge` command if required. The
standalone app-server and bridge should use the same user and the same `CODEX_HOME` so task UUIDs,
rollouts, queue state, and live turns refer to one account state directory.

### systemd user services

On Linux, `%t` is the private user runtime directory and `%h` is the home directory. First find the
absolute standalone Codex path with `command -v codex`, then create
`~/.config/systemd/user/codex-app-server.service`:

```ini
[Unit]
Description=Codex standalone app-server

[Service]
ExecStart=/ABSOLUTE/PATH/TO/codex app-server --listen unix://%t/codex-app-server.sock
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
```

Create `~/.config/systemd/user/codex-bridge.service`:

```ini
[Unit]
Description=Local Codex bridge
Requires=codex-app-server.service
After=codex-app-server.service

[Service]
ExecStart=%h/.cargo/bin/codex-bridge \
  --codex-bin /ABSOLUTE/PATH/TO/codex \
  --app-server-socket %t/codex-app-server.sock
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
```

To enable the Web UI, append `--web-ui`,
`--web-ui-password-file %h/.codex-bridge/web-ui-password`, and any listen/public-origin arguments
to the bridge's `ExecStart`.

Load and validate both services:

```sh
systemctl --user daemon-reload
systemctl --user enable --now codex-app-server.service codex-bridge.service
systemctl --user status codex-app-server.service codex-bridge.service
journalctl --user -u codex-app-server.service -u codex-bridge.service -n 100
codexctl status
```

If the bridge starts before the socket becomes available it returns explicit app-server errors for
live RPC until the server is ready; rollout history remains readable. `Restart=on-failure` handles
process failures, while normal app-server restarts do not require a bridge restart because each
RPC opens a new Unix-socket connection.

## 5. Network and ownership checks

- Keep app-server and daemon Unix sockets private to the service user.
- Keep `ws-unix-bridge` on `127.0.0.1`; it has no application-layer authentication.
- The Web UI requires HTTP Basic Auth. A non-loopback bind exposes task history and write controls.
- Exact HTTPS public origins are allowlisted; arbitrary forwarded `Host` and `Origin` values are
  rejected.
- Use one app-server instance for each live task writer boundary. Do not start a second resume
  process against a task merely to make remote control work.

For deeper transport inspection, logs, protocol probes, and isolated test setup, see
[../DEBUGGING.md](../DEBUGGING.md). For CLI usage after installation, see
[`codexctl.md`](codexctl.md).
