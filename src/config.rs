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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Address the HTTP listener binds to. 0.0.0.0 so the LAN can reach it.
    ///
    /// SECURITY: the server has no authentication and no TLS by design; binding
    /// beyond `127.0.0.1` exposes hostname, disk layout, network interfaces and
    /// SMART data to anyone who can reach the port. Keep the default on a
    /// trusted homelab LAN, or set `STACK_STACK_BIND=127.0.0.1` when it is not.
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
