#!/bin/sh
set -eu

enable_web_ui=false
start_services=true
start_suppressed=false
runtime_mode=standalone
runtime_mode_explicit=false
enable_whisper=false
while [ "$#" -gt 0 ]; do
  case "$1" in
    --web-ui) enable_web_ui=true ;;
    --no-start) start_services=false ;;
    --standalone) runtime_mode=standalone; runtime_mode_explicit=true ;;
    --desktop) runtime_mode=desktop; runtime_mode_explicit=true ;;
    --whisper) enable_whisper=true ;;
    -h|--help)
      echo "Usage: scripts/install-macos.sh [--standalone|--desktop] [--web-ui] [--whisper] [--no-start]"
      exit 0
      ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

if [ "$(uname -s)" != "Darwin" ]; then
  echo "install-macos.sh must run on macOS" >&2
  exit 1
fi

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cargo_bin_dir=${CARGO_HOME:-"$HOME/.cargo"}/bin
config_dir=${XDG_CONFIG_HOME:-"$HOME/.config"}/codex-bridge
config_path=$config_dir/config.toml
runtime_dir=$HOME/.codex-bridge
state_dir=${XDG_STATE_HOME:-"$HOME/.local/state"}/codex-bridge
launcher_dir=$HOME/Library/LaunchAgents
desktop_codex=/Applications/ChatGPT.app/Contents/Resources/codex
desktop_main=/Applications/ChatGPT.app/Contents/MacOS/ChatGPT
adapter_port=18790
domain=gui/$(id -u)
daemon_label=local.codex-bridge.daemon
legacy_label=com.lunghaa.codex-bridge
if [ -f "$launcher_dir/$legacy_label.plist" ] || launchctl print "$domain/$legacy_label" >/dev/null 2>&1; then
  if [ -f "$launcher_dir/$daemon_label.plist" ] || launchctl print "$domain/$daemon_label" >/dev/null 2>&1; then
    echo "Both Bridge LaunchAgents exist; resolve the duplicate before installing." >&2
    exit 1
  fi
  daemon_label=$legacy_label
fi

if [ -f "$config_path" ]; then
  configured_mode=$(sed -n 's/^[[:space:]]*mode[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$config_path" | head -n 1)
  if [ "$runtime_mode_explicit" = true ] && [ -n "$configured_mode" ] &&
    [ "$runtime_mode" != "$configured_mode" ]; then
    echo "Existing config selects $configured_mode; edit $config_path before changing runtime mode." >&2
    exit 1
  fi
  if [ "$runtime_mode_explicit" = false ]; then
    case "$configured_mode" in
      desktop|standalone) runtime_mode=$configured_mode ;;
    esac
  fi
fi
if [ -f "$config_path" ] && awk '
  /^\[services\.whisper\][[:space:]]*([#].*)?$/ { section = 1; next }
  /^\[/ { section = 0 }
  section && /^[[:space:]]*enabled[[:space:]]*=[[:space:]]*true([[:space:]]*(#.*)?)?$/ { found = 1 }
  END { exit !found }
' "$config_path"; then
  enable_whisper=true
fi

if [ "$runtime_mode" = desktop ]; then
  codex_bin=$desktop_codex
  app_server_socket=$runtime_dir/bundled-app-server.sock
  desktop_interposition=true
  if [ ! -x "$codex_bin" ]; then
    echo "Codex Desktop runtime not found at $codex_bin" >&2
    exit 1
  fi
else
  codex_bin=$(command -v codex || true)
  app_server_socket=$runtime_dir/codex-app-server.sock
  desktop_interposition=false
  if [ -z "$codex_bin" ]; then
    echo "standalone codex executable was not found on PATH" >&2
    echo "Install the standalone Codex CLI, or rerun with --desktop for App bundle integration." >&2
    exit 1
  fi
  case "$codex_bin" in
    /Applications/ChatGPT.app/*)
      echo "the codex executable on PATH resolves inside ChatGPT.app, not to a standalone install" >&2
      echo "Install the standalone Codex CLI, or rerun with --desktop for App bundle integration." >&2
      exit 1
      ;;
  esac
fi
if [ "$enable_web_ui" = true ] && ! command -v openssl >/dev/null 2>&1; then
  echo "openssl is required with --web-ui" >&2
  exit 1
fi
for command_name in cargo npm launchctl plutil; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "required command not found: $command_name" >&2
    exit 1
  }
done

mkdir -p "$config_dir" "$runtime_dir" "$state_dir" "$launcher_dir"
chmod 700 "$config_dir" "$runtime_dir" "$state_dir"

npm --prefix "$repo_root/web-ui" ci
npm --prefix "$repo_root/web-ui" run build
if [ "$enable_whisper" = true ]; then
  CARGO_TARGET_DIR="$repo_root/target" CARGO_INCREMENTAL=0 \
    cargo install --locked --force --features whisper --path "$repo_root/crates/codex-bridge"
else
  CARGO_TARGET_DIR="$repo_root/target" CARGO_INCREMENTAL=0 \
    cargo install --locked --force --path "$repo_root/crates/codex-bridge"
fi
CARGO_TARGET_DIR="$repo_root/target" CARGO_INCREMENTAL=0 \
  cargo install --locked --force --path "$repo_root/crates/codexctl"
if [ "$runtime_mode" = desktop ]; then
  CARGO_TARGET_DIR="$repo_root/target" CARGO_INCREMENTAL=0 \
    cargo install --locked --force --path "$repo_root/crates/codex-gui-bridge" \
    --bin ws-unix-bridge
fi

# Migrate only the old generated build-artifact path. Other local adapter
# choices remain untouched.
if [ "$runtime_mode" = desktop ] && [ -f "$config_path" ]; then
  old_adapter=$repo_root/target/release/ws-unix-bridge
  if awk -v old="$old_adapter" '
    /^[[:space:]]*ws_bridge_bin[[:space:]]*=/ {
      value = $0; sub(/^[^=]*=[[:space:]]*/, "", value)
      sub(/[[:space:]]*(#.*)?$/, "", value)
      if (value == "\"" old "\"") found = 1
    }
    END { exit !found }
  ' "$config_path"; then
    config_backup=$(mktemp "$config_path.before-cargo-install.XXXXXX")
    cp -p "$config_path" "$config_backup"
    config_updated=$(mktemp "$config_path.updated.XXXXXX")
    awk -v old="$old_adapter" -v new="$cargo_bin_dir/ws-unix-bridge" '
      /^[[:space:]]*ws_bridge_bin[[:space:]]*=/ {
        value = $0; sub(/^[^=]*=[[:space:]]*/, "", value)
        sub(/[[:space:]]*(#.*)?$/, "", value)
        if (value == "\"" old "\"") sub(/"[^"]*"/, "\"" new "\"")
      }
      { print }
    ' "$config_path" >"$config_updated"
    chmod 600 "$config_updated"
    mv "$config_updated" "$config_path"
    echo "Updated ws_bridge_bin to the Cargo-installed adapter (backup: $config_backup)"
  fi
fi

web_password=$runtime_dir/web-ui-password
if [ "$enable_web_ui" = true ] && [ ! -s "$web_password" ]; then
  umask 077
  openssl rand -base64 32 >"$web_password"
  chmod 600 "$web_password"
fi

managed_by_bridge=false
if [ ! -f "$config_path" ]; then
  cat >"$config_path" <<EOF
mode = "$runtime_mode"
codex_bin = "$codex_bin"
app_server_socket = "$app_server_socket"
app_server_thread_cache = 3

[web_ui]
enabled = $enable_web_ui
listen = "127.0.0.1:18791"
user = "codex"
password_file = "$web_password"
no_auth = false
public_origins = []

[services]
manage_app_server = true
desktop_interposition = $desktop_interposition
ws_bridge_listen = "127.0.0.1:$adapter_port"
ws_bridge_bin = "$cargo_bin_dir/ws-unix-bridge"
EOF
  chmod 600 "$config_path"
  managed_by_bridge=true
else
  echo "Preserving existing configuration: $config_path"
  if grep -Eq '^[[:space:]]*manage_app_server[[:space:]]*=[[:space:]]*true([[:space:]]*(#.*)?)?$' "$config_path"; then
    managed_by_bridge=true
  fi
fi

stdio_app_server_pids() {
  require_desktop_parent=0
  if [ "$runtime_mode" = desktop ]; then require_desktop_parent=1; fi
  ps -axo pid=,ppid=,command= | awk -v bin="$codex_bin" -v desktop="$desktop_main" -v require_parent="$require_desktop_parent" '
    {
      pid = $1; ppid = $2; $1 = ""; $2 = ""; sub(/^[[:space:]]+/, "");
      command[pid] = $0; parent[pid] = ppid;
      if (index($0, bin " ") == 1 && index($0, " app-server") && (!index($0, " --listen") || index($0, " --listen stdio://") || index($0, " --listen=stdio://")) && !index($0, " generate-json-schema")) candidate[pid] = 1;
    }
    END {
      for (pid in candidate) {
        parent_command = command[parent[pid]];
        if (!require_parent || parent_command == desktop || index(parent_command, desktop " ") == 1) print pid;
      }
    }
  '
}
if [ "$start_services" = true ] && [ "$managed_by_bridge" = true ]; then
  conflicting_pids=$(stdio_app_server_pids)
  if [ -n "$conflicting_pids" ]; then
    echo "Refusing to start the managed app-server while a conflicting stdio app-server is running." >&2
    echo "The service definition will be installed without loading it. Finish the active turn, exit that Codex process, then start local.codex-bridge.daemon." >&2
    start_services=false
    start_suppressed=true
  fi
fi

daemon_plist=$launcher_dir/$daemon_label.plist
if [ -f "$daemon_plist" ]; then
  if [ "$(plutil -extract Label raw -o - "$daemon_plist")" != "$daemon_label" ] ||
    [ "$(plutil -extract ProgramArguments.1 raw -o - "$daemon_plist")" != "--config" ] ||
    [ "$(plutil -extract ProgramArguments.2 raw -o - "$daemon_plist")" != "$config_path" ]; then
    echo "Existing LaunchAgent has unexpected arguments; inspect before updating: $daemon_plist" >&2
    exit 1
  fi
  if [ "$(plutil -extract ProgramArguments.0 raw -o - "$daemon_plist")" != "$cargo_bin_dir/codex-bridge" ]; then
    # plutil -replace on an array index inserts a new item on some macOS
    # versions, leaving the old executable as an extra argument.
    plutil -remove ProgramArguments.0 "$daemon_plist"
    plutil -insert ProgramArguments.0 -string "$cargo_bin_dir/codex-bridge" "$daemon_plist"
  fi
else
  cat >"$daemon_plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$daemon_label</string>
  <key>ProgramArguments</key><array>
    <string>$cargo_bin_dir/codex-bridge</string><string>--config</string><string>$config_path</string>
  </array>
  <key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$state_dir/bridge.log</string>
  <key>StandardErrorPath</key><string>$state_dir/bridge.log</string>
</dict></plist>
EOF
fi
chmod 600 "$daemon_plist"
plutil -lint "$daemon_plist"

if [ "$start_services" = true ]; then
  daemon_service=$domain/$daemon_label
  if [ "$managed_by_bridge" = true ]; then
    for label in desktop-env ws-adapter app-server; do
      launchctl bootout "$domain/local.codex-bridge.$label" >/dev/null 2>&1 || true
    done
    if [ "$runtime_mode" = desktop ]; then
      launchctl bootout "$domain/com.lunghaa.ws-unix-bridge" >/dev/null 2>&1 || true
      launchctl disable "$domain/com.lunghaa.ws-unix-bridge"
    fi
  fi
  launchctl bootout "$daemon_service" >/dev/null 2>&1 || true
  if ! launchctl bootstrap "$domain" "$daemon_plist"; then
    echo "Initial LaunchAgent bootstrap failed; enabling the user service and retrying." >&2
    launchctl enable "$daemon_service"
    if ! launchctl print "$daemon_service" >/dev/null 2>&1; then
      launchctl bootstrap "$domain" "$daemon_plist"
    fi
  fi
  if ! launchctl print "$daemon_service" >/dev/null 2>&1; then
    echo "LaunchAgent is unavailable after bootstrap: $daemon_service" >&2
    exit 1
  fi
  daemon_ready=false
  daemon_attempt=0
  while [ "$daemon_attempt" -lt 15 ]; do
    if "$cargo_bin_dir/codexctl" status >/dev/null 2>&1; then
      daemon_ready=true
      break
    fi
    daemon_attempt=$((daemon_attempt + 1))
    sleep 1
  done
  if [ "$daemon_ready" != true ]; then
    echo "LaunchAgent loaded, but the codex-bridge control socket did not become ready." >&2
    echo "Inspect: $state_dir/bridge.log" >&2
    exit 1
  fi
fi

echo "Installed user configuration: $config_path"
echo "Installed user LaunchAgent: $daemon_plist"
if [ "$start_suppressed" = true ]; then
  echo "Service start was deferred because an active private stdio app-server owns the rollout store."
fi
if [ "$enable_web_ui" = true ]; then
  echo "Web UI: http://127.0.0.1:18791/ (password file: $web_password)"
fi
if [ "$runtime_mode" = desktop ]; then
  echo "Fully quit and relaunch ChatGPT so it inherits CODEX_APP_SERVER_WS_URL."
else
  echo "Installed the recommended standalone topology; Codex Desktop remains independent."
fi
