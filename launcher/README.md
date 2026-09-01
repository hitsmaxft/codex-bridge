# macOS launcher

This directory is reserved for the CodexBridge launcher. A future implementation will:

1. Launch Codex.app with a private Chromium DevTools Protocol endpoint.
2. Discover the Codex Electron renderer target.
3. Pass the endpoint details to `codex-bridge` and manage the application lifecycle.

The first version will neither modify `app.asar` nor re-sign Codex.app.
