#!/usr/bin/env bash
#
# stack-stack deployment / update script for Debian (Fujitsu Q558).
#
# Idempotent: safe to re-run for updates. The documented update flow is
#
#     git pull && ./deploy-stack-stack.sh
#
# What it does, in order:
#   1. verifies it is running as root from the repository root
#   2. installs build + runtime dependencies (tolerating individual failures;
#      the binary degrades gracefully when a tool is absent)
#   3. installs rustup/cargo if there is no Rust toolchain yet
#   4. builds the release binary
#   5. installs it to /usr/local/bin/stack-stack
#   6. creates the config/state directories
#   7. writes the default config document if none exists yet
#   8. installs and (re)starts the systemd unit
#   9. health-checks the HTTP surface
#
# Everything is written only when its content actually changes, so a re-run
# does not needlessly restart the service.

set -euo pipefail

BINARY_NAME="stack-stack"
SERVICE_NAME="stack-stack.service"
INSTALL_BIN="/usr/local/bin/${BINARY_NAME}"
CONFIG_DIR="/etc/stack-stack"
STATE_DIR="/var/lib/stack-stack"
CONFIG_FILE="${CONFIG_DIR}/config.json"
UNIT_FILE="/etc/systemd/system/${SERVICE_NAME}"
HEALTH_URL="http://127.0.0.1:8553/api/health"

# Runtime tools the services layer shells out to. Missing packages degrade to an
# "unavailable" report in the UI, so a failed install is never fatal here.
APT_PACKAGES=(
  build-essential
  pkg-config
  git
  curl
  ca-certificates
  openssl
  samba
  nfs-kernel-server
  restic
  wireguard-tools
  qrencode
  smartmontools
  tar
  unzip
  zstd
  xz-utils
  qemu-system-x86
  qemu-utils
  lxc
)

log() { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m warn:\033[0m %s\n' "$*" >&2; }
die() {
  printf '\033[1;31merror:\033[0m %s\n' "$*" >&2
  exit 1
}

# Write $2 to path $1 only when the content differs. Returns 0 when the file was
# changed, 1 when it was already up to date — callers use that to decide whether
# a restart is required.
write_if_changed() {
  local path="$1" content="$2" tmp
  if [[ -f "$path" ]] && [[ "$(cat "$path")" == "$content" ]]; then
    return 1
  fi
  tmp="$(mktemp)"
  printf '%s' "$content" >"$tmp"
  install -D -m "${3:-0644}" "$tmp" "$path"
  rm -f "$tmp"
  return 0
}

require_root() {
  if [[ "${EUID}" -ne 0 ]]; then
    die "this script must run as root (Docker socket, nftables, ports 80/443 and systemd)"
  fi
}

require_repo_root() {
  local here
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  cd "$here"
  [[ -f "Cargo.toml" ]] || die "run this from the repository root (Cargo.toml not found)"
  [[ -f "src/main.rs" ]] || die "run this from the repository root (src/main.rs not found)"
  REPO_ROOT="$here"
}

install_packages() {
  if ! command -v apt-get >/dev/null 2>&1; then
    warn "apt-get not found; skipping dependency installation"
    return 0
  fi

  log "installing dependencies"
  export DEBIAN_FRONTEND=noninteractive
  local to_install=() pkg
  for pkg in "${APT_PACKAGES[@]}"; do
    if ! dpkg -s "$pkg" >/dev/null 2>&1; then
      to_install+=("$pkg")
    fi
  done

  if [[ "${#to_install[@]}" -eq 0 ]]; then
    log "all dependencies already present"
    return 0
  fi

  if ! apt-get update -qq; then
    warn "apt-get update failed; continuing with the packages already available"
  fi
  # Install one at a time so one unavailable package cannot block the rest.
  for pkg in "${to_install[@]}"; do
    if dpkg -s "$pkg" >/dev/null 2>&1; then
      continue
    fi
    if ! apt-get install -y -qq --no-install-recommends "$pkg"; then
      warn "could not install ${pkg}; the matching feature will report unavailable"
    fi
  done
}

ensure_rust() {
  if command -v cargo >/dev/null 2>&1; then
    return 0
  fi
  # rustup installs into ~/.cargo, which a non-login shell does not source.
  if [[ -f "${HOME}/.cargo/env" ]]; then
    # shellcheck disable=SC1091
    source "${HOME}/.cargo/env"
  fi
  if command -v cargo >/dev/null 2>&1; then
    return 0
  fi

  log "installing Rust toolchain via rustup"
  curl --proto '=https' --tlsv1.2 -fsS https://sh.rustup.rs | sh -s -- -y --profile minimal
  # shellcheck disable=SC1091
  source "${HOME}/.cargo/env"
  command -v cargo >/dev/null 2>&1 || die "rustup installed but cargo is still not on PATH"
}

build_release() {
  log "building release binary (this can take a while on first run)"
  cargo build --release --locked
  [[ -x "target/release/${BINARY_NAME}" ]] || die "build did not produce target/release/${BINARY_NAME}"
}

install_binary() {
  log "installing ${INSTALL_BIN}"
  install -D -m 0755 "target/release/${BINARY_NAME}" "${INSTALL_BIN}"
}

create_directories() {
  log "creating directories"
  install -d -m 0755 "${CONFIG_DIR}"
  install -d -m 0700 "${CONFIG_DIR}/apps"
  install -d -m 0700 "${CONFIG_DIR}/ca"
  install -d -m 0755 "${STATE_DIR}"
  install -d -m 0750 "${STATE_DIR}/guests"
  install -d -m 0750 "${STATE_DIR}/images"
}

write_default_config() {
  if [[ -f "${CONFIG_FILE}" ]]; then
    return 0
  fi
  log "writing default config to ${CONFIG_FILE}"
  write_if_changed "${CONFIG_FILE}" '{
  "bind": "0.0.0.0",
  "port": 8553,
  "sample_interval_ms": 1000,
  "smart_interval_s": 60
}
' 0600 || true
}

unit_contents() {
  cat <<'EOF'
# Managed by deploy-stack-stack.sh — edits here are overwritten on the next deploy.
[Unit]
Description=stack-stack homelab control plane
Documentation=https://github.com/om-itani/stack-stack
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
# The service owns the Docker socket, manages nftables and binds :80/:443, so it
# runs as root by design.
User=root
Group=root
Environment=STACK_STACK_CONFIG=/etc/stack-stack/config.json
Environment=RUST_LOG=info
ExecStart=/usr/local/bin/stack-stack
Restart=always
RestartSec=3
TimeoutStopSec=30
LimitNOFILE=65536
# A crash must be visible in the journal rather than silently restarting forever.
StartLimitIntervalSec=0

[Install]
WantedBy=multi-user.target
EOF
}

install_unit() {
  local contents unit_changed=0
  contents="$(unit_contents)"
  if write_if_changed "${UNIT_FILE}" "${contents}" 0644; then
    log "installing systemd unit"
    unit_changed=1
  fi
  systemctl daemon-reload
  if [[ "${unit_changed}" -eq 1 ]]; then
    systemctl enable "${SERVICE_NAME}" >/dev/null 2>&1 || warn "could not enable ${SERVICE_NAME}"
  fi
}

restart_service() {
  log "restarting ${SERVICE_NAME}"
  systemctl enable "${SERVICE_NAME}" >/dev/null 2>&1 || true
  if systemctl is-active --quiet "${SERVICE_NAME}"; then
    systemctl restart "${SERVICE_NAME}"
  else
    systemctl start "${SERVICE_NAME}"
  fi
}

health_check() {
  log "health-checking ${HEALTH_URL}"
  local attempt
  for attempt in $(seq 1 20); do
    if curl -fsS --max-time 2 "${HEALTH_URL}" >/dev/null 2>&1; then
      log "stack-stack is healthy"
      curl -fsS --max-time 2 "${HEALTH_URL}"
      printf '\n'
      return 0
    fi
    sleep 1
  done
  warn "${HEALTH_URL} did not answer in time; check: journalctl -u ${SERVICE_NAME} -n 50"
  return 1
}

main() {
  require_root
  require_repo_root
  log "deploying stack-stack from ${REPO_ROOT}"
  install_packages
  ensure_rust
  build_release
  install_binary
  create_directories
  write_default_config
  install_unit
  restart_service
  health_check
  log "done"
}

main "$@"
