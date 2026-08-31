# codexapp-cli 初步调试与修复手册

本文面向接手本仓库的第三方 Agent。目标是在不影响正在运行的 Codex Desktop 的前提下，
启动 `codex-bridge` 与 `codexctl`，使用隔离 fixture 验证读路径和写后端，定位协议、rollout
解析或 Codex CLI 调用问题，并完成可回归的修复。

## 1. 先理解当前边界

项目由两个进程组成：

- `codex-bridge` 是本地 daemon，监听 Unix socket，读取 Codex rollout 状态。
- `codexctl` 是一次性 CLI，通过 Unix socket 向 daemon 发送一行 JSON 请求并读取一行 JSON
  响应。

当前协议版本是 `4`。已经实现的命令：

| 命令 | 当前行为 |
| --- | --- |
| `status` | 返回 bridge、协议版本和 rollout store 状态 |
| `ls` | 列出 rollout、cwd 与 git branch，可限制数量或包含归档 |
| `select` | 在 daemon 内存中保存默认目标 thread |
| `current` | 返回最近修改的未归档 rollout |
| `show [THREAD_ID]` | 读取指定 thread 的 cwd、git branch 与 user/assistant 消息 |
| `send` | 通过 `codex queue` 给目标 thread 排队一条消息 |
| `steer` | 通过共享 app-server 的 WebSocket-over-UDS 调用 `turn/steer` |
| `interrupt` | 通过同一控制端点调用 `turn/interrupt` |
| `host-exec` | 在所选 thread cwd 中执行 allowlist 允许的宿主机 argv |

尚未实现的 `tail`、`scroll`、`pending`、`approve` 和 `decline` 必须返回
`not_implemented`。不要为了让演示“成功”而伪造执行结果。

`current` 也有明确限制：它只按 rollout 文件修改时间推断，不能证明哪个 Codex App 窗口
当前获得焦点。JSON 响应中的 `selection.authoritative` 必须保持为 `false`；需要确定性读取时，
使用 `show <THREAD_ID>`。

写命令的目标规则更严格：必须先运行 `select <THREAD_ID>`，或为单次命令传
`--thread <THREAD_ID>`。写路径绝不使用最近 mtime 猜测，未选择时必须返回
`thread_not_selected`。

## 2. 安全规则

初步调试必须遵守以下规则：

1. 使用仓库内的 `fixtures/codex-home`，不要直接使用 `~/.codex`。
2. 使用 `mktemp -d` 创建调试目录，并通过 `--socket` 指定其中的 socket。
3. 不启动 Codex App、不重启 Codex App、不连接它的 app-server、CDP 或 remote-debugging
   endpoint。
4. 不运行 launcher，不修改 `app.asar`，不发送按键、鼠标或 approval。
5. 不删除默认的 `~/.codex-bridge/control.sock`。如果默认 socket 已有服务，保持不动。
6. 不修改任何真实 rollout、`session_index.jsonl` 或 `state_5.sqlite`。
7. 未获得用户明确许可时，不运行以真实 `~/.codex` 为 `--codex-home` 的 smoke test。

只读 fixture 流程与正在运行的 Codex App 完全隔离。

## 3. 代码地图

- [`crates/codex-bridge/src/lib.rs`](crates/codex-bridge/src/lib.rs)：请求/响应结构与协议版本。
- [`crates/codex-bridge/src/sessions.rs`](crates/codex-bridge/src/sessions.rs)：rollout 扫描、标题索引、
  message 与活动 turn ID 解析。
- [`crates/codex-bridge/src/write_backend.rs`](crates/codex-bridge/src/write_backend.rs)：安全的
  Codex CLI 参数调用和 app-server WebSocket-over-UDS JSON-RPC。
- [`crates/codex-bridge/src/host_executor.rs`](crates/codex-bridge/src/host_executor.rs)：host-exec
  策略校验、限长输出、超时和进程组终止。
- [`crates/codex-bridge/src/main.rs`](crates/codex-bridge/src/main.rs)：socket 生命周期、请求大小限制和
  daemon dispatch。
- [`crates/codexctl/src/main.rs`](crates/codexctl/src/main.rs)：CLI 参数、协议客户端、human/JSON 输出。
- [`fixtures/codex-home`](fixtures/codex-home)：用于隔离调试的最小 Codex 状态目录。
- [`fixtures/fake-codex`](fixtures/fake-codex)：不会连接真实服务的可执行 write-backend fixture。

关键限制：

- 请求最大 `1 MiB`。
- 响应最大 `16 MiB`。
- `ls --limit` 范围是 `1..=1000`。
- `show --last` 范围是 `1..=10000`。
- backend stdout/stderr 最多各返回 `64 KiB`。
- host-exec stdout/stderr 各最多保留 `32 KiB`，默认超时 `300 秒`，策略上限 `3600 秒`。
- app-server WebSocket handshake、initialize 与 steer/interrupt 响应各等待最多 `10 秒`。
- daemon 创建的默认 socket 权限为 `0600`，默认 socket 目录权限为 `0700`。
- 自定义 socket 路径的既有父目录不会被擅自改权限。

## 4. 构建与单元测试

下列命令中的 `/path/to/codexapp-cli` 代表本机仓库根目录，请替换为实际绝对路径。从仓库
根目录执行：

```sh
cd /path/to/codexapp-cli
cargo fmt --all -- --check
CARGO_INCREMENTAL=0 cargo test --workspace
```

本项目的 Rust 产物统一复用仓库根目录的 `target`，不要为普通调试创建新的长期 target。
如果在链接 worktree 中工作，应显式复用主项目 target：

```sh
CARGO_TARGET_DIR=/path/to/main/codexapp-cli/target \
  CARGO_INCREMENTAL=0 cargo test --workspace
```

测试必须只使用临时目录或仓库 fixture，不能依赖当前用户的真实 `~/.codex`。当前测试会验证：

- tagged JSON 协议结构；
- CLI 参数映射；
- 新旧标题中选择较新的 `session_index.jsonl` 记录；
- 从 rollout `session_meta.payload.cwd` 返回工作路径，并在该目录解析 git branch；
- 非 Git 仓库或已删除 cwd 时稳定返回 `git_branch: null`；
- 只保留 user/assistant message；
- 忽略 developer/internal record；
- 缺失状态目录时返回空只读结果。
- write 命令不会回退到非权威 mtime thread；
- queue 参数不经过 shell；
- WebSocket-over-UDS handshake、initialize、steer 和 interrupt JSON-RPC 完整往返。
- host-exec shell/路径越界拒绝、策略替换、输出截断、非零退出和进程组超时。

## 5. 使用隔离 fixture 启动

先构建二进制：

```sh
cd /path/to/codexapp-cli
CARGO_INCREMENTAL=0 cargo build --workspace
```

在终端 A 创建临时 socket 目录并启动 daemon：

```sh
cd /path/to/codexapp-cli
debug_root="$(mktemp -d "${TMPDIR:-/tmp}/codexapp-cli.XXXXXX")"
bridge_socket="$debug_root/control.sock"
printf 'bridge socket: %s\n' "$bridge_socket"
./target/debug/codex-bridge \
  --socket "$bridge_socket" \
  --codex-home "$PWD/fixtures/codex-home" \
  --codex-bin "$PWD/fixtures/fake-codex"
```

预期 daemon 保持前台运行，并只输出类似：

```text
codex-bridge listening on /private/var/.../control.sock
```

将终端 A 打印的绝对 socket 路径复制到终端 B：

```sh
cd /path/to/codexapp-cli
bridge_socket='/private/var/.../control.sock'
```

然后按顺序验证：

```sh
./target/debug/codexctl --socket "$bridge_socket" status --json
./target/debug/codexctl --socket "$bridge_socket" ls --limit 10
./target/debug/codexctl --socket "$bridge_socket" current
./target/debug/codexctl --socket "$bridge_socket" show
./target/debug/codexctl --socket "$bridge_socket" show \
  00000000-0000-7000-8000-000000000001 --last 1 --json
./target/debug/codexctl --socket "$bridge_socket" select \
  00000000-0000-7000-8000-000000000001
./target/debug/codexctl --socket "$bridge_socket" send 'fixture message'
```

关键验收点：

- `status.protocol_version` 是 `4`。
- `status.rollout_store.available` 是 `true`，且 `read_only` 是 `true`。
- `ls` 只列出 fixture thread，状态显示为 `unarchived`，cwd 是 `/tmp`；fixture cwd 不是有效
  Git workspace 时 branch 显示为 `<unknown>`，JSON 中为 `null`。
- `current` 明确打印 `not authoritative for focused window`。
- `show` 显示 cwd 与 git branch，并只显示一条 user message 和两条 assistant message，不显示
  fixture 中的 developer message。
- `show --last 1 --json` 的 `messages_returned` 是 `1`，`messages_total` 是 `3`，最后一条
  内容是 `fixture final answer`。
- 显式 thread ID 的 `selection.authoritative` 是 `true`。
- `select` 后 `current` 使用 `selected_thread`，不再使用 mtime 推断。
- `send` 返回 `status=queued`、`backend.backend=codex_queue`，输出来自 fake Codex。

fixture 没有活动 turn，`steer` 和 `interrupt` 都应在连接 app-server 前安全失败：

```sh
./target/debug/codexctl --socket "$bridge_socket" steer 'fixture follow-up'
./target/debug/codexctl --socket "$bridge_socket" interrupt
```

预期 CLI 非零退出，并报告：

```text
no_active_turn: thread 00000000-0000-7000-8000-000000000001 has no active turn in its rollout
```

fixture daemon 的 `--codex-bin` 指向仓库 fake 程序，因此 `send` 不会接触真实 Codex CLI；无
活动 turn 的 `steer`/`interrupt` 也不会连接真实 app-server 或 Codex App。

## 6. 直接检查 JSON 行协议

只有在怀疑 CLI 参数映射或输出渲染有问题时，才绕过 `codexctl` 检查 wire protocol。
macOS 自带的 `nc` 可连接 Unix socket：

```sh
printf '%s\n' '{"command":"status"}' | nc -U "$bridge_socket"
printf '%s\n' '{"command":"ls","limit":5,"include_archived":false}' | nc -U "$bridge_socket"
printf '%s\n' \
  '{"command":"select","thread_id":"00000000-0000-7000-8000-000000000001"}' \
  | nc -U "$bridge_socket"
printf '%s\n' \
  '{"command":"show","thread_id":"00000000-0000-7000-8000-000000000001","last":1}' \
  | nc -U "$bridge_socket"
printf '%s\n' \
  '{"command":"send","thread_id":"00000000-0000-7000-8000-000000000001","text":"fixture"}' \
  | nc -U "$bridge_socket"
```

每个连接只发送一个以换行结束的 JSON request，daemon 也只返回一个以换行结束的 JSON
response。成功响应结构为：

```json
{"ok":true,"result":{}}
```

失败响应结构为：

```json
{"ok":false,"error":{"code":"...","message":"..."}}
```

如果直接协议成功而 `codexctl` 失败，问题通常在 CLI 参数映射、16 MiB 响应限制或 human
renderer；如果两者都失败，优先检查 daemon dispatch 和 session parser。

## 7. 停止与清理

1. 在终端 A 按 `Ctrl-C`，等待 daemon 正常退出。
2. daemon 的 `SocketGuard` 应删除它自己创建且 inode 未变化的 socket。
3. 确认 socket 已消失，再删除已解析的精确临时目录：

```sh
test ! -S "$bridge_socket"
rmdir "$debug_root"
```

如果第一条检查失败，停止清理并先确认 daemon 状态，不要递归删除目录。

4. 不要清理整个仓库 target；只清理本项目包产物：

```sh
cd /path/to/codexapp-cli
cargo clean -p codexctl -p codex-bridge
```

如果另一个 Cargo 进程正在使用共享 target，先停止清理并报告，不要与并发构建争用或删除其
产物。禁止使用 `rm -rf target`。

## 8. 常见故障

### `cannot connect to codex-bridge`

检查：

- daemon 是否仍在终端 A 前台运行；
- 两个终端使用的 socket 绝对路径是否完全一致；
- 是否误用了默认 socket；
- 临时目录是否被提前删除。

不要通过删除 `~/.codex-bridge/control.sock` 来“修复”自定义 socket 问题。

### `codex-bridge is already listening`

目标 socket 已有可连接的 daemon。为本次调试新建另一个 `mktemp -d` 目录，不要抢占或终止
未知服务。

### `refusing to replace non-socket path`

`--socket` 指向了普通文件或符号链接。换用新的临时路径；不要删除未知文件。

### `thread_not_found`

依次确认：

1. 文件位于 `sessions/**/*.jsonl` 或 `archived_sessions/*.jsonl`；
2. 存在 `type=session_meta` 记录；
3. `session_meta.payload.id` 或 `session_id` 与查询 ID 一致；
4. `session_meta.payload.cwd` 是字符串；
5. 查询归档 thread 时使用显式 `show <THREAD_ID>`。

`current` 只考虑未归档 rollout；显式 `show` 会同时搜索归档与未归档 rollout。

### thread 存在但标题为 `<untitled>`

标题优先来自 `session_index.jsonl` 中该 ID 最新的 `updated_at` 记录；没有索引时，parser
使用第一条 user `response_item` 的文本并截断为 120 个字符。检查字段名是否为
`thread_name`，时间是否是可按字典序比较的 ISO-8601 字符串。

### `show` 没有消息

当前 parser 只接收以下结构：

```json
{
  "type": "response_item",
  "payload": {
    "type": "message",
    "role": "user",
    "content": [{"type": "input_text", "text": "hello"}]
  }
}
```

`event_msg`、developer message、reasoning、command execution 和 tool output 会被跳过。如果新版
rollout 只保存另一种结构，应先制作最小脱敏 fixture 和回归测试，再扩展 parser；不要直接把
所有 payload 原样暴露给 CLI。

### `current` 选中了错误 thread

这不一定是 parser bug。当前算法就是“最近修改的未归档 rollout”。先使用
`show <THREAD_ID>` 完成确定性读取。真正的焦点窗口识别属于后续 CDP/UI backend，不能通过
继续增加文件时间猜测来伪装解决。

### `codex-bridge response exceeds 16 MiB`

先使用 `show --last N` 缩小响应。若仍需读取完整大 thread，正确修复方向是设计分页或流式
协议，并同步升级协议版本，而不是简单取消上限。

### `thread_not_selected`

写命令不会使用 `current` 的 mtime 推断。先运行 `codexctl select <THREAD_ID>`，或对单次命令
使用全局 `--thread <THREAD_ID>`：

```sh
codexctl --thread <THREAD_ID> send 'message'
codexctl --thread <THREAD_ID> steer 'follow-up'
codexctl --thread <THREAD_ID> interrupt
codexctl --thread <THREAD_ID> host-exec -- git status
```

`select` 只保存在当前 bridge daemon 内存里，重启 daemon 后需要重新选择。

### `host_exec_not_allowed`、`host_exec_unavailable` 或超时

- `host_exec_not_allowed` 表示 argv 没有匹配 daemon 策略；不要通过包一层 `sh -c` 绕过。
- `host_exec_unavailable` 表示规则允许，但 PATH 中没有可执行文件，或 workspace 脚本不存在、
  无执行位、解析到 workspace 外。
- CLI 默认超时 300 秒；超时后 bridge 杀死该调用的整个进程组，响应保留 `timed_out=true`、
  `exit_code=-1` 和已截断的输出。
- stdout/stderr 每路最多 32 KiB；`*_truncated=true` 代表后续内容已丢弃。

策略可由 daemon 的 `--host-exec-policy PATH` 或 `CODEX_BRIDGE_HOST_EXEC_POLICY` 指定；自定义
JSON 完全替换内置规则，先对照 `host-exec-policy.example.json` 检查。host-exec 总是在已选择
thread 的 rollout cwd 中执行，不接受任意 `--cwd`。

### `codex_cli_unavailable` 或 `codex_cli_failed`

- `codex_cli_unavailable` 表示无法启动 `--codex-bin` 指定的程序；检查绝对路径和执行权限。
- `codex_cli_failed` 表示 Codex CLI 已启动但非零退出；保留 stderr，检查 CLI 版本、登录状态、
  thread 是否存在，以及 active-writer 冲突。
- `send` 要求本机 Codex CLI 提供 `codex queue --thread --message`。
- `steer` 不使用 Codex 子进程，因此 steer 失败应查看下面的 `app_server_*` 错误，而不是
  `codex_cli_*`。

### `no_active_turn`

`steer` 和 `interrupt` 从 rollout 的 `task_started`、`task_complete` 和 `turn_aborted` 事件推导
活动 turn。没有未完成的 `task_started` 时不会调用 app-server。若新版 rollout 更改事件格式，
先增加脱敏 fixture 和 parser 测试。

### `app_server_unavailable`、`app_server_timeout` 或 `app_server_rejected`

steer/interrupt 直接连接默认的
`$CODEX_HOME/app-server-control/app-server-control.sock`。该 Unix socket 上承载 WebSocket：
先 HTTP Upgrade，再用 text frame 传输 JSON-RPC。可通过 daemon 的 `--app-server-socket PATH`
或 `CODEX_BRIDGE_APP_SERVER_SOCKET` 覆盖。

- socket 不存在或不是 Unix socket：`app_server_unavailable`；
- WebSocket handshake、initialize 或方法响应超时：`app_server_timeout`；
- Upgrade 失败、连接提前关闭或响应不是 JSON：`app_server_protocol_error`；
- `turnId` 过期、thread 不属于该 server 或 server 拒绝请求：`app_server_rejected`。

不要在这里重新引入 `codex app-server proxy`：Codex 0.151.0 的 proxy 只是把 stdio 原始字节
复制到 socket，没有执行 WebSocket Upgrade，因此服务端会在读取 JSON-RPC 前关闭连接。
`remoteControlEnabled=true` 描述 daemon 的远程控制能力，不会把本地控制 socket 改成 JSONL。

Codex Desktop 使用 private stdio app-server 时，共享 socket 无法 steer/interrupt 它的 turn，
这是 app-server 实例边界。Desktop 内置 `codex_app` MCP 暴露 `send_message_to_thread` 等工具，
但入口由 GUI 私有 pipe、代码签名 peer 校验和 approval 路由保护，不是可供外部 CLI 复用的
稳定 API。不要绕过签名校验、修改 `app.asar`，也不要 queue `/stop` 伪装成功。

### `invalid_request`

先运行 `status --json` 检查 daemon 协议版本。协议 v4 的典型请求是：

```json
{"command":"ls","limit":20,"include_archived":false}
{"command":"show","thread_id":null,"last":null}
{"command":"send","thread_id":"THREAD_ID","text":"message"}
{"command":"interrupt","thread_id":"THREAD_ID"}
{"command":"host_exec","thread_id":"THREAD_ID","argv":["git","status"],"timeout_seconds":30}
```

旧 daemon 与新 CLI 混用时应重新构建并重启 fixture daemon，不要连接正在运行的未知 bridge。

### `rollout_store_error`

保留完整错误链，检查路径、权限和发生错误的具体文件。活动 rollout 最后一行暂时无效或包含
不完整 UTF-8 时，已完成记录仍应可读；其他 I/O 错误不应被静默吞掉。

## 9. 标准修复流程

修复任何问题时遵守以下顺序：

1. 记录失败命令、精确 socket、协议版本、错误 code 和 message。
2. 在 `fixtures/codex-home` 的副本或 Rust 临时 fixture 中最小化问题。不要提交真实用户对话、
   token、路径或完整 rollout。
3. 先添加能复现问题的测试，并确认失败发生在预期层。
4. 只修改负责该行为的层：
   - JSON schema 或协议兼容性：`lib.rs`；
   - rollout 发现与解析：`sessions.rs`；
   - Codex 子进程与 app-server WebSocket RPC：`write_backend.rs`；
   - host 命令授权、限流和超时：`host_executor.rs`；
   - daemon 错误码、限制和 dispatch：bridge `main.rs`；
   - CLI 参数与显示：codexctl `main.rs`。
5. 协议字段有不兼容变化时递增 `PROTOCOL_VERSION`，同时更新 README、本文件和协议测试。
6. 保持错误语义稳定：用户输入错误用 `invalid_request`，找不到 thread 用
   `thread_not_found`，存储读取失败用 `rollout_store_error`，未实现能力用
   `not_implemented`；Codex CLI 与 app-server 错误使用各自的 `codex_cli_*`/
   `app_server_*` code；host-exec 使用 `host_exec_*` code。
7. 运行格式检查、针对性测试和全 workspace 测试。
8. 审查 `git diff --check` 与 `git status --short`，确认没有真实 Codex 状态、socket、日志或
   target 产物进入变更。
9. 停止 fixture daemon，再执行包级缓存清理。

推荐验证命令：

```sh
cargo fmt --all -- --check
CARGO_INCREMENTAL=0 cargo test -p codex-bridge sessions::tests
CARGO_INCREMENTAL=0 cargo test -p codex-bridge write_backend::tests
CARGO_INCREMENTAL=0 cargo test -p codex-bridge host_executor::tests
CARGO_INCREMENTAL=0 cargo test --workspace
git diff --check
git status --short
```

这里的“修复完成”至少意味着：最小回归测试通过、全 workspace 测试通过、fixture CLI 路径
通过、错误码符合约定，并且没有把静态/fixture 验证描述成真实 Codex App 行为验证。

## 10. 真实状态的可选 smoke test

只有用户明确允许后，才可让 daemon 指向真实 Codex 状态目录。即便获得许可，也必须继续使用
独立临时 socket，且只调用 `status`、`ls`、`current`、`show --last N`：

```sh
codex_state_dir="${CODEX_HOME:-${HOME}/.codex}"
debug_root="$(mktemp -d "${TMPDIR:-/tmp}/codexapp-cli-live-read.XXXXXX")"
bridge_socket="$debug_root/control.sock"
./target/debug/codex-bridge \
  --socket "$bridge_socket" \
  --codex-home "$codex_state_dir"
```

真实只读 smoke test 仍不能证明：

- 当前 UI 焦点窗口；
- active-writer 所有权；
- app-server 请求可用性；
- CDP 输入、滚动或按钮操作；
- approval、interrupt 或消息发送成功。

这些属于后续 backend 的独立验收门，必须在用户允许影响当前 App 的专门调试窗口中完成。

若只需确认 standalone control socket 的实际传输层，可运行 opt-in 的只读测试。它只完成
WebSocket Upgrade、initialize 和 `thread/loaded/list`，不会启动或修改 turn：

```sh
CODEX_BRIDGE_TEST_APP_SERVER_SOCKET="$codex_state_dir/app-server-control/app-server-control.sock" \
  CARGO_INCREMENTAL=0 cargo test -p codex-bridge \
  live_app_server_websocket_probe -- --ignored --nocapture
```

如果用户进一步明确允许真实写入，先从单条可识别、无副作用的 queue 消息开始：

```sh
./target/debug/codexctl --socket "$bridge_socket" select <THREAD_ID>
./target/debug/codexctl --socket "$bridge_socket" send 'codexapp-cli write smoke test'
```

只有确认 queue 到达正确 thread，并确认目标 thread 由指定共享 app-server 实例持有后，才分别
申请 steer 和 interrupt 验收。三者必须分开记录：

- send 通过只证明 `codex queue` 接受并排队；
- steer 通过证明共享 app-server 接受了对匹配活动 turn 的 `turn/steer`；
- interrupt 通过只证明目标 thread 位于指定共享 app-server 且活动 `turnId` 匹配；
- 任何一项都不证明 CDP/UI 控制可用。

不要用当前正在工作的会话做 steer smoke test。应启动独立 app-server socket、在临时 cwd 创建
新 thread，并只对这个新 thread 调用 `turn/start` 和 `turn/steer`；开始前把目标 thread ID 与
cwd 记入日志，确认不属于任何真实项目。测试结束后停止临时 daemon，但保留 rollout 作为审计
证据，除非用户明确要求删除。

仓库提供了显式 opt-in 的 live 测试，它会创建新临时 cwd 和新 thread，真实调用
`turn/start`、`turn/steer`，然后尝试 interrupt 清理；绝不能把 socket 指向 Desktop private
stdio server，也不要复用已有 thread：

```sh
CODEX_BRIDGE_TEST_ALLOW_WRITE=1 \
CODEX_BRIDGE_TEST_APP_SERVER_SOCKET="$codex_state_dir/app-server-control/app-server-control.sock" \
  CARGO_INCREMENTAL=0 cargo test -p codex-bridge \
  live_app_server_steers_new_isolated_thread -- --ignored --nocapture
```
