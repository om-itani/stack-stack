//! Persistence and on-disk layout for guests.
//!
//! Reads are forgiving (a missing file is the default document), but a file that
//! exists and does not parse is reported rather than silently replaced, so an
//! operator's hand-edit is never quietly thrown away. Writes are atomic and, on
//! unix, `0600`, because the document can carry initial guest passwords.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::ApiError;
use crate::virtualization::model::{VirtualizationConfig, VmConfig};

/// Load the document. The second value is a human-readable read/parse failure,
/// in which case the returned config is the default document.
pub fn load(path: &Path) -> (VirtualizationConfig, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<VirtualizationConfig>(&raw) {
            Ok(config) => (config, None),
            Err(err) => (
                VirtualizationConfig::default(),
                Some(format!(
                    "{} is not valid virtualization config JSON: {err}",
                    path.display()
                )),
            ),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            (VirtualizationConfig::default(), None)
        }
        Err(err) => (
            VirtualizationConfig::default(),
            Some(format!("cannot read {}: {err}", path.display())),
        ),
    }
}

/// Persist the document atomically, creating the parent directory if needed.
pub fn save(path: &Path, config: &VirtualizationConfig) -> Result<(), ApiError> {
    let mut body = serde_json::to_vec_pretty(config).map_err(|err| {
        ApiError::internal(format!("cannot serialise virtualization config: {err}"))
    })?;
    body.push(b'\n');
    write_private(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))
}

/// Temp file in the destination directory, then rename; `0600` on unix.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "bad path",
        ));
    };
    let tmp = path.with_file_name(format!(".{name}.tmp"));

    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(err) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }

    std::fs::rename(&tmp, path)
}

// ---------------------------------------------------------------------------
// On-disk layout
// ---------------------------------------------------------------------------

/// Root of LXC's storage, passed as `-P` to every `lxc-*` command so nothing
/// leaks into `/var/lib/lxc` and the whole guest set stays under one tree.
pub fn lxc_root(cfg: &Config) -> PathBuf {
    Path::new(&cfg.guests_dir).join("lxc")
}

pub fn guest_dir(cfg: &Config, id: &str) -> PathBuf {
    Path::new(&cfg.guests_dir).join(id)
}

pub fn disk_path(cfg: &Config, vm: &VmConfig) -> PathBuf {
    guest_dir(cfg, &vm.id).join(format!("disk.{}", vm.disk_format.as_qemu_img()))
}

/// Cloud-init NoCloud seed directory (user-data, meta-data, network-config).
pub fn seed_dir(cfg: &Config, id: &str) -> PathBuf {
    guest_dir(cfg, id).join("seed")
}

pub fn seed_iso_path(cfg: &Config, id: &str) -> PathBuf {
    guest_dir(cfg, id).join("seed.iso")
}

/// QEMU pid file, used for liveness and signalling.
pub fn pid_path(cfg: &Config, id: &str) -> PathBuf {
    guest_dir(cfg, id).join("qemu.pid")
}

/// QMP control socket: graceful shutdown and reboot without a signal race.
pub fn qmp_path(cfg: &Config, id: &str) -> PathBuf {
    guest_dir(cfg, id).join("qmp.sock")
}

/// Serial console socket bridged to the browser.
pub fn serial_path(cfg: &Config, id: &str) -> PathBuf {
    guest_dir(cfg, id).join("serial.sock")
}

/// LXC's own config file for the container, inside our `-P` root.
pub fn ct_dir(cfg: &Config, id: &str) -> PathBuf {
    lxc_root(cfg).join(id)
}

pub fn ct_config_path(cfg: &Config, id: &str) -> PathBuf {
    ct_dir(cfg, id).join("config")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cfg(name: &str) -> Config {
        let dir = std::env::temp_dir().join(format!(
            "stack-stack-virt-store-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Config {
            guests_dir: dir.join("guests").to_string_lossy().into_owned(),
            images_dir: dir.join("images").to_string_lossy().into_owned(),
            ..Config::default()
        }
    }

    #[test]
    fn missing_file_is_the_default_document() {
        let cfg = temp_cfg("missing");
        let (config, error) = load(Path::new(&cfg.virt_path));
        assert!(error.is_none());
        assert!(config.vms.is_empty());
    }

    #[test]
    fn save_then_load_round_trips_and_leaves_no_temp() {
        let cfg = temp_cfg("roundtrip");
        let path = Path::new(&cfg.guests_dir).join("virtualization.json");
        let mut config = VirtualizationConfig::default();
        config.vms.push(VmConfig {
            id: "web".to_string(),
            ..VmConfig::default()
        });
        save(&path, &config).expect("save");

        let (loaded, error) = load(&path);
        assert!(error.is_none(), "{error:?}");
        assert_eq!(loaded.vms[0].id, "web");
        assert!(!path.with_file_name(".virtualization.json.tmp").exists());
    }

    #[cfg(unix)]
    #[test]
    fn saved_document_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let cfg = temp_cfg("perms");
        let path = Path::new(&cfg.guests_dir).join("virtualization.json");
        save(&path, &VirtualizationConfig::default()).expect("save");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }

    #[test]
    fn malformed_file_is_reported_not_replaced() {
        let cfg = temp_cfg("malformed");
        let path = Path::new(&cfg.guests_dir).join("virtualization.json");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "{ not json").expect("write");

        let (config, error) = load(&path);
        assert!(error.is_some());
        assert!(config.vms.is_empty());
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "{ not json");
    }

    #[test]
    fn paths_are_scoped_under_the_guests_dir() {
        let cfg = temp_cfg("paths");
        let vm = VmConfig {
            id: "web".to_string(),
            ..VmConfig::default()
        };
        assert!(disk_path(&cfg, &vm).starts_with(&cfg.guests_dir));
        assert!(disk_path(&cfg, &vm).ends_with("disk.qcow2"));
        assert!(qmp_path(&cfg, "web").ends_with("qmp.sock"));
        assert!(ct_config_path(&cfg, "db").ends_with("config"));
        assert!(lxc_root(&cfg).ends_with("lxc"));
    }
}
