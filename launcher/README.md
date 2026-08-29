# macOS launcher

这里预留 CodexBridge launcher 的实现。后续 launcher 将负责：

1. 以私有 Chromium DevTools Protocol endpoint 启动 Codex.app；
2. 发现 Codex Electron renderer target；
3. 把 endpoint 信息交给 `codex-bridge`，并维持应用生命周期。

第一版不修改 `app.asar`，也不对 Codex.app 重新签名。
