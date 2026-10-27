# stack-stack

A single-binary control plane for one Debian box. It shows you the machine's
health, runs your containers and compose stacks, gives you a full router and
firewall, runs VMs and LXC containers, and puts files, shares, backups, VPN and
TLS in front of them. One login. One process. No database, no web server, no
container to run the tool itself.

The whole UI is compiled into the binary, so there is nothing to install on the
client side and nothing to keep running alongside it.

## What it does

The UI is a collapsible sidebar grouped into Overview, Compute, Storage, Network
and System, with a header that carries the hostname, uptime, load, a live
connection indicator, the notification bell, the background-activity drawer and
the sign-out control.

### System (dashboard)

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
click, remove with one click; an install runs as a tracked background job so its
progress is visible while it pulls.

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

Checks, redeploys and rollbacks run as tracked jobs; the scheduled auto-update
reapplies a stack only once its digest actually moved, records the audit entry
only when something changed, and keeps stacks serial so two redeploys never
overlap. Failures raise a notification.

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

### Background jobs and the activity drawer

Anything slow runs through one job runner instead of blocking a request. A job
has a name, a status (pending, running, success, failed), a percentage and a log
tail. The header activity drawer lists running jobs with live progress bars and
keeps the last 24 hours of finished ones, and pushes updates over a WebSocket
(with a slow poll as a fallback). A job that finishes raises a toast, or a red
one if it failed.

The operations that use it today are app install, stack deploy, guest image
download, and update check, redeploy and rollback, plus the scheduled
auto-update. The API for the future features is the same call: register a job,
report progress, and it appears in the drawer with no new UI.

### Alerts, notifications and health checks

A notification feed with a live stream, driven from two places:

- **The feed** (Settings → notifications): firewall rollbacks, backup failures,
  SMART pre-failures, service-down, expiring certificates and DDNS changes each
  have a toggle, and the feed can deliver to a webhook.
- **The alerting engine** (Alerts page): operator-defined rules evaluated every
  30 seconds against live telemetry. A rule reads as "if [metric] is
  [> / < / == / !=] [threshold] for [duration], send to [provider]". Metrics are
  `cpu_usage`, `memory_usage`, `load1`, `disk_usage` (or `disk_usage:/mount`) and
  `container_status:<name>`. A rule must hold for its duration before it fires,
  and then stays quiet for its cooldown (one hour by default), so a flapping
  metric does not spam you.

Providers are configured on the same page and can be tested from it: Discord and
Slack webhooks, a generic JSON webhook, and email through `sendmail`. Delivery
goes through the shared `curl`/command seam, and the webhook URL and body are
sent in a `curl -K` config on stdin so a webhook secret never appears in the
process argument list. Alert rules and `alerts.json` are validated before they
are saved, and the file is written mode 0600.

**Container health checks** are on the same page: an optional HTTP GET or TCP
connect per stack. A 30-second evaluator probes each check; after the retry
count is reached (three by default) it restarts the stack through the job runner
and raises a notification, then honours a per-stack cooldown so a genuinely
broken stack is not restarted in a loop. `health.json` records the last restart
so the cooldown survives a reload.

### Files

A file manager over roots you configure: browse, read, edit and save, create
directories and files, rename, change permissions and ownership, upload,
download, archive and extract. Paths are collapsed and resolved before use, and
a symlink that escapes a configured root is refused. Archive extraction is
filtered so it cannot write outside the destination.

The listing switches between a list and a grid. Right-clicking an entry (or its
`···` button) opens a menu with open, download, preview, edit, extract, rename,
copy path, permissions, owner and delete. Selecting a text file loads it into a
preview pane, and an image shows inline; uploads report real byte progress rather
than a spinner.

### Storage

Every physical disk and partition on the box, read from `lsblk`: its size,
model, filesystem, label and mount point, with the disk that backs the root
filesystem marked as untouchable. An unmounted disk or partition can be
formatted (ext4 or xfs) and mounted at a path under `/mnt`, `/srv` or `/media`,
with the option to add it to `/etc/fstab` by UUID so it returns after a reboot.
Unmounting removes the `/etc/fstab` entry stack-stack added, so a persisted mount
does not silently come back after an unmount.

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

The router side of the box, on one page with tabs:

- **Interfaces** — physical interfaces, VLANs, bridges, static or DHCP
  addressing, and Wi-Fi.
- **Firewall** — an nftables ruleset with a default policy, rules and port
  forwards. Applying a firewall change is two-phase: you apply, then confirm. If
  you do not confirm within the window, the previous rules are restored, so a
  bad rule cannot lock you out.
- **DHCP** — dnsmasq, with static reservations.
- **DNS** — unbound, with hostname rewrites and blocked domains. When unbound is
  enabled, dnsmasq is reduced to DHCP only (`port=0`) so the two can never fight
  over port 53.
- **Adblock** — blocklists without a second process. Subscriptions (a hosts file
  or a plain domain list) are fetched with `curl`, compiled into one unbound
  `local-zone` include, and applied. Five lists are offered disabled by default,
  a daily refresh runs after 03:00 local, and there is an allowlist that also
  covers subdomains, per-list counts and errors, a manual "update now", and a box
  to check whether a domain is blocked. After an update the resolver is
  re-rendered and restarted, so the very first list added after DNS was
  configured takes effect immediately.
- **SQM** — traffic shaping.
- **Tunnel** — an optional Cloudflare tunnel.
- **Certificates** — view installed certificates, generate a self-signed one, or
  import a PEM pair.
- **Proxy** — the front door: map hostnames to a container port or a URL, with
  certificates issued from an internal CA or self-signed.
- **Preview** — render the `nftables`, `dnsmasq`, `unbound`, `cloudflared` and
  SQM files before applying them.
- **Status** — the daemon list (unbound, dnsmasq, cloudflared, …) with state, and
  a plain-language note for the states that look alarming but are usually fine.

Dynamic DNS updates through Cloudflare live on the Settings page.

### Virtualization

QEMU/KVM virtual machines with cloud-init (user, password, SSH keys, static or
DHCP networking), BIOS or UEFI firmware, TPM, hugepages, CPU topology, discard
and multiqueue, the guest agent, VNC and a serial console. Lifecycle: create,
start, stop, graceful shutdown, reboot, force destroy, resize the disk, take
and apply qcow2 snapshots, and delete. The serial console is a WebSocket in the
browser.

LXC containers: create, start, stop, kill, delete, set the root password, and
open a console.

The guest catalog has 71 entries: 42 VM images and 29 LXC templates, each with a
URL and a download cache. An entry is only shown as verified when it carries a
pinned SHA-256; anything that cannot be pinned, like a rolling release, is
labelled `[unverified]` instead of pretending otherwise.

An **Images** tab covers anything outside the catalog. An image can be
downloaded from a URL or imported from a path already on this host; both are
validated with `qemu-img info` before they are offered, the transfer lands in the
images directory through a temporary file so a half-download is never visible,
and the tab lists the cache with sizes and dates so it can be pruned. URLs must
be `http(s)`, and names must be a plain `name.ext` with an image extension, so
neither can reach outside the images directory.

### Scheduled tasks

A Tasks page under System runs a shell command on a cron-like schedule. A task
needs a name, a command, and either a preset (hourly, daily at 03:00, weekly) or
a five-field cron expression. Supported syntax is deliberately small: `*`, `a`,
`a-b`, `*/n`, `a-b/n` and comma lists, per field. Commands run through `sh -c` as
the service user, so the page carries a root warning, every run is written to the
audit log, and a failure raises a notification. The last run's status and the tail
of its output are kept for the UI, and "run now" runs it immediately.

### Terminal and logs

A real PTY host shell behind the login gate, using your login shell. Container
exec terminals use the same component. Logs tail and follow journald with unit
and severity filters.

### Developer tools

A Developer page under System exists so a failure can be diagnosed without an
SSH session:

- **Ping all endpoints** — re-enters the live router server-side and reports the
  status and latency of every read route as a table, so a broken handler shows up
  as a red row instead of a guess.
- **Raw config** — pick any configuration document (`network`, `tasks`, `alerts`,
  `health`, `notify`, `files`, `proxy`, `updates`, `virtualization`, `samba`,
  `nfs`, `backup`, `ddns`, `wireguard`, `smart`, `guest_catalog`) and see the
  *effective* JSON: the service's own defaults merged with what is on disk, with a
  note saying whether it came from the file or from defaults. Secret documents
  (`auth.json`, `*.secrets.json`) are never exposed.
- **System log** — the last 50 lines of the stack-stack service's own journal, so
  a crash can be read from the browser. ANSI colour codes are stripped.
- **Force frontend error / force API error** — exercise the error pipeline on
  demand and confirm the toast and console logging work.

### Errors and observability

Every API error is one shape: `{ "error": <machine code>, "message": <human
text>, "details": <optional context> }`, where the code is a stable name like
`NotFound` or `Unavailable`. The frontend logs every failure to the console
under `[API Error]` with the path, status, code and message, and shows a red
toast. A global handler catches uncaught exceptions and unhandled promise
rejections, logs the full stack and shows the same toast. A failing background
job raises a red toast over the job WebSocket.

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
  `restic`, `wg`, `smartctl`, `journalctl`, `sendmail` and `curl` as subprocesses.
  Each of those sits behind one seam, so when a tool is missing the feature
  reports itself as unavailable while everything else keeps working.
- **Nothing blocks a request.** Compose pulls, image downloads and update
  redeploys run on the job runner and report progress, so a slow operation never
  holds an API worker or the UI.
- **Almost no dependencies.** Eighteen direct crates, most of them small.
  `bollard` is built without its HTTP/TLS surface because only the local Docker
  socket is used. There is a single crypto backend (`ring`), confined to the TLS
  module. No ORM, no templating engine, no HTTP client library.
- **Fast where it counts.** One WebSocket carries all telemetry instead of the
  page polling several endpoints. The container list is cached for two seconds
  and the cache is dropped the moment anything changes. Docker Hub responses are
  cached for five minutes with rate-limit backoff. The initial JavaScript bundle
  is about 195 KB (53 KB gzipped), and xterm, which is the largest dependency, is
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
   libvirt, LXC, dnsmasq and unbound (with `unbound-anchor` and `dns-root-data`),
   nftables, Samba, NFS, restic, WireGuard, smartmontools, `curl`, `openssl`,
   `qrencode` and the archive tools.
3. **Makes the resolver self-healing.** It frees port 53 so unbound can bind:
   disables `systemd-resolved` (replacing a stub `/etc/resolv.conf` with a static
   one if needed), generates the unbound root trust anchor in `/var/lib/unbound`,
   and keeps dnsmasq enabled but inert — a `00-stack-stack-base.conf` fragment
   with `port=0` and no DHCP range, so the unit runs without touching :53 and
   without offering leases, and a configured LAN DHCP keeps working across
   reboots. It also installs an unbound systemd drop-in that starts it after the
   network and retries with a backoff instead of giving up.
4. Hardens the system: enables unattended security upgrades, installs fail2ban
   configured for sshd through the systemd journal, sets ufw to deny inbound by
   default while allowing SSH, the UI port, and 80/443 when TLS is on, and
   writes a sysctl drop-in (reverse-path filtering, hidden kernel pointers,
   restricted dmesg, restricted ptrace).
5. Installs Rust through rustup if it is not already present.
6. Builds the release binary.
7. Installs it to `/usr/local/bin/stack-stack`, keeping the previous binary so a
   bad build can be rolled back.
8. Creates `/etc/stack-stack`, `/var/lib/stack-stack/{guests,images,stacks}`,
   `/var/log/stack-stack`, and the runtime directories LXC and libvirt need.
9. Writes a default config if none exists, then installs the systemd unit,
   reloads systemd and verifies the unit.
10. Starts the service and polls `/api/health` for up to 60 seconds. If it does
    not come up, the script prints the service journal and restores the previous
    binary. It also warns with the unit journal if `unbound.service` failed.

On first boot the service writes a default document for every configuration file
it owns (network, tasks, alerts, health, notify, files, proxy, updates,
virtualization, samba, nfs, backup, ddns, wireguard, smart), so a fresh box has a
complete `/etc/stack-stack` tree. Secret documents are never created until there
is a real secret to store.

To check it came up:

```bash
systemctl status stack-stack          # active (running)
curl -s http://127.0.0.1:8553/api/health
systemctl is-active unbound           # active
dig @127.0.0.1 google.com             # answers
journalctl -u stack-stack -n 50       # if it did not start
```

The script logs what it did to `/var/log/stack-stack-deploy.log`.

One thing to check after the first deploy: the firewall allows SSH on port 22.
If your SSH runs on a different port, allow it before you rely on ufw.

### Enabling HTTPS

The plain `:8553` listener is HTTP. A TLS front door on `:443` is optional and
off by default, so the box does not take over an existing web server. Turn it on
from **Network → proxy**: it runs a port check first, then creates a CA and a
certificate for your admin hostname. Point `stack-stack.lan`, or whatever name
you choose, at the box in DNS or `/etc/hosts` afterwards.

## Where things live

| Path | Contents |
| --- | --- |
| `/usr/local/bin/stack-stack` | The binary. |
| `/etc/stack-stack/` | Configuration documents (network, tasks, alerts, health, notify, files, proxy, updates, virtualization, shares, backups, and so on). |
| `/etc/stack-stack/config.json` | Runtime settings. Passed to the service by the unit. |
| `/etc/stack-stack/alerts.json`, `health.json` | Alert rules and providers, and per-stack health checks. Mode 0600. |
| `/etc/stack-stack/*.secrets.json` | Credentials. Mode 0600. |
| `/etc/dnsmasq.d/` | `00-stack-stack-base.conf` (inert) and the generated `stack-stack.conf`. |
| `/etc/systemd/system/unbound.service.d/` | The unbound start-order and restart drop-in. |
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

Most documents are edited through their page and validated before they are
saved. The Developer page can show the effective JSON of any of them, including
defaults for a file that has not been written yet.

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

### End-to-end tests

`tests/e2e/` is a Playwright suite that drives a real running instance. It signs
in (creating the first-run password), creates and runs a scheduled task, and
configures an HTTP health check. Start the binary, then:

```bash
cd tests/e2e
npm install
npx playwright install chromium
STACK_STACK_URL=http://127.0.0.1:8553 npx playwright test
```

CI (`.github/workflows/ci.yml`) runs three jobs: `fmt`/`clippy -D warnings`/tests
and a release build; the frontend type-check and bundle; and the E2E suite, which
boots the release binary in the background and fails the pipeline if a button or
an API call is broken.

## Roadmap

Things that are not in yet, roughly in the order they are likely to arrive:

- Container image management beyond the catalog: list, pull and delete
  arbitrary images and tags.
- Auto-update for standalone containers (recreating one on a new image), and
  automatic redeploy for app-store apps.
- Per-app health checks wired into install, and pinned checksums for more
  catalog entries, so an install can be verified rather than assumed.
- Query statistics per client. The adblock page reports what is compiled and
  what a lookup would do; it does not yet parse the unbound query log to count
  what was actually blocked.
- Multiple WireGuard interfaces. The VPN is one interface by design right now.
- Multiple admin accounts and roles. There is a single admin today.
- Support beyond x86_64. The deploy script currently refuses to run on anything
  else.
- Storage pooling. Drives are formatted and mounted one at a time; there is no
  mergerfs-style pool yet.

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
- Job history and the notification feed are in memory: they survive a restart of
  the process only as far as the filesystem does, which for jobs means not at
  all. A job is a view of work in flight, not durable state.

## License

MIT.
