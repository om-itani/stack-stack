# stack-stack

A single-binary control plane for one Debian box. It shows you the machine's
health, runs your containers and compose stacks, gives you a full router and
firewall, runs VMs and LXC containers, and puts files, shares, backups, VPN and
TLS in front of them. One login. One process. No database, no web server, no
container to run the tool itself.

The whole UI is compiled into the binary, so there is nothing to install on the
client side and nothing to keep running alongside it.

## What it does

### Dashboard

Live CPU (total, per-core, and a user/sys/io/idle split), memory with a full
breakdown (cached, buffers, available, swap), filesystems, per-interface
network throughput with history graphs, and a SMART summary. Data arrives over
a WebSocket at one sample per second; if the socket drops, the page falls back
to polling and says so in the header.

### Containers

Every container on the host, running or stopped, with its state, published
ports and ready-to-click web URLs. From here you can start, stop, restart and
remove a container, follow its logs live, open an interactive shell inside it,
or open a live stats panel showing that container's CPU, memory and network I/O
as Docker reports them. Containers are grouped by their compose project, so an
app or a stack shows up as one unit.

### App Store

33 ready-to-run applications across media, downloads, productivity, monitoring,
home automation, networking, development, files, security and utilities. Each
entry is a compose template pinned to a specific image tag. Install with one
click, remove with one click.

There is also a Docker Hub search, so anything not in the catalog can be found
and deployed: pick a result, and it opens the stack editor with the image
already filled in. Search results are cached for five minutes and the client is
rate-limit aware.

### Updates

Each stack can opt into checking for a newer image, on a daily or weekly cadence
(off by default). A check pulls the image and compares the digest; if it moved,
the stack is redeployed and the previous digest is kept. A stack that has a
previous digest shows a Rollback button, which pins that digest into the compose
file and brings it back up. Rollback refuses an image that is set from a
`${VARIABLE}`, because the real name is only known when compose runs.

Updates, rollbacks and failed checks are written to the audit log, and the
scheduler raises a notification on both outcomes.

Only stacks are updated automatically. A standalone container can be checked,
but not recreated unattended: reproducing its run arguments by guesswork is how a
rollback turns into an outage.

### Stacks

Paste any docker-compose document, give it a name, and deploy it. Each stack
gets its own directory and is brought up under an explicit compose project
name. The stack list shows the live state of the containers in each stack, and
you can edit, redeploy, remove, or force-remove it.

Files that stack-stack generates include sensible defaults (no-new-privileges
and bounded container logs). A compose file you paste is used exactly as you
wrote it.

### Files

A file manager over roots you configure: browse, read, edit and save, create
directories and files, rename, change permissions and ownership, upload,
download, archive and extract. Paths are collapsed and resolved before use, and
a symlink that escapes a configured root is refused. Archive extraction is
filtered so it cannot write outside the destination.

### Storage

Every physical disk and partition on the box, read from `lsblk`: its size,
model, filesystem, label and mount point, with the disk that backs the root
filesystem marked as untouchable. An unmounted disk or partition can be
formatted (ext4 or xfs) and mounted at a path under `/mnt`, `/srv` or `/media`,
with the option to add it to `/etc/fstab` by UUID so it returns after a reboot.

Formatting is destructive and treated as such: the confirmation dialog names the
device and its size, requires the device path to be typed out, and the backend
re-checks that the device is not mounted, does not back the root filesystem, and
is a disk or partition before it runs `mkfs`. The action is written to the audit
log.

### Shares

Samba shares and users, and NFS exports. Both are validated before they are
applied (`testparm` and `exportfs`), and each apply reports what it did step by
step. Samba passwords are set through `smbpasswd` and only ever touch stdin.

### Backups

restic jobs with schedules, retention and local, S3 or SFTP backends. You can
run a job, verify a repository, browse snapshots, restore a path or download a
single file. Repository passwords live in a separate 0600 file and reach restic
through the environment, not the command line. Jobs are serialised so two runs
cannot overlap, and a failed run raises a notification.

### Networking

The router side of the box:

- Interfaces, VLANs and bridges, plus Wi-Fi.
- An nftables firewall with a default policy, rules and port forwards. Applying
  a firewall change is two-phase: you apply, then confirm. If you do not
  confirm, the previous rules are restored, so a bad rule cannot lock you out.
- DHCP through dnsmasq, with static reservations.
- DNS through unbound, with hostname rewrites and blocked domains. When unbound
  is enabled, dnsmasq is reduced to DHCP only so the two do not fight over
  port 53.
- Adblocking without a second process. Blocklist subscriptions (a hosts file or a
  plain domain list) are fetched with `curl`, compiled into one unbound
  `local-zone` include, and applied with a reload. Five lists are offered
  disabled by default, a daily refresh runs after 03:00 local, and there is an
  allowlist that also covers subdomains, per-list counts and errors, a manual
  "update now", and a box to check whether a domain is blocked.
- Optional Cloudflare tunnel.
- SQM traffic shaping.
- Dynamic DNS updates through Cloudflare.
- WireGuard VPN with peer management and one-time client exports that include a
  QR code when `qrencode` is installed.
- A reverse proxy that maps hostnames to a container port or a URL, with
  certificates issued from an internal CA or self-signed.

### Virtualization

QEMU/KVM virtual machines with cloud-init (user, password, SSH keys, static or
DHCP networking), BIOS or UEFI firmware, TPM, hugepages, CPU topology, discard
and multiqueue, the guest agent, VNC and a serial console. Lifecycle: create,
start, stop, graceful shutdown, reboot, force destroy, resize the disk, take
and apply qcow2 snapshots, and delete. The serial console is a WebSocket in the
browser.

LXC containers: create, start, stop, kill, delete, set the root password, and
open a console.

The guest catalog has 46 entries: 25 VM images and 21 LXC templates, each with a
pinned URL and a download cache. Images that cannot be pinned, like rolling
releases, are marked unverified in the UI instead of pretending otherwise.

### Terminal and logs

A real PTY host shell behind the login gate, using your login shell. Container
exec terminals use the same component. Logs tail and follow journald with unit
and severity filters.

### Notifications

A notification feed with a live stream and an optional webhook. Firewall
rollbacks, backup failures, SMART results, disk thresholds and DDNS changes all
report through it.

### Security

- One admin account. The password is hashed with `openssl passwd -6`
  (sha512-crypt), and sessions are 256-bit tokens held in memory with a
  seven-day lifetime. Nothing is written to disk, so a restart signs everyone
  out. If the credential file becomes unreadable the box stays locked rather
  than reopening the setup screen.
- Every `/api` route except health and the setup/login endpoints requires a
  session.
- An optional TLS front door on `:443` with an internal CA and per-hostname
  certificates that renew automatically. `:80` redirects. SNI decides what is
  served: your admin hostname gets the UI, every other hostname is proxied to
  its backend with WebSocket upgrades intact.
- The service runs under systemd hardening (strict filesystem protection, a
  capability bounding set, a private `/tmp`).
- Destructive operations are appended to an audit log at
  `/var/log/stack-stack/audit.log`.

## Why it is built this way

The goal is a tool that is small enough to understand and quick enough that you
never wait for it.

- **One binary, no runtime.** There is no Python, Node or Go service to keep
  alive, no web server in front of it, and no database. Configuration is JSON
  files; state is the filesystem and the services it manages.
- **The frontend ships inside the binary.** The UI is embedded at build time, so
  serving it needs no extra process and no network fetch on the host.
- **It drives the tools Debian already has.** Docker through its socket, and
  `nft`, `ip`, `qemu-system-x86_64`, `lxc`, `openssl`, `smbd`, `exportfs`,
  `restic`, `wg`, `smartctl`, `journalctl` and `curl` as subprocesses. Each of
  those sits behind one seam, so when a tool is missing the feature reports
  itself as unavailable while everything else keeps working.
- **Almost no dependencies.** Fewer than twenty crates, most of them small.
  `bollard` is built without its HTTP/TLS surface because only the local Docker
  socket is used. There is a single crypto backend (`ring`), confined to the TLS
  module. No ORM, no templating engine, no HTTP client library.
- **Fast where it counts.** One WebSocket carries all telemetry instead of the
  page polling several endpoints. The container list is cached for two seconds
  and the cache is dropped the moment anything changes. Docker Hub responses are
  cached for five minutes with rate-limit backoff. The initial JavaScript bundle
  is about 145 KB (40 KB gzipped), and xterm, which is the largest dependency, is
  loaded only when you actually open a terminal. The release profile uses fat
  LTO, one codegen unit, `panic = abort` and stripped symbols.
- **It degrades instead of dying.** A missing tool means one unavailable
  feature, a malformed config file means defaults, and a missing Docker socket
  means an empty container list rather than a broken page. The box always comes
  up.

## Requirements

- Debian 13 (trixie) or Ubuntu, x86_64, with systemd.
- Root, or sudo.
- 5 GB free disk for the build. 2 GB RAM is enough to build; guests need more.
- Network access during the deploy (apt and crates.io).

## Install

```bash
git clone https://github.com/om-itani/stack-stack
cd stack-stack
sudo ./deploy-stack-stack.sh
```

Then open `http://<host-ip>:8553` and set the admin password on the first-run
screen. The minimum is 12 characters.

The script is safe to re-run; that is also how you update.

### What the script does

1. Checks the platform: x86_64, at least 5 GB free, `apt-get` present, and a
   reachable network.
2. Installs the tools the features use: Docker and Compose, QEMU/KVM and OVMF,
   libvirt, LXC, dnsmasq and unbound, nftables, Samba, NFS, restic, WireGuard,
   smartmontools, `curl`, `openssl`, `qrencode` and the archive tools.
3. Hardens the system: enables unattended security upgrades, installs fail2ban
   configured for sshd through the systemd journal, sets ufw to deny inbound by
   default while allowing SSH, the UI port, and 80/443 when TLS is on, and
   writes a sysctl drop-in (reverse-path filtering, hidden kernel pointers,
   restricted dmesg, restricted ptrace).
4. Installs Rust through rustup if it is not already present.
5. Builds the release binary.
6. Installs it to `/usr/local/bin/stack-stack`, keeping the previous binary so a
   bad build can be rolled back.
7. Creates `/etc/stack-stack`, `/var/lib/stack-stack/{guests,images,stacks}`,
   `/var/log/stack-stack`, and the runtime directories LXC and libvirt need.
8. Writes a default config if none exists, then installs the systemd unit,
   reloads systemd and verifies the unit.
9. Starts the service and polls `/api/health` for up to 60 seconds. If it does
   not come up, the script prints the service journal and restores the previous
   binary.

To check it came up:

```bash
systemctl status stack-stack          # active (running)
curl -s http://127.0.0.1:8553/api/health
journalctl -u stack-stack -n 50       # if it did not start
```

The script logs what it did to `/var/log/stack-stack-deploy.log`.

One thing to check after the first deploy: the firewall allows SSH on port 22.
If your SSH runs on a different port, allow it before you rely on ufw.

### Enabling HTTPS

The plain `:8553` listener is HTTP. A TLS front door on `:443` is optional and
off by default, so the box does not take over an existing web server. Turn it on
from the Settings page: it runs a port check first, then creates a CA and a
certificate for your admin hostname. Point `stack-stack.lan`, or whatever name
you choose, at the box in DNS or `/etc/hosts` afterwards.

## Where things live

| Path | Contents |
| --- | --- |
| `/usr/local/bin/stack-stack` | The binary. |
| `/etc/stack-stack/` | Configuration documents (network, virtualization, shares, backups, proxy, and so on). |
| `/etc/stack-stack/config.json` | Runtime settings. Passed to the service by the unit. |
| `/etc/stack-stack/*.secrets.json` | Credentials. Mode 0600. |
| `/var/lib/stack-stack/guests/` | VM disks, cloud-init seeds and QMP sockets. |
| `/var/lib/stack-stack/images/` | Downloaded cloud images and appliance ISOs. |
| `/var/lib/stack-stack/stacks/` | Compose files for stacks, one directory each. |
| `/var/log/stack-stack/audit.log` | Audit trail. JSON, one entry per line. |

## Configuration

Settings resolve in this order: built-in defaults, then `config.json`, then
environment overrides. The overrides are `STACK_STACK_CONFIG` (path to the
config file), `STACK_STACK_BIND`, `STACK_STACK_PORT` and
`STACK_STACK_SAMPLE_MS`.

An unreadable or malformed config file is not fatal. The service logs a warning
and boots with defaults.

## Updating

```bash
cd stack-stack
git pull
sudo ./deploy-stack-stack.sh
```

The script rebuilds, replaces the binary, and rolls back to the previous one if
the health check fails.

## Uninstall

```bash
sudo systemctl disable --now stack-stack
sudo rm /etc/systemd/system/stack-stack.service
sudo rm /usr/local/bin/stack-stack
sudo systemctl daemon-reload
```

`/etc/stack-stack` and `/var/lib/stack-stack` are left in place. Remove them
yourself if you want the configuration and guest data gone too.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test

cd frontend && npm ci && npm run build   # output is embedded by build.rs
```

The Rust side builds and tests on any platform; every external tool sits behind
one seam and reports itself unavailable when it is not there.

## Roadmap

Things that are not in yet, roughly in the order they are likely to arrive:

- Per-container resource usage. The container view shows state, ports and
  actions, but not live CPU, memory or network per container.
- Container image management beyond the catalog: list, pull and delete
  arbitrary images and tags.
- Scheduled and automatic updates, for example redeploying stacks or pulling new
  image tags on a schedule. Today only backups and SMART tests have schedules.
- Multiple WireGuard interfaces. The VPN is one interface by design right now.
- Multiple admin accounts and roles. There is a single admin today.
- More catalog entries with pinned checksums, and per-app health checks so an
  install can be verified rather than assumed.
- Support beyond x86_64. The deploy script currently refuses to run on anything
  else.
- Storage pooling. Drives are formatted and mounted one at a time; there is no
  mergerfs-style pool yet.
- Scheduled tasks and alerting rules. The schedules that exist today are
  backups, SMART self-tests, DDNS, the blocklist refresh and stack image checks.
- Auto-update for standalone containers (recreating one on a new image), and
  automatic redeploy for app-store apps.
- Query statistics per client. The adblock page reports what is compiled and
  what a lookup would do; it does not yet parse the unbound query log to count
  what was actually blocked.

## Deliberate limits

Some things are left out on purpose rather than pending:

- Only the local Docker socket. There is no remote or TLS Docker transport in
  this build.
- One WireGuard interface.
- One admin account.
- x86_64 only.
- Rolling-release images cannot be checksum pinned, so they are labelled
  unverified rather than silently trusted.
- A compose file you paste is deployed as written. stack-stack does not rewrite
  your services or inject limits into them.

## License

MIT.
