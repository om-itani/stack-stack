use axum::extract::{Path, State};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};

use crate::auth::{Role, UserSummary};
use crate::error::ApiError;
use crate::state::AppState;
use crate::web::auth::AuthUser;

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub role: Role,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRoleRequest {
    pub role: Role,
}

#[derive(Debug, Deserialize)]
pub struct SetPasswordRequest {
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct OkResponse {
    pub ok: bool,
}

pub async fn list_users(State(state): State<AppState>) -> Result<Json<Vec<UserSummary>>, ApiError> {
    let auth = state.auth.clone();
    let users = tokio::task::spawn_blocking(move || auth.list_users())
        .await
        .map_err(|err| ApiError::internal(format!("user task failed: {err}")))??;
    Ok(Json(users))
}

pub async fn create_user(
    State(state): State<AppState>,
    Json(payload): Json<CreateUserRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    let auth = state.auth.clone();
    tokio::task::spawn_blocking(move || {
        auth.add_user(&payload.username, &payload.password, payload.role)
    })
    .await
    .map_err(|err| ApiError::internal(format!("user task failed: {err}")))??;
    Ok(Json(OkResponse { ok: true }))
}

pub async fn update_user_role(
    State(state): State<AppState>,
    Path(username): Path<String>,
    Json(payload): Json<UpdateRoleRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    let auth = state.auth.clone();
    tokio::task::spawn_blocking(move || auth.update_user_role(&username, payload.role))
        .await
        .map_err(|err| ApiError::internal(format!("user task failed: {err}")))??;
    Ok(Json(OkResponse { ok: true }))
}

pub async fn set_user_password(
    State(state): State<AppState>,
    Path(username): Path<String>,
    Json(payload): Json<SetPasswordRequest>,
) -> Result<Json<OkResponse>, ApiError> {
    let auth = state.auth.clone();
    tokio::task::spawn_blocking(move || auth.set_user_password(&username, &payload.password))
        .await
        .map_err(|err| ApiError::internal(format!("user task failed: {err}")))??;
    Ok(Json(OkResponse { ok: true }))
}

pub async fn delete_user(
    State(state): State<AppState>,
    Extension(caller): Extension<AuthUser>,
    Path(username): Path<String>,
) -> Result<Json<OkResponse>, ApiError> {
    let auth = state.auth.clone();
    tokio::task::spawn_blocking(move || auth.delete_user(&username, &caller.username))
        .await
        .map_err(|err| ApiError::internal(format!("user task failed: {err}")))??;
    Ok(Json(OkResponse { ok: true }))
}
