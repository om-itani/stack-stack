//! HTTP surface for WireGuard.

use std::path::Path;

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::containers::host;
use crate::error::ApiError;
use crate::networking::apply::StepResult;
use crate::networking::model::Problem;
use crate::services::wireguard::{self, InterfaceStatus, WgConfig, WgPeer, WgSecrets, is_wg_key};
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct WgState {
    pub config: WgConfig,
    pub problems: Vec<Problem>,
    pub config_error: Option<String>,
    pub preview: String,
    /// True once a server private key exists.
    pub has_key: bool,
    /// Server public key, derived from the private key (`wg pubkey`).
    pub public_key: Option<String>,
    pub public_key_error: Option<String>,
    pub status: Option<InterfaceStatus>,
    pub status_error: Option<String>,
    /// `lan-ip:port` clients should dial.
    pub endpoint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ApplyReport {
    pub state: WgState,
    pub steps: Vec<StepResult>,
}

#[derive(Serialize)]
pub struct OkResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Deserialize)]
pub struct KeyRequest {
    /// Persist as the server key instead of returning it for a client export.
    #[serde(default)]
    pub as_server: bool,
}

#[derive(Serialize)]
pub struct GeneratedKey {
    /// Shown once; never stored (unless `as_server` was set).
    pub private_key: String,
    pub public_key: String,
}

#[derive(Deserialize)]
pub struct ExportRequest {
    pub peer_name: String,
    /// Tunnel address handed to the client, inside the interface network.
    pub address: String,
}

#[derive(Serialize)]
pub struct ClientExport {
    pub config: String,
    pub qr: Option<String>,
    pub qr_error: Option<String>,
}

#[derive(Deserialize)]
pub struct StatusQuery {
    pub live: Option<bool>,
}

#[derive(Serialize)]
struct ValidationError {
    error: String,
    problems: Vec<Problem>,
}

pub async fn state(
    State(state): State<AppState>,
    Query(query): Query<StatusQuery>,
) -> Json<WgState> {
    Json(collect(&state, query.live.unwrap_or(true)))
}

pub async fn save(
    State(state): State<AppState>,
    Json(config): Json<WgConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid(problems));
    }
    let path = state.wireguard.config_path.clone();
    let for_write = config.clone();
    tokio::task::spawn_blocking(move || wireguard::save(&path, &for_write))
        .await
        .map_err(|err| ApiError::internal(format!("wireguard task failed: {err}")))??;
    Ok(Json(collect(&state, true)).into_response())
}

pub async fn apply(
    State(state): State<AppState>,
    Json(config): Json<WgConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid(problems));
    }
    let path = state.wireguard.config_path.clone();
    let for_write = config.clone();
    tokio::task::spawn_blocking(move || wireguard::save(&path, &for_write))
        .await
        .map_err(|err| ApiError::internal(format!("wireguard task failed: {err}")))??;

    let steps = wireguard::apply(&state.wireguard, &config).await;
    Ok(Json(ApplyReport {
        state: collect(&state, true),
        steps,
    })
    .into_response())
}

/// Generate a keypair: persisted as the server key, or returned once for a
/// client export.
pub async fn generate_key(
    State(state): State<AppState>,
    Json(request): Json<KeyRequest>,
) -> Result<Json<GeneratedKey>, ApiError> {
    let runner = state.wireguard.runner.clone();
    let (private_key, public_key) =
        tokio::task::spawn_blocking(move || wireguard::generate_keypair(runner.as_ref()))
            .await
            .map_err(|err| ApiError::internal(format!("wireguard task failed: {err}")))??;

    if request.as_server {
        let path = state.wireguard.secrets_path.clone();
        let stored = private_key.clone();
        tokio::task::spawn_blocking(move || {
            wireguard::save_secrets(
                &path,
                &WgSecrets {
                    private_key: stored,
                },
            )
        })
        .await
        .map_err(|err| ApiError::internal(format!("wireguard task failed: {err}")))??;
    }
    Ok(Json(GeneratedKey {
        private_key,
        public_key,
    }))
}

/// Store a pasted server private key (e.g. after a restore).
pub async fn set_server_key(
    State(state): State<AppState>,
    Json(key): Json<String>,
) -> Result<Json<OkResult>, ApiError> {
    let trimmed = key.trim().to_string();
    if !is_wg_key(&trimmed) {
        return Err(ApiError::bad_request(
            "the private key must be 44 base64 characters ending in '='",
        ));
    }
    let path = state.wireguard.secrets_path.clone();
    tokio::task::spawn_blocking(move || {
        wireguard::save_secrets(
            &path,
            &WgSecrets {
                private_key: trimmed,
            },
        )
    })
    .await
    .map_err(|err| ApiError::internal(format!("wireguard task failed: {err}")))??;
    Ok(Json(OkResult {
        ok: true,
        detail: Some("server key stored".to_string()),
    }))
}

/// Generate a one-time client configuration (and a QR code when possible).
pub async fn export_client(
    State(state): State<AppState>,
    Json(request): Json<ExportRequest>,
) -> Result<Json<ClientExport>, ApiError> {
    let config = state.wireguard.config();
    if !config.enabled {
        return Err(ApiError::bad_request(
            "enable WireGuard before exporting clients",
        ));
    }
    let secrets = tokio::task::spawn_blocking({
        let path = state.wireguard.secrets_path.clone();
        move || wireguard::load_secrets(&path)
    })
    .await
    .map_err(|err| ApiError::internal(format!("wireguard task failed: {err}")))?;
    if !is_wg_key(&secrets.private_key) {
        return Err(ApiError::bad_request(
            "generate the server key before exporting clients",
        ));
    }
    let server_public = state.wireguard.public_key().map_err(ApiError::internal)?;
    if server_public.is_empty() {
        return Err(ApiError::internal(
            "the server public key could not be derived",
        ));
    }

    let endpoint = match endpoint(&state) {
        Some(endpoint) => endpoint,
        None => {
            return Err(ApiError::bad_request(
                "the host's LAN address could not be determined; set the endpoint manually",
            ));
        }
    };
    let dns = config
        .peers
        .iter()
        .find(|peer| peer.name == request.peer_name)
        .and_then(|peer| peer.dns.clone());

    let runner = state.wireguard.runner.clone();
    let (client_private, _client_public) =
        tokio::task::spawn_blocking(move || wireguard::generate_keypair(runner.as_ref()))
            .await
            .map_err(|err| ApiError::internal(format!("wireguard task failed: {err}")))??;

    let body = wireguard::render_client(
        &client_private,
        &request.address,
        &server_public,
        &endpoint,
        dns.as_deref(),
    );

    let runner = state.wireguard.runner.clone();
    let payload = body.clone();
    let (qr, qr_error) =
        match tokio::task::spawn_blocking(move || wireguard::qr_text(runner.as_ref(), &payload))
            .await
            .map_err(|err| ApiError::internal(format!("wireguard task failed: {err}")))?
        {
            Ok(qr) => (Some(qr), None),
            Err(err) => (None, Some(err.to_string())),
        };

    Ok(Json(ClientExport {
        config: body,
        qr,
        qr_error,
    }))
}

pub async fn add_peer(
    State(state): State<AppState>,
    Json(peer): Json<WgPeer>,
) -> Result<Response, ApiError> {
    let mut config = state.wireguard.config();
    if config
        .peers
        .iter()
        .any(|existing| existing.name == peer.name)
    {
        return Err(ApiError::bad_request(format!(
            "a peer named {:?} already exists",
            peer.name
        )));
    }
    config.peers.push(peer);
    finish(&state, config).await
}

pub async fn remove_peer(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Result<Response, ApiError> {
    let mut config = state.wireguard.config();
    let before = config.peers.len();
    config.peers.retain(|peer| peer.name != name);
    if config.peers.len() == before {
        return Err(ApiError::not_found(format!("no peer named {name:?}")));
    }
    finish(&state, config).await
}

async fn finish(state: &AppState, config: WgConfig) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid(problems));
    }
    let path = state.wireguard.config_path.clone();
    let for_write = config.clone();
    tokio::task::spawn_blocking(move || wireguard::save(&path, &for_write))
        .await
        .map_err(|err| ApiError::internal(format!("wireguard task failed: {err}")))??;
    Ok(Json(collect(state, true)).into_response())
}

fn collect(state: &AppState, live: bool) -> WgState {
    let path = Path::new(&state.wireguard.config_path);
    let (config, config_error) = wireguard::load(path);
    let problems = config.validate();
    let preview = if config.enabled {
        let secrets = wireguard::load_secrets(&state.wireguard.secrets_path);
        if is_wg_key(&secrets.private_key) {
            config.render(&secrets.private_key)
        } else {
            String::from("# generate a server key to see the rendered configuration\n")
        }
    } else {
        String::from("# wireguard is disabled\n")
    };

    let has_key = is_wg_key(&wireguard::load_secrets(&state.wireguard.secrets_path).private_key);
    let (public_key, public_key_error) = if has_key {
        match state.wireguard.public_key() {
            Ok(key) if !key.is_empty() => (Some(key), None),
            Ok(_) => (None, None),
            Err(err) => (None, Some(err)),
        }
    } else {
        (None, None)
    };

    let (status, status_error) = if live && config.enabled {
        match wireguard::status(state.wireguard.runner.as_ref(), &config.interface) {
            Ok(status) => (Some(status), None),
            Err(err) => (None, Some(err)),
        }
    } else {
        (None, None)
    };

    WgState {
        config,
        problems,
        config_error,
        preview,
        has_key,
        public_key,
        public_key_error,
        status,
        status_error,
        endpoint: endpoint(state),
    }
}

/// `lan-ip:listen-port` for client endpoints.
fn endpoint(state: &AppState) -> Option<String> {
    let ip = host::detect().ip;
    if ip == "0.0.0.0" {
        return None;
    }
    Some(format!("{}:{}", ip, state.wireguard.config().listen_port))
}

fn invalid(problems: Vec<Problem>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ValidationError {
            error: "wireguard configuration is not valid".to_string(),
            problems,
        }),
    )
        .into_response()
}
