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
  gnupg
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
  # mkfs.ext4 (e2fsprogs) and lsblk/mount/umount/blkid (util-linux) are essential
  # packages; xfs is not, and the Storage page offers it.
  xfsprogs
  qemu-system-x86
  qemu-utils
  qemu-kvm
  ovmf
  lxc
  lm-sensors
  libvirt-daemon-system
  libvirt-clients
  docker.io
  docker-compose
  unbound
  dnsmasq
  nftables
  iptables
  iproute2
  ipset
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

  # cloudflared is not a Debian package; pull it from Cloudflare's APT repo so
  # the tunnel feature works. If the repo add fails (offline, DNS, etc.) the
  # script warns and continues — the tunnel code degrades gracefully.
  if ! command -v cloudflared >/dev/null 2>&1; then
    install -d -m 0755 /usr/share/keyrings
    if ! curl -fsSL https://pkg.cloudflare.com/cloudflare-main.gpg \
        | gpg --dearmor -o /usr/share/keyrings/cloudflare-main.gpg 2>/dev/null; then
      warn_file "could not add Cloudflare signing key; cloudflared tunnel feature will be unavailable"
    else
      echo "deb [signed-by=/usr/share/keyrings/cloudflare-main.gpg] https://pkg.cloudflare.com/cloudflared any main" \
          > /etc/apt/sources.list.d/cloudflared.list
      if apt-get update -qq && apt-get install -y -qq cloudflared; then
        log_file "installed cloudflared"
      else
        warn_file "could not install cloudflared; the tunnel feature will report unavailable"
      fi
    fi
  fi

  # tc is provided by iproute2, not a separate package — verify the binary exists.
  command -v tc >/dev/null 2>&1 || warn_file "tc missing (iproute2 not installed?); SQM feature will be unavailable"

  # Unprivileged LXC containers need a subordinate UID/GID range for root.
  grep -q '^root:' /etc/subuid 2>/dev/null || echo 'root:100000:65536' >> /etc/subuid
  grep -q '^root:' /etc/subgid 2>/dev/null || echo 'root:100000:65536' >> /etc/subgid

  # Enable and start Docker
  systemctl enable docker >/dev/null 2>&1 || true
  systemctl start docker >/dev/null 2>&1 || warn_file "could not start docker service"

  # Enable and start libvirtd for QEMU/KVM
  systemctl enable --now libvirtd >/dev/null 2>&1 || true
  if ! systemctl is-active --quiet libvirtd; then
    warn_file "libvirtd failed to start; journal follows:"
    journalctl -u libvirtd --no-pager -n 30 >&2 || true
  fi
  # Ensure /run/libvirt exists even if libvirtd hasn't created it yet
  install -d -m 0755 /run/libvirt

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
  # Operator-authored compose stacks (see the "Create Stack" UI).
  install -d -m 0750 "${STATE_DIR}/stacks"
  # Append-only audit trail for destructive operations.
  install -d -m 0750 /var/log/stack-stack
  install -d -m 0755 "${BACKUP_DIR}"
  # The systemd unit references /run/libvirt; create it now so the first deploy
  # works even before libvirtd has ever run (it is /run, so tmpfs-recreated).
  install -d -m 0755 /run/libvirt
  # LXC needs /run/lxc/lock to create containers.
  install -d -m 0755 /run/lxc /run/lxc/lock
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
# These belong in [Unit]: systemd warns "Unknown key" and ignores them under
# [Service]. Five failures inside five seconds stops the loop so a crash loop is
# visible in the journal instead of restarting forever.
StartLimitIntervalSec=5
StartLimitBurst=5

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
# Security hardening
NoNewPrivileges=false
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/etc/stack-stack /var/lib/stack-stack /run/docker.sock -/run/libvirt -/run/lxc -/var/log/stack-stack
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
  # Verify the installed unit. This is exactly what catches a key systemd will
  # silently ignore because it sits in the wrong section (e.g. StartLimit* under
  # [Service]), so it fails at deploy time instead of flooding the journal.
  if command -v systemd-analyze >/dev/null 2>&1; then
    local verify
    verify="$(systemd-analyze verify "${UNIT_FILE}" 2>&1 || true)"
    if [[ -n "${verify}" ]]; then
      warn_file "systemd-analyze reported problems with ${UNIT_FILE}:"
      printf '%s\n' "${verify}" >&2
    fi
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

  # Fail fast: if the service is not even active, dumps the journal so the
  # actual error (e.g. 226/NAMESPACE) is visible instead of a 60s curl timeout.
  if ! systemctl is-active --quiet "${SERVICE_NAME}"; then
    warn_file "${SERVICE_NAME} is not active; journal follows:"
    journalctl -u "${SERVICE_NAME}" --no-pager -n 50 >&2 || true
    return 1
  fi

  local attempt
  for attempt in $(seq 1 60); do
    if curl -fsS --max-time 3 "${HEALTH_URL}" >/dev/null 2>&1; then
      log_file "stack-stack is healthy"
      curl -fsS --max-time 3 "${HEALTH_URL}"
      printf '\n'
      return 0
    fi
    sleep 1
  done
  warn_file "${HEALTH_URL} did not answer in time; journal follows:"
  journalctl -u "${SERVICE_NAME}" --no-pager -n 50 >&2 || true
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

# ---------------------------------------------------------------------------
# System hardening (defence in depth)
#
# All of this is best-effort: a homelab box must still deploy if, say, ufw
# cannot be installed. Each step warns and continues rather than aborting.
# ---------------------------------------------------------------------------
harden_system() {
  if ! command -v apt-get >/dev/null 2>&1; then
    warn_file "apt-get not found; skipping system hardening"
    return 0
  fi
  log_file "applying system hardening"
  export DEBIAN_FRONTEND=noninteractive

  # 1) Unattended security upgrades. Debian minimal installs often have the
  #    package present but no periodic policy configured.
  if ! dpkg -s unattended-upgrades >/dev/null 2>&1; then
    apt-get install -y unattended-upgrades >/dev/null 2>&1 \
      || warn_file "could not install unattended-upgrades"
  fi
  write_if_changed /etc/apt/apt.conf.d/20auto-upgrades \
    'APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
' >/dev/null || true

  # 2) fail2ban. Debian 12+ logs to the journal, so the systemd backend is what
  #    actually reads sshd events.
  if ! command -v fail2ban-client >/dev/null 2>&1; then
    apt-get install -y fail2ban >/dev/null 2>&1 || warn_file "could not install fail2ban"
  fi
  if [[ -d /etc/fail2ban ]]; then
    write_if_changed /etc/fail2ban/jail.d/stack-stack.local \
      '[DEFAULT]
backend = systemd
bantime = 1h
findtime = 10m
maxretry = 5

[sshd]
enabled = true
' >/dev/null || true
    systemctl enable fail2ban >/dev/null 2>&1 || true
    systemctl restart fail2ban >/dev/null 2>&1 || warn_file "could not start fail2ban"
  fi

  # 3) Firewall: default-deny inbound, allow only SSH and the UI (plus 80/443
  #    when the TLS front door is enabled).
  if ! command -v ufw >/dev/null 2>&1; then
    apt-get install -y ufw >/dev/null 2>&1 || warn_file "could not install ufw"
  fi
  if command -v ufw >/dev/null 2>&1; then
    ufw --force default deny incoming >/dev/null 2>&1 || true
    ufw --force default allow outgoing >/dev/null 2>&1 || true
    ufw allow 22/tcp >/dev/null 2>&1 || true
    ufw allow 8553/tcp >/dev/null 2>&1 || true
    if grep -q '"tls_enabled"[[:space:]]*:[[:space:]]*true' "${CONFIG_FILE}" 2>/dev/null; then
      ufw allow 80/tcp >/dev/null 2>&1 || true
      ufw allow 443/tcp >/dev/null 2>&1 || true
    fi
    # Default-deny inbound assumes SSH is on port 22. An operator on a custom
    # port must add it before the rule set takes effect at the next connection.
    warn_file "enabling ufw (default-deny inbound; SSH assumed on 22, UI on 8553) — verify your SSH port"
    ufw --force enable >/dev/null 2>&1 || warn_file "could not enable ufw"
  fi

  # 4) sysctl hardening drop-in.
  write_if_changed /etc/sysctl.d/99-stack-stack.conf \
    '# stack-stack hardening
net.ipv4.conf.all.rp_filter = 1
net.ipv4.conf.default.rp_filter = 1
kernel.kptr_restrict = 2
kernel.dmesg_restrict = 1
kernel.yama.ptrace_scope = 1
' >/dev/null || true
  sysctl --system >/dev/null 2>&1 || warn_file "could not apply sysctl settings"
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
  harden_system
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
