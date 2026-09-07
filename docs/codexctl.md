# `codexctl` command reference

`codexctl` is the terminal client for a running `codex-bridge` daemon. It sends one typed JSON
request over the bridge's Unix socket and prints either a concise human-readable response or JSON.
It does not connect to Electron or parse the Desktop interface.

For installation and service setup, start with [install.md](install.md).

## Connection and task targeting

The client and daemon use `~/.codex-bridge/control.sock` by default. Select another socket with a
global option or an environment variable:

```sh
codexctl --socket /path/to/control.sock status
CODEX_BRIDGE_SOCKET=/path/to/control.sock codexctl status
```

Read commands may use the selected task or the newest unarchived rollout. Write commands are more
strict: `send`, `steer`, `interrupt`, and `host-exec` require either a prior `select` or an explicit
global `--thread`. They never infer a write target from file modification time.

```sh
codexctl ls --limit 20
codexctl select <THREAD_ID>
codexctl current

# Override the daemon's in-memory selection for one command.
codexctl --thread <THREAD_ID> show --last 20
```

The selection made by `codexctl select` lives in daemon memory and is lost when the daemon
restarts. `current` and `show` may fall back to the newest rollout for convenient reading, but the
response marks that fallback as `authoritative=false`; it does not prove which Desktop window has
focus.

## Commands

| Command               | Purpose                                                | Backend status       |
| --------------------- | ------------------------------------------------------ | -------------------- |
| `ls`                  | List tasks from the rollout store                      | Implemented          |
| `select`              | Set the daemon's default write target                  | Implemented          |
| `current`             | Show the selected task, or the newest readable rollout | Implemented          |
| `status`              | Show daemon, protocol, and rollout-store status        | Implemented          |
| `show`                | Read user and assistant messages                       | Implemented          |
| `send`                | Queue a new turn with `codex queue`                    | Implemented          |
| `steer`               | Add guidance to the active turn through app-server     | Implemented          |
| `interrupt`           | Interrupt the active turn through app-server           | Implemented          |
| `host-exec`           | Run an allowlisted host command in the task workspace  | Implemented          |
| `tail`                | Stream new task items                                  | Protocol placeholder |
| `scroll`              | Control the Desktop message view                       | Protocol placeholder |
| `pending`             | List approval requests                                 | Protocol placeholder |
| `approve` / `decline` | Resolve an approval                                    | Protocol placeholder |

Placeholders return `not_implemented`; they do not report a successful no-op.

View the exact options installed on the current machine:

```sh
codexctl --help
codexctl show --help
codexctl host-exec --help
```

## Read tasks and messages

```sh
codexctl status
codexctl status --json

codexctl ls --limit 50
codexctl ls --include-archived --json

codexctl show
codexctl show --last 20
codexctl show <THREAD_ID> --json
codexctl --thread <THREAD_ID> show --last 50
```

`ls` and `show` read `$CODEX_HOME/sessions` and `session_index.jsonl` without resuming a task or
taking writer ownership. Message reads keep only user and assistant messages, tolerate a rollout
that is still being appended, and return the task's recorded working directory. The task list also
tries `git branch --show-current` in that directory; JSON uses `null` if the directory is gone, is
not a Git repository, or is on a detached HEAD.

Override the read store for an isolated or offline inspection by starting the daemon with
`--codex-home PATH`. This changes the daemon, not `codexctl` itself.

## Queue and steer

```sh
codexctl --thread <THREAD_ID> send "Continue checking the failing test"
codexctl --thread <THREAD_ID> send "Return structured output" --json

codexctl --thread <THREAD_ID> steer "Keep the public API backward compatible"
codexctl --thread <THREAD_ID> interrupt --json
```

`send` invokes `codex queue --thread ID --message TEXT`. The daemon uses the Codex executable from
`--codex-bin`, then `CODEX_BRIDGE_CODEX_BIN`, then the CLI bundled in `ChatGPT.app` when present,
and finally `codex` on `PATH`.

`steer` and `interrupt` connect to the configured app-server WebSocket-over-Unix-socket endpoint.
The daemon initializes that connection, obtains the active turn from the rollout, and sends the
typed app-server request. A missing endpoint, stale turn, or task owned by another app-server is an
error. `steer` does not start a second `codex exec resume` writer.

Configure the endpoint when starting `codex-bridge`:

```sh
codex-bridge --app-server-socket /path/to/app-server.sock

# Equivalent for a service manager:
CODEX_BRIDGE_APP_SERVER_SOCKET=/path/to/app-server.sock codex-bridge
```

## Host commands

`host-exec` exists for narrow host-side operations such as hardware flashing and smoke tests. The
request contains an argument array and never passes through a shell.

```sh
codexctl --thread <THREAD_ID> host-exec -- wlink --help
codexctl --thread <THREAD_ID> host-exec --timeout 600 -- \
  cases/run-ch585-smoke.sh
codexctl --thread <THREAD_ID> host-exec --json -- git status --short
```

The built-in policy allows only `wlink`, selected `cases/run-ch585-*.sh` scripts, and restricted Git
subcommands. Standard output and error are capped at 32 KiB each. The default timeout is 300
seconds and the maximum is 3600 seconds. A timeout or non-zero exit still returns captured output
and an exit code, while `codexctl` exits non-zero.

Replace the policy with `codex-bridge --host-exec-policy PATH` or
`CODEX_BRIDGE_HOST_EXEC_POLICY=PATH`. See
[`host-exec-policy.example.json`](../host-exec-policy.example.json). Allowing a repository script
means trusting its current contents; tighten or remove these rules for public or untrusted
workspaces.

## JSON output and failures

Commands with `--json` print the daemon response without converting it to presentation text. This
is the stable form for automation:

```sh
thread_id="$(codexctl ls --limit 1 --json | jq -r '.threads[0].id')"
codexctl --thread "$thread_id" show --last 5 --json
```

Common explicit failures include:

- `thread_not_selected`: a write command had no `--thread` and no daemon selection.
- `thread_not_found`: the selected UUID is absent from the rollout store.
- `no_active_turn`: `steer` or `interrupt` could not find an active turn.
- `app_server_unavailable`: no usable app-server Unix socket was configured.
- `not_implemented`: the command is present in the protocol but has no backend yet.

For wire-level inspection and isolated test procedures, see [../DEBUGGING.md](../DEBUGGING.md).
