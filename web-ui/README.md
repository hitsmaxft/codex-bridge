# Codex Bridge Web UI

The browser UI is built with Vite and embedded into the `codex-bridge` Rust binary.

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
