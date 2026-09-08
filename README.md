# Codex App Server WebUI for Homelab and NAS

Self-hosted Web UI and CLI control plane for OpenAI Codex app-server, designed for an always-on
homelab, NAS, Mac mini, or Linux server.

[Project site](https://gh.bhee.online/codex-bridge/) ·
[Install](docs/install.md) ·
[`codexctl` reference](docs/codexctl.md) ·
[Design](DESIGN.md) ·
[Debugging](DEBUGGING.md)

Run Codex on a machine where your projects already live, then access its tasks from a terminal,
phone, tablet, or private browser UI. Resume sessions, follow live runs, inspect tools and diffs,
and control Codex remotely without using the ChatGPT app as the remote-control layer. A Cloudflare
Tunnel with Cloudflare Access is a practical way to publish the loopback-only Web UI securely
without forwarding a NAS or homelab port to the public Internet.

`codex-bridge` is a local control plane around Codex app-server and its rollout store. It exposes
task history, live tool activity, queue/steer/interrupt controls, model settings, and task metadata
through one typed local protocol. Remote-control traffic travels directly between your clients and
your own bridge; it does not depend on the ChatGPT app as a remote-control relay or add another
hosted Codex control service. Model execution still uses the account and network configured by
Codex itself.

## Built for self-hosted Codex

- **Homelab and NAS:** keep Codex app-server beside your repositories on an always-on Linux host,
  NAS, home server, or Mac mini, then continue the same task from any personal device.
- **Remote development:** put the loopback Web UI behind Cloudflare Tunnel and Access, a private
  VPN, or another authenticated HTTPS reverse proxy instead of exposing the bridge port directly.
- **Away from the desktop:** review a long-running agent, inspect structured tool calls and diffs,
  queue the next message, steer an active turn, or stop it from a phone.
- **One local control plane:** use the responsive Web UI and `codexctl` against the same explicit
  sessions, persistent app-server connection, and bounded history cache.

Cloudflare is optional and transports browser traffic only. Codex model requests still use the
OpenAI account and network configured by Codex app-server.

## Release highlights

### v0.2.0 · 2026-09-08

- **Prompt-responsive live demo:** the GitHub Pages demo now chooses simulated workflows from each
  prompt and continuously updates queue, steer, withdrawal, cancellation, tool, and final-response
  states through the same Rust/WASM contract used by frontend regressions.
- **Editable voice transcription:** recorded or selected audio is decoded in the browser, converted
  to 24 kHz mono PCM, and transcribed through app-server realtime before the resulting text is
  inserted into the composer. Audio is never submitted as a message attachment.
- **Faster mobile task access:** swipe right in the conversation to open the multi-session Tasks
  view directly, while the session drawer remains available from its explicit button.
- **Stable mobile composer:** text remains above a fixed action row as it grows, eliminating
  focus-driven grid reflow. Submit/stop and Queue/Steer controls now use matching widths.
- **Clearer handoff queue:** queued work remains blue while Steer/follow-up messages use a distinct
  bean-green palette in both light and dark themes.

See the complete [v0.2.0 release notes](docs/releases/v0.2.0.md). The original feature baseline is
documented in the [v0.1.0 release notes](docs/releases/v0.1.0.md).

## Why use it

- Turn a homelab, NAS, Mac mini, or Linux workstation into a private Codex app-server host.
- Reach the Web UI remotely through Cloudflare Tunnel + Access without opening an inbound port.
- Continue or steer an existing Codex task without screen-scraping the Desktop UI.
- Read durable, paginated task history and structured app-server tool calls from another device.
- Use the same explicit task targeting from the Web UI, `codexctl`, or another local client.
- Keep the control plane local, with a Unix socket by default and authenticated Web UI when enabled.
- Preserve one app-server writer boundary instead of resuming the same task from a competing
  process.

## Web UI

[Try the interactive demo](https://gh.bhee.online/codex-bridge/demo/). It runs the production Vite
frontend against a live Rust/WASM simulator entirely in the browser. Submit different prompts to
see template-driven progress and structured tools, steer or queue another message while it runs,
withdraw queued work, cancel the run, or try simulated voice-to-text. Demo submissions and images
never contact Codex or leave the page.

[<img src="docs/assets/codex-bridge-mobile.jpg" alt="Codex App Server WebUI showing a live Codex task on mobile" width="360">](docs/assets/codex-bridge-mobile.jpg)

The private UI is responsive across mobile and desktop. It includes project and task navigation,
rendered tool calls and diffs, task pinning, renaming, and archiving, model selection, Git change summaries,
English and Chinese text, and queue/steer message handoff. Local file links can download regular
files smaller than 16 MiB; the server resolves each link against that task's workspace and rejects
paths or symlinks that escape it. Authenticated clients receive a random download ticket that
expires after five minutes and tolerates browser or proxy retries. Tickets live only in bridge
process memory and become invalid after a restart, so downloads do not expose a long-lived
anonymous file endpoint or depend on Basic Auth being forwarded by a navigation.

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

The app-server executable comes from `ChatGPT.app`. In the installed desktop mode,
`codex-bridge` supervises both that process and the loopback adapter, then publishes
`CODEX_APP_SERVER_WS_URL` through the user's launchd environment. launchd only has to keep the one
bridge daemon alive. This transport interposition is local and does not patch or re-sign the app
bundle.

### Standalone Codex, including Linux

```text
standalone codex app-server ── Unix WebSocket ── codex-bridge daemon ── Web UI / codexctl
                                                        │
                                                   rollout store
```

In the installed standalone mode, `codex-bridge` supervises the open-source app-server on its Unix
socket. Without Codex Desktop there is no Desktop connection to intercept, so `ws-unix-bridge` is
unnecessary.

See [Installation and service setup](docs/install.md) for global Cargo installation, launchd and
systemd examples, optional Web UI configuration, validation, and rollback.

The user-level installers build the embedded frontend, install the required Cargo binaries, write
`~/.config/codex-bridge/config.toml`, and start the appropriate user services without `sudo`:

```sh
./scripts/install-macos.sh --web-ui  # Codex Desktop + launchd
./scripts/install-linux.sh --web-ui  # standalone app-server + systemd --user
```

The generated launchd/systemd bridge service contains only `codex-bridge --config ...`. Runtime
mode, managed app-server/adapter lifecycle, sockets, Web UI, authentication, and cache choices live
in the TOML file rather than being duplicated across startup scripts. Existing argument-only and
externally managed deployments remain supported.

## Quick check

After installing and starting the services:

```sh
codexctl status
codexctl ls --limit 10
codexctl show --last 20
```

The optional Web UI listens on `127.0.0.1:18791` by default and normally requires a private
password file. Successful browser Basic Auth creates a seven-day HttpOnly session cookie whose
private token survives ordinary bridge restarts. A same-host authenticated reverse proxy may use
`--web-ui-no-auth`; that mode is rejected unless the listener is bound to a loopback address. See
the installation guide before binding another interface or placing it behind an HTTPS proxy.

## Components

- `crates/codex-bridge`: local daemon, shared protocol, rollout reader, app-server client, and
  embedded Web UI.
- `crates/codexctl`: terminal client for the bridge Unix socket.
- `crates/codex-gui-bridge`: `ws-unix-bridge` Desktop transport plus retained experimental broker
  tooling.
- `web-ui`: Vite source and deterministic production assets embedded in `codex-bridge` at compile
  time.
- `crates/codex-bridge-demo`: bounded Rust/WASM simulator used by GitHub Pages.
- `scripts/install-macos.sh` and `scripts/install-linux.sh`: user-owned service installers.
- `config.example.toml`: documented bridge runtime configuration template.

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
