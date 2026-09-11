#!/bin/sh
set -eu

enable_web_ui=false
start_services=true
start_suppressed=false
while [ "$#" -gt 0 ]; do
  case "$1" in
    --web-ui) enable_web_ui=true ;;
    --no-start) start_services=false ;;
    -h|--help)
      echo "Usage: scripts/install-macos.sh [--web-ui] [--no-start]"
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
app_server_socket=$runtime_dir/bundled-app-server.sock
adapter_port=18790

if [ ! -x "$desktop_codex" ]; then
  echo "Codex Desktop runtime not found at $desktop_codex" >&2
  exit 1
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
CARGO_TARGET_DIR="$repo_root/target" CARGO_INCREMENTAL=0 \
  cargo install --locked --force --path "$repo_root/crates/codex-bridge"
CARGO_TARGET_DIR="$repo_root/target" CARGO_INCREMENTAL=0 \
  cargo install --locked --force --path "$repo_root/crates/codexctl"
CARGO_TARGET_DIR="$repo_root/target" CARGO_INCREMENTAL=0 \
  cargo install --locked --force --path "$repo_root/crates/codex-gui-bridge" \
  --bin ws-unix-bridge

web_password=$runtime_dir/web-ui-password
if [ "$enable_web_ui" = true ] && [ ! -s "$web_password" ]; then
  umask 077
  openssl rand -base64 32 >"$web_password"
  chmod 600 "$web_password"
fi

managed_by_bridge=false
if [ ! -f "$config_path" ]; then
  cat >"$config_path" <<EOF
mode = "desktop"
codex_bin = "$desktop_codex"
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
desktop_interposition = true
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
  ps -axo pid=,command= | awk -v bin="$desktop_codex" '
    index($0, bin " ") && index($0, " app-server") && !index($0, " --listen") && !index($0, " generate-json-schema") { print $1 }
  '
}
if [ "$start_services" = true ] && [ "$managed_by_bridge" = true ]; then
  conflicting_pids=$(stdio_app_server_pids)
  if [ -n "$conflicting_pids" ]; then
    echo "Refusing to start the managed bundled app-server while a private stdio app-server is running." >&2
    echo "The service definition will be installed without loading it. Fully quit Codex/ChatGPT, then start local.codex-bridge.daemon." >&2
    start_services=false
    start_suppressed=true
  fi
fi

daemon_plist=$launcher_dir/local.codex-bridge.daemon.plist
cat >"$daemon_plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>local.codex-bridge.daemon</string>
  <key>ProgramArguments</key><array>
    <string>$cargo_bin_dir/codex-bridge</string><string>--config</string><string>$config_path</string>
  </array>
  <key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$state_dir/bridge.log</string>
  <key>StandardErrorPath</key><string>$state_dir/bridge.log</string>
</dict></plist>
EOF
chmod 600 "$daemon_plist"
plutil -lint "$daemon_plist"

if [ "$start_services" = true ]; then
  domain=gui/$(id -u)
  daemon_service=$domain/local.codex-bridge.daemon
  if [ "$managed_by_bridge" = true ]; then
    for label in desktop-env ws-adapter app-server; do
      launchctl bootout "$domain/local.codex-bridge.$label" >/dev/null 2>&1 || true
    done
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
echo "Fully quit and relaunch ChatGPT so it inherits CODEX_APP_SERVER_WS_URL."
