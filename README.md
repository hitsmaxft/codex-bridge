# codexapp-cli

`codexapp-cli` wraps a running Codex Desktop as a local CLI/RPC service. Following
[DESIGN.md](DESIGN.md), it uses a hybrid architecture with separate read and write paths:
rollout/app-server state is used to read sessions, while writes are performed by the Codex CLI.
A Chromium CDP channel is reserved for UI operations that have not yet been implemented.

## Layout

- `crates/codexctl`: the user-facing CLI, which encodes commands as JSON requests.
- `crates/codex-bridge`: the local daemon and shared protocol, listening on a Unix socket.
- `crates/codex-gui-bridge`: experimental Desktop/app-server transports. Cargo registers the
  transparent `ws-unix-bridge` plus the shared-connection broker, supervisor, and `codex-gui`
  client. All of them have fake-endpoint tests; none has completed live Desktop acceptance.
- `launcher`: a reserved macOS launcher that may later start Codex.app with a private CDP endpoint.

The CLI and daemon use `~/.codex-bridge/control.sock` by default. Override it on either side with
`--socket PATH` or the `CODEX_BRIDGE_SOCKET` environment variable. The daemon sets the default
socket directory to mode `0700` and the socket to `0600`. For custom paths, it only tightens
permissions on directories it creates and leaves existing parent directories unchanged. At
startup, it removes only stale sockets left by an abnormal exit that can no longer be connected to.

## GUI transport experiment

Codex Desktop still uses a private stdio app-server by default. As a result, the current
`codexctl steer` and `codexctl interrupt` commands can only operate on threads owned by the target
standalone/shared daemon.

The repository's `ws-unix-bridge` attempts to forward Desktop's TCP WebSocket unchanged to the
official daemon's WebSocket-over-UDS endpoint so both sides use the same app-server instance:

```sh
CARGO_INCREMENTAL=0 cargo test -p codex-gui-bridge --bin ws-unix-bridge
```

This test uses fake endpoints and proves only bidirectional frame forwarding. It does not prove
that the current Desktop honors `CODEX_APP_SERVER_WS_URL`, or that GUI sessions can already be
steered externally. Real validation requires explicit permission to restart Desktop and must use a
brand-new temporary thread. Never use an active project session for the first test.

The shared-connection broker is now part of the workspace build. Its fixture tests cover a fake
Desktop and app-server sharing one upstream, CLI response isolation, an initialized read-only
connection, token rejection, disconnect/reconnect, single-Desktop enforcement, private
`0600` Unix-socket IPC, Desktop-initialize gating, and supervisor child termination. This is
code-level evidence only: it
does not prove that a current Desktop honors `CODEX_APP_SERVER_WS_URL`, preserves approvals and
notifications through the broker, or can be controlled end to end. See
[crates/codex-gui-bridge/README.md](crates/codex-gui-bridge/README.md) for the remaining acceptance
gates.

## Currently runnable features

Build the project, then start the daemon and CLI separately:

```sh
cargo build
cargo run -p codex-bridge
```

```sh
cargo run -p codexctl -- status
```

`status` is wired end to end through the CLI, JSON-lines protocol, and daemon. It returns service
status, protocol version, and read-only rollout-store state. This first-stage backend does not
connect to a running Codex App. It reads `$CODEX_HOME/sessions` (default: `~/.codex/sessions`) and
`session_index.jsonl` instead:

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
- Codex Desktop 26.820.60940 still launches its own private stdio app-server, which exposes no
  control socket. Therefore, steer and interrupt currently work for standalone/shared-daemon
  sessions but cannot cross app-server instances to control private GUI sessions.
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
