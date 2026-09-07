# codex-bridge

[Project site](https://gh.bhee.online/codex-bridge/) ·
[Install](docs/install.md) ·
[`codexctl` reference](docs/codexctl.md) ·
[Design](DESIGN.md) ·
[Debugging](DEBUGGING.md)

Access your Codex tasks directly from a terminal, phone, or private browser UI.

`codex-bridge` is a local control plane around Codex app-server and its rollout store. It exposes
task history, live tool activity, queue/steer/interrupt controls, model settings, and task metadata
through one typed local protocol. Remote-control traffic travels directly between your clients and
your own bridge; it does not depend on the ChatGPT app as a remote-control relay or add another
hosted Codex control service. Model execution still uses the account and network configured by
Codex itself.

## Why use it

- Continue or steer an existing Codex task without screen-scraping the Desktop UI.
- Read durable, paginated task history and structured app-server tool calls from another device.
- Use the same explicit task targeting from the Web UI, `codexctl`, or another local client.
- Keep the control plane local, with a Unix socket by default and authenticated Web UI when enabled.
- Preserve one app-server writer boundary instead of resuming the same task from a competing
  process.

## Web UI

[Try the interactive demo](https://gh.bhee.online/codex-bridge/demo/). It runs the production Vite
frontend against a finite Rust/WASM simulator entirely in the browser. Demo submissions never
contact Codex.

[<img src="docs/assets/codex-bridge-mobile.jpg" alt="codex-bridge mobile Web UI showing a live Codex task" width="360">](docs/assets/codex-bridge-mobile.jpg)

The private UI is responsive across mobile and desktop. It includes project and task navigation,
rendered tool calls and diffs, task pinning and archiving, model selection, Git change summaries,
English and Chinese text, and queue/steer message handoff. Local file links can download regular
files smaller than 16 MiB; the server resolves each link against that task's workspace and rejects
paths or symlinks that escape it.

## How it connects

There are two supported deployment shapes.

### Codex Desktop on macOS

```text
Codex Desktop ── TCP WebSocket ── ws-unix-bridge ── Unix WebSocket ── app-server
                                                         ▲                 ▲
                                                         │                 │
                                             codex-bridge daemon ── rollout store
                                                    ▲
                                             Web UI / codexctl
```

The app-server executable comes from `ChatGPT.app`. Desktop must be launched with
`CODEX_APP_SERVER_WS_URL` pointing at the loopback adapter; `launchctl` keeps the app-server,
adapter, and bridge alive and can enable the Web UI. This transport interposition is local and does
not patch or re-sign the app bundle.

### Standalone Codex, including Linux

```text
standalone codex app-server ── Unix WebSocket ── codex-bridge daemon ── Web UI / codexctl
                                                        │
                                                   rollout store
```

Run the open-source standalone app-server and point `codex-bridge` at its Unix socket. Without
Codex Desktop there is no Desktop connection to intercept, so `ws-unix-bridge` is unnecessary.

See [Installation and service setup](docs/install.md) for global Cargo installation, launchd and
systemd examples, optional Web UI configuration, validation, and rollback.

## Quick check

After installing and starting the services:

```sh
codexctl status
codexctl ls --limit 10
codexctl show --last 20
```

The optional Web UI listens on `127.0.0.1:18791` by default and requires a private password file.
See the installation guide before binding another interface or placing it behind an HTTPS proxy.

## Components

- `crates/codex-bridge`: local daemon, shared protocol, rollout reader, app-server client, and
  embedded Web UI.
- `crates/codexctl`: terminal client for the bridge Unix socket.
- `crates/codex-gui-bridge`: `ws-unix-bridge` Desktop transport plus retained experimental broker
  tooling.
- `web-ui`: Vite source and deterministic production assets embedded in `codex-bridge` at compile
  time.
- `crates/codex-bridge-demo`: bounded Rust/WASM simulator used by GitHub Pages.

The daemon control socket defaults to `~/.codex-bridge/control.sock`; override it with `--socket`
or `CODEX_BRIDGE_SOCKET`. It creates its default directory as `0700` and socket as `0600`.

## Development

```sh
npm --prefix web-ui ci
npm --prefix web-ui run build
CARGO_INCREMENTAL=0 cargo test --workspace
```

The frontend production bundle is checked in under `web-ui/dist` and embedded with `include_str!`.
Rebuild it before compiling Rust after a UI change. For frontend-only work, run
`npm --prefix web-ui run dev`; Vite proxies `/api` to `127.0.0.1:18791`.

The Web UI and CLI share typed bridge requests. Rollout files provide durable reads; live
operations use the selected app-server endpoint. Commands never choose a write target by file
modification time. See [`docs/codexctl.md`](docs/codexctl.md) for command semantics and current
implementation limits.
