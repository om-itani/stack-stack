//! Persistence for the router configuration document.
//!
//! Reads are forgiving (a missing file is simply the default document) but a
//! file that exists and does not parse is reported rather than silently
//! replaced, so an operator's hand-edit is never quietly thrown away.

use std::io::Write;
use std::path::Path;

use crate::error::ApiError;
use crate::networking::model::NetworkConfig;

/// Load the document. The second value is a human-readable parse/read failure,
/// in which case the returned config is the default document.
pub fn load(path: &Path) -> (NetworkConfig, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<NetworkConfig>(&raw) {
            Ok(config) => (config, None),
            Err(err) => (
                NetworkConfig::default(),
                Some(format!(
                    "{} is not valid network config JSON: {err}",
                    path.display()
                )),
            ),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (NetworkConfig::default(), None),
        Err(err) => (
            NetworkConfig::default(),
            Some(format!("cannot read {}: {err}", path.display())),
        ),
    }
}

/// Persist the document atomically, creating the parent directory if needed.
pub fn save(path: &Path, config: &NetworkConfig) -> Result<(), ApiError> {
    let body = serde_json::to_vec_pretty(config)
        .map_err(|err| ApiError::internal(format!("cannot serialise network config: {err}")))?;
    let mut body = body;
    body.push(b'\n');
    write_atomic(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))
}

/// Temp file in the destination directory, then rename. A concurrent reader
/// therefore never observes a half-written file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    let tmp = match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => path.with_file_name(format!(".{name}.tmp")),
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "bad path",
            ));
        }
    };

    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(err) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }

    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::networking::model::FirewallAction;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stack-stack-net-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("network.json")
    }

    #[test]
    fn missing_file_is_the_default_document() {
        let path = temp_path("missing");
        let (config, error) = load(&path);
        assert!(error.is_none());
        assert_eq!(config.firewall.default_input, FirewallAction::Drop);
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_path("roundtrip");
        let mut config = NetworkConfig::default();
        config.dhcp.range_start = "192.168.1.100".to_string();
        save(&path, &config).expect("save");

        let (loaded, error) = load(&path);
        assert!(error.is_none(), "{error:?}");
        assert_eq!(loaded.dhcp.range_start, "192.168.1.100");

        // No temp file is left behind.
        assert!(!path.with_file_name(".network.json.tmp").exists());
    }

    #[test]
    fn malformed_file_is_reported_not_silently_replaced() {
        let path = temp_path("malformed");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "{ not json").expect("write");

        let (config, error) = load(&path);
        assert!(error.is_some());
        assert_eq!(config, NetworkConfig::default());
        // The operator's file is untouched.
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "{ not json");
    }
}
