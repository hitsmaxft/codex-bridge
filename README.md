# codex-bridge

[Project site](https://gh.bhee.online/codex-bridge/) · Local CLI · Private Web UI · Codex Desktop

`codex-bridge` wraps a running Codex Desktop as a local CLI/RPC service. Following
[DESIGN.md](DESIGN.md), it uses a hybrid architecture with separate read and write paths:
rollout/app-server state is used to read sessions, while writes are performed by the Codex CLI.
A Chromium CDP channel is reserved for UI operations that have not yet been implemented.

## Layout

- `crates/codexctl`: the user-facing CLI, which encodes commands as JSON requests.
- `crates/codex-bridge`: the local daemon and shared protocol, listening on a Unix socket.
- `crates/codex-gui-bridge`: Desktop/app-server transports. The deployed path is the transparent
  `ws-unix-bridge` in front of the managed standalone daemon. The crate also retains the
  shared-connection broker, supervisor, and `codex-gui` client as a tested but unadopted
  alternative.
- `launcher`: a reserved macOS launcher that may later start Codex.app with a private CDP endpoint.

The CLI and daemon use `~/.codex-bridge/control.sock` by default. Override it on either side with
`--socket PATH` or the `CODEX_BRIDGE_SOCKET` environment variable. The daemon sets the default
socket directory to mode `0700` and the socket to `0600`. For custom paths, it only tightens
permissions on directories it creates and leaves existing parent directories unchanged. At
startup, it removes only stale sockets left by an abnormal exit that can no longer be connected to.

## Deployed GUI transport: Solution B

The adopted local architecture shares the managed standalone app-server between Desktop, CLI
control, and remote control:

```text
Codex Desktop (TCP 127.0.0.1:61510)
  -> ws-unix-bridge (TCP 127.0.0.1:18790)
  -> WebSocket over ~/.codex/app-server-control/app-server-control.sock
  -> managed standalone app-server (PID 50310 during acceptance)
  -> remote control / mobile client
```

`ws-unix-bridge` changes only the transport. It accepts Desktop's TCP WebSocket, opens a WebSocket
over the daemon's Unix socket, and forwards frames without parsing or rewriting JSON-RPC. The
standalone daemon is therefore a core dependency of this deployment, not an optional experiment.
Keeping one app-server instance also keeps GUI, CLI, and mobile remote control on the same thread
store and writer-ownership boundary.

This path completed live Desktop acceptance on 2026-09-04 with Desktop `26.831.21537`, its bundled
CLI `0.152.1`, and standalone app-server `0.153.2`. The running Desktop inherited
`CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc`, established the expected TCP connection to the
bridge, initialized with `transport=websocket`, and received successful `thread/list`,
`thread/turns/list`, remote-control, and `turn/steer` responses. GUI history opened successfully;
mobile full-history synchronization was also accepted during the deployment check.

Those observations prove the current local Desktop-to-daemon path and session-history loading for
the exact accepted versions. They are not a general compatibility guarantee for future Desktop or
standalone releases, and they do not replace a login/reboot or long-duration recovery test. The
fake-endpoint test remains useful as a narrower forwarding regression:

```sh
CARGO_INCREMENTAL=0 cargo test -p codex-gui-bridge --bin ws-unix-bridge
```

The shared-connection `codex-gui-bridge` broker remains in the workspace but was not selected for
deployment. Its fixture tests cover protocol isolation, capability-token enforcement, private CLI
IPC, reconnect handling, and supervised-child cleanup; no live acceptance claim is made for that
alternative. See [crates/codex-gui-bridge/README.md](crates/codex-gui-bridge/README.md) for the
Solution B topology, manual validation, launchd persistence, rollback, evidence record, and broker
status.

## Currently runnable features

Build the project, then start the daemon and CLI separately:

```sh
cargo build
cargo run -p codex-bridge
```

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

The embedded UI first transfers a compact project index, then fetches sessions only for an opened
project (50 at a time), and conversation messages only for the selected thread (30 at a time).
Nested working directories are grouped under their nearest Git repository root. Older messages
are prepended by cursor without retransmitting or rebuilding the messages already on screen.
Internal `subagent` rollouts (including command-policy guardian evaluations) are excluded from the
project counts and thread list, matching the user-visible Codex history. Session summaries and
parsed messages are cached briefly in the daemon. The mobile layout follows
a single-column Codex-style conversation view with project and tool drawers, touch-sized controls,
an independently scrolling message area, and a safe-area-aware bottom composer. The composer has
an explicit `Steer`/`Queue` mode selector and keeps submitted messages visible while they are being
steered or queued. If no turn is active, a requested steer is automatically changed to queue/send.
A lightweight incremental activity poll updates the running/idle indicator every 1.5 seconds and
reloads messages only when the rollout changed and the reader is at the bottom; it does not poll
the full conversation. Message text is rendered as safe Markdown. Automatically inserted transcript
deltas, environment and instruction blocks, memory citations, and complete attachment groups are
transferred as compact summaries; opening a summary fetches its full content on demand. Tool calls
are likewise grouped below their preceding assistant message: the initial page includes only names,
status, and short previews, while nested expansion loads full arguments and results.
`exec_command` wrappers are reduced to their command and execution options; `apply_patch` wrappers
are shown as edited file lists with added/deleted line counts and expose the full patch only after
expansion. Session titles skip injected blocks. The HTML response disables browser caching so a
restarted daemon is reflected by the next reload.

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
- Without transport interposition, Desktop may own a separate private app-server and the commands
  cannot cross that process boundary. In the deployed Solution B configuration, Desktop connects
  through `ws-unix-bridge` to the same managed standalone daemon, so `steer` and `interrupt` can
  target threads owned by that shared instance.
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
isolated testing. On the write path, select the Codex executable with
`--codex-bin PATH`/`CODEX_BRIDGE_CODEX_BIN`, and select the shared WebSocket-over-UDS endpoint for
steer and interrupt with `--app-server-socket PATH`/`CODEX_BRIDGE_APP_SERVER_SOCKET`. Do not treat
that endpoint as JSONL: `codex app-server proxy` forwards raw bytes and cannot perform the
WebSocket Upgrade.

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
