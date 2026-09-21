# Web UI streaming and reconciliation specification

The Web UI presents rollout snapshots, app-server events, and local optimistic state as one
logical conversation. A transport payload is not a new visual timeline and must not reset the
reader's interaction state.

## Event ordering and recovery

- Every event published through the Bridge event bus carries a monotonically increasing
  `bridge_sequence`. The WebSocket ready frame reports the current sequence so a reconnect can
  detect events that were emitted while the browser was offline.
- Duplicate or older event frames are ignored. A forward gap or a sequence reset invalidates the
  affected browser projections and uses the snapshot path for recovery; it must not be treated as
  an ordinary incremental update.
- Live turn and tool lifecycle events form a temporary overlay over rollout activity. A delayed
  rollout snapshot must not replace that overlay until it confirms the same turn/tool transition.
- Rollout data remains the durable history source, but it is not allowed to act as an unversioned
  second writer for current activity state.

## Identity and incremental updates

- Reconcile threads, turns, messages, activities, tool groups, and tool calls by stable logical
  identifiers. Array position and the assistant message that most recently carried a snapshot are
  not stable identities.
- A tool group that grows by appending tool calls keeps the identity of its first call. The newest
  call is progress within that group, not a replacement group, and must not reset disclosure state.
- Prefer the app-server/rollout message ID, turn ID, tool call ID, or activity key. Message and tool
  indices are compatibility locators for lazy detail reads, not primary UI identities.
- Patch, insert, move, or remove only the nodes whose logical data changed. Do not detach and
  rebuild an unchanged conversation subtree on each event or snapshot.
- Snapshot and live-event representations of the same running turn are fused before rendering.
  A turn stays visually running until an authoritative completion or cancellation is observed.
- Keep each earlier tool group attached to its original message. Only the running turn's last tool
  group receives live activity styling and a not-yet-snapshotted tool placeholder; never flatten
  every tool call in the turn into one aggregate group.
- Completed-turn compaction may defer ordinary tool summaries and message bodies, but it retains
  image-bearing tool summaries. Their media previews remain visible outside the collapsed tool
  details and must not make the whole turn permanently expanded.
- Repeated polling activities such as `wait` and empty `write_stdin` replace their preceding
  activity state instead of accumulating duplicate rows.

## Persistent interaction state

- User-controlled disclosure belongs to the logical entity, not its current DOM parent. An open
  tool group or tool call must stay open when new commentary, tool progress, token usage, or a
  refreshed snapshot moves or updates that entity.
- Detached or superseded DOM nodes and programmatic disclosure synchronization must not write
  interaction state from delayed `toggle`, focus, resize, or observer callbacks. Persist disclosure
  from the user's summary activation intent; use `toggle` only for effects such as lazy loading.
- Preserve already loaded tool details when their group moves between message hosts. Streaming
  updates must not silently issue the same expensive detail request again.
- Preserve selection, focused composer state, and explicit turn/message expansion unless the user
  changes sessions or invokes a collapse action.
- Disclosure controls keep identical layout geometry in their open and closed states. State styling
  may change color, weight, shadow, or transforms, but not margins, borders, or row height.

## Viewport behavior

- When the reader follows the tail, appended content and asynchronous height changes keep the
  newest activity visible.
- When the reader has scrolled away from the tail, preserve a visible message/turn anchor and its
  viewport offset. New data must not jump the reader to either end.
- Layout changes caused by disclosure, image loading, lazy hydration, and snapshot fusion follow
  the same anchor rule as message insertion.

## Required regressions

Streaming changes must cover at least these sequences:

1. Expand a running turn's tool group and one tool call.
2. Apply multiple live progress events and refreshed snapshots that add assistant commentary and
   move the active tool presentation to a newer message.
3. Verify that the group and tool call remain open, loaded detail content is retained, and no
   duplicate detail request is made.
4. Add a later tool group and verify that earlier groups remain separate while only the last group
   carries the running state.
5. Exercise both tail-following and a reader anchored above the tail, including a mobile viewport.
