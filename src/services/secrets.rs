//! Per-app secrets management for Compose deployments.
//!
//! Stored in `/etc/stack-stack/secrets.json` with 0600 permissions.
//! Templated Compose files use `{{secret.NAME}}` placeholders. At deploy time,
//! secrets are interpolated into a temporary 0600 file, passed to `docker compose`,
//! and immediately scrubbed so plaintext secrets never persist on disk.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::store::write_atomic;

/// The persisted secrets document (0600 permissions).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSecrets {
    #[serde(default)]
    pub secrets: HashMap<String, String>,
}

/// Load secrets from disk. Missing or corrupt file returns empty map rather than crashing.
pub fn load_secrets(path: &Path) -> AppSecrets {
    match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str::<AppSecrets>(&raw).unwrap_or_default(),
        Err(_) => AppSecrets::default(),
    }
}

/// Save secrets document atomically with strict 0600 permissions.
pub fn save_secrets(path: &Path, secrets: &AppSecrets) -> Result<(), ApiError> {
    let body = serde_json::to_vec_pretty(secrets)
        .map_err(|err| ApiError::internal(format!("cannot serialize secrets: {err}")))?;

    write_atomic(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write secrets: {err}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        let _ = fs::set_permissions(path, perms);
    }

    Ok(())
}

/// Replace all `{{secret.NAME}}` and `{{ secret.NAME }}` placeholders with decrypted values.
/// Returns an error if any requested secret is not configured.
pub fn interpolate_secrets(yaml: &str, secrets: &AppSecrets) -> Result<String, ApiError> {
    let mut missing = Vec::new();
    let mut result = String::with_capacity(yaml.len());
    let mut cursor = 0;

    while let Some(start_pos) = yaml[cursor..].find("{{") {
        let actual_start = cursor + start_pos;
        result.push_str(&yaml[cursor..actual_start]);

        if let Some(end_pos) = yaml[actual_start..].find("}}") {
            let actual_end = actual_start + end_pos + 2;
            let tag = &yaml[actual_start + 2..actual_start + end_pos].trim();

            if let Some(secret_name) = tag.strip_prefix("secret.") {
                let name = secret_name.trim();
                if let Some(value) = secrets.secrets.get(name) {
                    result.push_str(value);
                } else {
                    missing.push(name.to_string());
                }
            } else {
                // Not a secret tag, leave untouched
                result.push_str(&yaml[actual_start..actual_end]);
            }
            cursor = actual_end;
        } else {
            // Unclosed {{, keep rest
            break;
        }
    }

    result.push_str(&yaml[cursor..]);

    if !missing.is_empty() {
        return Err(ApiError::bad_request(format!(
            "missing required secret(s) in Compose file: {}",
            missing.join(", ")
        )));
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolate_single_and_multiple_secrets() {
        let mut app_secrets = AppSecrets::default();
        app_secrets
            .secrets
            .insert("DB_PASS".to_string(), "supersecret123".to_string());
        app_secrets
            .secrets
            .insert("API_KEY".to_string(), "xyz-789".to_string());

        let yaml = "services:\n  db:\n    environment:\n      - PASS={{secret.DB_PASS}}\n      - KEY={{ secret.API_KEY }}\n";
        let rendered = interpolate_secrets(yaml, &app_secrets).expect("interpolation failed");
        assert!(rendered.contains("PASS=supersecret123"));
        assert!(rendered.contains("KEY=xyz-789"));
        assert!(!rendered.contains("{{secret.DB_PASS}}"));
    }

    #[test]
    fn missing_secret_reports_clear_error() {
        let app_secrets = AppSecrets::default();
        let yaml = "services:\n  db:\n    password: {{secret.UNDEFINED_SECRET}}\n";
        let err = interpolate_secrets(yaml, &app_secrets).expect_err("should have failed");
        assert!(err.to_string().contains("UNDEFINED_SECRET"));
    }

    #[test]
    fn save_and_load_round_trip() {
        let temp_dir = std::env::temp_dir().join(format!("ss-secrets-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_dir);
        let secrets_path = temp_dir.join("secrets.json");

        let mut initial = AppSecrets::default();
        initial
            .secrets
            .insert("TEST_KEY".to_string(), "val".to_string());
        save_secrets(&secrets_path, &initial).expect("save");

        let loaded = load_secrets(&secrets_path);
        assert_eq!(
            loaded.secrets.get("TEST_KEY").map(String::as_str),
            Some("val")
        );

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
