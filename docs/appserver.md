# Codex app-server interface inventory

This document describes the app-server protocol bundled with Codex Desktop and maps it to the
features currently implemented by `codex-bridge`. It is intended as a feature-planning reference,
not as a compatibility promise: app-server is versioned with Codex, and experimental methods can
change or disappear without notice.

The inventory was generated on 2026-09-07 from:

```text
/Applications/ChatGPT.app/Contents/Resources/codex
codex-cli 0.152.1
```

The bundled schema contains 101 stable client-to-server methods. Passing `--experimental` expands
that set to 157 methods. The same schema defines 11 server-to-client requests that require a
response and 83 one-way server notifications. The
[official App Server documentation](https://learn.chatgpt.com/docs/app-server) describes the
protocol and lifecycle. Exact method availability below is additionally checked against the
generated schema of the installed binary.

The schema baseline compiled into this repository is maintained in
`crates/codex-bridge/src/app_server_schema.rs`. Runtime status reports both that schema version and
the version returned by the app-server `initialize` response, with a `-bundled` or `-standalone`
topology suffix.

## Protocol and transport

app-server uses JSON-RPC-style messages over one of these transports:

- `stdio://`, the default;
- `unix://` or `unix://PATH`;
- `ws://IP:PORT`, with optional bearer-token or ChatGPT-token authentication.

A client first sends `initialize`, then `initialized`, before making ordinary calls. A long-lived
connection receives notifications and server-initiated requests. `codex-bridge` keeps one such
app-server connection and forwards its event stream to Web UI clients over `/api/events`.

Regenerate the machine-readable definitions for the installed version with:

```sh
codex app-server generate-json-schema --out /tmp/codex-app-server-schema
codex app-server generate-json-schema --experimental \
  --out /tmp/codex-app-server-schema-experimental
codex app-server generate-ts --experimental \
  --out /tmp/codex-app-server-types
```

On macOS, replace `codex` with
`/Applications/ChatGPT.app/Contents/Resources/codex` when the CLI is not on `PATH`.

## What codex-bridge uses today

“Direct” means a product path in the daemon calls the typed method. The generic
`app_server_rpc` debugging command can technically forward any method, but that does not count as
a completed bridge or Web UI feature.

| Interface | Current use |
| --- | --- |
| `initialize` | One handshake for the persistent app-server connection. |
| `thread/list` | Fetch app-server thread metadata used by the session list and pin state. |
| `thread/turns/list` | Compatibility inventory only; ordinary message refreshes no longer download full turns. |
| `thread/items/list` | Page recent native items for structured tool rendering and active-turn state helpers. |
| `thread/read` | Read a thread and its composer settings. |
| `config/read` | Fallback source for composer configuration. |
| `account/rateLimits/read` | Display account usage and limits. |
| `model/list` | Populate the model selector. |
| `project/list` | Resolve a project when starting a new thread. |
| `thread/start` | Create a new session. |
| `thread/settings/update` | Change the selected model and related thread settings. |
| `thread/archive` | Archive a session. |
| `thread/section/move` | Move a pinned session into or out of the pinned section. |
| `thread/queue/add`, `thread/queue/list`, `thread/queue/delete`, `thread/queue/start` | Native queue submission, reconciliation, withdrawal, and idle-thread startup. The bridge starts a newly queued submission explicitly when the authoritative thread state is idle. |
| `turn/steer` | Follow up on an active turn. |
| `turn/interrupt` | Stop an active turn. |
| `thread/resume`, `thread/unsubscribe` | Keep a bounded LRU set of recently viewed threads subscribed to events. |
| `turn/start` | Started by app-server's native queue processing after `thread/queue/add`. |

Rollout JSONL files remain the authoritative recovery source for paginated history. The bridge
parses append-only updates from its last complete-line offset instead of reparsing a growing file
on every refresh. File truncation or replacement invalidates that state and triggers a clean
rebuild. Ordinary message refreshes no longer download `itemsView: "full"` turns from app-server.
They page the smaller native item stream only until the visible assistant messages are resolved,
preserving structured `commandExecution`, `fileChange`, and MCP tool rendering without parsing
wrapper scripts. Large message bodies and tool details remain lazy in the browser.

Some older or very long threads return `-32601` for `thread/items/list` even after a successful
metadata-only resume. For those threads only, the rollout compatibility path recognizes the fixed
`text(await tools.<name>(...))` envelope and splits its calls into bounded structured tool entries.
It does not evaluate JavaScript; unknown or malformed wrappers remain visible as raw rollout data.

Event delivery accelerates live updates. The bridge warms local rollout caches as subscribed
threads emit events, periodically discovers active threads through `thread/loaded/list` plus the
metadata-only `thread/read(includeTurns: false)`, and subscribes those active threads. A bounded set
of pinned threads is also warmed locally without resuming every inactive pin into app-server
memory. Each WebSocket connection receives an immediate compact active-thread ID snapshot, and
each subsequent discovery pass sends another snapshot over the existing Web UI event stream. This
lets sidebar activity dots recover after a browser reconnect even when no new
`thread/status/changed` transition occurs. Polling remains as a compatibility and gap-recovery path.

## Client-to-server method catalogue

Methods marked **experimental** only appear when the schema is generated with `--experimental`.

### Session, turn, and history

| Interface | Purpose |
| --- | --- |
| `thread/start` | Create a thread with workspace, model, sandbox, approval, and initial settings. |
| `thread/resume` | Load an existing thread into the current app-server process. |
| `thread/read` | Read one thread; may include its turns. |
| `thread/list` | List threads with pagination and filters. |
| `thread/loaded/list` | List threads currently loaded by this server process. |
| `thread/search` **experimental** | Full-text or substring search across thread metadata/history, with paging and sorting. |
| `thread/searchOccurrences` **experimental** | Find matching occurrences inside threads for search-result previews and navigation. |
| `thread/turns/list` | Page through a thread's structured turns. |
| `thread/items/list` | Page through structured items independently of full turns. |
| `thread/timeline/list` **experimental** | Read a chronological timeline of thread activity. |
| `thread/name/set` | Rename a thread. |
| `thread/metadata/update` | Change project assignment or stored Git metadata. |
| `thread/settings/update` **experimental** | Update persistent thread execution/model settings. |
| `thread/section/move` | Move a thread between UI sections, including the pinned section. |
| `threadSection/list`, `threadSection/create`, `threadSection/update`, `threadSection/delete` | Manage the independently persisted sections used to organize threads. |
| `thread/archive`, `thread/unarchive` | Archive or restore a thread. |
| `thread/delete` | Permanently remove a thread. |
| `thread/fork` | Create a new thread from existing history. |
| `thread/revert` | Replace durable history with the prefix before a selected turn; it does not revert files. |
| `thread/rollback` | Deprecated turn-count form of history rollback; it also does not revert files. |
| `thread/compact/start` | Start context compaction for a long thread. |
| `thread/inject_items` | Append raw Responses API items to model-visible history. This is a low-level integration hook. |
| `thread/unsubscribe` | Stop receiving events for a subscribed thread. |
| `thread/increment_elicitation`, `thread/decrement_elicitation` **experimental** | Track active elicitation/user-input state. |
| `thread/memoryMode/set` **experimental** | Select the thread memory mode. |
| `thread/shellCommand` | Run a shell command in thread context and record it as thread activity. |
| `thread/backgroundTerminals/list`, `thread/backgroundTerminals/terminate`, `thread/backgroundTerminals/clean` **experimental** | Inspect, stop, and clean up background terminals associated with a thread. |
| `turn/start` | Submit user input and start a new agent turn. |
| `turn/steer` | Add input to the active turn. |
| `turn/interrupt` | Interrupt the active turn. |
| `turn/settings/update` **experimental** | Change settings for the active turn. |
| `review/start` | Start a code review against a review target. |
| `getConversationSummary` | Compatibility endpoint for a compact conversation summary. |

### Native queue and goals

| Interface | Purpose |
| --- | --- |
| `thread/queue/add` **experimental** | Add a queued user submission with a client message ID. |
| `thread/queue/list` **experimental** | List queued submissions for a thread. |
| `thread/queue/update` **experimental** | Edit an existing queued submission. |
| `thread/queue/reorder` **experimental** | Change queued-submission order. |
| `thread/queue/delete` **experimental** | Withdraw a queued submission. |
| `thread/queue/start` **experimental** | Start one queued submission, or the next available submission. |
| `thread/goal/set`, `thread/goal/get`, `thread/goal/clear` | Manage the thread objective, status, and optional token budget. |
| `thread/approveGuardianDeniedAction` | Explicitly approve an action previously denied by Guardian. |

### Models, account, configuration, and permissions

| Interface | Purpose |
| --- | --- |
| `model/list` | List selectable models and presentation metadata. |
| `modelProvider/capabilities/read` | Read provider capabilities. |
| `collaborationMode/list` **experimental** | List supported collaboration modes and their settings. |
| `account/read`, `getAuthStatus` | Read account and legacy authentication state. |
| `account/login/start`, `account/login/cancel`, `account/logout` | Manage login lifecycle. |
| `account/rateLimits/read`, `account/usage/read` | Read rate-limit windows, credits, and usage information. |
| `account/rateLimitResetCredit/consume` | Consume a rate-limit reset credit. |
| `account/sendAddCreditsNudgeEmail` | Request an add-credits reminder email. |
| `account/workspaceMessages/read` | Read account/workspace notices. |
| `account/bedrock/discover`, `account/bedrock/setup` **experimental** | Discover and configure an Amazon Bedrock-backed account/provider. |
| `config/read` | Read effective configuration and origins. |
| `config/value/write`, `config/batchWrite` | Persist one or several configuration values. |
| `config/mcpServer/reload` | Reload MCP server configuration. |
| `configRequirements/read` | Read enforced configuration requirements. |
| `permissionProfile/list` | List reusable permission profiles. |
| `experimentalFeature/list`, `experimentalFeature/enablement/set` | Discover feature flags and change their enablement. |
| `skills/list`, `skills/config/write`, `skills/extraRoots/set` | Discover skills and manage skill configuration/search roots. |
| `hooks/list` | List configured hooks. |

### Apps, plugins, marketplaces, and MCP

| Interface | Purpose |
| --- | --- |
| `app/list`, `app/read`, `app/installed` | Discover apps/connectors and inspect installation state. |
| `plugin/list`, `plugin/read`, `plugin/installed` | Discover plugins and inspect installed state. |
| `plugin/search` **experimental** | Search available plugins. |
| `plugin/install`, `plugin/uninstall` | Install or remove a plugin. |
| `plugin/skill/read` | Read a skill supplied by a plugin. |
| `plugin/share/list`, `plugin/share/save`, `plugin/share/checkout`, `plugin/share/delete`, `plugin/share/updateTargets` | Manage shared plugin-development records and targets. |
| `marketplace/add`, `marketplace/remove`, `marketplace/upgrade` | Manage plugin marketplace sources. |
| `mcpServerStatus/list` | Read configured MCP server startup and tool/resource status. |
| `mcpServer/oauth/login` | Begin OAuth for an MCP server. |
| `mcpServer/tool/call` | Invoke an MCP tool through app-server. |
| `mcpServer/resource/read` | Read an MCP resource through app-server. |
| `mcpServer/event/stream/start`, `mcpServer/event/stream/stop` **experimental** | Subscribe to or stop a streamed MCP event source. |

### Projects, environments, and imported agents

| Interface | Purpose |
| --- | --- |
| `project/list`, `project/read` **experimental** | List and inspect projects/workspaces. (`project/list` is present in the experimental generated set for this build.) |
| `project/create`, `project/update`, `project/move`, `project/delete`, `project/import` **experimental** | Manage project records, roots, names, ordering, and imported workspaces. |
| `environment/add`, `environment/info`, `environment/status` **experimental** | Register and inspect remote execution environments. |
| `externalAgentConfig/detect` | Detect configuration from another supported agent. |
| `externalAgentConfig/import`, `externalAgentConfig/import/readHistories`, `externalAgentConfig/import/recordHistory` | Import external-agent configuration and track import history. |

### Commands, host processes, and files

| Interface | Purpose |
| --- | --- |
| `command/exec` | Run an argv-based command in the Codex sandbox, optionally with PTY and streamed output. |
| `command/exec/write`, `command/exec/resize`, `command/exec/terminate` | Control a streamed `command/exec` process. |
| `process/spawn`, `process/writeStdin`, `process/resizePty`, `process/kill` **experimental** | Run and control an unsandboxed host process. This is more privileged than `command/exec`. |
| `fs/readFile`, `fs/readDirectory`, `fs/getMetadata` | Read files, directories, and metadata. |
| `fs/writeFile`, `fs/createDirectory`, `fs/copy`, `fs/remove` | Modify the filesystem. |
| `fs/watch`, `fs/unwatch` | Subscribe to filesystem changes. |
| `fuzzyFileSearch` | Run a one-shot fuzzy workspace file search. |
| `fuzzyFileSearch/sessionStart`, `fuzzyFileSearch/sessionUpdate`, `fuzzyFileSearch/sessionStop` **experimental** | Maintain an incremental fuzzy-search session. |
| `gitDiffToRemote` | Compatibility endpoint for a Git diff against the configured remote/base. |

### Remote control and realtime

| Interface | Purpose |
| --- | --- |
| `remoteControl/enable`, `remoteControl/disable`, `remoteControl/status/read` **experimental** | Enable, disable, and inspect Codex's native remote-control service. |
| `remoteControl/pairing/start`, `remoteControl/pairing/status` **experimental** | Start and inspect device pairing. |
| `remoteControl/client/list`, `remoteControl/client/revoke` **experimental** | List paired clients and revoke access. |
| `thread/realtime/start`, `thread/realtime/stop` **experimental** | Start or stop a realtime session. The Web UI uses a text-output, client-managed session for voice transcription. |
| `thread/realtime/appendText`, `thread/realtime/appendAudio`, `thread/realtime/appendSpeech` **experimental** | Stream text, raw PCM audio, or already-transcribed speakable text into realtime. The Web UI resamples recorded audio to 24 kHz mono PCM and consumes `thread/realtime/transcript/done`; it never submits the recording as a turn attachment. |
| `thread/realtime/listVoices` **experimental** | List voices available for realtime output. |

The current bundled schema exposes no standalone dictation RPC. Its speech-to-text result is the
`thread/realtime/transcript/done` notification. In the bundled build tested on 2026-09-08,
realtime v2 with text output rejects ChatGPT-subscription authentication and requires API-key
authentication. The bridge therefore reports this capability error explicitly; it never falls back
to sending recorded audio as an ordinary Codex message attachment.

### Diagnostics and platform support

| Interface | Purpose |
| --- | --- |
| `server/diagnostics` **experimental** | Read app-server diagnostics useful for a health/debug panel. |
| `feedback/upload` | Upload user feedback and associated diagnostics. |
| `windowsSandbox/readiness`, `windowsSandbox/setupStart` | Inspect and initialize Windows sandbox support. |
| `memory/reset` **experimental** | Reset app-server memory state; destructive and unsuitable for a routine UI action. |
| `mock/experimentalMethod` **experimental** | Protocol test fixture, not a product feature. |

## Server-to-client requests

These are not notifications: app-server waits for the connected client to send a result. A remote
UI must authenticate the user, present the decision or question, and correlate the reply with the
request ID.

| Interface | Client responsibility |
| --- | --- |
| `item/commandExecution/requestApproval` | Approve or reject a command. |
| `item/fileChange/requestApproval` | Approve or reject a file modification. |
| `item/permissions/requestApproval` | Approve or reject a requested permission change. |
| `item/tool/requestUserInput` | Present structured questions and return the user's answers. |
| `mcpServer/elicitation/request` | Present an MCP elicitation request and return a response. |
| `item/tool/call` | Execute a client-provided dynamic tool and return its result. |
| `account/chatgptAuthTokens/refresh` | Refresh ChatGPT authentication tokens for the server. |
| `attestation/generate` | Produce the requested client attestation. |
| `currentTime/read` | Return the client's current time context. |
| `applyPatchApproval` | Legacy file-patch approval request. |
| `execCommandApproval` | Legacy command approval request. |

None of these 11 request types currently has an end-to-end approval/input flow in the Web UI.
The protocol placeholders exposed by `codexctl pending`, `approve`, and `decline` remain
`not_implemented`.

## Server notifications

The 83 notification methods are grouped below. A persistent initialized connection is required to
use them reliably.

| Group | Notifications | Typical UI use |
| --- | --- | --- |
| Thread lifecycle | `thread/started`, `thread/status/changed`, `thread/archived`, `thread/unarchived`, `thread/deleted`, `thread/closed`, `thread/reverted`, `thread/compacted` | Update the session list and active/running state without polling. |
| Thread metadata | `thread/name/updated`, `thread/project/updated`, `thread/settings/updated`, `thread/tokenUsage/updated`, `thread/goal/updated`, `thread/goal/cleared`, `thread/queue/changed` | Keep title, project, model, usage, goal, and queue views synchronized. |
| Environment/project | `project/changed`, `thread/environment/connected`, `thread/environment/disconnected` | Refresh workspace and remote-environment indicators. |
| Turn lifecycle | `turn/started`, `turn/completed`, `turn/diff/updated`, `turn/plan/updated`, `turn/moderationMetadata` | Drive processing state, diff counters, plans, and completion handoff. |
| Item lifecycle | `item/started`, `item/completed`, `rawResponseItem/completed`, `rawResponse/completed` | Add and finalize structured message/tool items. |
| Streaming content | `item/agentMessage/delta`, `item/plan/delta`, `item/reasoning/summaryTextDelta`, `item/reasoning/summaryPartAdded`, `item/reasoning/textDelta` | Stream assistant text, plans, and reasoning summaries. |
| Commands and files | `command/exec/outputDelta`, `process/outputDelta`, `process/exited`, `item/commandExecution/outputDelta`, `item/commandExecution/terminalInteraction`, `item/fileChange/outputDelta`, `item/fileChange/patchUpdated`, `fs/changed` | Live terminal output, tool progress, patches, and file refresh. |
| Approvals and safety | `serverRequest/resolved`, `item/autoApprovalReview/started`, `item/autoApprovalReview/completed`, `autoApprovalReview/strictReviewRequired`, `guardianWarning` | Resolve approval cards and show safety review state. |
| MCP, skills, apps | `item/mcpToolCall/progress`, `mcpServer/oauthLogin/completed`, `mcpServer/startupStatus/updated`, `mcpServer/event/stream/notification`, `skills/changed`, `app/list/updated` | Live tool progress and integration/configuration refresh. |
| Account and model | `account/login/completed`, `account/updated`, `account/rateLimits/updated`, `model/rerouted`, `model/verification`, `model/safetyBuffering/updated`, `modelProvider/authRecoveryStarted`, `modelProvider/authRecoveryCompleted` | Refresh login, quota, effective model, verification, and auth recovery. |
| Hooks and imports | `hook/started`, `hook/completed`, `externalAgentConfig/import/progress`, `externalAgentConfig/import/completed` | Show hook and import progress. |
| Fuzzy search | `fuzzyFileSearch/sessionUpdated`, `fuzzyFileSearch/sessionCompleted` | Render incremental file-search results. |
| Remote control | `remoteControl/status/changed` | Refresh pairing and connection state. |
| Realtime | `thread/realtime/started`, `thread/realtime/itemAdded`, `thread/realtime/item/started`, `thread/realtime/item/transcript/delta`, `thread/realtime/item/completed`, `thread/realtime/transcript/delta`, `thread/realtime/transcript/done`, `thread/realtime/outputAudio/delta`, `thread/realtime/sdp`, `thread/realtime/error`, `thread/realtime/closed` | Implement realtime voice/text transport. |
| Warnings and diagnostics | `error`, `warning`, `deprecationNotice`, `configWarning`, `windows/worldWritableWarning`, `windowsSandbox/setupCompleted` | Surface actionable failures and compatibility warnings. |

## Useful features not yet integrated

### Implemented foundation: persistent app-server session

The bridge now keeps one initialized connection alive, forwards app-server events to authenticated
Web UI WebSockets, and subscribes up to three recently viewed or active threads by default. Use
`--app-server-thread-cache COUNT` to change the bounded subscription cache. It consumes
`thread/status/changed`, turn/item lifecycle events, `thread/queue/changed`, and related updates to
improve:

- active-run and Stop-button detection without reloading;
- the queued-to-accepted-to-processing handoff;
- incremental assistant/tool output;
- live diff counts and model/rate-limit refresh;
- recovery after a brief network gap by reconciling event state with metadata-only `thread/read`
  and incrementally parsed rollout data.

The implementation correlates RPC responses, bounds browser event buffering, reconnects the
browser event stream, and retains polling after an event gap. Rollout reads still finalize the
displayed history, but the compatibility reader is deliberately isolated from the live app-server
transport:

- it reads only response-item envelopes that can affect visible messages and skips compaction,
  reasoning, token, world-state, and event payloads without materializing their contents;
- it keeps an append-only in-memory index of message and tool-output record offsets;
- message pages retain lightweight tool metadata, while the full output is read from one indexed
  JSONL record only when the user expands that tool;
- file identity and processed length invalidate or extend the index when a rollout is replaced or
  appended.

This boundary is intentional. Once a supported app-server history API can supply complete,
pageable messages and tool details, the rollout compatibility reader can be disabled or removed
without changing the Web UI protocol. Until then, disabling it would make older and offline
sessions incomplete, so JSONL remains a fallback rather than a second live event source.

### Strong user-facing candidates

1. **Native queue editor** — add editing and drag ordering through `thread/queue/update` and
   `thread/queue/reorder`. Submission, listing, identity reconciliation, and deletion already use
   the native queue; unsupported app-server versions retain the CLI/SQLite compatibility path.
2. **Search and navigation** — `thread/search`, `thread/searchOccurrences`, and
   `thread/timeline/list` can provide global session search and jump-to-match. These are also
   experimental.
3. **Restore and delete** — session rename already uses `thread/name/set`; expose
   `thread/unarchive` and optionally guarded `thread/delete` for the remaining lifecycle actions.
4. **Fork and history undo** — `thread/fork` and `thread/revert` enable “branch from here” and
   “forget turns after here.” Make it explicit that history revert does not undo workspace files;
   do not build new UI on deprecated `thread/rollback`.
5. **Compaction and goals** — expose manual `thread/compact/start` and goal state from
   `thread/goal/*`, including token budget where useful.
6. **Remote approvals and questions** — implement the server-request loop for command/file/
   permission approvals and `item/tool/requestUserInput`. This is necessary for unattended remote
   control, but requires strong authentication, expiry, and clear target identity.
7. **Code review view** — combine `review/start`, structured file-change items, and
   `turn/diff/updated` for a review-oriented task view.
8. **Integration settings** — use MCP, skills, apps, plugins, and marketplace interfaces for a
   diagnostics/settings panel rather than exposing raw JSON-RPC.

### Higher-risk or exploratory candidates

- `command/exec` could replace part of the custom host-command path while retaining Codex sandbox
  and permission policy. The experimental `process/*` family is explicitly unsandboxed and should
  not be exposed to a remote browser without a separate hardened allowlist.
- `fs/*` and file watching could support a repository browser, but write/remove operations expand
  the bridge's attack surface substantially.
- `remoteControl/*` overlaps this project's purpose and is worth evaluating, but it is
  experimental. Its pairing, network path, authentication, and Desktop dependencies must be
  tested before treating it as a bridge replacement.
- The remaining `thread/realtime/*` output-audio and conversational voice paths could add full
  voice control. The current integration deliberately stops at speech-to-text so the user can edit
  the transcript before submitting it to a Codex turn.
- project/environment management could provide remote workspace creation and attachment, but the
  relevant APIs are experimental in this build.

## Compatibility policy for new integrations

- Generate schemas from the exact app-server binary deployed with the bridge; do not assume a
  schema from another Desktop or CLI version is compatible.
- Feature-detect methods during initialization and hide unsupported controls.
- Keep experimental methods behind a capability flag and provide a fallback where the feature is
  essential.
- Treat server requests as privileged actions, not ordinary notifications.
- After reconnect or a detected event gap, re-read authoritative thread/turn/queue state before
  applying further deltas.
- Keep rollout-file parsing as a recovery path until persistent app-server event delivery has been
  validated across Desktop restart, bridge restart, sleep/wake, and network interruption.
