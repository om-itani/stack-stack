//! HTTP endpoints for full system configuration disaster recovery (export and import).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::runner::SystemRunner;
use crate::services::system_backup;
use crate::state::AppState;

/// Stream full system backup .tar.gz archive.
pub async fn export(State(state): State<AppState>) -> Result<Response, ApiError> {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let filename = format!("stack-stack-backup-{now_secs}.tar.gz");
    let temp_archive = std::env::temp_dir().join(format!("ss-export-{now_secs}.tar.gz"));

    let config_path = PathBuf::from(&state.config.auth_path);
    let config_dir = config_path
        .parent()
        .unwrap_or(Path::new("/etc/stack-stack"))
        .to_path_buf();
    let stacks_dir = PathBuf::from(&state.config.stacks_dir);
    let archive_path = temp_archive.clone();

    tokio::task::spawn_blocking(move || {
        let runner = SystemRunner::default();
        system_backup::create_system_backup(&runner, &config_dir, &stacks_dir, &archive_path)
    })
    .await
    .map_err(|err| ApiError::internal(format!("backup task panicked: {err}")))??;

    let bytes = fs::read(&temp_archive)
        .map_err(|err| ApiError::internal(format!("failed to read backup archive: {err}")))?;
    let _ = fs::remove_file(&temp_archive);

    state.audit("system.backup", "export", &filename);

    let disp = header::HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
        .unwrap_or_else(|_| header::HeaderValue::from_static("attachment"));

    let response = (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/gzip"),
            ),
            (header::CONTENT_DISPOSITION, disp),
        ],
        bytes,
    )
        .into_response();

    Ok(response)
}

/// Restore full system configuration from uploaded .tar.gz archive.
pub async fn import(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.is_empty() {
        return Err(ApiError::bad_request("uploaded backup file is empty"));
    }

    let temp_archive =
        std::env::temp_dir().join(format!("ss-import-{}.tar.gz", std::process::id()));
    fs::write(&temp_archive, &body).map_err(|err| {
        ApiError::internal(format!("failed to write temporary upload file: {err}"))
    })?;

    let config_path = PathBuf::from(&state.config.auth_path);
    let config_dir = config_path
        .parent()
        .unwrap_or(Path::new("/etc/stack-stack"))
        .to_path_buf();
    let stacks_dir = PathBuf::from(&state.config.stacks_dir);
    let archive_path = temp_archive.clone();

    let res = tokio::task::spawn_blocking(move || {
        let runner = SystemRunner::default();
        system_backup::restore_system_backup(&runner, &archive_path, &config_dir, &stacks_dir)
    })
    .await
    .map_err(|err| ApiError::internal(format!("restore task panicked: {err}")))?;

    let _ = fs::remove_file(&temp_archive);
    res?;

    state.audit(
        "system.backup",
        "import",
        "restored configurations and stacks",
    );

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": "system configuration and compose stacks restored successfully"
    })))
}
