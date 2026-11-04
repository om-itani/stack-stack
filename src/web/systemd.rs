//! HTTP endpoints for host systemd services management.

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::services::systemd::{
    CreateServiceRequest, ServiceUnit, create_service, list_services, service_action,
};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ActionRequest {
    pub action: String,
}

#[derive(Debug, Serialize)]
pub struct ActionResult {
    pub ok: bool,
    pub message: String,
}

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<ServiceUnit>>, ApiError> {
    let runner = state.auth.runner.clone();
    let units = tokio::task::spawn_blocking(move || list_services(runner.as_ref()))
        .await
        .map_err(|err| ApiError::internal(format!("services task failed: {err}")))??;
    Ok(Json(units))
}

pub async fn action(
    State(state): State<AppState>,
    Path(unit): Path<String>,
    Json(payload): Json<ActionRequest>,
) -> Result<Json<ActionResult>, ApiError> {
    let runner = state.auth.runner.clone();
    let message = tokio::task::spawn_blocking(move || {
        service_action(runner.as_ref(), &unit, &payload.action)
    })
    .await
    .map_err(|err| ApiError::internal(format!("services action task failed: {err}")))??;

    Ok(Json(ActionResult { ok: true, message }))
}

pub async fn create(
    State(state): State<AppState>,
    Json(payload): Json<CreateServiceRequest>,
) -> Result<Json<ActionResult>, ApiError> {
    let runner = state.auth.runner.clone();
    let unit_dir = std::path::PathBuf::from("/etc/systemd/system");
    let name =
        tokio::task::spawn_blocking(move || create_service(runner.as_ref(), &payload, &unit_dir))
            .await
            .map_err(|err| ApiError::internal(format!("create service task failed: {err}")))??;

    Ok(Json(ActionResult {
        ok: true,
        message: format!("created service {name}"),
    }))
}
