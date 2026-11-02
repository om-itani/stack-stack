#!/usr/bin/env bash
#
# stack-stack automated deployment and update script for Debian/Ubuntu systems.
#
# Idempotent: safe to run from fresh install or to update an existing deployment.
# Documented update flow:
#     git pull && ./deploy-stack-stack.sh
#

set -euo pipefail

# ---------------------------------------------------------------------------
# Global Constants & Paths
# ---------------------------------------------------------------------------
DEPLOY_VERSION="$(date +%Y%m%d-%H%M%S)"
BINARY_NAME="stack-stack"
SERVICE_NAME="stack-stack.service"
SERVICE_GROUP="stack-stack"
INSTALL_BIN="/usr/local/bin/${BINARY_NAME}"
PREVIOUS_BIN="${INSTALL_BIN}.prev"
CONFIG_DIR="/etc/stack-stack"
STATE_DIR="/var/lib/stack-stack"
BACKUP_DIR="/var/lib/stack-stack/deploy-backups"
CONFIG_FILE="${CONFIG_DIR}/config.json"
UNIT_FILE="/etc/systemd/system/${SERVICE_NAME}"
HEALTH_URL="http://127.0.0.1:8553/api/health"
DEPLOY_LOG="/var/log/stack-stack-deploy.log"

TOTAL_STEPS=12
CURRENT_STEP=0
ACTIVE_STEP=1
SCRIPT_START_TIME="$(date +%s)"

# Comprehensive list of build and runtime dependencies
APT_PACKAGES=(
  build-essential
  pkg-config
  libssl-dev
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
  unbound-anchor
  dns-root-data
  dnsmasq
  nftables
  iptables
  iproute2
  ipset
  jq
  hostapd
  wpa_supplicant
  wireless-tools
  iw
  ufw
  fail2ban
  unattended-upgrades
)

# ---------------------------------------------------------------------------
# UI / Progress Bar Engine
# ---------------------------------------------------------------------------
# Color palette
C_RESET="\033[0m"
C_BOLD="\033[1m"
C_CYAN="\033[1;36m"
C_BLUE="\033[1;34m"
C_GREEN="\033[1;32m"
C_YELLOW="\033[1;33m"
C_RED="\033[1;31m"
C_MAGENTA="\033[1;35m"
C_GRAY="\033[0;90m"
C_WHITE="\033[1;37m"

# Disable colors if not in an interactive terminal
if [[ ! -t 1 ]]; then
  C_RESET=""
  C_BOLD=""
  C_CYAN=""
  C_BLUE=""
  C_GREEN=""
  C_YELLOW=""
  C_RED=""
  C_MAGENTA=""
  C_GRAY=""
  C_WHITE=""
fi

ensure_root() {
  if [[ "${EUID}" -ne 0 ]]; then
    if command -v sudo >/dev/null 2>&1; then
      local script_path
      script_path="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
      printf "  \033[1;36m==>\033[0m Elevating permissions with sudo...\n"
      exec sudo -E bash "${script_path}" "$@"
    else
      printf "\033[1;31merror:\033[0m this script must run as root. Please run: sudo %s\n" "$0" >&2
      exit 1
    fi
  fi
}

log_file() {
  local log_dir
  log_dir="$(dirname "${DEPLOY_LOG}")"
  if [[ -w "${DEPLOY_LOG}" || ( ! -e "${DEPLOY_LOG}" && -w "${log_dir}" ) ]]; then
    { echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*" >> "${DEPLOY_LOG}"; } 2>/dev/null || true
  fi
}

print_banner() {
  printf "\n"
  printf "${C_CYAN}  ╔══════════════════════════════════════════════════════════════════╗${C_RESET}\n"
  printf "${C_CYAN}  ║${C_RESET}                   ${C_BOLD}${C_WHITE}stack-stack automated deploy${C_RESET}                   ${C_CYAN}║${C_RESET}\n"
  printf "${C_CYAN}  ║${C_RESET}             ${C_GRAY}homelab control plane & edge router${C_RESET}                  ${C_CYAN}║${C_RESET}\n"
  printf "${C_CYAN}  ╚══════════════════════════════════════════════════════════════════╝${C_RESET}\n\n"
}

render_progress_bar() {
  local completed="$1"
  local active="$2"
  local total="$3"
  local status_text="$4"
  local width=28

  local percent=$(( completed * 100 / total ))
  (( percent > 100 )) && percent=100
  local filled=$(( completed * width / total ))
  (( filled > width )) && filled=width
  local empty=$(( width - filled ))

  local bar=""
  for ((i = 0; i < filled; i++)); do bar="${bar}█"; done
  for ((i = 0; i < empty; i++)); do bar="${bar}░"; done

  if [[ -t 1 ]]; then
    printf "\r\033[K  ${C_BLUE}Progress: [${C_CYAN}%s${C_BLUE}]${C_RESET} ${C_GREEN}%3d%%${C_RESET} ${C_GRAY}(%2d/%2d)${C_RESET} %b" \
      "$bar" "$percent" "$active" "$total" "$status_text"
  fi
}

step_start() {
  local step="$1"
  local label="$2"
  CURRENT_STEP="$(( step - 1 ))"
  ACTIVE_STEP="$step"
  log_file "STEP [${step}/${TOTAL_STEPS}] START: ${label}"
  if [[ -t 1 ]]; then
    render_progress_bar "$CURRENT_STEP" "$ACTIVE_STEP" "$TOTAL_STEPS" "${C_YELLOW}➜${C_RESET} ${C_WHITE}${label}...${C_RESET}"
  else
    printf "  [%2d/%2d] ➜ %s...\n" "$step" "$TOTAL_STEPS" "$label"
  fi
}

step_done() {
  local step="$1"
  local label="$2"
  local duration="${3:-}"
  CURRENT_STEP="$step"
  log_file "STEP [${step}/${TOTAL_STEPS}] DONE: ${label} (${duration}s)"
  local dur_str=""
  [[ -n "$duration" ]] && dur_str=" ${C_GRAY}(${duration}s)${C_RESET}"
  if [[ -t 1 ]]; then
    printf "\r\033[K  ${C_GREEN}✔${C_RESET} ${C_GRAY}[%2d/%2d]${C_RESET} ${C_WHITE}%s${C_RESET}%b\n" \
      "$step" "$TOTAL_STEPS" "$label" "$dur_str"
  else
    printf "  ✔ [%2d/%2d] %s%b\n" "$step" "$TOTAL_STEPS" "$label" "$dur_str"
  fi
}

step_fail() {
  local step="$1"
  local label="$2"
  log_file "STEP [${step}/${TOTAL_STEPS}] FAILED: ${label}"
  if [[ -t 1 ]]; then
    printf "\r\033[K  ${C_RED}✗${C_RESET} ${C_GRAY}[%2d/%2d]${C_RESET} ${C_RED}%s FAILED!${C_RESET}\n\n" \
      "$step" "$TOTAL_STEPS" "$label"
  else
    printf "  ✗ [%2d/%2d] %s FAILED!\n\n" "$step" "$TOTAL_STEPS" "$label"
  fi
}

# Run a command with a smooth live spinner while streaming logs to $DEPLOY_LOG
run_with_spinner() {
  local task_name="$1"
  shift

  log_file "EXEC: $*"

  if [[ ! -t 1 ]]; then
    if "$@" >> "${DEPLOY_LOG}" 2>&1; then
      return 0
    else
      local rc=$?
      log_file "EXEC ERROR (exit $rc): $*"
      return $rc
    fi
  fi

  "$@" >> "${DEPLOY_LOG}" 2>&1 &
  local pid=$!
  local spin=('⠋' '⠙' '⠹' '⠸' '⠼' '⠴' '⠦' '⠧' '⠇' '⠏')
  local idx=0
  local start_time
  start_time="$(date +%s)"

  while kill -0 "$pid" 2>/dev/null; do
    local now
    now="$(date +%s)"
    local elapsed=$(( now - start_time ))
    local s="${spin[idx]}"
    idx=$(( (idx + 1) % ${#spin[@]} ))
    render_progress_bar "${CURRENT_STEP}" "${ACTIVE_STEP}" "${TOTAL_STEPS}" \
      "${C_YELLOW}${s}${C_RESET} ${C_WHITE}${task_name}${C_RESET} ${C_GRAY}(${elapsed}s)${C_RESET}"
    sleep 0.08
  done

  wait "$pid"
  local rc=$?
  if [[ $rc -ne 0 ]]; then
    log_file "EXEC ERROR (exit $rc): $*"
  fi
  return $rc
}

die() {
  local msg="$*"
  log_file "FATAL ERROR: $msg"
  printf "\n${C_RED}error:${C_RESET} %s\n" "$msg" >&2
  if [[ -f "${DEPLOY_LOG}" ]]; then
    printf "${C_GRAY}Last 15 lines of deploy log (${DEPLOY_LOG}):${C_RESET}\n" >&2
    tail -n 15 "${DEPLOY_LOG}" | sed 's/^/  /' >&2
  fi
  exit 1
}

# Write $2 to path $1 only when content differs. Returns 0 if changed, 1 if already identical.
write_if_changed() {
  local path="$1" content="$2" mode="${3:-0644}" tmp
  if [[ -f "$path" ]] && [[ "$(cat "$path" 2>/dev/null)" == "$content" ]]; then
    return 1
  fi
  tmp="$(mktemp)"
  printf '%s' "$content" > "$tmp"
  install -D -m "$mode" "$tmp" "$path"
  rm -f "$tmp"
  return 0
}

# ---------------------------------------------------------------------------
# Step Implementations
# ---------------------------------------------------------------------------

# Step 1: Privilege & Repository Validation
setup_environment() {
  local t0
  t0="$(date +%s)"

  local here
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  cd "$here"
  [[ -f "Cargo.toml" ]] || die "must be run from repository root (Cargo.toml missing)"
  [[ -f "src/main.rs" ]] || die "must be run from repository root (src/main.rs missing)"
  REPO_ROOT="$here"

  mkdir -p "$(dirname "${DEPLOY_LOG}")" 2>/dev/null || true
  { echo "=== stack-stack automated deploy ${DEPLOY_VERSION} ===" >> "${DEPLOY_LOG}"; } 2>/dev/null || true

  step_start 1 "Privilege and repository validation"
  [[ "${EUID}" -eq 0 ]] || die "this script must run as root."
  log_file "Repository root: ${REPO_ROOT}"

  local t1
  t1="$(date +%s)"
  step_done 1 "Privilege and repository validation" "$(( t1 - t0 ))"
}

# Step 2: Pre-flight checks
run_preflight_checks() {
  local t0
  t0="$(date +%s)"
  step_start 2 "Pre-flight system checks"

  # 1. Architecture check
  local arch
  arch="$(uname -m)"
  if [[ "$arch" != "x86_64" ]] && [[ "${STACK_STACK_FORCE:-0}" != "1" ]]; then
    die "unsupported architecture: $arch (x86_64 required)"
  fi

  # 2. Disk space check (need at least 3GB free in repo partition)
  local avail_kb
  avail_kb="$(df -k "$REPO_ROOT" | awk 'NR==2 {print $4}')"
  if [[ -n "$avail_kb" && "$avail_kb" -lt 3145728 ]] && [[ "${STACK_STACK_FORCE:-0}" != "1" ]]; then
    die "insufficient disk space: $(( avail_kb / 1024 ))MB free, need at least 3000MB"
  fi

  # 3. Memory detection
  local mem_kb
  mem_kb="$(grep MemTotal /proc/meminfo 2>/dev/null | awk '{print $2}' || echo 4194304)"
  if [[ "$mem_kb" -lt 1500000 ]]; then
    log_file "Low memory detected (${mem_kb}KB), will tune cargo build parallelism"
  fi

  # 4. OS check
  if ! command -v apt-get >/dev/null 2>&1; then
    die "this deploy script requires Debian or Ubuntu with apt-get"
  fi

  # 5. Network connectivity
  if ! curl -fsS --max-time 4 https://github.com >/dev/null 2>&1 && \
     ! curl -fsS --max-time 4 https://1.1.1.1 >/dev/null 2>&1; then
    log_file "Warning: outbound internet check was slow or unreachable"
  fi

  local t1
  t1="$(date +%s)"
  step_done 2 "Pre-flight system checks" "$(( t1 - t0 ))"
}

# Step 3: Fast Batch Package Installation
install_system_packages() {
  local t0
  t0="$(date +%s)"
  step_start 3 "System package dependencies"

  export DEBIAN_FRONTEND=noninteractive

  # Fast detection of missing packages
  local missing=()
  for pkg in "${APT_PACKAGES[@]}"; do
    if ! dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null | grep -q "install ok installed"; then
      missing+=("$pkg")
    fi
  done

  if [[ "${#missing[@]}" -eq 0 ]]; then
    log_file "all ${#APT_PACKAGES[@]} system dependencies already installed"
    local t1
    t1="$(date +%s)"
    step_done 3 "System package dependencies (already installed)" "$(( t1 - t0 ))"
    return 0
  fi

  log_file "installing ${#missing[@]} missing packages: ${missing[*]}"

  # Run apt-get update once
  run_with_spinner "Updating package indexes" apt-get update -qq -y || true

  # Batch install (fast!)
  if ! run_with_spinner "Installing dependencies (${#missing[@]} packages)" \
      apt-get install -y --no-install-recommends -qq "${missing[@]}"; then
    log_file "Batch install failed, falling back to individual installs"
    # Fallback: install individually so one obscure package doesn't block the rest
    for pkg in "${missing[@]}"; do
      if ! dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null | grep -q "install ok installed"; then
        run_with_spinner "Installing ${pkg}" apt-get install -y --no-install-recommends -qq "$pkg" || true
      fi
    done
  fi

  local t1
  t1="$(date +%s)"
  step_done 3 "System package dependencies" "$(( t1 - t0 ))"
}

# Step 4: Cloudflared Tunnel Service
setup_cloudflared() {
  local t0
  t0="$(date +%s)"
  step_start 4 "Cloudflared tunnel utility"

  if command -v cloudflared >/dev/null 2>&1; then
    local t1
    t1="$(date +%s)"
    step_done 4 "Cloudflared tunnel utility (already present)" "$(( t1 - t0 ))"
    return 0
  fi

  install -d -m 0755 /usr/share/keyrings /etc/apt/sources.list.d
  local gpg_file="/usr/share/keyrings/cloudflare-main.gpg"

  if curl -fsSL https://pkg.cloudflare.com/cloudflare-main.gpg | gpg --dearmor -o "$gpg_file" 2>/dev/null; then
    echo "deb [arch=amd64 signed-by=${gpg_file}] https://pkg.cloudflare.com/cloudflared any main" \
      > /etc/apt/sources.list.d/cloudflared.list
    run_with_spinner "Installing cloudflared" bash -c \
      "apt-get update -o Dir::Etc::sourcelist='sources.list.d/cloudflared.list' -o Dir::Etc::sourceparts='-' -o APT::Get::List-Cleanup='0' -qq && apt-get install -y -qq cloudflared" || true
  else
    log_file "could not add cloudflare keyring; cloudflared will report unavailable"
  fi

  local t1
  t1="$(date +%s)"
  step_done 4 "Cloudflared tunnel utility" "$(( t1 - t0 ))"
}

# Step 5: Container & Hypervisor Services
setup_hypervisors() {
  local t0
  t0="$(date +%s)"
  step_start 5 "Container & virtualization hypervisors"

  # Unprivileged LXC subordinate UIDs
  grep -q '^root:' /etc/subuid 2>/dev/null || echo 'root:100000:65536' >> /etc/subuid
  grep -q '^root:' /etc/subgid 2>/dev/null || echo 'root:100000:65536' >> /etc/subgid

  # Enable and activate Docker daemon
  if command -v systemctl >/dev/null 2>&1; then
    systemctl enable docker >/dev/null 2>&1 || true
    systemctl start docker >/dev/null 2>&1 || log_file "docker service failed to start"

    # Enable and activate Libvirtd daemon
    systemctl enable --now libvirtd >/dev/null 2>&1 || true
  fi

  # Add user to groups
  local target_user="${SUDO_USER:-root}"
  if [[ "$target_user" != "root" ]] && id -u "$target_user" >/dev/null 2>&1; then
    usermod -aG docker,libvirt,kvm "$target_user" 2>/dev/null || true
  fi

  local t1
  t1="$(date +%s)"
  step_done 5 "Container & virtualization hypervisors" "$(( t1 - t0 ))"
}

# Step 6: Rust Toolchain Setup
setup_rust_toolchain() {
  local t0
  t0="$(date +%s)"
  step_start 6 "Rust toolchain configuration"

  # Source potential rustup environments
  local sudo_cargo_bin=""
  if [[ -n "${SUDO_USER:-}" ]]; then
    local sudo_home
    sudo_home="$(eval echo "~${SUDO_USER}")"
    if [[ -d "${sudo_home}/.cargo/bin" ]]; then
      sudo_cargo_bin="${sudo_home}/.cargo/bin:"
    fi
    if [[ -f "${sudo_home}/.cargo/env" ]]; then
      # shellcheck disable=SC1090
      source "${sudo_home}/.cargo/env" 2>/dev/null || true
    fi
  fi

  for env_file in "${HOME}/.cargo/env" "/root/.cargo/env"; do
    if [[ -f "$env_file" ]]; then
      # shellcheck disable=SC1090
      source "$env_file" 2>/dev/null || true
    fi
  done

  export PATH="${sudo_cargo_bin}${HOME}/.cargo/bin:/root/.cargo/bin:${PATH}"

  # Verify if cargo and rustc are functional with an active toolchain
  local rust_works=0
  if cargo --version >/dev/null 2>&1 && rustc --version >/dev/null 2>&1; then
    rust_works=1
  fi

  if [[ $rust_works -eq 0 ]]; then
    if command -v rustup >/dev/null 2>&1; then
      log_file "rustup found but default toolchain unconfigured; configuring stable..."
      run_with_spinner "Configuring Rust stable toolchain" bash -c \
        "rustup default stable 2>/dev/null || (rustup toolchain install stable --profile minimal --no-self-update && rustup default stable)" || true
      if cargo --version >/dev/null 2>&1 && rustc --version >/dev/null 2>&1; then
        rust_works=1
      fi
    fi
  fi

  if [[ $rust_works -eq 0 ]]; then
    log_file "Rust toolchain missing or unconfigured; installing via rustup minimal profile"
    run_with_spinner "Installing Rust stable toolchain" bash -c \
      "curl --proto '=https' --tlsv1.2 -fsS https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable" || die "rustup installation failed"
    # shellcheck disable=SC1090
    source "${HOME}/.cargo/env" 2>/dev/null || true
    source "/root/.cargo/env" 2>/dev/null || true
    rustup default stable >/dev/null 2>&1 || true
  fi

  cargo --version >/dev/null 2>&1 || die "cargo binary not functional (run 'rustup default stable')"
  rustc --version >/dev/null 2>&1 || die "rustc binary not functional (run 'rustup default stable')"

  local rust_ver
  rust_ver="$(rustc --version 2>/dev/null | awk '{print $2}' || echo 'stable')"
  log_file "Using rustc version: ${rust_ver}"

  local t1
  t1="$(date +%s)"
  step_done 6 "Rust toolchain (v${rust_ver})" "$(( t1 - t0 ))"
}

# Step 7: Release Compilation
compile_binary() {
  local t0
  t0="$(date +%s)"
  step_start 7 "Compiling release binary"

  local num_cpus
  num_cpus="$(nproc 2>/dev/null || echo 2)"

  # Adjust parallelism for memory safety
  local mem_kb
  mem_kb="$(grep MemTotal /proc/meminfo 2>/dev/null | awk '{print $2}' || echo 4000000)"
  local jobs="$num_cpus"
  if [[ "$mem_kb" -lt 2500000 && "$jobs" -gt 2 ]]; then
    jobs=2
    log_file "Low memory detected ($mem_kb KB); limiting cargo jobs to 2"
  fi

  # Build with native CPU architecture optimizations
  export RUSTFLAGS="-C target-cpu=native"

  local build_cmd=(cargo build --release --locked --jobs "$jobs")
  if ! run_with_spinner "Compiling stack-stack (jobs: ${jobs})" "${build_cmd[@]}"; then
    log_file "cargo build with --locked failed; attempting release build without --locked"
    if ! run_with_spinner "Compiling stack-stack (jobs: ${jobs})" cargo build --release --jobs "$jobs"; then
      step_fail 7 "Compiling release binary"
      die "cargo build failed"
    fi
  fi

  [[ -x "target/release/${BINARY_NAME}" ]] || die "build did not produce target/release/${BINARY_NAME}"

  # Restore target permissions to invoking user if invoked with sudo
  if [[ -n "${SUDO_USER:-}" && -d "target" ]]; then
    chown -R "${SUDO_USER}:" "target" 2>/dev/null || true
  fi

  local t1
  t1="$(date +%s)"
  step_done 7 "Compiling release binary" "$(( t1 - t0 ))"
}

# Step 8: Install Binary & State Hierarchy
install_files_and_directories() {
  local t0
  t0="$(date +%s)"
  step_start 8 "Binary install and state hierarchy"

  # Backup previous binary
  if [[ -x "${INSTALL_BIN}" ]]; then
    cp -f "${INSTALL_BIN}" "${PREVIOUS_BIN}"
    log_file "backed up previous binary to ${PREVIOUS_BIN}"
  fi

  # Install binary
  install -D -m 0755 "target/release/${BINARY_NAME}" "${INSTALL_BIN}"

  # State group
  local state_group="root"
  if ! getent group "${SERVICE_GROUP}" >/dev/null 2>&1; then
    groupadd --system "${SERVICE_GROUP}" 2>/dev/null || true
  fi
  if getent group "${SERVICE_GROUP}" >/dev/null 2>&1; then
    state_group="${SERVICE_GROUP}"
    local admin="${SUDO_USER:-}"
    if [[ -n "${admin}" && "${admin}" != "root" ]] && id -u "${admin}" >/dev/null 2>&1; then
      usermod -aG "${SERVICE_GROUP}" "${admin}" 2>/dev/null || true
    fi
  fi

  # Create all required filesystem directories
  install -d -m 0755 "${CONFIG_DIR}"
  install -d -m 0700 "${CONFIG_DIR}/apps"
  install -d -m 0700 "${CONFIG_DIR}/ca"
  install -d -m 0755 "${CONFIG_DIR}/net"
  install -d -m 0755 "${STATE_DIR}"
  install -d -m 0750 -o root -g "${state_group}" "${STATE_DIR}/guests"
  install -d -m 0750 -o root -g "${state_group}" "${STATE_DIR}/images"
  install -d -m 0750 -o root -g "${state_group}" "${STATE_DIR}/stacks"
  install -d -m 0750 /var/log/stack-stack
  install -d -m 0755 "${BACKUP_DIR}"

  # Hypervisor run directories
  install -d -m 0755 /run/libvirt /run/lxc /run/lxc/lock /var/cache/lxc /var/cache/lxc/download

  # Storage mount & share hierarchy
  install -d -m 0755 /mnt /srv /media /etc/exports.d /etc/samba /var/lib/samba
  install -d -m 0700 /var/lib/samba/private

  # Networking & DNS configs
  install -d -m 0755 /etc/unbound/unbound.conf.d /etc/dnsmasq.d /etc/cloudflared /etc/systemd/system/unbound.service.d

  # Sudo shim for root terminal shells
  cat <<'SHIM' > /etc/profile.d/00-stack-stack-shell.sh
if [ "$(id -u)" = "0" ]; then
  sudo() { "$@"; }
  export -f sudo
fi
SHIM
  chmod 0644 /etc/profile.d/00-stack-stack-shell.sh
  if [[ -f /root/.bashrc ]] && ! grep -q "sudo() {" /root/.bashrc; then
    printf '\nif [ "$(id -u)" = "0" ]; then sudo() { "$@"; }; export -f sudo; fi\n' >> /root/.bashrc
  fi

  # Initialize Samba passdb if missing
  if command -v smbpasswd >/dev/null 2>&1 && [[ ! -f /var/lib/samba/private/passdb.tdb ]]; then
    (printf "stackdummy\nstackdummy\n" | smbpasswd -a -s root >/dev/null 2>&1 && smbpasswd -x root >/dev/null 2>&1) || true
  fi

  # Write default config if missing
  if [[ ! -f "${CONFIG_FILE}" ]]; then
    write_if_changed "${CONFIG_FILE}" '{
  "bind": "0.0.0.0",
  "port": 8553,
  "sample_interval_ms": 1000,
  "smart_interval_s": 60
}
' 0600 || true
  fi

  local t1
  t1="$(date +%s)"
  step_done 8 "Binary install and state hierarchy" "$(( t1 - t0 ))"
}

# Step 9: DNS Resolver & Port 53 Stabilization
stabilize_dns() {
  local t0
  t0="$(date +%s)"
  step_start 9 "DNS resolvers and port 53 stabilization"

  # 1. Disable systemd-resolved stub listener if active
  if systemctl list-unit-files systemd-resolved.service >/dev/null 2>&1; then
    if systemctl is-enabled --quiet systemd-resolved 2>/dev/null || systemctl is-active --quiet systemd-resolved 2>/dev/null; then
      systemctl disable --now systemd-resolved >/dev/null 2>&1 || true
      if [[ -e /etc/resolv.conf ]] && grep -q '127\.0\.0\.53' /etc/resolv.conf 2>/dev/null; then
        rm -f /etc/resolv.conf
        write_if_changed /etc/resolv.conf "nameserver 1.1.1.1\nnameserver 1.0.0.1\n" || true
      fi
    fi
  fi

  # 2. Configure dnsmasq to read drop-in directory
  if [[ -d /etc/dnsmasq.d || -f /etc/dnsmasq.conf ]]; then
    install -d -m 0755 /etc/dnsmasq.d
    if ! grep -q '^conf-dir=/etc/dnsmasq.d' /etc/dnsmasq.conf 2>/dev/null; then
      printf '\n# stack-stack: load per-service dnsmasq fragments.\nconf-dir=/etc/dnsmasq.d/,*.conf\n' >> /etc/dnsmasq.conf
    fi
  fi

  # 3. Base inert config for dnsmasq (port=0 so unbound owns :53)
  write_if_changed /etc/dnsmasq.d/00-stack-stack-base.conf '# Managed by deploy-stack-stack.sh
# DNS is owned by unbound; dnsmasq is DHCP-only unless explicitly configured.
port=0
' >/dev/null || true

  if systemctl list-unit-files dnsmasq.service >/dev/null 2>&1; then
    systemctl enable dnsmasq >/dev/null 2>&1 || true
    systemctl restart dnsmasq >/dev/null 2>&1 || true
  fi

  # 4. Generate unbound trust anchor
  install -d -m 0755 /var/lib/unbound
  if command -v unbound-anchor >/dev/null 2>&1; then
    unbound-anchor -a /var/lib/unbound/root.key >/dev/null 2>&1 || true
    chown unbound:unbound /var/lib/unbound/root.key 2>/dev/null || true
  fi

  # 5. Drop-in restart configuration for unbound
  install -d -m 0755 /etc/systemd/system/unbound.service.d
  write_if_changed /etc/systemd/system/unbound.service.d/stack-stack.conf \
'[Unit]
After=network.target network-online.target
Wants=network-online.target
StartLimitIntervalSec=0

[Service]
Restart=on-failure
RestartSec=3
' >/dev/null || true

  systemctl daemon-reload >/dev/null 2>&1 || true

  local t1
  t1="$(date +%s)"
  step_done 9 "DNS resolvers and port 53 stabilization" "$(( t1 - t0 ))"
}

# Step 10: Security & Firewall Hardening
apply_hardening() {
  local t0
  t0="$(date +%s)"
  step_start 10 "System security and firewall hardening"

  # 1. Unattended upgrades
  write_if_changed /etc/apt/apt.conf.d/20auto-upgrades \
'APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
' >/dev/null || true

  # 2. Fail2ban configuration
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
    systemctl restart fail2ban >/dev/null 2>&1 || true
  fi

  # 3. UFW firewall rules (permit SSH, UI, and optional TLS front door)
  if command -v ufw >/dev/null 2>&1; then
    ufw --force default deny incoming >/dev/null 2>&1 || true
    ufw --force default allow outgoing >/dev/null 2>&1 || true
    ufw allow 22/tcp >/dev/null 2>&1 || true
    ufw allow 8553/tcp >/dev/null 2>&1 || true
    if grep -q '"tls_enabled"[[:space:]]*:[[:space:]]*true' "${CONFIG_FILE}" 2>/dev/null; then
      ufw allow 80/tcp >/dev/null 2>&1 || true
      ufw allow 443/tcp >/dev/null 2>&1 || true
    fi
    ufw --force enable >/dev/null 2>&1 || true
  fi

  # 4. Kernel sysctl parameters
  write_if_changed /etc/sysctl.d/99-stack-stack.conf \
'# stack-stack hardening
net.ipv4.conf.all.rp_filter = 1
net.ipv4.conf.default.rp_filter = 1
kernel.kptr_restrict = 2
kernel.dmesg_restrict = 1
kernel.yama.ptrace_scope = 1
' >/dev/null || true
  sysctl --system >/dev/null 2>&1 || true

  local t1
  t1="$(date +%s)"
  step_done 10 "System security and firewall hardening" "$(( t1 - t0 ))"
}

# Step 11: Systemd Service Unit Installation
deploy_systemd_unit() {
  local t0
  t0="$(date +%s)"
  step_start 11 "Systemd service installation & start"

  local unit_content
  unit_content="# Managed by deploy-stack-stack.sh — edits here are overwritten on next deploy.
[Unit]
Description=stack-stack homelab control plane
Documentation=https://github.com/om-itani/stack-stack
After=network-online.target docker.service libvirtd.service
Wants=network-online.target docker.service libvirtd.service
StartLimitIntervalSec=5
StartLimitBurst=5

[Service]
Type=simple
User=root
Group=root
Environment=STACK_STACK_CONFIG=/etc/stack-stack/config.json
Environment=RUST_LOG=info
ExecStart=/usr/local/bin/stack-stack
Restart=always
RestartSec=3
TimeoutStopSec=30
LimitNOFILE=65536
NoNewPrivileges=false
PrivateTmp=false
ProtectSystem=strict
ReadWritePaths=/etc/stack-stack /var/lib/stack-stack /run/docker.sock -/run/libvirt -/run/lxc -/run/qemu -/var/cache/lxc -/var/log/stack-stack /mnt /srv /media -/etc/exports -/etc/exports.d /etc/samba /var/lib/samba -/etc/unbound/unbound.conf.d -/etc/dnsmasq.d -/etc/nftables.conf -/etc/cloudflared -/etc/systemd/system/unbound.service.d
CapabilityBoundingSet=CAP_NET_BIND_SERVICE CAP_NET_ADMIN CAP_NET_RAW CAP_SYS_ADMIN CAP_DAC_OVERRIDE CAP_CHOWN CAP_FOWNER CAP_SYS_RESOURCE
AmbientCapabilities=CAP_NET_BIND_SERVICE CAP_NET_ADMIN CAP_NET_RAW

[Install]
WantedBy=multi-user.target
"

  write_if_changed "${UNIT_FILE}" "${unit_content}" 0644 || true

  systemctl daemon-reload
  systemctl enable "${SERVICE_NAME}" >/dev/null 2>&1 || true

  # Optional verify check
  if command -v systemd-analyze >/dev/null 2>&1; then
    local verify
    verify="$(systemd-analyze verify "${UNIT_FILE}" 2>&1 || true)"
    if [[ -n "$verify" ]]; then
      log_file "systemd-analyze verify notes: ${verify}"
    fi
  fi

  # Restart service
  if systemctl is-active --quiet "${SERVICE_NAME}"; then
    systemctl restart "${SERVICE_NAME}"
  else
    systemctl start "${SERVICE_NAME}"
  fi

  local t1
  t1="$(date +%s)"
  step_done 11 "Systemd service installation & start" "$(( t1 - t0 ))"
}

# Step 12: Health Probes & Verification
verify_deployment() {
  local t0
  t0="$(date +%s)"
  step_start 12 "Health check & verification"

  # 1. Fast check if service crashed immediately
  if ! systemctl is-active --quiet "${SERVICE_NAME}"; then
    step_fail 12 "Health check"
    journalctl -u "${SERVICE_NAME}" --no-pager -n 40 >> "${DEPLOY_LOG}" 2>&1 || true
    die "${SERVICE_NAME} failed to start. Check ${DEPLOY_LOG} for details."
  fi

  # 2. Health poll with timeout
  local healthy=0
  for attempt in $(seq 1 45); do
    if curl -fsS --max-time 2 "${HEALTH_URL}" >/dev/null 2>&1; then
      healthy=1
      break
    fi
    sleep 1
  done

  if [[ $healthy -ne 1 ]]; then
    step_fail 12 "Health check"
    log_file "Health check failed after 45s. Initiating rollback..."
    if [[ -x "${PREVIOUS_BIN}" ]]; then
      cp -f "${PREVIOUS_BIN}" "${INSTALL_BIN}"
      systemctl restart "${SERVICE_NAME}" 2>/dev/null || true
      die "Health check failed; restored previous binary from ${PREVIOUS_BIN}"
    else
      die "Health check timed out and no previous binary exists for rollback"
    fi
  fi

  # 3. Post-deploy API endpoint verification
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

  local passed=0
  for ep in "${endpoints[@]}"; do
    if curl -fsS --max-time 3 "http://127.0.0.1:8553${ep}" >/dev/null 2>&1; then
      ((passed++))
      log_file "  ✓ endpoint ready: ${ep}"
    else
      log_file "  - endpoint requires setup or auth: ${ep}"
    fi
  done

  local t1
  t1="$(date +%s)"
  step_done 12 "Health check & verification (${passed}/${#endpoints[@]} endpoints ready)" "$(( t1 - t0 ))"
}

# ---------------------------------------------------------------------------
# Final Summary Card
# ---------------------------------------------------------------------------
show_summary() {
  local end_time
  end_time="$(date +%s)"
  local total_duration=$(( end_time - SCRIPT_START_TIME ))

  local primary_ip
  primary_ip="$(hostname -I 2>/dev/null | awk '{print $1}' || echo "127.0.0.1")"
  [[ -z "$primary_ip" ]] && primary_ip="127.0.0.1"

  printf "\n"
  printf "  ${C_GREEN}╭────────────────────────────────────────────────────────────────────────╮${C_RESET}\n"
  printf "  ${C_GREEN}│${C_RESET}                   ${C_BOLD}${C_GREEN}✔ stack-stack deployment successful!${C_RESET}                 ${C_GREEN}│${C_RESET}\n"
  printf "  ${C_GREEN}├────────────────────────────────────────────────────────────────────────┤${C_RESET}\n"
  printf "  ${C_GREEN}│${C_RESET}  ${C_WHITE}Control Plane URL${C_RESET} : ${C_CYAN}http://%s:8553${C_RESET}%*s${C_GREEN}│${C_RESET}\n" \
    "$primary_ip" $(( 46 - ${#primary_ip} )) ""
  printf "  ${C_GREEN}│${C_RESET}  ${C_WHITE}Localhost URL${C_RESET}     : ${C_CYAN}http://127.0.0.1:8553${C_RESET}                            ${C_GREEN}│${C_RESET}\n"
  printf "  ${C_GREEN}│${C_RESET}  ${C_WHITE}Service Status${C_RESET}    : ${C_GREEN}active (running)${C_RESET}                                 ${C_GREEN}│${C_RESET}\n"
  printf "  ${C_GREEN}│${C_RESET}  ${C_WHITE}Binary Installed${C_RESET}  : %-49s${C_GREEN}│${C_RESET}\n" "$INSTALL_BIN"
  printf "  ${C_GREEN}│${C_RESET}  ${C_WHITE}Config File${C_RESET}       : %-49s${C_GREEN}│${C_RESET}\n" "$CONFIG_FILE"
  printf "  ${C_GREEN}│${C_RESET}  ${C_WHITE}Total Deploy Time${C_RESET} : %-49s${C_GREEN}│${C_RESET}\n" "${total_duration}s"
  printf "  ${C_GREEN}│${C_RESET}  ${C_WHITE}Detailed Log${C_RESET}      : %-49s${C_GREEN}│${C_RESET}\n" "$DEPLOY_LOG"
  printf "  ${C_GREEN}├────────────────────────────────────────────────────────────────────────┤${C_RESET}\n"
  printf "  ${C_GREEN}│${C_RESET}  ${C_BOLD}Commands for Management:${C_RESET}                                             ${C_GREEN}│${C_RESET}\n"
  printf "  ${C_GREEN}│${C_RESET}    • View status : ${C_YELLOW}systemctl status ${SERVICE_NAME}${C_RESET}%*s${C_GREEN}│${C_RESET}\n" \
    $(( 38 - ${#SERVICE_NAME} )) ""
  printf "  ${C_GREEN}│${C_RESET}    • Stream logs : ${C_YELLOW}journalctl -u ${SERVICE_NAME} -f${C_RESET}%*s${C_GREEN}│${C_RESET}\n" \
    $(( 39 - ${#SERVICE_NAME} )) ""
  printf "  ${C_GREEN}│${C_RESET}    • Fast update : ${C_YELLOW}git pull && ./deploy-stack-stack.sh${C_RESET}                 ${C_GREEN}│${C_RESET}\n"
  printf "  ${C_GREEN}╰────────────────────────────────────────────────────────────────────────╯${C_RESET}\n\n"
}

# ---------------------------------------------------------------------------
# Main Entry Point
# ---------------------------------------------------------------------------
main() {
  ensure_root "$@"

  print_banner

  setup_environment
  run_preflight_checks
  install_system_packages
  setup_cloudflared
  setup_hypervisors
  setup_rust_toolchain
  compile_binary
  install_files_and_directories
  stabilize_dns
  apply_hardening
  deploy_systemd_unit
  verify_deployment

  show_summary
}

main "$@"
