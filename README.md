# codexapp-cli

`codexapp-cli` 把正在运行的 Codex Desktop 包装成本地 CLI/RPC 服务。它按
[DESIGN.md](DESIGN.md) 采用读写分离的混合架构：rollout/app-server 状态用于读取会话，
写消息由 Codex CLI 执行；尚未实现的 UI 操作才预留 Chromium CDP 通道。

## 目录

- `crates/codexctl`：面向用户的 CLI，把命令编码为 JSON 请求。
- `crates/codex-bridge`：本地 daemon 与共享协议，监听 Unix socket。
- `crates/codex-gui-bridge`：把 Desktop 的 TCP WebSocket 透明桥接到官方
  app-server remote-control daemon 的 WebSocket-over-Unix-socket 端点。
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
codexctl host-exec -- wlink --help
codexctl --thread <THREAD_ID> host-exec --timeout 600 -- \
  cases/run-ch585-smoke.sh
```

- `ls` 按 rollout 文件修改时间列出线程，并支持包含归档线程。每条 session 都返回 rollout
  创建时记录的 `cwd`，并在该目录运行 `git branch --show-current` 填充可空的 `git_branch`；
  目录已删除、非 Git 仓库或 detached HEAD 时返回 `null`/显示 `<unknown>`。
- `show <THREAD_ID>` 是确定性的；解析结果只保留 user/assistant message，跳过内部记录，
  也允许读取仍在追加的 JSONL 文件，并显示相同的 `cwd` 与 `git_branch`。
- 无参数的 `current` 与 `show` 暂时选择最近修改的未归档 rollout。返回的 `selection`
  会明确标记 `authoritative=false`，因为这不能证明哪个 Codex Desktop 窗口当前获得焦点。
- `select` 在 daemon 内存中保存默认 thread；`--thread` 可为 `show`、`send`、`steer`、
  `interrupt` 单次覆盖选择。写命令没有 mtime fallback，未选择目标时返回
  `thread_not_selected`。
- `send` 调用 `codex queue --thread ID --message TEXT`，不通过 resume 抢占 active writer。
- `steer` 从 rollout 解析活动 turn ID，再通过共享 app-server 的
  WebSocket-over-Unix-socket 控制端点发送 `turn/steer`。这是真正的 same-turn 注入，不会另起
  `codex exec resume` writer。
- `interrupt` 使用相同端点发送 `turn/interrupt`。目标 thread 必须由该 app-server 实例持有，
  且 rollout 中的活动 turn ID 必须仍然匹配；否则返回明确错误，不能伪装成功。
- Codex Desktop 26.820.60940 仍自行启动 private stdio app-server，它没有公开 control socket。
  因此上述 steer/interrupt 当前可操作 standalone/shared daemon 会话，不能跨实例操作 GUI
  private 会话。
- `host-exec` 在所选 thread 的 `cwd` 中由宿主机 bridge 执行命令，用于 USB 刷机和硬件测试。
  请求传递 argv 数组且不经过 shell；默认只允许 `wlink`、`cases/run-ch585-*.sh` 和受限的
  Git 子命令。stdout/stderr 各最多保留 32 KiB，默认超时 300 秒，整个子进程组最长允许
  3600 秒。命令非零退出或超时后，结果仍包含输出和 exit code，同时 `codexctl` 返回非零。

`tail`、`scroll`、`pending` 和审批仍只有 CLI/协议骨架；在
app-server/CDP backend 接入前会明确返回 `not_implemented`，不会伪装成已执行。

daemon 可通过 `--codex-home PATH` 指向另一份只读状态目录，便于离线使用和隔离测试。
写路径可通过 `--codex-bin PATH`/`CODEX_BRIDGE_CODEX_BIN` 指定 Codex CLI，通过
`--app-server-socket PATH`/`CODEX_BRIDGE_APP_SERVER_SOCKET` 指定 steer/interrupt 使用的共享
WebSocket-over-UDS socket。不要把该端点当作 JSONL socket；`codex app-server proxy` 只做原始
字节转发，无法完成 WebSocket Upgrade。
host-exec 默认策略可由 `--host-exec-policy PATH` 或 `CODEX_BRIDGE_HOST_EXEC_POLICY` 指向的
JSON 完全替换；格式见 [host-exec-policy.example.json](host-exec-policy.example.json)。允许
workspace 脚本意味着信任该脚本的当前内容，公开或不可信仓库应收紧/移除这类规则。
第三方 Agent 的完整隔离启动、协议检查、故障定位和修复流程见
[DEBUGGING.md](DEBUGGING.md)。

查看完整命令：

```sh
cargo run -p codexctl -- --help
cargo run -p codexctl -- scroll --help
```
