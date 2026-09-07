# Codex Bridge Web UI

This is the mobile-friendly browser client for `codex-bridge`. It lets an authenticated user read
Codex Desktop tasks, follow live activity, steer or queue messages, inspect tool output and diffs,
and pin or archive tasks without operating the Electron window directly. When direct app-server
access is available, pins use the same native pinned section as Codex Desktop. It is not a
standalone backend; all data and control requests go through the bridge daemon.

Clean Git states are hidden. Queued-message delivery uses compact, background-free status text in
the empty lane beside the message, with its withdrawal action arranged vertically underneath.

The composer uses a single compact **Steer/Queue** button. Clicking it switches how the next
message is delivered without consuming space for two side-by-side controls.

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
