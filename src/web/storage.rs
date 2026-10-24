//! Storage page: physical disks and the format/mount operations on them.
//!
//! Every operation runs on the blocking pool because they shell out to tools
//! that can take minutes (`mkfs` on a large disk). The handler stays thin; the
//! guards and validation live in `system::storage`.

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AppState;
use crate::system::storage::StorageState;

#[derive(Serialize)]
pub struct StorageResult {
    pub ok: bool,
    pub message: String,
}

pub async fn state(State(state): State<AppState>) -> Result<Json<StorageState>, ApiError> {
    let storage = state.storage.clone();
    let snapshot = tokio::task::spawn_blocking(move || storage.state())
        .await
        .map_err(|err| ApiError::internal(format!("storage task failed: {err}")))??;
    Ok(Json(snapshot))
}

#[derive(Deserialize)]
pub struct FormatRequest {
    pub device: String,
    pub fs: String,
}

/// Format a disk. Destructive, so the confirmation happens in the UI and the
/// device is validated again here.
pub async fn format(
    State(state): State<AppState>,
    Json(request): Json<FormatRequest>,
) -> Result<Json<StorageResult>, ApiError> {
    let storage = state.storage.clone();
    let device = request.device.clone();
    let fs = request.fs.clone();
    let message = tokio::task::spawn_blocking(move || storage.format(&device, &fs))
        .await
        .map_err(|err| ApiError::internal(format!("format task failed: {err}")))??;
    state.audit("storage.format", &request.device, &request.fs);
    Ok(Json(StorageResult { ok: true, message }))
}

#[derive(Deserialize)]
pub struct MountRequest {
    pub device: String,
    pub path: String,
    #[serde(default)]
    pub persist: bool,
}

pub async fn mount(
    State(state): State<AppState>,
    Json(request): Json<MountRequest>,
) -> Result<Json<StorageResult>, ApiError> {
    let storage = state.storage.clone();
    let device = request.device.clone();
    let path = request.path.clone();
    let persist = request.persist;
    let message = tokio::task::spawn_blocking(move || storage.mount(&device, &path, persist))
        .await
        .map_err(|err| ApiError::internal(format!("mount task failed: {err}")))??;
    state.audit(
        "storage.mount",
        &request.device,
        &format!("{} (persist={persist})", request.path),
    );
    Ok(Json(StorageResult { ok: true, message }))
}

#[derive(Deserialize)]
pub struct UnmountRequest {
    /// Mount point or device path to unmount.
    pub target: String,
}

pub async fn unmount(
    State(state): State<AppState>,
    Json(request): Json<UnmountRequest>,
) -> Result<Json<StorageResult>, ApiError> {
    let storage = state.storage.clone();
    let target = request.target.clone();
    let message = tokio::task::spawn_blocking(move || storage.unmount(&target))
        .await
        .map_err(|err| ApiError::internal(format!("unmount task failed: {err}")))??;
    state.audit("storage.unmount", &request.target, "");
    Ok(Json(StorageResult { ok: true, message }))
}
