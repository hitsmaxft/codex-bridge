# Repository workflow

## Build cache

- Reuse this repository's `target` directory for Rust and WASM builds.
- Set `CARGO_INCREMENTAL=0` for validation and release builds. Do not create per-task target
  directories or delete the shared target directory as routine cleanup.

## Web UI and demo validation order

The demo test reads the already-built release WASM artifact directly from
`target/wasm32-unknown-unknown/release/codex_bridge_demo.wasm`. Whenever
`crates/codex-bridge-demo`, the Web UI status contract, or another demo-visible API changes, rebuild
that artifact before running the JavaScript regression test. Do not run the regression once against
a known-stale WASM file merely to discover that it is stale.

Use this order:

```sh
CARGO_INCREMENTAL=0 cargo build --release --target wasm32-unknown-unknown \
  -p codex-bridge-demo
npm --prefix web-ui run test:demo-wasm
npm --prefix web-ui run build
CARGO_INCREMENTAL=0 cargo test --workspace --no-fail-fast
```

Run the Vite production build before compiling or packaging `codex-bridge`, because the daemon
embeds files from `web-ui/dist` at compile time. For a deployed release, rebuild the daemon only
after that sequence, then restart it and verify both `codexctl status` and the HTTP endpoint.
