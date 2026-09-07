# Codex Bridge Web UI

This is the mobile-friendly browser client for `codex-bridge`. It lets an authenticated user read
Codex Desktop tasks, follow live activity, steer or queue messages, inspect tool output and diffs,
and pin or archive tasks without operating the Electron window directly. When direct app-server
access is available, pins use the same native pinned section as Codex Desktop. It is not a
standalone backend; all data and control requests go through the bridge daemon.

GitHub Pages builds this same frontend with `VITE_CODEX_BRIDGE_DEMO=1`. In that build,
`src/api.js` sends the existing request objects to the Rust `codex-bridge-demo` WebAssembly state
machine instead of `/api/command`. The demo is deliberately local and finite: it provides sample
sessions, pagination, typed tool details, model settings, and a simulated queue-to-response flow,
but never connects to Codex or executes host operations.

Clean Git states are hidden. Queued-message delivery uses compact, background-free status text in
the empty lane beside the message, with its withdrawal action arranged vertically underneath.
The lane only shows the withdrawal action while a message is buffered; transient delivery labels
remain reserved for the handoff phase. Messages being submitted use a light dashed outline.

Message text and attachments come from durable rollout pagination. Tool groups prefer app-server's
typed `thread/turns/list` items and fall back to raw rollout calls only when app-server data is not
available, avoiding JavaScript-wrapper parsing in the normal path.

The composer uses a single compact **Steer/Queue** button. Clicking it switches how the next
message is delivered without consuming space for two side-by-side controls.

On mobile, focusing the composer moves its mode and widened submit controls above the text so the
editor grows downward. On desktop, the editor stays above a bottom control row and expands upward.

The responsive breakpoint is 800 px. Wider desktop layouts keep both the session history and tools
panels expanded; narrower layouts turn them into left and right drawers.

## Development

The frontend is built with Vite and embedded into the `codex-bridge` Rust binary.

```sh
npm ci
npm run dev
npm run build
```

The development server proxies `/api` to the default bridge address at
`http://127.0.0.1:18791`. The bridge still requires its configured HTTP Basic Auth credentials.

`dist/` is checked in because Rust uses `include_str!` to package these deterministic asset
names into the executable. Run `npm run build` after changing `index.html` or anything under
`src/`.

## Language support

The interface defaults to English. The button beside **Status** switches between English and
Simplified Chinese and stores the choice in browser `localStorage` under
`codex-bridge.language.v1`. Static labels and dynamic task, tool, queue, model, usage, and Git status
messages use the same translation layer.
