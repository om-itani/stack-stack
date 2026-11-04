//! HTTP surface for per-app secrets management.
//!
//! Exposes key listing (names only, values are redacted), adding/updating secrets,
//! and deletion.

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::services::secrets;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct SecretSummary {
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct SecretsResponse {
    pub secrets: Vec<SecretSummary>,
}

#[derive(Debug, Deserialize)]
pub struct SetSecretRequest {
    pub name: String,
    pub value: String,
}

/// List all secret keys. Secret values are NEVER returned in this response.
pub async fn list(State(state): State<AppState>) -> Result<Json<SecretsResponse>, ApiError> {
    let path = std::path::PathBuf::from(&state.config.app_secrets_path);
    let secrets = tokio::task::spawn_blocking(move || secrets::load_secrets(&path))
        .await
        .map_err(|err| ApiError::internal(format!("failed to load secrets: {err}")))?;

    let mut names: Vec<String> = secrets.secrets.keys().cloned().collect();
    names.sort();

    Ok(Json(SecretsResponse {
        secrets: names
            .into_iter()
            .map(|name| SecretSummary { name })
            .collect(),
    }))
}

/// Add or update a secret key-value pair.
pub async fn set(
    State(state): State<AppState>,
    Json(payload): Json<SetSecretRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let name = payload.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request("secret name cannot be empty"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(ApiError::bad_request(
            "secret name can only contain letters, numbers, underscores and dashes",
        ));
    }

    let path = std::path::PathBuf::from(&state.config.app_secrets_path);
    tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
        let mut current = secrets::load_secrets(&path);
        current.secrets.insert(name.clone(), payload.value);
        secrets::save_secrets(&path, &current)?;
        tracing::info!(secret = %name, "stored application secret");
        Ok(())
    })
    .await
    .map_err(|err| ApiError::internal(format!("failed to save secret: {err}")))??;

    Ok(Json(serde_json::json!({ "status": "ok" })))
}

/// Delete a secret.
pub async fn remove(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let path = std::path::PathBuf::from(&state.config.app_secrets_path);
    tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
        let mut current = secrets::load_secrets(&path);
        if current.secrets.remove(&name).is_some() {
            secrets::save_secrets(&path, &current)?;
            tracing::info!(secret = %name, "removed application secret");
        }
        Ok(())
    })
    .await
    .map_err(|err| ApiError::internal(format!("failed to remove secret: {err}")))??;

    Ok(Json(serde_json::json!({ "status": "ok" })))
}
