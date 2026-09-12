# Installation and user services

`codex-bridge` can install entirely within one user account. The installer builds the embedded
Vite frontend, installs the Rust binaries with Cargo, writes
`~/.config/codex-bridge/config.toml`, and installs one launchd or systemd user service. It never
requires `sudo` and does not modify the signed Codex Desktop application.

| Platform                 | Mode         | App-server                     | User service manager | Adapter          |
| ------------------------ | ------------ | ------------------------------ | -------------------- | ---------------- |
| macOS with Codex Desktop | `desktop`    | Bundled in `ChatGPT.app`       | launchd user agent   | `ws-unix-bridge` |
| Linux without Desktop    | `standalone` | Open-source `codex app-server` | systemd user service | Not needed       |

The bridge reads its runtime choices from the config file and can supervise the selected
app-server itself. In macOS desktop mode it can also supervise `ws-unix-bridge` and publish the
Desktop connection URL. Service definitions therefore start only
`codex-bridge --config /absolute/path/config.toml`; process topology, Web UI, sockets, and runtime
mode no longer need to be duplicated in launchd or systemd arguments.

## Quick installation

Requirements:

- current stable Rust and Cargo;
- Node.js and npm for the one-time frontend build;
- macOS: `/Applications/ChatGPT.app` with its bundled `codex` executable;
- Linux: a standalone `codex` on `PATH` whose `app-server --help` supports a Unix listener.

Clone the repository and run the platform installer as the target user:

```sh
git clone https://github.com/hitsmaxft/codex-bridge.git
cd codex-bridge

# macOS with Codex Desktop
./scripts/install-macos.sh

# Linux with standalone Codex
./scripts/install-linux.sh
```

Pass `--web-ui` to enable a password-protected Web UI on `127.0.0.1:18791`. The installer creates
a private password file but prints only its path. Pass `--no-start` to install files without
loading or enabling services:

```sh
./scripts/install-macos.sh --web-ui
./scripts/install-linux.sh --web-ui --no-start
```

Both scripts:

1. run `npm ci` and build `web-ui/dist`;
2. reuse the checkout's `target` directory with `CARGO_INCREMENTAL=0`;
3. install `codex-bridge` and `codexctl` under `${CARGO_HOME:-$HOME/.cargo}/bin`;
4. preserve an existing config file instead of overwriting local changes;
5. install and optionally start user-owned services.

The macOS script also installs `ws-unix-bridge`. The Linux script deliberately does not install or
start the Desktop adapter.

## Configuration

The default path is `${XDG_CONFIG_HOME:-$HOME/.config}/codex-bridge/config.toml`. Start from
[`../config.example.toml`](../config.example.toml) when configuring manually.

```toml
mode = "desktop" # auto, desktop, or standalone
codex_bin = "/Applications/ChatGPT.app/Contents/Resources/codex"
app_server_socket = "~/.codex-bridge/bundled-app-server.sock"
app_server_thread_cache = 3

[web_ui]
enabled = true
listen = "127.0.0.1:18791"
user = "codex"
password_file = "~/.codex-bridge/web-ui-password"
no_auth = false
public_origins = []

[services]
manage_app_server = true
desktop_interposition = true
ws_bridge_listen = "127.0.0.1:18790"
ws_bridge_bin = "~/.cargo/bin/ws-unix-bridge"

# Optional proxy or other variables for only the app-server child:
[services.app_server_environment]
HTTPS_PROXY = "http://127.0.0.1:7897"
HTTP_PROXY = "http://127.0.0.1:7897"
```

Paths in the TOML file may be absolute or start with `~/`. Unknown fields and invalid values stop
startup with the config filename in the error instead of being ignored.

Configuration precedence is:

```text
command-line option > environment variable > config.toml > selected-mode default
```

Existing launch scripts remain compatible. Use `--config PATH` for another file, `--no-config` to
retain legacy argument-only behavior, or `--mode auto|desktop|standalone` for a one-off mode
override. `--web-ui`/`--no-web-ui` and `--web-ui-no-auth`/`--web-ui-auth` provide explicit boolean
overrides.

Mode defaults apply only when a path was not set explicitly:

- `desktop` selects the Codex executable bundled in `ChatGPT.app` and
  `~/.codex-bridge/bundled-app-server.sock`;
- `standalone` selects `codex` and `$XDG_RUNTIME_DIR/codex-app-server.sock` (falling back to
  `~/.codex-bridge/codex-app-server.sock`);
- `auto` preserves the previous behavior and does not invent an app-server endpoint.

`services.manage_app_server = true` makes the bridge own, stop, start, and restart the selected
`codex app-server` process. This lets an explicit session maintenance command stop the writer
before an atomic rollout repair. A normal Bridge restart preserves a healthy app-server; explicit
app-server restart and ordinal repair remain disruptive. Explicitly disabling management leaves
externally started app-servers untouched. `services.desktop_interposition = true` is valid only in
`desktop` mode;
it also supervises `ws-unix-bridge` and sets `CODEX_APP_SERVER_WS_URL` in the user's launchd
environment. In this topology the bridge starts the app-server with:

```sh
codex -c 'mcp_servers.codex_app={command="",enabled=false}' app-server --listen unix://SOCKET
```

Desktop supplies only an incremental `enabled_tools` value for `mcp_servers.codex_app`; the
disabled base entry keeps that partial configuration structurally valid. If another service
manager owns the shared app-server, it must use the same `-c` override. Leave both service options
false when another manager owns the processes and supplies the required arguments itself.
`services.app_server_environment` is passed only to the managed app-server; the daemon removes
Desktop interposition variables from that child to avoid a recursive connection.

This is a direct-resume compatibility path, not a complete private-MCP handoff. Current Desktop
builds deliberately replace `codex_app` with a disabled entry when app-server uses an external
WebSocket: they do not create or export the native host pipe required by the bundled
`codex-app-tools` process. Bridge therefore cannot make those private tools ready without a future
Desktop/app-server capability handoff. The status panel reports this topology as connected but
limited instead of treating it as a startup failure.

The Web UI status card reports each managed component's live state, restart count, listen endpoint,
and most recent startup/exit error. The event WebSocket publishes a compact service snapshot every
three seconds, so an open settings panel follows recovery without a page reload.

The macOS installer also verifies both the LaunchAgent label and the control socket. If launchd
returns error 5 immediately after unloading an older service, the installer enables the per-user
label, retries bootstrap when necessary, and waits for `codexctl status` before reporting success.

### Exclusive rollout writer safety

All app-servers that use the same `CODEX_HOME` can write the same rollout history. Bundled and
standalone releases may reject each other, but two copies of the same release do not consistently
take a shared writer lock. Never run a private stdio app-server and the Bridge-managed listener at
the same time.

The macOS installer inspects the selected `codex` executable before starting the user service. It
defers startup only when a stdio app-server—either without `--listen` or with the newer explicit
`--listen stdio://` form—is a direct child of the Codex/ChatGPT Desktop process. Short-lived stdio
servers created by Computer Use, tests, terminals, or another app-server are ignored and do not
interrupt Bridge's managed listener. A Desktop-owned conflict commonly appears when installation
runs from an active Desktop session:

```sh
./scripts/install-macos.sh --web-ui --no-start
# Finish the current turn, fully quit Codex Desktop, then start the LaunchAgent.
```

Linux has no Desktop parent to distinguish, so it retains the stricter executable-based rule for
the configured standalone runtime. The installer and daemon never kill a detected stdio process
because it may own an active turn. In bundled macOS mode, runtime rechecks apply only to a direct
Desktop child; unrelated stdio helpers cannot trigger managed app-server replacement.

While Desktop interposition is enabled, Bridge also verifies the launchd user environment every two
seconds. If a Desktop update removes or changes `CODEX_APP_SERVER_WS_URL` (or restores
`CODEX_APP_SERVER_USE_LOCAL_DAEMON`), Bridge restores the WebSocket launch environment. An already
running Desktop process cannot change transport in place; restart that Desktop instance after the
status panel reports a direct Desktop-owned stdio conflict.

Restarting the Bridge daemon does not restart a healthy managed app-server. Bridge leaves the Unix
listener running in an independent process group, probes it for a stability window, and adopts it
after startup. This prevents launchd job cleanup from terminating app-server-owned long-running
exec sessions during a Bridge-only upgrade. Bridge launches a replacement only when the listener
is unavailable or fails the stability probe. When an app-server restart is actually required,
expand **App server** under Settings → Managed components and send the explicit restart request.
The Web UI confirms the disruptive action and reports stop/failure/recovery transitions through
its live event connection.

Bridge also holds an advisory lock at `${CODEX_HOME}/.codex-bridge-managed-writer.lock`. That lock
prevents two current Bridge daemons from managing writers for one history store. It cannot make an
older or independently launched app-server participate in the lock, so process detection is a
safety fence rather than a transactional guarantee. For strict isolation, give unrelated
app-server instances different `CODEX_HOME` directories; their histories will also be separate.

### Optional local Whisper transcription

Build `codex-bridge` with the `whisper` feature and provide a `whisper-server` binary plus a
multilingual whisper.cpp model:

```sh
CARGO_INCREMENTAL=0 cargo install --locked --force \
  --features whisper --path crates/codex-bridge
```

For example, the whisper.cpp multilingual `base` model occupies about 142 MiB on disk and is a
lightweight starting point. Mixed Chinese-English dictation benefits from a larger multilingual
model. Configure the fallback separately from app-server:

```toml
[services.whisper]
enabled = true
bin = "/absolute/path/to/whisper-server"
model = "~/.local/share/whisper.cpp/ggml-base.bin"
listen = "127.0.0.1:18792"
language = "auto"
# Optional decoder context. This is especially useful for mixed-language vocabulary.
# prompt = "简体中文和 English 混合的技术讨论；保留命令、文件名和英文专有名词。"
# Convert Traditional Chinese characters in the transcript to Simplified Chinese.
# simplify_chinese = true
# threads = 4
```

`language` selects Whisper's primary spoken-language token; `auto` detects one primary language but
does not provide a separate mixed-language mode. For speech that is mainly Mandarin with embedded
English terms, `language = "zh"` plus a short `prompt` containing the expected English vocabulary is
usually more stable than auto-detection. The bridge sends both values on every `/inference` request.
`simplify_chinese = true` then applies an embedded OpenCC-compatible `t2s` conversion to the final
text. This conversion preserves Latin text and is independent of recognition, so it cannot repair a
misrecognized English term.

These tuning values are local-only configuration. The Web UI shows the active model filename,
language, prompt, simplified-Chinese setting, and optional thread count under **Settings → Managed
components → Whisper transcription**, but intentionally does not edit them. Restart the bridge after
changing the TOML file. For higher mixed Chinese-English accuracy, use a multilingual `small`,
`medium`, or `turbo` model as host memory and latency permit; do not use an `.en` model for Chinese.

This does not create a second always-running user service. `codex-bridge` remains the
launchd/systemd daemon; it supervises `whisper-server` as a loopback-only child while app-server
realtime transcription is unavailable, and stops it when native transcription recovers. Recorded
PCM stays on the host for this fallback. Enabling Whisper with a bridge binary built without the
feature is a startup error rather than a silently unavailable backend.

### Web UI security

The normal Web UI configuration requires a mode-`0600` password file. To place the loopback
listener behind an authenticated same-host proxy, use:

```toml
[web_ui]
enabled = true
listen = "127.0.0.1:18791"
no_auth = true
public_origins = ["https://codex.example.com"]
```

Unauthenticated Web UI startup is rejected on non-loopback addresses. A direct LAN bind should
retain bridge authentication, and cross-network access should use TLS or a VPN.

## macOS Desktop topology

The installer creates one file, `~/Library/LaunchAgents/local.codex-bridge.daemon.plist`, which
starts the bridge with only `--config`. The daemon owns the bundled app-server and WS adapter child
processes and writes their diagnostics to its log under
`${XDG_STATE_HOME:-$HOME/.local/state}/codex-bridge`. All files are owned by the current user.

Fully quit ChatGPT before installation or before changing this topology. After installation,
relaunch it so the new process inherits:

```text
CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc
```

Verify without relying on a window being open:

```sh
launchctl getenv CODEX_APP_SERVER_WS_URL
test -S "$HOME/.codex-bridge/bundled-app-server.sock"
lsof -nP -iTCP:18790 -sTCP:LISTEN
launchctl print "gui/$(id -u)/local.codex-bridge.daemon"
codexctl status
```

To restart after editing `config.toml`:

```sh
launchctl kickstart -k "gui/$(id -u)/local.codex-bridge.daemon"
```

When upgrading from v0.2.4 or earlier, Bridge may initially detect an app-server left running by
the previous lifecycle. Fully quit ChatGPT, identify the process that owns
`~/.codex-bridge/bundled-app-server.sock`, stop that exact process once, and restart the daemon.
This one-time restart transfers lifecycle ownership and applies the Desktop MCP base configuration.

After relaunching ChatGPT, open an existing thread directly in Desktop before opening it in the Web
UI. A successful direct resume is the acceptance check for the incremental MCP configuration;
`codexctl status` and socket ownership alone verify transport health, not this configuration path.

To disable Desktop interposition while retaining installed files, fully quit ChatGPT and run:

```sh
# Set desktop_interposition = false in config.toml, then restart the daemon.
launchctl kickstart -k "gui/$(id -u)/local.codex-bridge.daemon"
launchctl unsetenv CODEX_APP_SERVER_WS_URL
launchctl unsetenv CODEX_APP_SERVER_USE_LOCAL_DAEMON
```

## Linux standalone topology

The Linux installer creates only `~/.config/systemd/user/codex-bridge.service`. The bridge starts
and supervises the standalone app-server selected by `mode = "standalone"`. There is no Desktop
WebSocket interception and no `ws-unix-bridge`; both bridge and child run as the logged-in user and
share the same Codex home directory.

Verify or inspect logs with:

```sh
systemctl --user status codex-bridge.service
journalctl --user -u codex-bridge.service -n 100
codexctl status
```

After editing `config.toml`:

```sh
systemctl --user restart codex-bridge.service
```

systemd user services normally start when that user logs in. Running them before login requires
administrator-controlled lingering policy; the installer intentionally does not change it.

## Updating

Pull the desired revision and rerun the same installer. It replaces installed binaries and service
definitions but preserves `config.toml` and an existing Web UI password. Installer flags only
choose defaults when creating a config for the first time; edit the existing TOML to change Web UI
or runtime settings:

```sh
git pull --ff-only
./scripts/install-macos.sh   # or install-linux.sh
```

Confirm the effective startup selection in the service log. The bridge prints its mode and loaded
config path before opening sockets.

For command semantics see [`codexctl.md`](codexctl.md). For app-server methods, compatibility, and
debugging boundaries see [`appserver.md`](appserver.md) and [the debugging guide](../DEBUGGING.md).
