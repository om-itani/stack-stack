//! First-boot configuration seeding.
//!
//! Every configuration document is written lazily by its own subsystem, so a
//! fresh box has an `/etc/stack-stack` tree that is missing most of its files
//! until the first edit. That makes diagnostics confusing — the Developer page
//! reports "missing" and an operator cannot see the shape of a document before
//! touching it.
//!
//! This writes a default document for every file the service owns the first time
//! it boots. Existing files are never touched. Secret documents (`auth.json` and
//! the `*.secrets.json` files) are deliberately not created here: they should
//! only exist once there is a real secret to store.

use std::path::Path;

use crate::config::Config;
use crate::networking::store::write_atomic;

/// Write `value` as pretty JSON to `path` when the file is absent. Returns
/// `true` when it was written.
fn seed<T: serde::Serialize>(path: &str, value: &T) -> std::io::Result<bool> {
    let target = Path::new(path);
    if target.exists() {
        return Ok(false);
    }
    if let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut body = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    body.push(b'\n');
    write_atomic(target, &body)?;
    Ok(true)
}

/// Seed every owned configuration document that does not exist yet. Returns the
/// names of the documents that were written.
pub fn ensure_config_files(cfg: &Config) -> std::io::Result<Vec<&'static str>> {
    let mut written: Vec<&'static str> = Vec::new();

    // One line per document: seed it if absent and remember the name.
    macro_rules! seed_doc {
        ($name:literal, $path:expr, $value:expr) => {
            if seed($path, &$value)? {
                written.push($name);
            }
        };
    }

    seed_doc!(
        "network",
        &cfg.network_path,
        crate::networking::model::NetworkConfig::default()
    );
    seed_doc!(
        "tasks",
        &cfg.tasks_path,
        crate::services::tasks::TasksDoc::default()
    );
    seed_doc!(
        "alerts",
        &cfg.alerts_path,
        crate::alerts::AlertsDoc::default()
    );
    seed_doc!(
        "health",
        &cfg.health_path,
        crate::services::health::HealthDoc::default()
    );
    seed_doc!(
        "notify",
        &cfg.notify_path,
        crate::notify::NotifyConfig::default()
    );
    seed_doc!(
        "files",
        &cfg.files_path,
        crate::files::FilesConfig::default()
    );
    seed_doc!(
        "proxy",
        &cfg.proxy_path,
        crate::proxy::ProxyConfig::default()
    );
    seed_doc!(
        "updates",
        &cfg.updates_path,
        crate::services::updates::UpdatesDoc::default()
    );
    seed_doc!(
        "virtualization",
        &cfg.virt_path,
        crate::virtualization::model::VirtualizationConfig::default()
    );
    seed_doc!(
        "samba",
        &cfg.samba_path,
        crate::services::samba::SmbConfig::default()
    );
    seed_doc!(
        "nfs",
        &cfg.nfs_path,
        crate::services::nfs::NfsConfig::default()
    );
    seed_doc!(
        "backup",
        &cfg.backup_path,
        crate::services::backup::BackupConfig::default()
    );
    seed_doc!(
        "ddns",
        &cfg.ddns_path,
        crate::services::ddns::DdnsConfig::default()
    );
    seed_doc!(
        "wireguard",
        &cfg.wg_path,
        crate::services::wireguard::WgConfig::default()
    );
    seed_doc!(
        "smart",
        &cfg.smart_path,
        crate::services::smart::SmartConfig::default()
    );

    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Config` whose documents all point inside one temp directory.
    fn temp_config(name: &str) -> (Config, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("ss-bootstrap-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = |file: &str| dir.join(file).to_string_lossy().into_owned();
        let cfg = Config {
            network_path: path("network.json"),
            tasks_path: path("tasks.json"),
            alerts_path: path("alerts.json"),
            health_path: path("health.json"),
            notify_path: path("notify.json"),
            files_path: path("files.json"),
            proxy_path: path("proxy.json"),
            updates_path: path("updates.json"),
            virt_path: path("virtualization.json"),
            samba_path: path("samba.json"),
            nfs_path: path("nfs.json"),
            backup_path: path("backup.json"),
            ddns_path: path("ddns.json"),
            wg_path: path("wireguard.json"),
            smart_path: path("smart.json"),
            ..Config::default()
        };
        (cfg, dir)
    }

    #[test]
    fn seeds_every_document_once() {
        let (cfg, dir) = temp_config("seed");
        let written = ensure_config_files(&cfg).expect("seed");
        assert_eq!(written.len(), 15, "every owned document is created");
        for name in ["network", "tasks", "alerts", "health", "smart"] {
            let file = dir.join(format!("{name}.json"));
            assert!(file.is_file(), "{name}.json should exist");
        }

        // A second run writes nothing.
        let again = ensure_config_files(&cfg).expect("seed again");
        assert!(again.is_empty(), "existing files are left alone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preserves_an_existing_document() {
        let (cfg, dir) = temp_config("preserve");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(&cfg.smart_path, "{\"devices\":[]}\n").expect("write");
        let written = ensure_config_files(&cfg).expect("seed");
        assert!(!written.contains(&"smart"), "an existing file is untouched");
        assert_eq!(
            std::fs::read_to_string(&cfg.smart_path).expect("read"),
            "{\"devices\":[]}\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn never_creates_auth_or_secret_documents() {
        let (cfg, dir) = temp_config("secrets");
        ensure_config_files(&cfg).expect("seed");
        assert!(!dir.join("auth.json").exists());
        assert!(!dir.join("backup.secrets.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
