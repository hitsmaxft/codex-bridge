# codex-bridge Initial Debugging & Fix Guide

This guide is for a third-party agent taking over this repository. The goal is to start `codex-bridge` and `codexctl` without disturbing a running Codex Desktop, validate the read path and write backends against isolated fixtures, track down protocol, rollout parsing, or Codex CLI invocation issues, and land regression-safe fixes.

## 1. Understand the current boundaries first

The project consists of two processes:

- `codex-bridge` is a local daemon that listens on a Unix socket and reads Codex rollout state.
- `codexctl` is a one-shot CLI that sends a single line of JSON over the Unix socket to the daemon and reads a single line of JSON back.

The current protocol version is `14`. Implemented commands:

| Command | Current behavior |
| --- | --- |
| `status` | Returns bridge, protocol version, and rollout store status |
| `ls` | Lists rollouts, cwd, and git branch; can be limited or include archives |
| `select` | Saves the default target thread in the daemon's memory |
| `current` | Returns the most recently modified unarchived rollout |
| `show [THREAD_ID]` | Reads a given thread's cwd, git branch, and user/assistant messages |
| `send` | Queues a message to the target thread via `codex queue` |
| `steer` | Calls `turn/steer` via WebSocket-over-UDS on the shared app-server |
| `interrupt` | Calls `turn/interrupt` on the same control endpoint |
| `host-exec` | Runs allowlist-permitted host argv in the selected thread's cwd |
| `thread_archive` | Archives the selected thread through app-server `thread/archive` |
| `workspace_diff` | Compares the workspace with `thread/read.gitInfo.sha` |

The not-yet-implemented `tail`, `scroll`, `pending`, `approve`, and `decline` must return `not_implemented`. Do not fake execution results to make a demo "succeed".

`current` also has an explicit limitation: it only infers from rollout file modification times and cannot prove which Codex App window currently has focus. `selection.authoritative` in the JSON response must stay `false`; when you need a deterministic read, use `show <THREAD_ID>`.

The target rules for write commands are stricter: you must first run `select <THREAD_ID>`, or pass `--thread <THREAD_ID>` for a single command. The write path never guesses from the most recent mtime and must return `thread_not_selected` when nothing is selected.

## 2. Safety rules

Initial debugging must follow these rules:

1. Use the repository's `fixtures/codex-home`, not `~/.codex` directly.
2. Create debugging directories with `mktemp -d` and specify a socket inside them via `--socket`.
3. Do not launch Codex App, restart Codex App, or connect to its app-server, CDP, or remote-debugging endpoint.
4. Do not run the launcher, modify `app.asar`, or send keystrokes, mouse input, or approvals.
5. Do not delete the default `~/.codex-bridge/control.sock`. If something is already serving on the default socket, leave it alone.
6. Do not modify any real rollout, `session_index.jsonl`, or `state_5.sqlite`.
7. Do not run a smoke test with the real `~/.codex` as `--codex-home` without the user's explicit permission.

The read-only fixture flow is fully isolated from a running Codex App.

## 3. Code map

- [`crates/codex-bridge/src/lib.rs`](crates/codex-bridge/src/lib.rs): request/response structs and protocol version.
- [`crates/codex-bridge/src/sessions.rs`](crates/codex-bridge/src/sessions.rs): rollout scanning, title indexing, message and active turn ID parsing.
- [`crates/codex-bridge/src/write_backend.rs`](crates/codex-bridge/src/write_backend.rs): safe Codex CLI argument invocation and app-server WebSocket-over-UDS JSON-RPC.
- [`crates/codex-bridge/src/host_executor.rs`](crates/codex-bridge/src/host_executor.rs): host-exec policy validation, output length limits, timeouts, and process-group termination.
- [`crates/codex-bridge/src/main.rs`](crates/codex-bridge/src/main.rs): socket lifecycle, request size limits, and daemon dispatch.
- [`web-ui`](web-ui): Vite frontend source; `dist` is the deterministic bundle embedded by the daemon.
- [`crates/codexctl/src/main.rs`](crates/codexctl/src/main.rs): CLI arguments, protocol client, human/JSON output.
- [`fixtures/codex-home`](fixtures/codex-home): a minimal Codex state directory for isolated debugging.
- [`fixtures/fake-codex`](fixtures/fake-codex): an executable write-backend fixture that does not connect to real services.

Key limits:

- Requests are capped at `1 MiB`.
- Responses are capped at `16 MiB`.
- `ls --limit` range is `1..=1000`.
- `show --last` range is `1..=10000`.
- Backend stdout/stderr each return at most `64 KiB`.
- host-exec stdout/stderr each keep at most `32 KiB`, with a default timeout of `300 seconds` and a policy cap of `3600 seconds`.
- The app-server WebSocket handshake, initialize, and steer/interrupt responses each wait at most `10 seconds`.
- The daemon-created default socket is `0600`; the default socket directory is `0700`.
- Pre-existing parent directories of custom socket paths are not silently re-permissioned.

## 4. Build and unit tests

In the commands below, `/path/to/codex-bridge` stands for this repository's root on your machine; replace it with the real absolute path. Run from the repository root:

```sh
cd /path/to/codex-bridge
npm --prefix web-ui ci
npm --prefix web-ui run format:check
npm --prefix web-ui run build
cargo fmt --all -- --check
CARGO_INCREMENTAL=0 cargo test --workspace
```

Run the Vite build before Rust whenever `web-ui/index.html` or `web-ui/src` changes. Cargo does not
invoke npm implicitly: this keeps offline Rust builds reproducible and makes the checked-in bundle
an explicit reviewable artifact.

The project's Rust artifacts all share the repository root's `target`. Don't create a new long-lived target for routine debugging. If you work in a linked worktree, explicitly reuse the main project's target:

```sh
CARGO_TARGET_DIR=/path/to/main/codex-bridge/target \
  CARGO_INCREMENTAL=0 cargo test --workspace
```

Tests must use only temporary directories or repository fixtures; they must not depend on the current user's real `~/.codex`. Current tests verify:

- the tagged JSON protocol structure;
- CLI argument mapping;
- selecting the newer `session_index.jsonl` record between new and old titles;
- returning the working path from the rollout's `session_meta.payload.cwd` and resolving the git branch in that directory;
- stably returning `git_branch: null` for non-Git repositories or deleted cwds;
- keeping only user/assistant messages;
- ignoring developer/internal records;
- returning empty read-only results when the state directory is missing;
- write commands not falling back to a non-authoritative mtime thread;
- queue arguments not passing through a shell;
- full WebSocket-over-UDS handshake, initialize, steer, and interrupt JSON-RPC round trips;
- host-exec shell/path escape rejection, policy substitution, output truncation, non-zero exit, and process-group timeout.

## 5. Launching with isolated fixtures

First build the binaries:

```sh
cd /path/to/codex-bridge
CARGO_INCREMENTAL=0 cargo build --workspace
```

In terminal A, create a temporary socket directory and start the daemon:

```sh
cd /path/to/codex-bridge
debug_root="$(mktemp -d "${TMPDIR:-/tmp}/codex-bridge.XXXXXX")"
bridge_socket="$debug_root/control.sock"
printf 'bridge socket: %s\n' "$bridge_socket"
./target/debug/codex-bridge \
  --socket "$bridge_socket" \
  --codex-home "$PWD/fixtures/codex-home" \
  --codex-bin "$PWD/fixtures/fake-codex"
```

Expect the daemon to stay in the foreground and print only something like:

```text
codex-bridge listening on /private/var/.../control.sock
```

Copy the absolute socket path printed in terminal A into terminal B:

```sh
cd /path/to/codex-bridge
bridge_socket='/private/var/.../control.sock'
```

Then verify in order:

```sh
./target/debug/codexctl --socket "$bridge_socket" status --json
./target/debug/codexctl --socket "$bridge_socket" ls --limit 10
./target/debug/codexctl --socket "$bridge_socket" current
./target/debug/codexctl --socket "$bridge_socket" show
./target/debug/codexctl --socket "$bridge_socket" show \
  00000000-0000-7000-8000-000000000001 --last 1 --json
./target/debug/codexctl --socket "$bridge_socket" select \
  00000000-0000-7000-8000-000000000001
./target/debug/codexctl --socket "$bridge_socket" send 'fixture message'
```

Key acceptance points:

- `status.protocol_version` is `4`.
- `status.rollout_store.available` is `true`, and `read_only` is `true`.
- `ls` lists only the fixture thread with status `unarchived` and cwd `/tmp`; when the fixture cwd is not a valid Git workspace, the branch shows as `<unknown>` and as `null` in JSON.
- `current` explicitly prints `not authoritative for focused window`.
- `show` displays the cwd and git branch, shows only one user message and two assistant messages, and does not show the fixture's developer message.
- `show --last 1 --json` has `messages_returned` equal to `1`, `messages_total` equal to `3`, and the last message's content is `fixture final answer`.
- `selection.authoritative` is `true` with an explicit thread ID.
- After `select`, `current` uses `selected_thread` and no longer uses mtime inference.
- `send` returns `status=queued`, `backend.backend=codex_queue`, and output comes from the fake Codex.

The fixture has no active turn, so both `steer` and `interrupt` should fail safely before connecting to the app-server:

```sh
./target/debug/codexctl --socket "$bridge_socket" steer 'fixture follow-up'
./target/debug/codexctl --socket "$bridge_socket" interrupt
```

Expect a non-zero CLI exit reporting:

```text
no_active_turn: thread 00000000-0000-7000-8000-000000000001 has no active turn in its rollout
```

The fixture daemon's `--codex-bin` points at the repository's fake program, so `send` never touches the real Codex CLI, and the no-active-turn `steer`/`interrupt` never connect to the real app-server or Codex App.

## 6. Inspecting the JSON-lines protocol directly

Only bypass `codexctl` to check the wire protocol if you suspect a CLI argument-mapping or output-rendering problem. macOS's built-in `nc` can connect to a Unix socket:

```sh
printf '%s\n' '{"command":"status"}' | nc -U "$bridge_socket"
printf '%s\n' '{"command":"ls","limit":5,"include_archived":false}' | nc -U "$bridge_socket"
printf '%s\n' \
  '{"command":"select","thread_id":"00000000-0000-7000-8000-000000000001"}' \
  | nc -U "$bridge_socket"
printf '%s\n' \
  '{"command":"show","thread_id":"00000000-0000-7000-8000-000000000001","last":1}' \
  | nc -U "$bridge_socket"
printf '%s\n' \
  '{"command":"send","thread_id":"00000000-0000-7000-8000-000000000001","text":"fixture"}' \
  | nc -U "$bridge_socket"
```

Send exactly one newline-terminated JSON request per connection; the daemon returns exactly one newline-terminated JSON response. A successful response looks like:

```json
{"ok":true,"result":{}}
```

A failed response looks like:

```json
{"ok":false,"error":{"code":"...","message":"..."}}
```

If the raw protocol succeeds but `codexctl` fails, the problem is usually in the CLI argument mapping, the 16 MiB response limit, or the human renderer; if both fail, check the daemon dispatch and session parser first.

## 7. Stopping and cleaning up

1. Press `Ctrl-C` in terminal A and wait for the daemon to exit cleanly.
2. The daemon's `SocketGuard` should remove the socket it created, provided the inode is unchanged.
3. Confirm the socket is gone, then remove the exact resolved temporary directory:

```sh
test ! -S "$bridge_socket"
rmdir "$debug_root"
```

If the first check fails, stop the cleanup and verify the daemon's state first; do not recursively delete the directory.

4. Don't clean the whole repository target; only clean this project's package artifacts:

```sh
cd /path/to/codex-bridge
cargo clean -p codexctl
cargo clean -p codex-bridge
```

If another Cargo process is using the shared target, stop the cleanup and report it instead of racing the concurrent build or deleting its artifacts. `rm -rf target` is forbidden.

## 8. Common failures

### `cannot connect to codex-bridge`

Check:

- whether the daemon is still running in the foreground in terminal A;
- whether both terminals use exactly the same absolute socket path;
- whether you accidentally used the default socket;
- whether the temporary directory was deleted too early.

Don't "fix" a custom-socket problem by deleting `~/.codex-bridge/control.sock`.

### `codex-bridge is already listening`

The target socket already has a reachable daemon. Create another `mktemp -d` directory for this debugging session; don't seize or kill the unknown service.

### `refusing to replace non-socket path`

`--socket` points to a regular file or symlink. Use a fresh temporary path; don't delete unknown files.

### `thread_not_found`

Confirm in order:

1. The file is at `sessions/**/*.jsonl` or `archived_sessions/*.jsonl`;
2. A `type=session_meta` record exists;
3. `session_meta.payload.id` or `session_id` matches the queried ID;
4. `session_meta.payload.cwd` is a string;
5. Use an explicit `show <THREAD_ID>` when querying archived threads.

`current` only considers unarchived rollouts; explicit `show` searches both archived and unarchived rollouts.

### Thread exists but titled `<untitled>`

Titles prefer the `session_index.jsonl` record for that ID with the latest `updated_at`; with no index, the parser uses the text of the first user `response_item` truncated to 120 characters. Check that the field name is `thread_name` and that the time is an ISO-8601 string that compares lexicographically.

### `show` has no messages

The current parser only accepts structures like:

```json
{
  "type": "response_item",
  "payload": {
    "type": "message",
    "role": "user",
    "content": [{"type": "input_text", "text": "hello"}]
  }
}
```

`event_msg`, developer messages, reasoning, command execution, and tool output are skipped. If a newer rollout stores only a different structure, first build a minimal redacted fixture and a regression test, then extend the parser; don't expose all payloads verbatim to the CLI.

### `current` selects the wrong thread

This isn't necessarily a parser bug. The current algorithm is literally "most recently modified unarchived rollout." Use `show <THREAD_ID>` for deterministic reads first. True focused-window identification belongs to a future CDP/UI backend and can't be faked by piling on more file-time heuristics.

### `codex-bridge response exceeds 16 MiB`

First shrink the response with `show --last N`. If you still need to read a full large thread, the correct fix is to design a paginated or streaming protocol and bump the protocol version accordingly, rather than simply removing the cap.

### `thread_not_selected`

Write commands don't use `current`'s mtime inference. Run `codexctl select <THREAD_ID>` first, or use the global `--thread <THREAD_ID>` for a single command:

```sh
codexctl --thread <THREAD_ID> send 'message'
codexctl --thread <THREAD_ID> steer 'follow-up'
codexctl --thread <THREAD_ID> interrupt
codexctl --thread <THREAD_ID> host-exec -- git status
```

`select` is only stored in the current bridge daemon's memory; you must re-select after restarting the daemon.

### `host_exec_not_allowed`, `host_exec_unavailable`, or timeouts

- `host_exec_not_allowed` means the argv didn't match the daemon policy; don't try to bypass it by wrapping in `sh -c`.
- `host_exec_unavailable` means the rule permits it, but there's no executable on PATH, or the workspace script doesn't exist, has no execute bit, or resolves outside the workspace.
- The CLI default timeout is 300 seconds; on timeout, the bridge kills the entire process group of the invocation, and the response keeps `timed_out=true`, `exit_code=-1`, and the truncated output.
- stdout/stderr are capped at 32 KiB each; `*_truncated=true` means later content was discarded.

Policy can be supplied via the daemon's `--host-exec-policy PATH` or `CODEX_BRIDGE_HOST_EXEC_POLICY`; a custom JSON fully replaces the built-in rules, so check it against `host-exec-policy.example.json` first. host-exec always runs in the selected thread's rollout cwd and does not accept an arbitrary `--cwd`.

### `codex_cli_unavailable` or `codex_cli_failed`

- `codex_cli_unavailable` means the selected Codex program couldn't be started. Without an
  explicit `--codex-bin` or `CODEX_BRIDGE_CODEX_BIN`, macOS prefers
  `/Applications/ChatGPT.app/Contents/Resources/codex` and otherwise falls back to `codex` on
  `PATH`; check the reported absolute path and execute permissions.
- `codex_cli_failed` means the Codex CLI started but exited non-zero; keep the stderr and check the CLI version, login state, whether the thread exists, and active-writer conflicts.
- `send` requires the local Codex CLI to provide `codex queue --thread --message`.
- `steer` doesn't use a Codex subprocess, so steer failures should be diagnosed with the `app_server_*` errors below, not `codex_cli_*`.

### `no_active_turn`

`steer` and `interrupt` derive the active turn from the rollout's `task_started`, `task_complete`, and `turn_aborted` events. The app-server isn't called when there's no outstanding `task_started`. If a newer rollout changes the event format, add a redacted fixture and parser test first.

### `app_server_unavailable`, `app_server_timeout`, or `app_server_rejected`

steer/interrupt require an explicitly configured bundled app-server socket, normally
`~/.codex-bridge/bundled-app-server.sock`. A WebSocket runs on this Unix socket: an HTTP Upgrade
first, then JSON-RPC over text frames. Select it with `--app-server-socket PATH` or
`CODEX_BRIDGE_APP_SERVER_SOCKET`. There is deliberately no fallback to
`$CODEX_HOME/app-server-control/app-server-control.sock`, which belongs to the retired standalone
daemon.

- Socket missing or not a Unix socket: `app_server_unavailable`;
- WebSocket handshake, initialize, or method response timeout: `app_server_timeout`;
- Upgrade failure, premature close, or a response that isn't JSON: `app_server_protocol_error`;
- `turnId` expired, thread not owned by that server, or the server rejected the request: `app_server_rejected`.

Don't reintroduce `codex app-server proxy` here: the proxy in Codex 0.151.0 only copies raw stdio bytes to the socket and never performs the WebSocket Upgrade, so the server closes the connection before reading the JSON-RPC. `remoteControlEnabled=true` describes the daemon's remote-control capability and does not turn the local control socket into JSONL.

When Codex Desktop uses a private stdio app-server, the shared socket can't steer/interrupt its turns — that's an app-server instance boundary. Desktop's built-in `codex_app` MCP exposes tools like `send_message_to_thread`, but the entry point is guarded by the GUI's private pipe, code-signed peer verification, and approval routing; it's not a stable API for external CLIs to reuse. Don't bypass signature checks, modify `app.asar`, or queue `/stop` to fake success.

### `invalid_request`

Run `status --json` first to check the daemon's protocol version. Typical protocol v4 requests are:

```json
{"command":"ls","limit":20,"include_archived":false}
{"command":"show","thread_id":null,"last":null}
{"command":"send","thread_id":"THREAD_ID","text":"message"}
{"command":"interrupt","thread_id":"THREAD_ID"}
{"command":"host_exec","thread_id":"THREAD_ID","argv":["git","status"],"timeout_seconds":30}
```

If mixing an old daemon with a new CLI, rebuild and restart the fixture daemon; don't connect to a running unknown bridge.

### `rollout_store_error`

Keep the full error chain and check the path, permissions, and the specific file where the error occurred. If an active rollout's last line is temporarily invalid or contains incomplete UTF-8, completed records should still be readable; other I/O errors shouldn't be silently swallowed.

## 9. Standard fix workflow

Follow this order for any fix:

1. Record the failing command, exact socket, protocol version, error code, and message.
2. Minimize the problem in a copy of `fixtures/codex-home` or a Rust temporary fixture. Don't commit real user conversations, tokens, paths, or full rollouts.
3. Add a test that reproduces the problem first, and confirm the failure occurs in the expected layer.
4. Modify only the layer responsible for the behavior:
   - JSON schema or protocol compatibility: `lib.rs`;
   - rollout discovery and parsing: `sessions.rs`;
   - Codex subprocess and app-server WebSocket RPC: `write_backend.rs`;
   - host command authorization, rate limiting, and timeouts: `host_executor.rs`;
   - daemon error codes, limits, and dispatch: bridge `main.rs`;
   - CLI arguments and display: codexctl `main.rs`.
5. Increment `PROTOCOL_VERSION` on any incompatible protocol-field change, and update the README, this file, and the protocol tests at the same time.
6. Keep error semantics stable: `invalid_request` for bad user input, `thread_not_found` for a missing thread, `rollout_store_error` for storage read failures, `not_implemented` for unimplemented capabilities; Codex CLI and app-server errors use their own `codex_cli_*`/`app_server_*` codes; host-exec uses `host_exec_*` codes.
7. Run formatting checks, targeted tests, and the full workspace tests.
8. Review `git diff --check` and `git status --short` to confirm no real Codex state, sockets, logs, or target artifacts enter the changes.
9. Stop the fixture daemon, then run package-level cache cleanup.

Recommended verification commands:

```sh
cargo fmt --all -- --check
CARGO_INCREMENTAL=0 cargo test -p codex-bridge sessions::tests
CARGO_INCREMENTAL=0 cargo test -p codex-bridge write_backend::tests
CARGO_INCREMENTAL=0 cargo test -p codex-bridge host_executor::tests
CARGO_INCREMENTAL=0 cargo test --workspace
git diff --check
git status --short
```

Here, "fix complete" means at minimum: the minimal regression test passes, the full workspace test suite passes, the fixture CLI paths pass, error codes follow the conventions, and static/fixture validation is never described as verification of real Codex App behavior.

## 10. Optional smoke test on real state

Only after the user explicitly permits it may the daemon point at the real Codex state directory. Even with permission, keep using a separate temporary socket and only call `status`, `ls`, `current`, and `show --last N`:

```sh
codex_state_dir="${CODEX_HOME:-${HOME}/.codex}"
debug_root="$(mktemp -d "${TMPDIR:-/tmp}/codex-bridge-live-read.XXXXXX")"
bridge_socket="$debug_root/control.sock"
./target/debug/codex-bridge \
  --socket "$bridge_socket" \
  --codex-home "$codex_state_dir"
```

A real read-only smoke test still cannot prove:

- the current UI focused window;
- active-writer ownership;
- app-server request availability;
- CDP input, scrolling, or button actions;
- approval, interrupt, or message-send success.

Those are separate acceptance gates for later backends and must be completed in a dedicated debugging window where the user permits affecting the current App.

To confirm the bundled socket transport, run the opt-in read-only test. It only performs the
WebSocket Upgrade, initialize, and `thread/loaded/list`; it won't start or modify a turn:

```sh
CODEX_BRIDGE_TEST_APP_SERVER_SOCKET="$HOME/.codex-bridge/bundled-app-server.sock" \
  CARGO_INCREMENTAL=0 cargo test -p codex-bridge \
  live_app_server_websocket_probe -- --ignored --nocapture
```

If the user further explicitly permits real writes, start with a single identifiable, side-effect-free queue message:

```sh
./target/debug/codexctl --socket "$bridge_socket" select <THREAD_ID>
./target/debug/codexctl --socket "$bridge_socket" send 'codex-bridge write smoke test'
```

Only after confirming the queue message reaches the right thread, and that the target thread is held by the designated shared app-server instance, should you separately request steer and interrupt acceptance. The three must be logged separately:

- send passing only proves `codex queue` accepted and queued the message;
- steer passing proves the shared app-server accepted `turn/steer` for a matching active turn;
- interrupt passing only proves the target thread is on the designated shared app-server and the active `turnId` matches;
- none of them proves CDP/UI control availability.

Don't run the steer smoke test on a session you're currently using. Start a separate app-server socket, create a new thread in a temporary cwd, and call `turn/start` and `turn/steer` only on this new thread; log the target thread ID and cwd before starting, confirming they don't belong to any real project. After the test, stop the temporary daemon but keep the rollout as audit evidence unless the user explicitly asks to delete it.

The repository provides an explicit opt-in live test that creates a new temporary cwd and a new thread, actually calls `turn/start` and `turn/steer`, then tries an interrupt cleanup; never point the socket at the Desktop's private stdio server, and don't reuse an existing thread:

```sh
CODEX_BRIDGE_TEST_ALLOW_WRITE=1 \
CODEX_BRIDGE_TEST_APP_SERVER_SOCKET="$HOME/.codex-bridge/bundled-app-server.sock" \
  CARGO_INCREMENTAL=0 cargo test -p codex-bridge \
  live_app_server_steers_new_isolated_thread -- --ignored --nocapture
```

## 11. Check order for GUI transport experiments

First distinguish the two code bases under `crates/codex-gui-bridge`:

- `ws-unix-bridge` is a transparent TCP-WebSocket to Unix-WebSocket adapter.
- `codex-gui-bridge`, its library, and `codex-gui` are registered experimental broker targets.
  They compile in the workspace and have fake end-to-end tests, but no live Desktop acceptance.

The transparent bridge has only one safety test:

```sh
CARGO_INCREMENTAL=0 cargo test -p codex-gui-bridge --bin ws-unix-bridge
```

It uses a fake Desktop and fake app-server, and its only acceptance criterion is that text frames forward verbatim, bidirectionally, between a TCP WebSocket and a Unix-socket WebSocket. It doesn't validate environment variables, GUI startup, thread ownership, steer, interrupt, or approval.

Run the complete broker fixture suite separately:

```sh
CARGO_INCREMENTAL=0 cargo test -p codex-gui-bridge --all-targets
```

This verifies fake Desktop ↔ broker ↔ fake app-server ↔ CLI routing, read-only
initialization, token rejection, single-Desktop enforcement,
disconnect/reconnect cleanup, current-thread request/response correlation,
same-UID private socket IPC, stale-path protection, and supervised-child
termination. It still does not start Desktop or a real app-server.

Don't do the following during initial debugging:

- quit or restart the current Codex Desktop;
- inject `CODEX_APP_SERVER_WS_URL` into the current GUI;
- launch a second default remote-control daemon;
- send/steer/interrupt on an existing project thread;
- run the broker/supervisor against the real Desktop or a real app-server.

If the user specifically authorizes GUI acceptance, still first record the existing Desktop/daemon PIDs and socket owner, use a brand-new temporary thread, and preserve the following independent evidence in order:

1. Desktop does connect to the loopback bridge;
2. the bridge does connect to the intended daemon/app-server;
3. the thread created by the GUI is visible to the same server's read-only API;
4. `turn/steer` on the new temporary thread is accepted by the same turn;
5. Desktop still receives the full notification, approval, and final message.

Those code-side prerequisites are now covered by the fixture suite. Real GUI
acceptance still requires explicit authorization, the capability-bearing
`CODEX_APP_SERVER_WS_URL` printed by the daemon, and a new temporary thread.
Do not interpret successful fixture tests as Desktop acceptance.
