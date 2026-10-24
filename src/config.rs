//! Runtime configuration.
//!
//! Precedence: built-in defaults -> config file -> environment overrides.
//! On the Fujitsu the config file lives at `/etc/stack-stack/config.json`
//! (the systemd unit passes it explicitly, see `deploy-stack-stack.sh`).
//! Anything unreadable or malformed degrades to defaults rather than failing
//! the boot: a metrics box should always come up.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/stack-stack/config.json";
pub const DEFAULT_PORT: u16 = 8553;
pub const DEFAULT_SAMPLE_INTERVAL_MS: u64 = 1000;
pub const DEFAULT_DOCKER_SOCKET: &str = "/var/run/docker.sock";
pub const DEFAULT_APPS_DIR: &str = "/etc/stack-stack/apps";
pub const DEFAULT_CATALOG_PATH: &str = "/etc/stack-stack/catalog.json";
/// Operator-authored compose stacks, one directory per stack name. Kept under
/// `/var/lib` (not `/etc`) because these are mutable runtime deployments, like
/// guests and images, not configuration documents.
pub const DEFAULT_STACKS_DIR: &str = "/var/lib/stack-stack/stacks";
/// Append-only audit trail for destructive operations.
pub const DEFAULT_AUDIT_PATH: &str = "/var/log/stack-stack/audit.log";
pub const DEFAULT_COMPOSE_TIMEOUT_S: u64 = 300;
/// Auto-update document: per-stack opt-in, schedule and digest history.
pub const DEFAULT_UPDATES_PATH: &str = "/etc/stack-stack/updates.json";
pub const DEFAULT_NETWORK_PATH: &str = "/etc/stack-stack/network.json";
pub const DEFAULT_VIRT_PATH: &str = "/etc/stack-stack/virtualization.json";
pub const DEFAULT_GUEST_CATALOG_PATH: &str = "/etc/stack-stack/guests.json";
pub const DEFAULT_GUESTS_DIR: &str = "/var/lib/stack-stack/guests";
pub const DEFAULT_IMAGES_DIR: &str = "/var/lib/stack-stack/images";
pub const DEFAULT_VIRT_TIMEOUT_S: u64 = 1800;
pub const DEFAULT_AUTH_PATH: &str = "/etc/stack-stack/auth.json";
pub const DEFAULT_SESSION_TTL_H: u64 = 168;
pub const DEFAULT_NOTIFY_PATH: &str = "/etc/stack-stack/notify.json";
pub const DEFAULT_CA_DIR: &str = "/etc/stack-stack/ca";
pub const DEFAULT_PROXY_PATH: &str = "/etc/stack-stack/proxy.json";
pub const DEFAULT_FILES_PATH: &str = "/etc/stack-stack/files.json";
pub const DEFAULT_SAMBA_PATH: &str = "/etc/stack-stack/samba.json";
pub const DEFAULT_NFS_PATH: &str = "/etc/stack-stack/nfs.json";
pub const DEFAULT_BACKUP_PATH: &str = "/etc/stack-stack/backup.json";
pub const DEFAULT_BACKUP_SECRETS_PATH: &str = "/etc/stack-stack/backup.secrets.json";
pub const DEFAULT_RESTIC_TIMEOUT_S: u64 = 7200;
pub const DEFAULT_WG_PATH: &str = "/etc/stack-stack/wireguard.json";
pub const DEFAULT_WG_SECRETS_PATH: &str = "/etc/stack-stack/wireguard.secrets.json";
pub const DEFAULT_SMART_PATH: &str = "/etc/stack-stack/smart.json";
pub const DEFAULT_DDNS_PATH: &str = "/etc/stack-stack/ddns.json";
pub const DEFAULT_DDNS_SECRETS_PATH: &str = "/etc/stack-stack/ddns.secrets.json";
pub const DEFAULT_TLS_HTTP_PORT: u16 = 80;
pub const DEFAULT_TLS_HTTPS_PORT: u16 = 443;
pub const DEFAULT_TLS_ADMIN_HOSTNAME: &str = "stack-stack.lan";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Address the HTTP listener binds to. 0.0.0.0 so the LAN can reach it.
    ///
    /// SECURITY: every `/api/*` route except `/api/health` and the auth
    /// bootstrap endpoints is behind the single-admin login gate (see
    /// `auth_path`/`session_ttl_h`). The `:8553` listener itself is still plain
    /// HTTP; enable the TLS front door (`tls_enabled`) for HTTPS on `:443`, or
    /// keep this listener on `127.0.0.1` when the box is not on a trusted LAN.
    pub bind: String,
    /// HTTP port. Locked to 8553 in the deployment plan.
    pub port: u16,
    /// Telemetry sample period. Clients receive one JSON frame per tick.
    pub sample_interval_ms: u64,
    /// Where SMART data is cached, in seconds, to avoid hammering the disks.
    pub smart_interval_s: u64,
    /// Filesystem types that should never show up as a disk in the UI.
    ///
    /// Values from the config file are *merged with* the built-in list (and
    /// de-duplicated) rather than replacing it, so adding one type does not
    /// silently drop every default. Omitting the key keeps the built-in list.
    #[serde(deserialize_with = "extend_ignored_fs")]
    pub ignored_fs: Vec<String>,
    /// Docker daemon socket. Only a local Unix socket is supported; there is no
    /// remote/TLS transport in this build.
    pub docker_socket: String,
    /// Where app-store compose files live, one directory per app id.
    pub apps_dir: String,
    /// Optional catalog override merged over the embedded seed catalog.
    pub catalog_path: String,
    /// Where operator-authored compose stacks live, one directory per stack.
    pub stacks_dir: String,
    /// Append-only audit trail for destructive operations.
    pub audit_path: String,
    /// Hard limit on a single `docker compose` invocation (pull + up can be slow).
    pub compose_timeout_s: u64,
    /// Auto-update document: which stacks update themselves, and how often.
    pub updates_path: String,
    /// Router/network configuration document (interfaces, firewall, DNS, …).
    pub network_path: String,
    /// Virtualization document: every KVM guest and LXC container this box owns.
    pub virt_path: String,
    /// Optional guest image/template override merged over the embedded catalog.
    pub guest_catalog_path: String,
    /// Per-guest working directory: disks, seeds, pidfiles, QMP sockets.
    pub guests_dir: String,
    /// Cache for downloaded cloud images and appliance ISOs.
    pub images_dir: String,
    /// Hard limit on one guest lifecycle command (image pull + disk create).
    pub virt_timeout_s: u64,
    /// Admin credential document. Missing or empty means "setup mode": the UI
    /// shows a first-run screen and every other `/api/*` route is gated.
    pub auth_path: String,
    /// Session lifetime, in hours. Tokens are in-memory only and never persisted.
    pub session_ttl_h: u64,
    /// Notification document: per-event toggles, thresholds and the webhook URL.
    pub notify_path: String,
    /// Whether the TLS front door (`:80` redirect + `:443` HTTPS) is enabled.
    /// Off by default so a box already running nginx/apache is not taken over.
    pub tls_enabled: bool,
    /// Plain HTTP port used only to redirect to HTTPS.
    pub tls_http_port: u16,
    /// HTTPS port: terminates TLS, serves the admin UI and proxies routes.
    pub tls_https_port: u16,
    /// Hostname the admin UI answers on over HTTPS.
    pub tls_admin_hostname: String,
    /// Certificate name used when a SNI has no matching managed certificate.
    pub tls_default_cert: Option<String>,
    /// Internal CA directory: root key/cert and per-hostname leaves.
    pub ca_dir: String,
    /// Reverse-proxy route document.
    pub proxy_path: String,
    /// File-manager document: writable roots, hidden-file and size limits.
    pub files_path: String,
    /// Samba share document.
    pub samba_path: String,
    /// NFS export document.
    pub nfs_path: String,
    /// Backup job document (secret-free).
    pub backup_path: String,
    /// Backup credentials document (0600): repository passwords and keys.
    pub backup_secrets_path: String,
    /// Hard limit on one restic command.
    pub restic_timeout_s: u64,
    /// WireGuard topology document (secret-free).
    pub wg_path: String,
    /// WireGuard server private key document (0600).
    pub wg_secrets_path: String,

    /// SMART self-test schedule document.
    pub smart_path: String,
    /// Dynamic DNS document.
    pub ddns_path: String,
    /// Dynamic DNS token document (0600).
    pub ddns_secrets_path: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0".to_string(),
            port: DEFAULT_PORT,
            sample_interval_ms: DEFAULT_SAMPLE_INTERVAL_MS,
            smart_interval_s: 60,
            ignored_fs: [
                "autofs",
                "binfmt_misc",
                "bpf",
                "cgroup",
                "cgroup2",
                "configfs",
                "debugfs",
                "devpts",
                "devtmpfs",
                "efivarfs",
                "fusectl",
                "hugetlbfs",
                "mqueue",
                "nsfs",
                "overlay",
                "proc",
                "pstore",
                "ramfs",
                "rpc_pipefs",
                "securityfs",
                "squashfs",
                "sysfs",
                "tmpfs",
                "tracefs",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            docker_socket: DEFAULT_DOCKER_SOCKET.to_string(),
            apps_dir: DEFAULT_APPS_DIR.to_string(),
            catalog_path: DEFAULT_CATALOG_PATH.to_string(),
            stacks_dir: DEFAULT_STACKS_DIR.to_string(),
            audit_path: DEFAULT_AUDIT_PATH.to_string(),
            compose_timeout_s: DEFAULT_COMPOSE_TIMEOUT_S,
            updates_path: DEFAULT_UPDATES_PATH.to_string(),
            network_path: DEFAULT_NETWORK_PATH.to_string(),
            virt_path: DEFAULT_VIRT_PATH.to_string(),
            guest_catalog_path: DEFAULT_GUEST_CATALOG_PATH.to_string(),
            guests_dir: DEFAULT_GUESTS_DIR.to_string(),
            images_dir: DEFAULT_IMAGES_DIR.to_string(),
            virt_timeout_s: DEFAULT_VIRT_TIMEOUT_S,
            auth_path: DEFAULT_AUTH_PATH.to_string(),
            session_ttl_h: DEFAULT_SESSION_TTL_H,
            notify_path: DEFAULT_NOTIFY_PATH.to_string(),
            tls_enabled: false,
            tls_http_port: DEFAULT_TLS_HTTP_PORT,
            tls_https_port: DEFAULT_TLS_HTTPS_PORT,
            tls_admin_hostname: DEFAULT_TLS_ADMIN_HOSTNAME.to_string(),
            tls_default_cert: None,
            ca_dir: DEFAULT_CA_DIR.to_string(),
            proxy_path: DEFAULT_PROXY_PATH.to_string(),
            files_path: DEFAULT_FILES_PATH.to_string(),
            samba_path: DEFAULT_SAMBA_PATH.to_string(),
            nfs_path: DEFAULT_NFS_PATH.to_string(),
            backup_path: DEFAULT_BACKUP_PATH.to_string(),
            backup_secrets_path: DEFAULT_BACKUP_SECRETS_PATH.to_string(),
            restic_timeout_s: DEFAULT_RESTIC_TIMEOUT_S,
            wg_path: DEFAULT_WG_PATH.to_string(),
            wg_secrets_path: DEFAULT_WG_SECRETS_PATH.to_string(),

            smart_path: DEFAULT_SMART_PATH.to_string(),
            ddns_path: DEFAULT_DDNS_PATH.to_string(),
            ddns_secrets_path: DEFAULT_DDNS_SECRETS_PATH.to_string(),
        }
    }
}

impl Config {
    /// Load configuration from `path` (or `$STACK_STACK_CONFIG`), applying
    /// `$STACK_STACK_BIND` / `$STACK_STACK_PORT` / `$STACK_STACK_SAMPLE_MS` overrides.
    pub fn load() -> Self {
        let path = std::env::var("STACK_STACK_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_PATH));
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Self {
        let mut cfg = match std::fs::read_to_string(path) {
            Ok(raw) => match serde_json::from_str::<Config>(&raw) {
                Ok(parsed) => {
                    tracing::info!(path = %path.display(), "loaded config file");
                    parsed
                }
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err, "config file is not valid JSON, using defaults");
                    Config::default()
                }
            },
            Err(_) => {
                tracing::debug!(path = %path.display(), "no config file, using defaults");
                Config::default()
            }
        };

        if let Ok(bind) = std::env::var("STACK_STACK_BIND") {
            cfg.bind = bind;
        }
        if let Some(port) = env_parse::<u16>("STACK_STACK_PORT") {
            cfg.port = port;
        }
        if let Some(ms) = env_parse::<u64>("STACK_STACK_SAMPLE_MS") {
            cfg.sample_interval_ms = ms.max(100);
        }

        cfg
    }

    pub fn addr(&self) -> String {
        format!("{}:{}", self.bind, self.port)
    }
}

/// Merge a user-supplied `ignored_fs` list with the built-in defaults instead of
/// letting serde replace the field wholesale. Missing keys are still handled by
/// the container-level `#[serde(default)]`, which yields the full default list.
fn extend_ignored_fs<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut merged = Config::default().ignored_fs;
    for fs in Vec::<String>::deserialize(deserializer)? {
        if !merged.iter().any(|known| known == &fs) {
            merged.push(fs);
        }
    }
    Ok(merged)
}

fn env_parse<T: std::str::FromStr>(key: &str) -> Option<T> {
    let raw = std::env::var(key).ok()?;
    match raw.trim().parse::<T>() {
        Ok(v) => Some(v),
        Err(_) => {
            tracing::warn!(key, value = %raw, "ignoring unparseable environment override");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_ignored_fs_merges_with_defaults() {
        let cfg: Config = serde_json::from_str(r#"{"ignored_fs":["myfs"]}"#).expect("parse");
        assert!(cfg.ignored_fs.iter().any(|f| f == "myfs"));
        assert!(cfg.ignored_fs.iter().any(|f| f == "tmpfs"));
        assert!(cfg.ignored_fs.iter().any(|f| f == "proc"));
    }

    #[test]
    fn user_ignored_fs_deduplicates_builtins() {
        let cfg: Config = serde_json::from_str(r#"{"ignored_fs":["tmpfs"]}"#).expect("parse");
        assert_eq!(cfg.ignored_fs.iter().filter(|f| *f == "tmpfs").count(), 1);
    }

    #[test]
    fn missing_keys_fall_back_to_defaults() {
        let cfg: Config = serde_json::from_str("{}").expect("parse");
        assert_eq!(cfg.bind, "0.0.0.0");
        assert_eq!(cfg.port, DEFAULT_PORT);
        assert_eq!(cfg.sample_interval_ms, DEFAULT_SAMPLE_INTERVAL_MS);
        assert!(cfg.ignored_fs.iter().any(|f| f == "tmpfs"));
    }
}
