# codexapp-cli — Codex App CLI 化方案

## 目标

把正在运行的 Codex App（macOS ChatGPT.app / Codex Desktop）包装成稳定的 CLI/RPC 服务。
采用「状态层 + UI 控制层」混合方案，**不是**单纯模拟键鼠。

## 现实条件

1. Codex Desktop 是 Electron 应用，前端在 app.asar，底层依赖 Codex app-server。
2. 当前 macOS 版 Codex 的 Accessibility 暴露有问题——实测 AXWindows=nil、attribute 为空，
   不能把 AXUIElement 当主接口。
3. Codex app-server 本身已提供完整的结构化接口：thread/read、thread/turns/list、
   turn/start、流式 item events、approval 等。

## 架构

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

## 1. 读取内容：不读屏幕，直接读 thread

命令示例：

```
codexctl current
codexctl messages
codexctl messages --json
codexctl status
codexctl tail
```

内部优先从 app-server 获取：

- `thread/read` — 不 resume thread 就读取持久化会话（对 single-writer 限制非常重要，无需抢 writer）
- `thread/turns/list`
- `thread/items/list`

返回结构：

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

比从窗口解析 Markdown/code block 稳定得多。即使 Codex 窗口滚出历史、虚拟列表卸载 DOM，
仍能获得完整历史。

## 2. 操作 UI：推荐 CDP，而不是 AX

控制 Codex App 启动方式，让 Electron renderer 开启 Chromium DevTools Protocol。
daemon 可调用：

- Browser.getVersion
- Target.getTargets
- Runtime.evaluate
- DOM.getDocument
- DOM.querySelector
- DOM.getBoxModel
- Input.dispatchKeyEvent
- Input.dispatchMouseEvent

命令：

```
codexctl ui dump
codexctl ui find --role textbox
codexctl send "检查这个 PR"
codexctl scroll +800
codexctl click "Allow"
```

读取 DOM（conversation → user-message / assistant-message → markdown / code-block / tool-call / composer），
而不是 pixel → OCR → 猜。

**关键**：Codex 已正常启动且启动时未开 CDP 时，通常不能事后无侵入地加 remote-debugging endpoint。
所以做一个 launcher：

```
CodexBridge.app
    ↓
启动 Codex.app + private CDP endpoint
    ↓
发现 renderer target
    ↓
保持 CDP WebSocket
```

更激进的 app.asar patch（在 Electron main process 植入 Unix-domain socket bridge）稳定性更高，
但每次 Codex 更新都要重新 patch/重新签名，**第一版不做**。

## 3. 输入：模拟真实 Chromium input event

不要主要依赖 `textarea.value = "hello"`（React controlled input 易不同步）。
通过 CDP：

```
focus composer
→ Input.insertText
→ Input.dispatchKeyEvent Enter
```

点击按钮同样：

```
DOM.querySelector
→ DOM.getBoxModel
→ Input.dispatchMouseEvent
```

`codexctl send "继续修复"` 实际执行：

```
locate composer → focus → insert text → keyDown Enter → keyUp Enter → wait conversation state changes
```

## 4. 滚动

物理 UI：

```
codexctl scroll up
codexctl scroll down
codexctl scroll --pixels 1200
codexctl scroll --to bottom
```

用 CDP wheel event。

语义滚动：

```
codexctl scroll --to-message <id>
codexctl scroll --to "command failed"
```

先从结构化 history 找 message，再从 DOM 找对应节点 → scrollIntoView()。

## 5. approval / request-user-input 结构化

App-server 的 approval 是 JSON-RPC server request（文件修改、命令执行、permission request），
客户端返回结构化 decision：

- accept
- acceptForSession
- decline
- cancel

命令：

```
$ codexctl pending
REQUEST 81
type       commandExecution
command    cargo test
cwd        ~/projects/foo
reason     Need to verify tests
$ codexctl approve 81
```

不做「找 Allow 按钮 → 算坐标 → mouse click」。

## 6. active-writer 问题（已知限制）

0.147 以后，thread 有 active-writer ownership。Desktop 已打开某 thread 后，独立 CLI 执行
`codex resume <thread>` 可能报：

```
thread ... already has an active writer
```

且 App 存在不及时释放 writer 的问题。

理想架构：same app-server 供 App 和 codexctl 共用。Codex 支持 Unix socket app-server：
`~/.codex/app-server-control/app-server-control.sock`，CLI 也有 remote app-server 连接能力。
macOS Desktop 曾可用 `CODEX_APP_SERVER_USE_LOCAL_DAEMON=1` 与 CLI 共用 managed daemon。

⚠️ **2026-08-29 最新状态**：Desktop 26.820.60940 已出现 regression，又忽略 local-daemon 配置，
自行启动 private stdio app-server。**不要把「共享 App Server」作为唯一实现基础。**

## 第一版方案

- **codex-bridge**：Rust daemon + 很薄的 macOS launcher
- 对外 Unix socket：`~/.codex-bridge/control.sock`
- CLI：`codexctl`

命令集：

```
codexctl ls
codexctl current
codexctl status
codexctl show
codexctl show --json
codexctl tail
codexctl send "继续"
codexctl steer "先不要改代码，分析原因"
codexctl scroll down
codexctl scroll --to bottom
codexctl pending
codexctl approve <id>
codexctl decline <id>
codexctl interrupt
```

内部优先级：

- 读取：app-server/thread persisted state → CDP DOM → AX → screen capture + vision/OCR
- 写入：same app-server turn/start / turn/steer → CDP Input → CGEvent

**「读」与「写」不强制走同一条路径**。例如 App 正占用 writer：

- 读取历史 → thread/read
- 判断运行状态 → app-server state/session files
- 发送文字 → Codex UI CDP
- 审批 → UI CDP
- 滚动 → UI CDP

## 结论

- AXUIElement + OCR 做完整 Codex parser 不值得投入（AX 暴露不可靠）
- 最实用：**Codex protocol 负责语义，Electron CDP 负责操作当前 App UI**
- 最终目标：让另一个 Agent 程序化操纵 Codex App → 在 codexctl 上直接加 MCP server，
  提供 `codex_read_thread` / `codex_send` / `codex_scroll` / `codex_approve` / `codex_interrupt`
  一组工具，Agent 不需要自己理解 UI。

## 项目结构（建议）

```
codexapp-cli/
├── DESIGN.md          # 本方案
├── Cargo.toml         # workspace
├── crates/
│   ├── codexctl/      # CLI 入口（clap）
│   └── codex-bridge/  # daemon（Rust）
├── launcher/          # macOS launcher（启动 Codex.app + CDP）
└── docs/
```
