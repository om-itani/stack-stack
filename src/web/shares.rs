//! HTTP surface for Samba and NFS shares.
//!
//! One page, two documents. Each GET returns the document, its live validation,
//! the rendered config (so the operator sees exactly what will be written) and
//! the service's own reported state. Applying validates, writes, checks with the
//! tool's parser and restarts — reporting a step for each.

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::apply::StepResult;
use crate::networking::ipmath::Ipv4Net;
use crate::networking::model::Problem;
use crate::networking::store;
use crate::services::nfs::{self, ActiveExport, NfsConfig};
use crate::services::samba::{self, SmbConfig, SmbUserStatus};
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct SambaState {
    pub config: SmbConfig,
    pub problems: Vec<Problem>,
    pub config_error: Option<String>,
    /// Exactly what `smb.conf` will contain.
    pub preview: String,
    pub lan_cidr: Option<String>,
    pub users: Vec<SmbUserStatus>,
    /// Set when `pdbedit` could not be run (Samba not installed).
    pub users_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct NfsState {
    pub config: NfsConfig,
    pub problems: Vec<Problem>,
    pub config_error: Option<String>,
    pub preview: String,
    pub active: Vec<ActiveExport>,
    pub active_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SharesReport<T> {
    pub state: T,
    pub steps: Vec<StepResult>,
}

#[derive(Deserialize)]
pub struct UserRequest {
    pub username: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Serialize)]
pub struct OkResult {
    pub ok: bool,
}

#[derive(Serialize)]
struct ValidationError {
    error: String,
    problems: Vec<Problem>,
}

// ---------------------------------------------------------------------------
// Samba
// ---------------------------------------------------------------------------

pub async fn samba_state(State(state): State<AppState>) -> Json<SambaState> {
    Json(collect_samba(&state))
}

pub async fn samba_save(
    State(state): State<AppState>,
    Json(config): Json<SmbConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid("samba configuration is not valid", problems));
    }
    let path = state.samba.config_path.clone();
    let for_write = config.clone();
    tokio::task::spawn_blocking(move || samba::save(&path, &for_write))
        .await
        .map_err(|err| ApiError::internal(format!("samba task failed: {err}")))??;
    Ok(Json(collect_samba(&state)).into_response())
}

pub async fn samba_apply(
    State(state): State<AppState>,
    Json(config): Json<SmbConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid("samba configuration is not valid", problems));
    }
    let path = state.samba.config_path.clone();
    let for_write = config.clone();
    tokio::task::spawn_blocking(move || samba::save(&path, &for_write))
        .await
        .map_err(|err| ApiError::internal(format!("samba task failed: {err}")))??;

    let lan_cidr = lan_cidr(&state);
    let steps = samba::apply(&state.samba, &config, lan_cidr).await;
    Ok(Json(SharesReport {
        state: collect_samba(&state),
        steps,
    })
    .into_response())
}

pub async fn samba_set_user(
    State(state): State<AppState>,
    Json(request): Json<UserRequest>,
) -> Result<Json<OkResult>, ApiError> {
    let runner = state.samba.runner.clone();
    let username = request.username.trim().to_string();
    if username.is_empty() {
        return Err(ApiError::bad_request("a username is required"));
    }
    let password = request.password;
    let enabled = request.enabled;

    tokio::task::spawn_blocking(move || {
        if let Some(password) = password.as_deref().filter(|value| !value.is_empty()) {
            samba::set_password(runner.as_ref(), &username, password)?;
        }
        if let Some(enabled) = enabled {
            samba::set_enabled(runner.as_ref(), &username, enabled)?;
        }
        Ok::<(), ApiError>(())
    })
    .await
    .map_err(|err| ApiError::internal(format!("samba task failed: {err}")))??;

    Ok(Json(OkResult { ok: true }))
}

pub async fn samba_remove_user(
    State(state): State<AppState>,
    AxumPath(username): AxumPath<String>,
) -> Result<Json<OkResult>, ApiError> {
    let runner = state.samba.runner.clone();
    tokio::task::spawn_blocking(move || samba::delete_user(runner.as_ref(), &username))
        .await
        .map_err(|err| ApiError::internal(format!("samba task failed: {err}")))??;
    Ok(Json(OkResult { ok: true }))
}

// ---------------------------------------------------------------------------
// NFS
// ---------------------------------------------------------------------------

pub async fn nfs_state(State(state): State<AppState>) -> Json<NfsState> {
    Json(collect_nfs(&state))
}

pub async fn nfs_save(
    State(state): State<AppState>,
    Json(config): Json<NfsConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid("nfs configuration is not valid", problems));
    }
    let path = state.nfs.config_path.clone();
    let for_write = config.clone();
    tokio::task::spawn_blocking(move || nfs::save(&path, &for_write))
        .await
        .map_err(|err| ApiError::internal(format!("nfs task failed: {err}")))??;
    Ok(Json(collect_nfs(&state)).into_response())
}

pub async fn nfs_apply(
    State(state): State<AppState>,
    Json(config): Json<NfsConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid("nfs configuration is not valid", problems));
    }
    let path = state.nfs.config_path.clone();
    let for_write = config.clone();
    tokio::task::spawn_blocking(move || nfs::save(&path, &for_write))
        .await
        .map_err(|err| ApiError::internal(format!("nfs task failed: {err}")))??;

    let steps = nfs::apply(&state.nfs, &config).await;
    Ok(Json(SharesReport {
        state: collect_nfs(&state),
        steps,
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn collect_samba(state: &AppState) -> SambaState {
    let path = Path::new(&state.samba.config_path);
    let (config, config_error) = samba::load(path);
    let problems = config.validate();
    let lan = lan_cidr(state);
    let preview = samba::render(&config, lan.as_deref());
    let (users, users_error) = match samba::list_users(state.samba.runner.as_ref()) {
        Ok(users) => (users, None),
        Err(err) => (Vec::new(), Some(err)),
    };
    SambaState {
        config,
        problems,
        config_error,
        preview,
        lan_cidr: lan,
        users,
        users_error,
    }
}

fn collect_nfs(state: &AppState) -> NfsState {
    let path = Path::new(&state.nfs.config_path);
    let (config, config_error) = nfs::load(path);
    let problems = config.validate();
    let preview = config.render();
    let (active, active_error) = match nfs::active_exports(state.nfs.runner.as_ref()) {
        Ok(active) => (active, None),
        Err(err) => (Vec::new(), Some(err)),
    };
    NfsState {
        config,
        problems,
        config_error,
        preview,
        active,
        active_error,
    }
}

/// The LAN network in CIDR form, from the router document's first addressed
/// interface or bridge. Used for Samba's `hosts allow`.
fn lan_cidr(state: &AppState) -> Option<String> {
    let path = PathBuf::from(&state.config.network_path);
    let (config, _) = store::load(&path);
    for bridge in &config.bridges {
        if let Some(address) = bridge.address.as_deref()
            && let Ok(net) = Ipv4Net::parse(address)
        {
            return Some(net.network_cidr());
        }
    }
    None
}

fn invalid(error: &str, problems: Vec<Problem>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ValidationError {
            error: error.to_string(),
            problems,
        }),
    )
        .into_response()
}
