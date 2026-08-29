# codexapp-cli

`codexapp-cli` 把正在运行的 Codex Desktop 包装成本地 CLI/RPC 服务。它按
[DESIGN.md](DESIGN.md) 采用混合架构：结构化的 Codex/app-server 状态用于读取会话，
Chromium CDP 等 UI 控制通道用于向当前桌面应用输入和操作。

## 目录

- `crates/codexctl`：面向用户的 CLI，把命令编码为 JSON 请求。
- `crates/codex-bridge`：本地 daemon 与共享协议，监听 Unix socket。
- `launcher`：预留的 macOS launcher，后续负责带私有 CDP endpoint 启动 Codex.app。

CLI 和 daemon 默认使用 `~/.codex-bridge/control.sock`。可通过双方的 `--socket PATH`
参数或 `CODEX_BRIDGE_SOCKET` 环境变量覆盖。daemon 将默认 socket 目录权限设为 `0700`、
socket 权限设为 `0600`；对于自定义路径，只会收紧由 daemon 新建的目录，不修改既有父目录。
启动时只会清理由上次异常退出遗留且已无法连接的 socket。

## 当前可运行范围

构建并分别启动 daemon、CLI：

```sh
cargo build
cargo run -p codex-bridge
```

```sh
cargo run -p codexctl -- status
```

`status` 已贯通 CLI、JSON 行协议和 daemon，可返回服务状态与协议版本。设计中的
`ls`、`current`、`show`、`tail`、`send`、`steer`、`scroll`、`pending`、审批和中断命令
已经进入 CLI/协议骨架；在 app-server/CDP backend 接入前，它们会明确返回
`not_implemented`，不会伪装成已执行。

查看完整命令：

```sh
cargo run -p codexctl -- --help
cargo run -p codexctl -- scroll --help
```
