# codexapp-cli

`codexapp-cli` 把正在运行的 Codex Desktop 包装成本地 CLI/RPC 服务。它按
[DESIGN.md](DESIGN.md) 采用读写分离的混合架构：rollout/app-server 状态用于读取会话，
写消息由 Codex CLI 执行；尚未实现的 UI 操作才预留 Chromium CDP 通道。

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

`status` 已贯通 CLI、JSON 行协议和 daemon，可返回服务状态、协议版本和只读 rollout
存储状态。第一阶段的状态后端不连接正在运行的 Codex App，而是读取
`$CODEX_HOME/sessions`（默认 `~/.codex/sessions`）与 `session_index.jsonl`：

```sh
codexctl ls --limit 20
codexctl ls --include-archived --json
codexctl current
codexctl show
codexctl show --last 20
codexctl show <THREAD_ID> --json
codexctl select <THREAD_ID> --json
codexctl send "继续检查" --json
codexctl --thread <THREAD_ID> send "直接指定目标"
codexctl steer "补充约束"
codexctl interrupt --json
```

- `ls` 按 rollout 文件修改时间列出线程，并支持包含归档线程。
- `show <THREAD_ID>` 是确定性的；解析结果只保留 user/assistant message，跳过内部记录，
  也允许读取仍在追加的 JSONL 文件。
- 无参数的 `current` 与 `show` 暂时选择最近修改的未归档 rollout。返回的 `selection`
  会明确标记 `authoritative=false`，因为这不能证明哪个 Codex Desktop 窗口当前获得焦点。
- `select` 在 daemon 内存中保存默认 thread；`--thread` 可为 `show`、`send`、`steer`、
  `interrupt` 单次覆盖选择。写命令没有 mtime fallback，未选择目标时返回
  `thread_not_selected`。
- `send` 调用 `codex queue --thread ID --message TEXT`，不通过 resume 抢占 active writer。
- `steer` 在当前 Codex CLI 没有 queue-steer 参数时调用 `codex exec resume ID PROMPT`。
  这是“续写 follow-up”fallback，不承诺能注入一个已运行的 turn；遇到 active-writer 冲突会
  原样返回 `codex_cli_failed`。
- `interrupt` 从 rollout 解析活动 turn ID，再通过 `codex app-server proxy` 向共享 daemon
  发送 `turn/interrupt`。Desktop 使用 private stdio app-server 时会返回明确错误，不能把失败
  报告成已中断。

`tail`、`scroll`、`pending` 和审批仍只有 CLI/协议骨架；在
app-server/CDP backend 接入前会明确返回 `not_implemented`，不会伪装成已执行。

daemon 可通过 `--codex-home PATH` 指向另一份只读状态目录，便于离线使用和隔离测试。
写路径可通过 `--codex-bin PATH`/`CODEX_BRIDGE_CODEX_BIN` 指定 Codex CLI，通过
`--app-server-socket PATH`/`CODEX_BRIDGE_APP_SERVER_SOCKET` 指定 interrupt 使用的共享 socket。
第三方 Agent 的完整隔离启动、协议检查、故障定位和修复流程见
[DEBUGGING.md](DEBUGGING.md)。

查看完整命令：

```sh
cargo run -p codexctl -- --help
cargo run -p codexctl -- scroll --help
```
