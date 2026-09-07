# codex-bridge — Turning the Codex App into a CLI

> **Implementation note:** this document records the original architecture and exploration path.
> The running service now uses rollout files for structured history and the app-server bundled in
> ChatGPT.app for live task control inside Desktop's writer boundary. CDP remains a reserved path,
> not the primary deployed transport. See [README.md](README.md) for current behavior and setup.

## Goal

Wrap the running Codex App (macOS ChatGPT.app / Codex Desktop) into a stable CLI/RPC service. It uses a hybrid **state layer + UI control layer** approach — **not** pure keyboard/mouse simulation.

## Current Reality

1. Codex Desktop is an Electron app; the frontend lives in app.asar and it depends on the Codex app-server underneath.
2. The current macOS Codex Accessibility exposure is broken — in testing, `AXWindows=nil` and attributes are empty, so `AXUIElement` can't be the primary interface.
3. The Codex app-server already provides a complete structured interface: `thread/read`, `thread/turns/list`, `turn/start`, streaming item events, approval, and more.

## Architecture

```
                 ┌─────────────────────┐
CLI: codexctl ──▶│ Local Bridge Daemon │
                 └─────────┬───────────┘
                           │
              ┌────────────┴────────────┐
              │                         │
       Structured State             UI Controller
              │                         │
     app-server / sessions       Chromium CDP / CGEvent
              │                         │
       thread / turn / item        Codex Electron UI
```

## 1. Reading content: read the thread, not the screen

Example commands:

```
codexctl current
codexctl messages
codexctl messages --json
codexctl status
codexctl tail
```

Internally, prefer fetching from the app-server:

- `thread/read` — reads the persisted session without resuming the thread (important given the single-writer restriction; no need to grab the writer)
- `thread/turns/list`
- `thread/items/list`

Return structure:

```json
{
  "thread_id": "...",
  "status": "active",
  "turns": [
    { "role": "user", "items": [...] },
    { "role": "assistant", "items": [
        {"type": "agentMessage", "...": "..."},
        {"type": "commandExecution", "...": "..."}
    ]}
  ]
}
```

This is far more stable than parsing Markdown/code blocks out of the window. Even when the Codex window has scrolled past history and the virtual list unloads DOM nodes, you still get the full history.

## 2. Driving the UI: prefer CDP over AX

Control how the Codex App launches so the Electron renderer enables the Chromium DevTools Protocol. The daemon can then call:

- Browser.getVersion
- Target.getTargets
- Runtime.evaluate
- DOM.getDocument
- DOM.querySelector
- DOM.getBoxModel
- Input.dispatchKeyEvent
- Input.dispatchMouseEvent

Commands:

```
codexctl ui dump
codexctl ui find --role textbox
codexctl send "Review this PR"
codexctl scroll +800
codexctl click "Allow"
```

Read the DOM (conversation → user-message / assistant-message → markdown / code-block / tool-call / composer) instead of pixel → OCR → guess.

**Key point**: Once Codex is already running normally without CDP enabled at launch, you generally can't non-intrusively attach a remote-debugging endpoint afterward. So we build a launcher:

```
CodexBridge.app
    ↓
launch Codex.app + private CDP endpoint
    ↓
discover renderer target
    ↓
keep CDP WebSocket open
```

A more aggressive app.asar patch (embedding a Unix-domain socket bridge in the Electron main process) would be more stable, but every Codex update would require re-patching and re-signing — **not for the first version**.

## 3. Input: simulate real Chromium input events

Don't rely primarily on `textarea.value = "hello"` (React controlled inputs easily get out of sync). Via CDP:

```
focus composer
→ Input.insertText
→ Input.dispatchKeyEvent Enter
```

Clicking buttons works the same way:

```
DOM.querySelector
→ DOM.getBoxModel
→ Input.dispatchMouseEvent
```

`codexctl send "keep fixing"` actually executes:

```
locate composer → focus → insert text → keyDown Enter → keyUp Enter → wait conversation state changes
```

## 4. Scrolling

Physical UI:

```
codexctl scroll up
codexctl scroll down
codexctl scroll --pixels 1200
codexctl scroll --to bottom
```

Uses CDP wheel events.

Semantic scrolling:

```
codexctl scroll --to-message <id>
codexctl scroll --to "command failed"
```

First find the message in the structured history, then locate the corresponding node in the DOM → `scrollIntoView()`.

## 5. Structured approval / request-user-input

App-server approvals are JSON-RPC server requests (file modification, command execution, permission requests); the client returns a structured decision:

- accept
- acceptForSession
- decline
- cancel

Commands:

```
$ codexctl pending
REQUEST 81
type       commandExecution
command    cargo test
cwd        ~/projects/foo
reason     Need to verify tests
$ codexctl approve 81
```

We do **not** do "find the Allow button → compute coordinates → mouse click".

## 6. The active-writer problem (known limitation)

Since 0.147, threads have active-writer ownership. After Desktop has opened a thread, an independent CLI running `codex resume <thread>` may report:

```
thread ... already has an active writer
```

Also, the App has a problem of not releasing the writer in a timely manner.

The adopted architecture is one app-server process shared by Desktop and
`codex-bridge`. It runs the `codex` binary bundled inside ChatGPT.app with an
explicit Unix listener at `~/.codex-bridge/bundled-app-server.sock`. That Unix
socket carries WebSocket: clients must first perform an HTTP Upgrade and then
send JSON-RPC as WebSocket text frames; it is **not** a JSONL/raw Unix stream.
The managed standalone socket under `~/.codex/app-server-control/` is retired
and must never be selected as a fallback.

### GUI transport experiments

The deployed `ws-unix-bridge` is a minimal transparent adapter layer: Desktop
connects to a loopback TCP WebSocket selected by
`CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc`, and the bridge completes the
WebSocket handshake against the bundled app-server's Unix socket. It neither
parses nor rewrites JSON-RPC. Desktop and `codex-bridge` therefore reach the
same app-server instance and stay within one writer-ownership boundary.

The workspace now builds a broker/supervisor experiment that starts its own TCP
WebSocket app-server and shares one upstream connection between GUI traffic and
CLI write requests; read requests use initialized temporary connections. Fake
end-to-end tests now cover injected-response routing, a per-run Desktop
capability token, private same-UID Unix-socket IPC, supervised child shutdown,
disconnect/reconnect, rejection of simultaneous Desktop connections, and
thread tracking from request/response correlation. This still can't replace
the existing `codex-bridge` or claim to have solved Desktop GUI control until
an explicitly authorized temporary GUI thread proves the real environment
variable, notification/approval flow, and UI behavior.

The evidence boundary remains important: a fake WebSocket proves only frame
forwarding. Live Desktop connection, initialization, history loading, and
write operations must be validated separately, using a disposable GUI thread
for the first write test rather than an active project session.

## First-version plan

- **codex-bridge**: a Rust daemon plus a very thin macOS launcher
- Client-facing Unix socket: `~/.codex-bridge/control.sock`
- CLI: `codexctl`

The current implementation stage already separates reads from writes: reads scan `~/.codex/sessions` and `session_index.jsonl`; an explicit thread ID can be read deterministically, and until CDP provides focused-window information, `current` can only use the most recently modified unarchived rollout as a non-authoritative guess. Writes must use an explicit `--thread` or an in-memory `select` result in the daemon; mtime-based inference is forbidden for write commands.

Each session's working path is taken from `session_meta.payload.cwd` recorded when the rollout was created. `ls`, `current`, and `show` run a read-only `git branch --show-current` in that path to populate `git_branch`; if the directory doesn't exist, isn't a Git repository, or is in detached HEAD state, the field is `null` and doesn't affect session reads.

The write backend uses the current Codex CLI and the shared app-server: normal messages go through `codex queue`; for steer/interrupt, the bridge reads the active turn ID from the rollout, opens a WebSocket-over-UDS connection directly, and after completing `initialize`/`initialized`, sends a structured `turn/steer` or `turn/interrupt` respectively. Both paths can only operate on the same app-server instance that owns the target thread; if the Desktop private stdio server is unreachable, it must return an error — it must not fall back to `exec resume` or fake success.

Commands that need host resources such as USB go through a dedicated host-executor: the bridge spawns argv directly in the explicitly selected thread cwd, without using a shell. The built-in policy only allows `wlink`, the CH585 case runner, and a restricted set of Git subcommands; the daemon administrator can replace the policy with JSON. The execution environment strips non-essential variables, caps stdout/stderr at 32 KiB each, defaults to a 300-second timeout, and kills the entire process group on expiry. It is an explicit, controlled write path — not read-only rollout parsing, and not an arbitrary host shell.

Command set:

```
codexctl ls
codexctl current
codexctl status
codexctl show
codexctl show --json
codexctl tail
codexctl send "continue"
codexctl steer "Don't change the code yet; analyze the cause"
codexctl scroll down
codexctl scroll --to bottom
codexctl pending
codexctl approve <id>
codexctl decline <id>
codexctl interrupt
```

Future priorities for the target architecture:

- Read: app-server/thread persisted state → CDP DOM → AX → screen capture + vision/OCR
- Write: same app-server turn/start / turn/steer → CDP Input → CGEvent

**Reads and writes are not forced down the same path.** For example, while the App holds the writer:

- Read history → `thread/read`
- Determine running state → app-server state/session files
- Send normal messages → the current implementation uses `codex queue`; same app-server/CDP may be added later by capability
- Approvals → UI CDP
- Scrolling → UI CDP

## Conclusion

- Building a full Codex parser on AXUIElement + OCR isn't worth the effort (AX exposure is unreliable)
- Most practical: **the Codex protocol handles semantics; Electron CDP manipulates the current App UI**
- End goal: let another agent manipulate the Codex App programmatically → add an MCP server directly on top of codexctl, exposing a toolset of `codex_read_thread` / `codex_send` / `codex_scroll` / `codex_approve` / `codex_interrupt`, so the agent doesn't need to understand the UI itself.

## Project structure (suggested)

```
codex-bridge/
├── DESIGN.md          # this design document
├── Cargo.toml         # workspace
├── crates/
│   ├── codexctl/      # CLI entry point (clap)
│   ├── codex-bridge/  # current daemon (Rust)
│   └── codex-gui-bridge/ # registered experimental transports; fake-tested only
├── launcher/          # macOS launcher (launches Codex.app + CDP)
└── docs/
```
