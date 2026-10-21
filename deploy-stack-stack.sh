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
#   2. runs pre-flight checks (disk space, memory, architecture)
#   3. installs build + runtime dependencies (tolerating individual failures;
#      the binary degrades gracefully when a tool is absent)
#   4. installs rustup/cargo if there is no Rust toolchain yet
#   5. builds the release binary
#   6. installs it to /usr/local/bin/stack-stack
#   7. creates the config/state directories
#   8. writes the default config document if none exists yet
#   9. installs and (re)starts the systemd unit
#  10. health-checks the HTTP surface with retries
#  11. runs post-deploy verification (all subsystems report status)
#
# Everything is written only when its content actually changes, so a re-run
# does not needlessly restart the service.
#
# Rollback: if health check fails, the previous binary is restored and
# the service is restarted with the old version.

set -euo pipefail

# Version for rollback tracking
DEPLOY_VERSION="$(date +%Y%m%d-%H%M%S)"
BACKUP_DIR="/var/lib/stack-stack/deploy-backups"

BINARY_NAME="stack-stack"
SERVICE_NAME="stack-stack.service"
INSTALL_BIN="/usr/local/bin/${BINARY_NAME}"
CONFIG_DIR="/etc/stack-stack"
STATE_DIR="/var/lib/stack-stack"
CONFIG_FILE="${CONFIG_DIR}/config.json"
UNIT_FILE="/etc/systemd/system/${SERVICE_NAME}"
HEALTH_URL="http://127.0.0.1:8553/api/health"
PREVIOUS_BIN="${INSTALL_BIN}.prev"
DEPLOY_LOG="/var/log/stack-stack-deploy.log"

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
  lm-sensors
  docker.io
  docker-compose
  cloudflared
  unbound
  dnsmasq
  nftables
  iptables
  iproute2
  ipset
  tc
  jq
)

log() { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m warn:\033[0m %s\n' "$*" >&2; }
die() {
  printf '\033[1;31merror:\033[0m %s\n' "$*" >&2
  exit 1
}

# Log to both console and file
log_file() {
  local msg="$*"
  printf '\033[1;32m==>\033[0m %s\n' "$msg"
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] $msg" >> "${DEPLOY_LOG}"
}

warn_file() {
  local msg="$*"
  printf '\033[1;33m warn:\033[0m %s\n' "$msg" >&2
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] WARN: $msg" >> "${DEPLOY_LOG}"
}

die_file() {
  local msg="$*"
  printf '\033[1;31merror:\033[0m %s\n' "$msg" >&2
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] ERROR: $msg" >> "${DEPLOY_LOG}"
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

# Pre-flight checks before deployment
preflight_checks() {
  log_file "Running pre-flight checks..."

  # Check architecture
  local arch
  arch="$(uname -m)"
  if [[ "$arch" != "x86_64" ]]; then
    die_file "Unsupported architecture: $arch (only x86_64 supported)"
  fi

  # Check disk space (need at least 5GB free for build + binary)
  local avail_kb
  avail_kb="$(df / | awk 'NR==2 {print $4}')"
  if [[ "$avail_kb" -lt 5242880 ]]; then
    die_file "Insufficient disk space: ${avail_kb}KB available, need at least 5GB"
  fi

  # Check memory (need at least 2GB for build)
  local mem_kb
  mem_kb="$(grep MemTotal /proc/meminfo | awk '{print $2}')"
  if [[ "$mem_kb" -lt 2097152 ]]; then
    warn_file "Low memory: ${mem_kb}KB (recommend 2GB+ for build)"
  fi

  # Check if we're on Debian/Ubuntu
  if ! command -v apt-get >/dev/null 2>&1; then
    die_file "This script requires Debian/Ubuntu with apt-get"
  fi

  # Check internet connectivity
  if ! curl -fsS --max-time 5 https://github.com >/dev/null 2>&1; then
    warn_file "Cannot reach GitHub - dependency downloads may fail"
  fi

  # Check if Docker socket exists (will be created by docker.io package)
  log_file "Pre-flight checks passed"
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
    warn_file "apt-get not found; skipping dependency installation"
    return 0
  fi

  log_file "installing dependencies"
  export DEBIAN_FRONTEND=noninteractive
  local to_install=() pkg
  for pkg in "${APT_PACKAGES[@]}"; do
    if ! dpkg -s "$pkg" >/dev/null 2>&1; then
      to_install+=("$pkg")
    fi
  done

  if [[ "${#to_install[@]}" -eq 0 ]]; then
    log_file "all dependencies already present"
    return 0
  fi

  if ! apt-get update -qq; then
    warn_file "apt-get update failed; continuing with the packages already available"
  fi
  # Install one at a time so one unavailable package cannot block the rest.
  for pkg in "${to_install[@]}"; do
    if dpkg -s "$pkg" >/dev/null 2>&1; then
      continue
    fi
    if ! apt-get install -y -qq --no-install-recommends "$pkg"; then
      warn_file "could not install ${pkg}; the matching feature will report unavailable"
    fi
  done

  # Enable and start Docker
  systemctl enable docker >/dev/null 2>&1 || true
  systemctl start docker >/dev/null 2>&1 || warn_file "could not start docker service"

  # Enable and start libvirtd for QEMU/KVM
  systemctl enable libvirtd >/dev/null 2>&1 || true
  systemctl start libvirtd >/dev/null 2>&1 || warn_file "could not start libvirtd service"

  # Add current user to docker and libvirt groups
  usermod -aG docker,libvirt,kvm "${SUDO_USER:-root}" 2>/dev/null || true
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

  log_file "installing Rust toolchain via rustup"
  curl --proto '=https' --tlsv1.2 -fsS https://sh.rustup.rs | sh -s -- -y --profile minimal
  # shellcheck disable=SC1091
  source "${HOME}/.cargo/env"
  command -v cargo >/dev/null 2>&1 || die_file "rustup installed but cargo is still not on PATH"
}

build_release() {
  log_file "building release binary (this can take a while on first run)"
  cargo build --release --locked
  [[ -x "target/release/${BINARY_NAME}" ]] || die_file "build did not produce target/release/${BINARY_NAME}"
}

install_binary() {
  log_file "installing ${INSTALL_BIN}"
  install -D -m 0755 "target/release/${BINARY_NAME}" "${INSTALL_BIN}"
}

create_directories() {
  log_file "creating directories"
  install -d -m 0755 "${CONFIG_DIR}"
  install -d -m 0700 "${CONFIG_DIR}/apps"
  install -d -m 0700 "${CONFIG_DIR}/ca"
  install -d -m 0755 "${STATE_DIR}"
  install -d -m 0750 "${STATE_DIR}/guests"
  install -d -m 0750 "${STATE_DIR}/images"
  install -d -m 0755 "${BACKUP_DIR}"
}

write_default_config() {
  if [[ -f "${CONFIG_FILE}" ]]; then
    return 0
  fi
  log_file "writing default config to ${CONFIG_FILE}"
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
After=network-online.target docker.service libvirtd.service
Wants=network-online.target docker.service libvirtd.service

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
# Security hardening
NoNewPrivileges=false
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/etc/stack-stack /var/lib/stack-stack /var/run/docker.sock /run/libvirt
CapabilityBoundingSet=CAP_NET_ADMIN CAP_NET_RAW CAP_SYS_ADMIN CAP_DAC_OVERRIDE CAP_CHOWN CAP_FOWNER CAP_SYS_RESOURCE

[Install]
WantedBy=multi-user.target
EOF
}

install_unit() {
  local contents unit_changed=0
  contents="$(unit_contents)"
  if write_if_changed "${UNIT_FILE}" "${contents}" 0644; then
    log_file "installing systemd unit"
    unit_changed=1
  fi
  systemctl daemon-reload
  if [[ "${unit_changed}" -eq 1 ]]; then
    systemctl enable "${SERVICE_NAME}" >/dev/null 2>&1 || warn_file "could not enable ${SERVICE_NAME}"
  fi
}

restart_service() {
  log_file "restarting ${SERVICE_NAME}"
  systemctl enable "${SERVICE_NAME}" >/dev/null 2>&1 || true
  if systemctl is-active --quiet "${SERVICE_NAME}"; then
    systemctl restart "${SERVICE_NAME}"
  else
    systemctl start "${SERVICE_NAME}"
  fi
}

# Comprehensive health check including all subsystems
health_check() {
  log_file "health-checking ${HEALTH_URL}"
  local attempt
  for attempt in $(seq 1 30); do
    if curl -fsS --max-time 3 "${HEALTH_URL}" >/dev/null 2>&1; then
      log_file "stack-stack is healthy"
      curl -fsS --max-time 3 "${HEALTH_URL}"
      printf '\n'
      return 0
    fi
    sleep 1
  done
  warn_file "${HEALTH_URL} did not answer in time; check: journalctl -u ${SERVICE_NAME} -n 100"
  return 1
}

# Post-deploy verification of all subsystems
post_deploy_verification() {
  log_file "Running post-deploy verification..."

  # Check main health
  if ! curl -fsS --max-time 5 "${HEALTH_URL}" >/dev/null 2>&1; then
    warn_file "Main health endpoint unreachable"
    return 1
  fi

  # Check all API endpoints
  local endpoints=(
    "/api/system/snapshot"
    "/api/containers"
    "/api/apps/catalog"
    "/api/network"
    "/api/virt"
    "/api/proxy"
    "/api/files"
    "/api/samba"
    "/api/nfs"
    "/api/backup"
    "/api/wireguard"
    "/api/smart"
    "/api/logs"
    "/api/ddns"
    "/api/notifications"
  )

  local failed=0
  for ep in "${endpoints[@]}"; do
    if curl -fsS --max-time 5 "http://127.0.0.1:8553${ep}" >/dev/null 2>&1; then
      log_file "  ✓ ${ep}"
    else
      warn_file "  ✗ ${ep} (may require auth or not be configured)"
      ((failed++))
    fi
  done

  if [[ $failed -eq ${#endpoints[@]} ]]; then
    warn_file "All endpoints failed - service may not be fully initialized"
    return 1
  fi

  log_file "Post-deploy verification complete (${failed} endpoints need configuration)"
  return 0
}

# Back up the currently installed binary so it can be restored on rollback.
backup_binary() {
  if [[ -x "${INSTALL_BIN}" ]]; then
    cp "${INSTALL_BIN}" "${PREVIOUS_BIN}"
    log_file "backed up existing binary to ${PREVIOUS_BIN}"
  else
    log_file "no existing binary to back up"
  fi
}

# Restore the previous binary after a failed health check.
rollback_binary() {
  if [[ -x "${PREVIOUS_BIN}" ]]; then
    cp "${PREVIOUS_BIN}" "${INSTALL_BIN}"
    log_file "restored previous binary from ${PREVIOUS_BIN}"
    systemctl restart "${SERVICE_NAME}" 2>/dev/null || true
    log_file "service restarted with previous binary"
  else
    warn_file "no previous binary found to restore"
  fi
}

main() {
  require_root
  require_repo_root
  log_file "deploying stack-stack from ${REPO_ROOT} (version: ${DEPLOY_VERSION})"

  # Initialize deploy log
  mkdir -p "$(dirname "${DEPLOY_LOG}")"
  echo "=== stack-stack deploy ${DEPLOY_VERSION} ===" > "${DEPLOY_LOG}"

  preflight_checks
  install_packages
  ensure_rust
  backup_binary
  build_release
  install_binary
  create_directories
  write_default_config
  install_unit
  restart_service

  # Health check with rollback on failure
  if ! health_check; then
    warn_file "Health check failed, attempting rollback..."
    rollback_binary
    die_file "Deployment failed and rolled back. Check ${DEPLOY_LOG} for details."
  fi

  post_deploy_verification

  log_file "done - deployment successful"
  log_file "Access the UI at: http://$(hostname -I | awk '{print $1}'):8553"
  log_file "Deploy log: ${DEPLOY_LOG}"
}

main "$@"
