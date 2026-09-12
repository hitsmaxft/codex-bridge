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
      echo "Usage: scripts/install-linux.sh [--web-ui] [--no-start]"
      exit 0
      ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

if [ "$(uname -s)" != "Linux" ]; then
  echo "install-linux.sh must run on Linux" >&2
  exit 1
fi

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cargo_bin_dir=${CARGO_HOME:-"$HOME/.cargo"}/bin
config_dir=${XDG_CONFIG_HOME:-"$HOME/.config"}/codex-bridge
config_path=$config_dir/config.toml
runtime_dir=${XDG_RUNTIME_DIR:-"/run/user/$(id -u)"}
private_dir=$HOME/.codex-bridge
systemd_dir=${XDG_CONFIG_HOME:-"$HOME/.config"}/systemd/user
codex_bin=$(command -v codex || true)
app_server_socket=$runtime_dir/codex-app-server.sock

if [ -z "$codex_bin" ]; then
  echo "standalone codex executable was not found on PATH" >&2
  exit 1
fi
if [ "$enable_web_ui" = true ] && ! command -v openssl >/dev/null 2>&1; then
  echo "openssl is required with --web-ui" >&2
  exit 1
fi
for command_name in cargo npm systemctl; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "required command not found: $command_name" >&2
    exit 1
  }
done

mkdir -p "$config_dir" "$private_dir" "$systemd_dir"
chmod 700 "$config_dir" "$private_dir"

npm --prefix "$repo_root/web-ui" ci
npm --prefix "$repo_root/web-ui" run build
CARGO_TARGET_DIR="$repo_root/target" CARGO_INCREMENTAL=0 \
  cargo install --locked --force --path "$repo_root/crates/codex-bridge"
CARGO_TARGET_DIR="$repo_root/target" CARGO_INCREMENTAL=0 \
  cargo install --locked --force --path "$repo_root/crates/codexctl"

web_password=$private_dir/web-ui-password
if [ "$enable_web_ui" = true ] && [ ! -s "$web_password" ]; then
  umask 077
  openssl rand -base64 32 >"$web_password"
  chmod 600 "$web_password"
fi

managed_by_bridge=false
if [ ! -f "$config_path" ]; then
  cat >"$config_path" <<EOF
mode = "standalone"
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
desktop_interposition = false
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
  ps -axo pid=,args= | awk -v bin="$codex_bin" '
    index($0, bin " ") && index($0, " app-server") && (!index($0, " --listen") || index($0, " --listen stdio://") || index($0, " --listen=stdio://")) && !index($0, " generate-json-schema") { print $1 }
  '
}
if [ "$start_services" = true ] && [ "$managed_by_bridge" = true ]; then
  conflicting_pids=$(stdio_app_server_pids)
  if [ -n "$conflicting_pids" ]; then
    echo "Refusing to start the managed app-server while a private stdio app-server is running." >&2
    echo "The service definition will be installed without enabling it. Exit the active Codex process, then start codex-bridge.service." >&2
    start_services=false
    start_suppressed=true
  fi
fi

cat >"$systemd_dir/codex-bridge.service" <<EOF
[Unit]
Description=Codex App Server WebUI bridge
After=network.target

[Service]
ExecStart="$cargo_bin_dir/codex-bridge" --config "$config_path"
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
EOF

if [ "$start_services" = true ]; then
  systemctl --user daemon-reload
  if [ "$managed_by_bridge" = true ]; then
    systemctl --user disable --now codex-app-server.service >/dev/null 2>&1 || true
  fi
  systemctl --user enable --now codex-bridge.service
fi

echo "Installed user configuration: $config_path"
echo "Installed systemd user services under: $systemd_dir"
if [ "$start_suppressed" = true ]; then
  echo "Service start was deferred because an active private stdio app-server owns the rollout store."
fi
if [ "$enable_web_ui" = true ]; then
  echo "Web UI: http://127.0.0.1:18791/ (password file: $web_password)"
fi
