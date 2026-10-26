//! HTTP surface for the reverse proxy and the internal CA.
//!
//! Reads gather the proxy document, live validation, front-door state and CA
//! inventory in one call. Mutations are transactional in the useful sense: a
//! route is validated and persisted before its certificate is issued and its DNS
//! rewrite is written, and every side effect is reported as a step so a missing
//! `openssl` or `unbound` is visible rather than silent.

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::ca::{CaInfo, LeafInfo, TrustInstruction};
use crate::error::ApiError;
use crate::networking::apply::StepResult;
use crate::networking::model::Problem;
use crate::networking::store;
use crate::proxy::{self, ProxyConfig, ProxyRoute, ProxyTls, Upstream};
use crate::state::AppState;
use crate::tls::{self, PortCheck};

/// Everything the proxy page needs for its first paint.
#[derive(Debug, Clone, Serialize)]
pub struct ProxyState {
    pub config: ProxyConfig,
    pub problems: Vec<Problem>,
    pub config_error: Option<String>,
    /// Whether the `:80`/`:443` listeners are currently running.
    pub running: bool,
    pub admin_hostname: String,
    pub http_port: u16,
    pub https_port: u16,
    pub preflight: Vec<PortCheck>,
    /// At least one certificate is loaded and HTTPS can complete a handshake.
    pub tls_ready: bool,
    pub ca: CaInfo,
    pub certificates: Vec<LeafInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteReport {
    pub state: ProxyState,
    pub steps: Vec<StepResult>,
}

#[derive(Debug, Serialize)]
pub struct CaState {
    pub info: CaInfo,
    pub leaves: Vec<LeafInfo>,
    pub trust: Vec<TrustInstruction>,
}

#[derive(Serialize)]
pub struct OkResult {
    pub ok: bool,
}

#[derive(Deserialize)]
pub struct EnableRequest {
    pub enabled: bool,
}

#[derive(Deserialize)]
pub struct IssueRequest {
    pub hostname: String,
    #[serde(default)]
    pub extra_dns: Vec<String>,
    #[serde(default)]
    pub ip: Option<String>,
}

#[derive(Serialize)]
struct ValidationError {
    error: String,
    problems: Vec<Problem>,
}

// ---------------------------------------------------------------------------
// Proxy state
// ---------------------------------------------------------------------------

pub async fn state(State(state): State<AppState>) -> Result<Json<ProxyState>, ApiError> {
    Ok(Json(collect(&state).await?))
}

/// Persist a whole document (validated), refresh TLS, but perform no per-route
/// side effects. Route create/update/delete use the finer endpoints instead.
pub async fn save(
    State(state): State<AppState>,
    Json(config): Json<ProxyConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid(problems));
    }
    let path = PathBuf::from(&state.config.proxy_path);
    let config_for_write = config.clone();
    tokio::task::spawn_blocking(move || proxy::save(&path, &config_for_write))
        .await
        .map_err(|err| ApiError::internal(format!("proxy save task failed: {err}")))??;

    tls::refresh(&state)?;
    Ok(Json(collect(&state).await?).into_response())
}

pub async fn create_route(
    State(state): State<AppState>,
    Json(route): Json<ProxyRoute>,
) -> Result<Response, ApiError> {
    let route = with_defaults(route);
    let mut config = load_config(&state);
    if config.find_by_id(&route.id).is_some() {
        return Err(ApiError::bad_request(format!(
            "a route with id {:?} already exists",
            route.id
        )));
    }
    config.routes.push(route);
    Ok(Json(upsert(&state, config).await?).into_response())
}

pub async fn update_route(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(route): Json<ProxyRoute>,
) -> Result<Response, ApiError> {
    let mut config = load_config(&state);
    let slot = config
        .routes
        .iter_mut()
        .find(|existing| existing.id == id)
        .ok_or_else(|| ApiError::not_found(format!("no proxy route with id {id:?}")))?;
    // A rename is handled by the full DNS reconcile inside `upsert`: rewrites
    // for hostnames no longer routed are dropped, the new one is added.
    *slot = route;
    Ok(Json(upsert(&state, config).await?).into_response())
}

pub async fn delete_route(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    let mut config = load_config(&state);
    let before = config.routes.len();
    config.routes.retain(|route| route.id != id);
    if config.routes.len() == before {
        return Err(ApiError::not_found(format!(
            "no proxy route with id {id:?}"
        )));
    }
    Ok(Json(upsert(&state, config).await?).into_response())
}

pub async fn preflight(State(state): State<AppState>) -> Json<Vec<PortCheck>> {
    Json(tls::preflight(&state))
}

/// Turn the front door on or off. Enabling refuses when a port is occupied so a
/// box already serving 80/443 is never taken over by surprise.
pub async fn enable(
    State(state): State<AppState>,
    Json(request): Json<EnableRequest>,
) -> Result<Response, ApiError> {
    if request.enabled {
        let checks = tls::preflight(&state);
        if checks.iter().any(|check| !check.available) {
            return Ok((
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "Conflict",
                    "message": "the front-door ports are not free",
                    "preflight": checks,
                })),
            )
                .into_response());
        }
        tls::ensure_admin_cert(&state)?;
        tls::refresh(&state)?;
        persist_enabled(&state, true)?;
        state.tls.supervisor.start(state.clone()).await?;
    } else {
        persist_enabled(&state, false)?;
        state.tls.supervisor.stop().await;
    }
    Ok(Json(collect(&state).await?).into_response())
}

// ---------------------------------------------------------------------------
// CA
// ---------------------------------------------------------------------------

pub async fn ca_state(State(state): State<AppState>) -> Result<Json<CaState>, ApiError> {
    let ca = state.tls.ca.clone();
    let payload = tokio::task::spawn_blocking(move || CaState {
        info: ca.info(),
        leaves: ca.list(),
        trust: crate::ca::trust_instructions(),
    })
    .await
    .map_err(|err| ApiError::internal(format!("ca task failed: {err}")))?;
    Ok(Json(payload))
}

pub async fn ca_init(State(state): State<AppState>) -> Result<Json<CaInfo>, ApiError> {
    let ca = state.tls.ca.clone();
    let info = tokio::task::spawn_blocking(move || ca.ensure_root())
        .await
        .map_err(|err| ApiError::internal(format!("ca task failed: {err}")))??;
    tls::refresh(&state)?;
    Ok(Json(info))
}

pub async fn ca_download(State(state): State<AppState>) -> Result<Response, ApiError> {
    let path = state.tls.ca.root_cert_path();
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(&path))
        .await
        .map_err(|err| ApiError::internal(format!("ca task failed: {err}")))?
        .map_err(|err| {
            ApiError::not_found(format!("the CA certificate is not available yet: {err}"))
        })?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-x509-ca-cert"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"stack-stack-ca.crt\"",
            ),
        ],
        bytes,
    )
        .into_response())
}

pub async fn ca_issue(
    State(state): State<AppState>,
    Json(request): Json<IssueRequest>,
) -> Result<Json<LeafInfo>, ApiError> {
    let ca = state.tls.ca.clone();
    let leaf = tokio::task::spawn_blocking(move || {
        ca.issue_leaf(&request.hostname, &request.extra_dns, request.ip.as_deref())
    })
    .await
    .map_err(|err| ApiError::internal(format!("ca task failed: {err}")))??;
    tls::refresh(&state)?;
    Ok(Json(leaf))
}

pub async fn ca_remove(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<OkResult>, ApiError> {
    let ca = state.tls.ca.clone();
    tokio::task::spawn_blocking(move || ca.remove(&name))
        .await
        .map_err(|err| ApiError::internal(format!("ca task failed: {err}")))??;
    tls::refresh(&state)?;
    Ok(Json(OkResult { ok: true }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Fill in a route's id and hostname when the client left them blank, so the
/// UI can offer a one-click "add this container" without inventing a name.
fn with_defaults(mut route: ProxyRoute) -> ProxyRoute {
    if route.hostname.trim().is_empty() {
        let name = match &route.upstream {
            Upstream::Container { id, .. } => id.clone(),
            Upstream::Url { url } => proxy::parse_upstream_url(url)
                .map(|parsed| parsed.host)
                .unwrap_or_else(|| "app".to_string()),
        };
        route.hostname = proxy::default_hostname(&name);
    }
    if route.id.trim().is_empty() {
        route.id = proxy::normalise_host(&route.hostname).replace('.', "-");
    }
    route
}

fn load_config(state: &AppState) -> ProxyConfig {
    let path = Path::new(&state.config.proxy_path);
    let (config, error) = proxy::load(path);
    if let Some(error) = error {
        tracing::warn!(%error, "proxy document could not be parsed, starting from an empty one");
    }
    config
}

/// Validate, persist, then issue certificates, reconcile DNS rewrites and
/// refresh the TLS map. Every side effect is a reported step.
async fn upsert(state: &AppState, config: ProxyConfig) -> Result<RouteReport, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Err(ApiError::bad_request(
            problems
                .iter()
                .map(|problem| format!("{}: {}", problem.field, problem.message))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }

    let path = PathBuf::from(&state.config.proxy_path);
    let for_write = config.clone();
    tokio::task::spawn_blocking(move || proxy::save(&path, &for_write))
        .await
        .map_err(|err| ApiError::internal(format!("proxy save task failed: {err}")))??;

    let mut steps = Vec::new();

    // Certificates: issue a leaf for every CA-managed route that lacks a fresh
    // one. A failure is reported but never blocks the route itself.
    let due: Vec<String> = {
        let leaves = state.tls.ca.list();
        config
            .routes
            .iter()
            .filter(|route| route.enabled && route.tls == ProxyTls::CaManaged)
            .map(|route| route.hostname.clone())
            .filter(|host| {
                leaves
                    .iter()
                    .find(|leaf| leaf.hostname.eq_ignore_ascii_case(host))
                    .is_none_or(|leaf| leaf.needs_renewal)
            })
            .collect()
    };
    if due.is_empty() {
        steps.push(ok_step(
            "certificates",
            "all CA-managed routes have fresh certificates",
        ));
    } else {
        let ca = state.tls.ca.clone();
        let ip = crate::containers::host::detect().ip;
        let ip = (ip != "0.0.0.0").then_some(ip);
        let issued = tokio::task::spawn_blocking(move || {
            let mut lines = Vec::new();
            for host in &due {
                match ca.issue_leaf(host, &[], ip.as_deref()) {
                    Ok(_) => lines.push(host.clone()),
                    Err(err) => {
                        return Err(format!("could not issue a certificate for {host}: {err}"));
                    }
                }
            }
            Ok(lines)
        })
        .await
        .map_err(|err| ApiError::internal(format!("certificate task failed: {err}")))?;

        match issued {
            Ok(lines) => steps.push(ok_step(
                "certificates",
                format!("issued/renewed {}", lines.join(", ")),
            )),
            Err(detail) => steps.push(StepResult {
                subsystem: "certificates".to_string(),
                ok: false,
                detail,
            }),
        }
    }

    steps.push(reconcile_dns(state, &config).await);
    tls::refresh(state)?;
    steps.push(ok_step(
        "tls",
        format!(
            "{} hostnames in the certificate map",
            state.tls.resolver.len()
        ),
    ));

    Ok(RouteReport {
        state: collect(state).await?,
        steps,
    })
}

/// Rewrite the router document's local rewrites so every route hostname points
/// at this box, then apply only the DNS subsystem.
async fn reconcile_dns(state: &AppState, proxy_config: &ProxyConfig) -> StepResult {
    let network_path = PathBuf::from(&state.config.network_path);
    let ip = crate::containers::host::detect().ip;
    let wanted: Vec<String> = proxy_config
        .routes
        .iter()
        .filter(|route| route.enabled)
        .map(|route| proxy::normalise_host(&route.hostname))
        .collect();

    let (network, _) = store::load(&network_path);

    if ip == "0.0.0.0" && !wanted.is_empty() {
        return StepResult {
            subsystem: "dns".to_string(),
            ok: false,
            detail: "cannot determine this host's LAN address; local rewrites were not written"
                .to_string(),
        };
    }

    // Everything that is not one of our route rewrites is left untouched; the
    // route rewrites are rebuilt from scratch, which also retracts a rename.
    let owned = |host: &str| {
        proxy_config
            .routes
            .iter()
            .any(|route| route.hostname.eq_ignore_ascii_case(host))
    };
    let mut next: Vec<crate::networking::model::DnsRewrite> = network
        .dns
        .rewrites
        .iter()
        .filter(|rewrite| !owned(&rewrite.host))
        .cloned()
        .collect();
    for host in &wanted {
        next.push(crate::networking::model::DnsRewrite {
            host: host.clone(),
            address: ip.clone(),
            comment: "stack-stack proxy route".to_string(),
        });
    }

    if next == network.dns.rewrites {
        return ok_step("dns", "local rewrites already current");
    }
    let mut network = network;
    network.dns.rewrites = next;

    if let Err(err) = store::save(&network_path, &network) {
        return StepResult {
            subsystem: "dns".to_string(),
            ok: false,
            detail: format!("cannot write {}: {err}", network_path.display()),
        };
    }

    match state.network.apply.apply_dns(&network).await {
        Ok(step) => step,
        Err(err) => StepResult {
            subsystem: "dns".to_string(),
            ok: false,
            detail: err.to_string(),
        },
    }
}

/// Write `tls_enabled` back to the config file so the choice survives a restart.
fn persist_enabled(state: &AppState, enabled: bool) -> Result<(), ApiError> {
    let path = std::env::var("STACK_STACK_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(crate::config::DEFAULT_CONFIG_PATH));
    let mut config = match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str::<crate::config::Config>(&raw).unwrap_or_default(),
        Err(_) => crate::config::Config::default(),
    };
    config.tls_enabled = enabled;
    let mut body = serde_json::to_vec_pretty(&config)
        .map_err(|err| ApiError::internal(format!("cannot serialise config: {err}")))?;
    body.push(b'\n');
    store::write_atomic(&path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))?;
    tracing::info!(enabled, path = %path.display(), "front door toggled");
    let _ = state;
    Ok(())
}

fn ok_step(subsystem: &str, detail: impl Into<String>) -> StepResult {
    StepResult {
        subsystem: subsystem.to_string(),
        ok: true,
        detail: detail.into(),
    }
}

async fn collect(state: &AppState) -> Result<ProxyState, ApiError> {
    let path = PathBuf::from(&state.config.proxy_path);
    let (config, config_error) = proxy::load(&path);
    let problems = config.validate();
    let running = state.tls.supervisor.running().await;
    let preflight = tls::preflight(state);
    let ca = state.tls.ca.clone();
    let info = tokio::task::spawn_blocking(move || ca.info())
        .await
        .map_err(|err| ApiError::internal(format!("ca task failed: {err}")))?;
    let ca2 = state.tls.ca.clone();
    let certificates = tokio::task::spawn_blocking(move || ca2.list())
        .await
        .map_err(|err| ApiError::internal(format!("ca task failed: {err}")))?;

    Ok(ProxyState {
        config,
        problems,
        config_error,
        running,
        admin_hostname: state.tls.admin_hostname.clone(),
        http_port: state.tls.http_port,
        https_port: state.tls.https_port,
        preflight,
        tls_ready: !state.tls.resolver.is_empty(),
        ca: info,
        certificates,
    })
}

fn invalid(problems: Vec<Problem>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ValidationError {
            error: "proxy configuration is not valid".to_string(),
            problems,
        }),
    )
        .into_response()
}
