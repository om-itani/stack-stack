# stack-stack

A single-binary homelab control plane for a Debian box: metrics, Docker and an
app store, a full network stack, virtualization, and — since Phase 5 — the whole
services layer around them, fronted by one login gate.

Everything runs in one Rust binary with no runtime dependencies beyond the
system tools it already drives (`nft`, `ip`, `qemu-system-x86_64`, `lxc`,
`openssl`, `smbd`, `exportfs`, `restic`, `wg`, `smartctl`, `journalctl`, `curl`).
Every subsystem follows the same shape: a validated JSON document, a pure
generator, an atomic write, and an apply that reports one step at a time.

## Run it

```bash
sudo ./deploy-stack-stack.sh      # installs deps, builds, installs, starts
```

The service listens on `:8553` (plain HTTP, login-gated). The first visit is a
setup screen that sets the admin password (`openssl passwd -6`, sessions are
in-memory 256-bit tokens, never persisted).

## The services layer (Phase 5)

| Surface | What it does |
| --- | --- |
| **Login gate** | Single-admin auth. Every `/api/*` route except health and the bootstrap endpoints requires a session. A corrupt `auth.json` locks the box instead of reopening setup. |
| **TLS front door** | `:443` terminates TLS with an internal `openssl` CA (per-hostname leaves, auto-renewed 21 days before expiry); `:80` only redirects. SNI picks the certificate; the admin hostname is served the SPA, every other hostname is raw-proxied with WebSocket upgrades intact. |
| **Reverse proxy** | Routes map hostnames to a container's published port or a URL. Creating a route issues a certificate and writes the matching unbound rewrite. |
| **Files** | A guarded file manager over configurable roots. Paths are collapsed and canonicalised; symlinks that escape a root are refused. Upload streams to `.part`, archives extract behind a traversal filter. |
| **Shares** | Samba and NFS as validated documents with `testparm`/`exportfs` checks and per-step apply reports. Samba accounts are managed through `smbpasswd` (passwords on stdin only). |
| **Backups** | restic jobs with schedules, retention and local/S3/SFTP backends. Secrets live in a 0600 document and reach restic via the environment. Runs are serialised; failures notify. |
| **VPN** | One WireGuard interface, generated `wg-quick` config, one-time client exports with a QR code when `qrencode` is installed. |
| **Terminal** | A real PTY (`fork`/`execvp` of `bash -l`) behind the login gate, bridged with the same frame protocol as container exec. |
| **SMART** | Weekly short and monthly long self-tests per disk, with progress and history. |
| **Logs** | journald tail and follow with severity/unit filters and a per-client cap. |
| **DDNS** | Cloudflare updates driven by the WAN address from the interface document; the API token is passed to `curl` over stdin, never argv. |
| **Notifications** | Ring buffer + live stream + optional webhook. Firewall rollback, backup failures, SMART results, disk thresholds and DDNS changes all report through it. |

## Configuration

Documents live in `/etc/stack-stack` and are created on first use; secrets
(`auth.json`, `*.secrets.json`) are 0600 and never travel in the shareable
documents. All listeners beyond `:8553` are off until explicitly enabled, and
enabling them runs a port preflight first.

## Development

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
cd frontend && npm run build   # embeds dist/ via build.rs
```

Nothing in the tree requires a Linux host to build or test; every external tool
sits behind the `Runner` seam and degrades to an "unavailable" report when
absent.
