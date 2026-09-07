# codex-bridge

[Project site](https://gh.bhee.online/codex-bridge/) · Local CLI · Private Web UI · Codex Desktop

`codex-bridge` is a local control plane for a running Codex Desktop. It makes the same tasks that
Desktop owns available through a CLI, a typed local RPC protocol, and an authenticated browser UI.
It is useful when you want to inspect or continue a Codex task from a terminal, phone, or another
local tool without scraping the Desktop window or creating a competing writer for the thread.

## What the service provides

- **Structured task access:** list projects and sessions, read paginated conversation history,
  inspect tool calls, approvals, usage, activity, and the Git diff from the task's creation SHA.
- **Safe task control:** send, queue, steer, interrupt, pin, archive, and update thread settings through
  the bundled app-server used by Desktop. Targets are explicit; write commands never guess from
  file modification time.
- **A mobile-friendly Web UI:** follow active work, restore queued or steered text, inspect rendered
  patch diffs, pin tasks, and switch between English and Chinese from an authenticated browser.
- **A small automation surface:** `codexctl` and the JSON-lines daemon protocol expose the same
  operations for scripts without coupling them to Electron DOM details.

The service deliberately separates persisted reads from live control. Session history comes from
Codex rollout files, while live operations use the app-server bundled in ChatGPT.app and remain in
Desktop's writer-ownership boundary. See [DESIGN.md](DESIGN.md) for the original architecture and
its evidence limits.

Conversation text and attachments retain the rollout store's durable pagination. When app-server
is available, tool groups are overlaid from typed `thread/turns/list` items such as
`commandExecution`, `fileChange`, and `mcpToolCall`; raw rollout tool wrappers are only a fallback.

## Web UI preview

[<img src="docs/assets/codex-bridge-mobile.jpg" alt="codex-bridge mobile Web UI showing a live Codex task" width="360">](docs/assets/codex-bridge-mobile.jpg)

The private browser UI keeps task history, tool activity, the creation-SHA Git diff, usage, model
selection, and Steer/Queue controls available from a phone without replacing Codex Desktop.
Pinned tasks stay synchronized through app-server's native pinned section; the bridge never edits
Codex's SQLite state directly. Clean worktrees omit the Git status label, while delivery state uses
compact, background-free text beside queued messages.

```text
codexctl / Web UI / local clients
              │
              ▼
       codex-bridge daemon
          │          │
          │          └── rollout files ── structured history
          └── bundled app-server ─────── live task control
                       ▲
                       │
                 Codex Desktop
```

## Layout

- `crates/codexctl`: the user-facing CLI, which encodes commands as JSON requests.
- `crates/codex-bridge`: the local daemon and shared protocol, listening on a Unix socket.
- `web-ui`: the Vite frontend source and deterministic production bundle embedded by
  `codex-bridge`.
- `crates/codex-gui-bridge`: Desktop/app-server transports. The deployed path is the transparent
  `ws-unix-bridge` in front of the app-server binary bundled in ChatGPT.app. The crate also retains
  the shared-connection broker, supervisor, and `codex-gui` client as experimental alternatives.
- `launcher`: a reserved macOS launcher that may later start Codex.app with a private CDP endpoint.

The CLI and daemon use `~/.codex-bridge/control.sock` by default. Override it on either side with
`--socket PATH` or the `CODEX_BRIDGE_SOCKET` environment variable. The daemon sets the default
socket directory to mode `0700` and the socket to `0600`. For custom paths, it only tightens
permissions on directories it creates and leaves existing parent directories unchanged. At
startup, it removes only stale sockets left by an abnormal exit that can no longer be connected to.

## Deployed GUI transport

The local architecture uses the Codex binary bundled in ChatGPT.app. It neither installs nor
starts the managed standalone daemon, and it never falls back to one:

```text
Codex Desktop
  -> ws-unix-bridge (TCP 127.0.0.1:18790)
  -> WebSocket over ~/.codex-bridge/bundled-app-server.sock
  -> /Applications/ChatGPT.app/Contents/Resources/codex app-server
```

`ws-unix-bridge` changes only the transport. It accepts Desktop's TCP WebSocket, opens a WebSocket
over the bundled app-server's Unix socket, and forwards frames without parsing or rewriting
JSON-RPC. `CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc` selects that bridge.
`CODEX_APP_SERVER_USE_LOCAL_DAEMON` must be unset. `codex-bridge` receives the bundled socket path
explicitly; if no endpoint is configured, direct RPC is unavailable rather than silently using
`~/.codex/app-server-control/app-server-control.sock`.

The transport forwarding regression remains:

```sh
CARGO_INCREMENTAL=0 cargo test -p codex-gui-bridge --bin ws-unix-bridge
```

See [crates/codex-gui-bridge/README.md](crates/codex-gui-bridge/README.md) for launchd persistence,
validation boundaries, and the retained broker experiment.

## Run the service

Build the frontend bundle and Rust workspace, then start the daemon and CLI separately:

```sh
npm --prefix web-ui ci
npm --prefix web-ui run build
cargo build
cargo run -p codex-bridge
```

Frontend development uses `npm --prefix web-ui run dev`; Vite proxies `/api` to the default
bridge listener at `127.0.0.1:18791`. The checked-in `web-ui/dist` assets use fixed names and are
embedded into the Rust executable with `include_str!`, so rebuild them before compiling Rust after
any UI source change.

```sh
cargo run -p codexctl -- status
```

Start the optional Web UI with a same-user private password file:

```sh
chmod 600 ~/.codex-bridge/web-ui-password
cargo run -p codex-bridge -- \
  --web-ui \
  --web-ui-user codex \
  --web-ui-password-file ~/.codex-bridge/web-ui-password
```

The default listener is `127.0.0.1:18791`; use `--web-ui-listen IP:PORT` for another interface.
HTTP Basic Auth is mandatory, and the daemon rejects password files that are not regular,
same-user, and private from group/other users. It also checks browser `Host` and `Origin` headers;
IP-address hosts and `localhost` are accepted on the configured port, while arbitrary hostnames
are rejected. Binding a LAN interface exposes session content and write operations to that
network, so use it only on a trusted LAN and keep the password private.

When the UI is published through an HTTPS reverse proxy such as Cloudflare Tunnel, add the exact
external origin. The value may be repeated for multiple trusted hostnames:

```sh
codex-bridge --web-ui --web-ui-listen 127.0.0.1:47653 \
  --web-ui-user codex \
  --web-ui-password-file ~/.codex-bridge/web-ui-password \
  --web-ui-public-origin https://codex.example.com
```

Public origins must use HTTPS and cannot contain a path, query, or fragment. Their `Host` and
`Origin` values are matched exactly; arbitrary proxied hostnames remain rejected.

## Web UI behavior

The embedded UI is designed to stay responsive even with a large task history:

- It fetches a compact project index first, sessions 50 at a time for expanded projects, and 30
  messages at a time for the selected task. Older messages are prepended by cursor.
- Nested working directories are grouped under their nearest Git repository root. Internal
  `subagent` and command-policy guardian rollouts are excluded from user-facing counts.
- A lightweight activity poll checks incremental rollout state every 1.5 seconds. Full conversation
  pages reload only when the rollout changed and the reader is at the bottom.
- Large injected contexts and attachments are transferred as summaries and fetched only when
  expanded. Tool calls initially contain only status, counts, and a short semantic preview.

The composer uses one compact **Steer/Queue** button that toggles the send mode on each click.
Submitted text stays visible while it is pending, and deleting a remembered item restores its text
to the composer. Deleting a queued item first cancels the corresponding app-server queue entry. A
steer requested without an active turn is safely converted to a queued send.

The status row compares the working tree with the Git SHA captured when the task was created,
including bounded counts for untracked text files. Collapsed tool groups keep an animated current
operation until the whole turn completes; completed groups summarize tool and edited-file counts.
`apply_patch` expands into a colored unified diff, `web__run` shows concise search titles, and
`write_stdin` is presented as **Waiting for output**. The tools drawer can archive the current task;
Git commit is intentionally not exposed.

The interface defaults to English. The button beside **Status** switches between English and
Simplified Chinese and persists the selection in browser `localStorage`. The mobile layout uses a
single conversation column, project and tool drawers, touch-sized controls, safe-area padding, and
an independently scrolling message pane. At widths above 800 px, the project history and tools
panels are both visible by default; at 800 px and below, they become swipeable drawers.

Message Markdown is rendered safely. Session titles skip injected blocks, and the HTML, JavaScript,
and CSS responses disable browser caching so a restarted daemon appears on the next reload.

The UI exposes the same typed request set as `codexctl`, including explicit session selection,
send, steer, interrupt, approval commands, scroll, and allowlisted host execution. A one-shot
native app-server RPC panel also covers methods that `codexctl` has not wrapped. Commands whose
daemon backend is still a skeleton return the same `not_implemented` response in both interfaces.

`status` is wired end to end through the CLI, JSON-lines protocol, and daemon. It returns service
status, protocol version, and rollout-store state. Reads use `$CODEX_HOME/sessions` (default:
`~/.codex/sessions`) and `session_index.jsonl` without resuming or taking ownership of a thread;
writes use `codex queue` or the configured shared app-server control endpoint:

```sh
codexctl ls --limit 20
codexctl ls --include-archived --json
codexctl current
codexctl show
codexctl show --last 20
codexctl show <THREAD_ID> --json
codexctl select <THREAD_ID> --json
codexctl send "Continue checking" --json
codexctl --thread <THREAD_ID> send "Target this thread explicitly"
codexctl steer "Apply this additional constraint"
codexctl interrupt --json
codexctl host-exec -- wlink --help
codexctl --thread <THREAD_ID> host-exec --timeout 600 -- \
  cases/run-ch585-smoke.sh
```

- `ls` lists threads by rollout file modification time and can include archived threads. Each
  session returns the `cwd` recorded when the rollout was created and runs
  `git branch --show-current` there to populate the nullable `git_branch`. If the directory has
  been deleted, is not a Git repository, or is in detached HEAD state, JSON returns `null` and the
  human-readable output shows `<unknown>`.
- `show <THREAD_ID>` is deterministic. It keeps only user and assistant messages, skips internal
  records, can read a JSONL file while it is still being appended, and displays the same `cwd` and
  `git_branch` fields.
- `current` and `show` without an argument select the most recently modified unarchived rollout.
  Their `selection` is explicitly marked `authoritative=false`, because this does not prove which
  Codex Desktop window has focus.
- `select` stores a default thread in daemon memory. `--thread` overrides that selection for one
  `show`, `send`, `steer`, or `interrupt` command. Write commands never fall back to mtime and
  return `thread_not_selected` when no target has been selected.
- `send` invokes `codex queue --thread ID --message TEXT`; it does not use resume or take ownership
  from the active writer.
- `steer` reads the active turn ID from the rollout and sends `turn/steer` through the shared
  app-server's WebSocket-over-UDS control endpoint. This is genuine same-turn injection and does
  not spawn a separate `codex exec resume` writer.
- `interrupt` sends `turn/interrupt` through the same endpoint. The target thread must belong to
  that app-server instance, and the rollout's active turn ID must still match. Otherwise the
  command returns an explicit error rather than pretending to succeed.
- Desktop connects through `ws-unix-bridge` to the same bundled app-server endpoint used by
  `codex-bridge`, so `steer` and `interrupt` stay within one writer-ownership boundary. There is no
  standalone fallback.
- `host-exec` runs an allowlisted host command in the selected thread's `cwd`, primarily for USB
  flashing and hardware tests. Requests carry an argv array and never pass through a shell. The
  built-in policy allows only `wlink`, `cases/run-ch585-*.sh`, and restricted Git subcommands.
  stdout and stderr are each capped at 32 KiB, the default timeout is 300 seconds, and the maximum
  process-group lifetime is 3600 seconds. A non-zero exit or timeout still returns captured output
  and the exit code, while `codexctl` exits non-zero.

`tail`, `scroll`, `pending`, and approval commands are still CLI/protocol skeletons. Until an
app-server or CDP backend is connected, they return `not_implemented` instead of pretending that
an action occurred.

Use `--codex-home PATH` to point the daemon at another read-only state directory for offline or
isolated testing. On the write path, the bridge prefers the CLI bundled at
`/Applications/ChatGPT.app/Contents/Resources/codex`, then falls back to `codex` on `PATH`.
Override it with `--codex-bin PATH`/`CODEX_BRIDGE_CODEX_BIN`, and select the shared
WebSocket-over-UDS endpoint for steer and interrupt with
`--app-server-socket PATH`/`CODEX_BRIDGE_APP_SERVER_SOCKET`. Do not treat that endpoint as JSONL:
`codex app-server proxy` forwards raw bytes and cannot perform the WebSocket Upgrade.

Replace the built-in host-exec policy with a JSON file via `--host-exec-policy PATH` or
`CODEX_BRIDGE_HOST_EXEC_POLICY`; see
[host-exec-policy.example.json](host-exec-policy.example.json). Allowing a workspace script means
trusting its current contents, so public or untrusted repositories should tighten or remove those
rules. See [DEBUGGING.md](DEBUGGING.md) for the complete isolated startup, protocol inspection,
troubleshooting, and repair workflow for third-party agents.

View the complete command help:

```sh
cargo run -p codexctl -- --help
cargo run -p codexctl -- scroll --help
```
