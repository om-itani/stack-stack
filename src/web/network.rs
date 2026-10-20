//! HTTP surface for the router.
//!
//! Reads gather the stored document, live interfaces, daemon status and
//! certificates in one call so the page renders in a single round trip.
//! Everything that forks a process happens on the blocking pool.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::apply::PendingInfo;
use crate::networking::certs::{self, CertInfo, CertUpload};
use crate::networking::interfaces::{self, InterfaceInfo};
use crate::networking::model::{NetworkConfig, Problem};
use crate::networking::status::{self, NetworkStatus};
use crate::networking::{self, NetworkContext, PreviewBundle, store};
use crate::state::AppState;

/// Everything the network page needs for its first paint.
#[derive(Debug, Clone, Serialize)]
pub struct NetworkState {
    pub config: NetworkConfig,
    /// Validation problems for the stored document, if any.
    pub problems: Vec<Problem>,
    /// Set when `network.json` exists but could not be parsed.
    pub config_error: Option<String>,
    pub interfaces: Vec<InterfaceInfo>,
    pub status: NetworkStatus,
    pub certificates: Vec<CertInfo>,
    /// Present while a firewall change is still reversible.
    pub pending: Option<PendingInfo>,
}

#[derive(Serialize)]
struct ValidationError {
    error: String,
    problems: Vec<Problem>,
}

#[derive(Serialize)]
pub struct ConfirmResult {
    pub ok: bool,
    pub generation: u64,
}

#[derive(Serialize)]
pub struct OkResult {
    pub ok: bool,
}

#[derive(Deserialize)]
pub struct SelfSignedRequest {
    pub name: String,
}

pub async fn state(State(state): State<AppState>) -> Result<Json<NetworkState>, ApiError> {
    Ok(Json(collect(state.network.clone()).await?))
}

/// Persist the document. Rejects an invalid configuration before it can be
/// written, so a bad save can never become the file the next boot loads.
pub async fn update_config(
    State(state): State<AppState>,
    Json(config): Json<NetworkConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid(problems));
    }

    let ctx = state.network.clone();
    persist(&ctx, &config).await?;
    Ok(Json(collect(ctx).await?).into_response())
}

/// Render every subsystem's config without touching the system.
pub async fn preview(Json(config): Json<NetworkConfig>) -> Json<PreviewBundle> {
    Json(networking::preview(&config))
}

/// Persist, then push the configuration to the system.
pub async fn apply(
    State(state): State<AppState>,
    Json(config): Json<NetworkConfig>,
) -> Result<Response, ApiError> {
    // The generators emit absolute POSIX paths and drive nft/dnsmasq/unbound.
    // Applying anywhere else would splatter files into nonsense locations, so
    // refuse rather than half-succeed. Saving and previewing work anywhere.
    if !cfg!(unix) {
        return Err(ApiError::unavailable(
            "applying network configuration requires a unix host; save and preview are available everywhere",
        ));
    }

    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid(problems));
    }

    let ctx = state.network.clone();
    persist(&ctx, &config).await?;
    let report = ctx.apply.apply(&config).await?;
    Ok(Json(report).into_response())
}

/// Accept a firewall change, cancelling the automatic rollback.
pub async fn confirm(State(state): State<AppState>) -> Result<Json<ConfirmResult>, ApiError> {
    let generation = state.network.apply.confirm().await?;
    Ok(Json(ConfirmResult {
        ok: true,
        generation,
    }))
}

pub async fn certificate_list(
    State(state): State<AppState>,
) -> Result<Json<Vec<CertInfo>>, ApiError> {
    let ctx = state.network.clone();
    let certificates = tokio::task::spawn_blocking(move || {
        let (config, _) = store::load(&ctx.config_path);
        certs::list(&config, ctx.runner.as_ref())
    })
    .await
    .map_err(|err| ApiError::internal(format!("certificate task failed: {err}")))?;
    Ok(Json(certificates))
}

pub async fn certificate_import(
    State(state): State<AppState>,
    Json(upload): Json<CertUpload>,
) -> Result<Json<CertInfo>, ApiError> {
    let ctx = state.network.clone();
    let info = tokio::task::spawn_blocking(move || {
        let (config, _) = store::load(&ctx.config_path);
        certs::import(&config, ctx.runner.as_ref(), &upload)
    })
    .await
    .map_err(|err| ApiError::internal(format!("certificate task failed: {err}")))??;
    Ok(Json(info))
}

pub async fn certificate_generate(
    State(state): State<AppState>,
    Json(request): Json<SelfSignedRequest>,
) -> Result<Json<CertInfo>, ApiError> {
    let ctx = state.network.clone();
    let info = tokio::task::spawn_blocking(move || {
        let (config, _) = store::load(&ctx.config_path);
        certs::generate_self_signed(&config, ctx.runner.as_ref(), &request.name)
    })
    .await
    .map_err(|err| ApiError::internal(format!("certificate task failed: {err}")))??;
    Ok(Json(info))
}

pub async fn certificate_remove(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<OkResult>, ApiError> {
    let ctx = state.network.clone();
    tokio::task::spawn_blocking(move || {
        let (config, _) = store::load(&ctx.config_path);
        certs::remove(&config, &name)
    })
    .await
    .map_err(|err| ApiError::internal(format!("certificate task failed: {err}")))??;
    Ok(Json(OkResult { ok: true }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Store the document, then gather the fresh state around it.
async fn persist(ctx: &NetworkContext, config: &NetworkConfig) -> Result<(), ApiError> {
    let path = ctx.config_path.clone();
    let config = config.clone();
    tokio::task::spawn_blocking(move || store::save(&path, &config))
        .await
        .map_err(|err| ApiError::internal(format!("config save task failed: {err}")))?
}

async fn collect(ctx: NetworkContext) -> Result<NetworkState, ApiError> {
    // Take the pending firewall window before moving the context into the task.
    let pending = ctx.apply.pending().await;

    tokio::task::spawn_blocking(move || {
        let (config, config_error) = store::load(&ctx.config_path);
        let problems = config.validate();
        let interfaces = interfaces::discover(ctx.runner.as_ref());
        let status = status::probe(&config, ctx.runner.as_ref());
        let certificates = certs::list(&config, ctx.runner.as_ref());
        NetworkState {
            config,
            problems,
            config_error,
            interfaces,
            status,
            certificates,
            pending,
        }
    })
    .await
    .map_err(|err| ApiError::internal(format!("network probe failed: {err}")))
}

fn invalid(problems: Vec<Problem>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ValidationError {
            error: "configuration is not valid".to_string(),
            problems,
        }),
    )
        .into_response()
}
