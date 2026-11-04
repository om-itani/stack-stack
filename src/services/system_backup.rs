//! Full system configuration backup and disaster recovery.
//!
//! Archives all `/etc/stack-stack/*.json` configuration files and `/var/lib/stack-stack/stacks/`
//! compose definitions into a single gzip-compressed tarball (`.tar.gz`).
//! On restore, entries are strictly sanitized against directory traversal and unpacked with
//! 0600/0700 permissions.

use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::error::ApiError;
use crate::files::safe_entry_name;
use crate::runner::Runner;

/// Create a complete disaster recovery `.tar.gz` archive.
pub fn create_system_backup(
    runner: &dyn Runner,
    config_dir: &Path,
    stacks_dir: &Path,
    output_tar_gz: &Path,
) -> Result<(), ApiError> {
    let staging_root = std::env::temp_dir().join(format!("ss-backup-stage-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging_root);
    fs::create_dir_all(&staging_root)
        .map_err(|err| ApiError::internal(format!("cannot create staging directory: {err}")))?;

    let staging_config = staging_root.join("config");
    let staging_stacks = staging_root.join("stacks");
    let _ = fs::create_dir_all(&staging_config);
    let _ = fs::create_dir_all(&staging_stacks);

    // 1. Copy all JSON config files from config_dir
    if config_dir.is_dir()
        && let Ok(entries) = fs::read_dir(config_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
                && let Some(file_name) = path.file_name()
            {
                let _ = fs::copy(&path, staging_config.join(file_name));
            }
        }
    }

    // 2. Copy stacks directories
    if stacks_dir.is_dir() {
        copy_dir_recursive(stacks_dir, &staging_stacks)?;
    }

    // 3. Create .tar.gz archive using tar
    let args = vec![
        "-czf".to_string(),
        output_tar_gz.to_string_lossy().to_string(),
        "-C".to_string(),
        staging_root.to_string_lossy().to_string(),
        "config".to_string(),
        "stacks".to_string(),
    ];

    let output = runner
        .run_timeout("tar", &args, Duration::from_secs(300))
        .map_err(|err| ApiError::internal(format!("tar is not available: {err}")))?;

    let _ = fs::remove_dir_all(&staging_root);

    if !output.success() {
        return Err(ApiError::internal(format!(
            "failed to create backup archive: {}",
            output.stderr
        )));
    }

    Ok(())
}

/// Restore a disaster recovery `.tar.gz` archive.
pub fn restore_system_backup(
    runner: &dyn Runner,
    archive_tar_gz: &Path,
    config_dir: &Path,
    stacks_dir: &Path,
) -> Result<(), ApiError> {
    // 1. List archive contents to verify safe entries
    let list_args = vec![
        "-tzf".to_string(),
        archive_tar_gz.to_string_lossy().to_string(),
    ];
    let list_output = runner
        .run_timeout("tar", &list_args, Duration::from_secs(60))
        .map_err(|err| ApiError::internal(format!("tar is not available: {err}")))?;

    if !list_output.success() {
        return Err(ApiError::bad_request("invalid backup archive file"));
    }

    for line in list_output.stdout.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !safe_entry_name(trimmed) {
            return Err(ApiError::bad_request(format!(
                "backup archive contains an unsafe entry: {trimmed:?}"
            )));
        }
    }

    // 2. Extract into staging directory
    let staging_root =
        std::env::temp_dir().join(format!("ss-restore-stage-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging_root);
    fs::create_dir_all(&staging_root)
        .map_err(|err| ApiError::internal(format!("cannot create staging directory: {err}")))?;

    let extract_args = vec![
        "-xzf".to_string(),
        archive_tar_gz.to_string_lossy().to_string(),
        "-C".to_string(),
        staging_root.to_string_lossy().to_string(),
    ];
    let extract_output = runner
        .run_timeout("tar", &extract_args, Duration::from_secs(300))
        .map_err(|err| ApiError::internal(format!("tar is not available: {err}")))?;

    if !extract_output.success() {
        let _ = fs::remove_dir_all(&staging_root);
        return Err(ApiError::internal(format!(
            "failed to extract backup archive: {}",
            extract_output.stderr
        )));
    }

    // 3. Restore config files
    let staged_config = staging_root.join("config");
    if staged_config.is_dir() {
        let _ = fs::create_dir_all(config_dir);
        if let Ok(entries) = fs::read_dir(&staged_config) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file()
                    && let Some(file_name) = path.file_name()
                {
                    let dest = config_dir.join(file_name);
                    let _ = fs::copy(&path, &dest);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(0o600));
                    }
                }
            }
        }
    }

    // 4. Restore stacks directory
    let staged_stacks = staging_root.join("stacks");
    if staged_stacks.is_dir() {
        let _ = fs::create_dir_all(stacks_dir);
        copy_dir_recursive(&staged_stacks, stacks_dir)?;
    }

    let _ = fs::remove_dir_all(&staging_root);
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), ApiError> {
    if !dst.exists() {
        fs::create_dir_all(dst).map_err(|err| {
            ApiError::internal(format!("cannot create dir {}: {err}", dst.display()))
        })?;
    }
    for entry in fs::read_dir(src)
        .map_err(|err| ApiError::internal(format!("cannot read dir {}: {err}", src.display())))?
        .flatten()
    {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path).map_err(|err| {
                ApiError::internal(format!("cannot copy {}: {err}", src_path.display()))
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&dst_path, fs::Permissions::from_mode(0o600));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::SystemRunner;

    #[test]
    fn system_backup_export_and_restore_cycle() {
        let temp_base = std::env::temp_dir().join(format!("ss-test-cycle-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_base);
        let src_config = temp_base.join("src_config");
        let src_stacks = temp_base.join("src_stacks");
        let dest_config = temp_base.join("dest_config");
        let dest_stacks = temp_base.join("dest_stacks");
        let archive = temp_base.join("backup.tar.gz");

        fs::create_dir_all(&src_config).unwrap();
        fs::create_dir_all(src_stacks.join("app1")).unwrap();
        fs::write(src_config.join("auth.json"), r#"{"password_hash":"xyz"}"#).unwrap();
        fs::write(
            src_stacks.join("app1").join("docker-compose.yml"),
            "version: '3'",
        )
        .unwrap();

        let runner = SystemRunner::default();
        create_system_backup(&runner, &src_config, &src_stacks, &archive).expect("create backup");
        assert!(archive.exists());

        restore_system_backup(&runner, &archive, &dest_config, &dest_stacks)
            .expect("restore backup");
        assert!(dest_config.join("auth.json").exists());
        assert!(dest_stacks.join("app1").join("docker-compose.yml").exists());

        let _ = fs::remove_dir_all(&temp_base);
    }
}
