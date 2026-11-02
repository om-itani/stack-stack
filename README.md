# stack-stack

A lightweight, single-binary homelab control plane and edge router for Linux. It combines host monitoring, Docker container and compose management, QEMU/KVM virtual machines, LXC containers, network routing (nftables, DNS, DHCP, WireGuard), storage management, Samba/NFS shares, and encrypted backups into a unified web interface with a sub-50MB RAM footprint.

No container required to manage containers. No external database. No runtime dependencies.

---

## Overview

Unlike traditional homelab dashboards or hypervisors that require hundreds of megabytes or gigabytes of memory just to run background daemons, stack-stack compiles the backend service and embedded web frontend into a single statically optimized binary written in Rust. It interacts directly with standard Linux interfaces (`/proc`, `nftables`, systemd, Docker socket, KVM/QEMU, and storage subsystems).

- **Ultra-lightweight**: Typically idles at ~42 MB resident RAM (0.1% of a standard host).
- **Single binary**: The frontend is compiled into the binary—no Node.js, Python, or external web server is required on the host.
- **Self-contained edge router**: Native nftables firewall with two-phase rollback, unbound DNS resolver with built-in adblocking, dnsmasq DHCP, and WireGuard VPN.
- **Fail-safe architecture**: Individual subsystem degradation rather than crashes. If a tool like Docker or WireGuard is absent or stopped, that module reports unavailable while the rest of the control plane operates uninterrupted.

---

## Getting Started

### Prerequisites

- **Operating System**: Debian 12 / 13 (Bookworm / Trixie) or Ubuntu 22.04+ (x86_64)
- **Permissions**: Root or user with `sudo` privileges
- **Network**: Internet access during deployment to install packages and compile the binary

### Installation

1. Ensure `git` and `curl` are installed on your host:
   ```bash
   sudo apt-get update && sudo apt-get install -y git curl
   ```

2. Clone the repository:
   ```bash
   git clone https://github.com/om-itani/stack-stack.git
   cd stack-stack
   ```

3. Run the automated deployment script:
   ```bash
   chmod +x deploy-stack-stack.sh
   sudo ./deploy-stack-stack.sh
   ```

4. Once the deployment finishes, open your browser and navigate to:
   ```text
   http://<host-ip>:8553
   ```
   On first launch, you will be prompted to set the administrative password (minimum 12 characters).

### Updating

To upgrade to the latest version, pull the repository changes and re-run the deployment script:
```bash
git pull
sudo ./deploy-stack-stack.sh
```
The script performs health validation before activating the new build. If a build or start fails, it automatically reverts to the previous binary backup.

---

## Key Features

### System & Telemetry
- **Real-Time Telemetry**: 1 Hz live metrics stream over WebSocket covering per-core CPU usage, detailed memory allocation (active, cached, buffers, available, swap), disk I/O, network bandwidth, and SMART drive status.
- **24-Hour Metric History**: Rolling in-memory ring buffer recording minute-resolution snapshots for 1h, 6h, and 24h historical charts.
- **Background Suspension**: When the browser tab is minimized or out of focus, telemetry and polling pause automatically to conserve client and server resources.

### Containers & Stacks
- **Full Docker Fleet Management**: Start, stop, restart, delete, inspect, view live container logs, and open interactive terminal sessions inside any running container.
- **Docker Compose Stacks**: Deploy, edit, and manage multi-container applications with isolated workspace directories and automated rollback safety.
- **Integrated App Store**: Catalog of 33 curated homelab applications deployable in one click, plus Docker Hub search for discovering and drafting compose files for external images.
- **Scheduled Image Updates**: Opt-in digest checking and automated redeployments with single-click digest rollback.

### Virtualization (VMs & LXC)
- **QEMU/KVM Virtual Machines**: Cloud-init provisioning (SSH keys, credentials, static/DHCP IP), UEFI/BIOS firmware selection, disk resizing, snapshot management, VNC display, and serial console over WebSocket.
- **LXC System Containers**: Fast, lightweight Linux containers with direct console access and root credential configuration.
- **Image Catalog**: Built-in repository of verified cloud images and distribution appliances with SHA-256 verification and custom image upload/download tools.

### Network & Security
- **Firewall & Port Forwarding**: nftables management with two-phase commit: changes apply with a configurable timeout and automatically roll back unless confirmed by the operator.
- **DNS Resolver**: Recursive unbound resolver with DNSSEC root trust anchoring, local domain overrides, and integrated domain blocklists (adblocking) compiled directly into resolver rules.
- **DHCP Server**: dnsmasq integration for LAN IP lease allocation, static IP reservations, and router gateway assignment.
- **WireGuard VPN**: Single-click peer onboarding, QR code generation for mobile clients, and cryptographic key generation.
- **Reverse Proxy & TLS**: Internal Certificate Authority (CA) with automatic TLS certificate generation for hostnames and services on port 443.

### Storage & Files
- **Drive & Mount Management**: Hardware disk discovery via `lsblk`, partition creation, filesystem formatting (ext4, xfs), and persistent UUID mounting in `/etc/fstab`.
- **File Explorer**: Fast file manager with breadcrumb omnibar, file upload/download, archive compression/extraction (zip, tar, gz), ownership and permission editors (`chmod`/`chown`).
- **In-Browser Code & Config Editor**: Built-in text editor with line numbers, syntax indentation, and `Ctrl+S` saving.
- **Network Shares**: Samba (SMB) and NFS server export configuration with permission validation.
- **Encrypted Backups**: restic backup integration supporting local storage, S3 object storage, and SFTP endpoints with automated retention rules.

### Automation, Alerts & Diagnostics
- **Scheduled Cron Tasks**: Visual cron task manager for running periodic shell commands with status tracking and execution logs.
- **Alerting Engine**: Configurable threshold rules (CPU, memory, disk, container health) delivering alerts via Discord webhooks, Slack, generic JSON endpoints, or email.
- **Host Terminal**: Full interactive root PTY session in the browser with terminal emulation (xterm.js).
- **Audit Logging**: Structured JSON logging of every administrative action in `/var/log/stack-stack/audit.log`.

---

## File Hierarchy

| Path | Purpose |
| :--- | :--- |
| `/usr/local/bin/stack-stack` | Compiled executable binary. |
| `/etc/stack-stack/` | JSON configuration state (network, tasks, alerts, proxy, shares). |
| `/etc/stack-stack/config.json` | Main daemon runtime settings. |
| `/etc/stack-stack/*.secrets.json` | Encrypted credential stores (mode `0600`). |
| `/var/lib/stack-stack/stacks/` | Docker Compose project files and environment directories. |
| `/var/lib/stack-stack/guests/` | Virtual machine disks, cloud-init seeds, and QMP sockets. |
| `/var/lib/stack-stack/images/` | Downloaded VM cloud images and ISO installers. |
| `/var/log/stack-stack/audit.log` | Administrative audit trail. |

---

## Management Commands

The service runs under systemd as `stack-stack.service`:

```bash
# Check service status
sudo systemctl status stack-stack

# View live system logs
sudo journalctl -u stack-stack -f

# Restart the service
sudo systemctl restart stack-stack

# Stop the service
sudo systemctl stop stack-stack
```

---

## Roadmap

Planned features and architectural enhancements:

- **Storage Pooling**: MergerFS and Btrfs/ZFS pool orchestration for multi-drive media arrays.
- **OCI Image & Registry Manager**: Direct local Docker image inspection, dangling layer cleanup, and private container registry authentication.
- **Multi-Interface WireGuard & Site-to-Site**: Support for secondary WireGuard tunnels, peer-to-peer mesh routing, and split-tunnel presets.
- **Fine-Grained Role-Based Access Control (RBAC)**: Support for read-only user accounts, scoped access per container or stack, and OIDC / OAuth2 sign-in.
- **Prometheus Metrics Exporter**: Native `/metrics` endpoint exposing host and container telemetry for external Prometheus or VictoriaMetrics instances.
- **Hardware Transcoding & GPU Passthrough**: One-click GPU capability detection (Intel QuickSync, NVIDIA NVENC) and pass-through configuration for containers and VMs.

---

## License

MIT License. See [LICENSE](LICENSE) for details.
