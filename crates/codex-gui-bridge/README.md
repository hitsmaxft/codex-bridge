# codex-gui-bridge

这个 crate 目前包含两层不同成熟度的实现：

- `ws-unix-bridge` 是 `Cargo.toml` 中唯一注册的 binary。它已有 fake WebSocket/Unix-socket
  往返测试，可作为实验性的透明传输适配器。
- `src/main.rs`、`src/lib.rs`、`broker.rs`、`cli_api.rs`、`protocol.rs`、`supervisor.rs` 和
  `src/bin/codex-gui.rs` 是工作区中的 broker/supervisor 草案。当前 crate 设置了
  `autolib = false`、`autobins = false`，尚未注册这些 target，也缺少相应依赖配置；普通
  `cargo build --workspace` 不会编译或验证它们。

因此，下面“透明桥”部分描述当前可构建路径；“共享连接 broker 草案”只记录设计和待办，不能
作为功能已完成的证据。

## 当前可构建路径：ws-unix-bridge

`ws-unix-bridge` 的目标是让 Codex Desktop 和官方 remote-control daemon 使用同一个
app-server 实例。它终止 Desktop 的 TCP WebSocket 连接，建立到 daemon Unix socket 的另一个
WebSocket 连接，并在两侧之间原样转发 frame，不解析或改写 JSON-RPC。

```text
Codex Desktop
  CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc
          |
          | WebSocket over TCP
          v
  ws-unix-bridge (127.0.0.1:18790)
          |
          | WebSocket over Unix socket
          v
  codex app-server --remote-control --listen unix://
  ~/.codex/app-server-control/app-server-control.sock
```

bridge 会转发 text、binary、ping、pong 和 close frame。它不会自行发送 `initialize`：Desktop
自己的 handshake 及后续 app-server 流量原样通过。每个 Desktop 连接对应一个上游 Unix-socket
连接；任一侧断开都会结束这一对连接。

这条路径只有在 Desktop 实际读取 `CODEX_APP_SERVER_WS_URL` 并连接到 bridge 时才可能让 GUI
与 `codexctl` 共用 daemon。目前自动测试只证明 frame 透明转发，不证明任何 Desktop 版本接受
该环境变量，也不证明 GUI thread 已能被 steer/interrupt。

### 启动

先确认官方 remote-control daemon 是否已持有默认 socket；已有实例时不要启动第二份：

```sh
codex app-server --remote-control --listen unix://
```

然后从仓库启动 bridge：

```sh
CARGO_INCREMENTAL=0 cargo run -p codex-gui-bridge --bin ws-unix-bridge -- \
  --listen 127.0.0.1:18790 \
  --upstream-socket "$HOME/.codex/app-server-control/app-server-control.sock"
```

`--listen` defaults to `127.0.0.1:18790`. `--upstream-socket` defaults to the
path shown above.

只有用户明确允许关闭并重启 Codex Desktop、且已确认不会影响现有工作时，才能让 Desktop 继承
实验性 transport 环境变量：

```sh
CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:18790/rpc \
  /Applications/ChatGPT.app/Contents/MacOS/ChatGPT
```

如果 app bundle 位于其他路径，只替换 executable 路径。不要在普通 fixture 调试中执行这一步，
也不要用现有 585、kof96 或其他真实项目会话做首个写入测试。

bridge 默认只绑定 loopback。它不增加认证或授权，绝不能监听非 loopback 地址或暴露到不可信
网络。

### 测试

```sh
CARGO_INCREMENTAL=0 cargo test -p codex-gui-bridge --bin ws-unix-bridge
```

测试使用临时 Unix socket、fake WebSocket app-server 和 fake Desktop loopback 连接，只验证
双向 frame 原样到达；不会启动真实 app-server、daemon 或 Desktop。

## 尚未接入构建：共享连接 broker 草案

未跟踪源码中的目标架构是：

```text
Desktop ──WS 127.0.0.1:18790/rpc──> broker
                                        │ shared upstream connection
                                        v
                         supervised app-server 127.0.0.1:18791/rpc
                                        ^
                                        │ injected JSON-RPC
codex-gui ──Unix socket /tmp/codex-gui.sock──┘
```

设计意图是让 GUI 流量和 `send`、`steer`、`interrupt` 共用同一条 app-server upstream；只读的
`threads`、`read`、`turns` 使用临时连接。草案 CLI 还定义了 `status`、`current` 和 `tail`。

在把它写成可运行功能前，至少需要完成：

1. 在 Cargo 中显式注册 library、daemon 和 `codex-gui` binary，并补齐 serde、Tokio process/
   time/sync 等依赖后通过 build、Clippy 和测试。
2. 将 CLI socket 放进用户私有目录，设置 `0600` 权限，并用 inode/ownership 检查处理 stale
   socket；不能无条件删除固定 `/tmp/codex-gui.sock`。
3. 为 loopback broker 增加本地授权边界，或明确证明只有受信任 Desktop 能连接；当前任一本地
   进程都可能尝试注入 RPC。
4. 正确终止 supervisor 启动的 app-server child，限制 crash-loop，并验证 Desktop 断线、重连
   和多连接时 upstream 不会串线。
5. 从 app-server response/notification 可靠追踪 GUI 当前 thread；仅观察带 `threadId` 的
   Desktop request 不足以覆盖新建 thread。
6. 增加 fake Desktop ↔ broker ↔ fake app-server ↔ CLI 的端到端测试，再在用户授权的全新临时
   thread 上做真实 GUI 验收。
7. 明确它与现有 `codexctl`/`codex-bridge` 的合并方案，避免长期维护第二套 CLI 协议和输出格式。

完成这些门槛前，不要运行或发布草案 daemon，也不要把源码存在描述成 GUI 控制已完成。
