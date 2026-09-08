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
a known-stale WASM file merely to discover that it is stale. In demo mode, Vite serves and packages
that exact artifact through `demo-wasm-asset`; do not create or maintain a second copied WASM file.

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

## Privacy before publication

- Before every commit or push, scan tracked and staged files for real usernames, absolute home
  paths, email addresses, device names, local hostnames, private IP addresses, internal URLs,
  cookies, tokens, passwords, API keys, rollout data, and local configuration values.
- Use portable placeholders in documentation and fixtures, such as `~`, `${HOME}`,
  `/Users/yourname`, `example.com`, and the RFC 5737 documentation address ranges. Do not copy a
  developer's actual filesystem layout or network coordinates into examples.
- Treat screenshots and other media as data: inspect visible account/path information and remove
  EXIF location, device, author, and creation metadata before adding them.
- Do not print suspected secrets while auditing. Report only redacted matches and file locations.
  Public project URLs and GitHub noreply identities may remain only when they are intentional.
- Never add local logs, databases, session rollouts, password files, auth caches, download tickets,
  or private service configuration to the repository.
- If sensitive material has already been committed or pushed, stop and report its scope. Removing
  it in a later commit does not remove it from history; coordinate credential rotation and history
  rewriting explicitly rather than doing either silently.
