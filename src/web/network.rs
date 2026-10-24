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
use crate::networking::blocklists::{self, BlocklistStats};
use crate::networking::certs::{self, CertInfo, CertUpload};
use crate::networking::interfaces::{self, InterfaceInfo};
use crate::networking::model::{BlocklistSource, NetworkConfig, Problem};
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
    Json(mut config): Json<NetworkConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid(problems));
    }

    let ctx = state.network.clone();
    // The adblock subscriptions and allowlist are owned by the blocklist
    // endpoints, not by the DNS form. Preserve the stored values so saving an
    // unrelated network change cannot revert a blocklist edit made in between.
    preserve_blocklists(&ctx, &mut config).await;
    persist(&ctx, &config).await?;
    Ok(Json(collect(ctx).await?).into_response())
}

/// Copy the stored `dns.blocklists` / `dns.allowlist` onto an incoming document.
async fn preserve_blocklists(ctx: &NetworkContext, config: &mut NetworkConfig) {
    let path = ctx.config_path.clone();
    let Ok(stored) = tokio::task::spawn_blocking(move || store::load(&path).0).await else {
        return;
    };
    config.dns.blocklists = stored.dns.blocklists;
    config.dns.allowlist = stored.dns.allowlist;
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
// Adblock (blocklist subscriptions compiled into unbound local zones)
// ---------------------------------------------------------------------------

/// One subscription as the UI table needs it: the stored entry plus whatever the
/// last fetch recorded.
#[derive(Debug, Clone, Serialize)]
pub struct BlocklistRow {
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub builtin: bool,
    pub status: Option<String>,
    pub error: Option<String>,
    pub domains: Option<usize>,
    pub fetched_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlocklistState {
    pub sources: Vec<BlocklistRow>,
    pub allowlist: Vec<String>,
    pub total: usize,
    pub updated_at: Option<u64>,
    /// Path of the generated unbound include.
    pub compiled_file: String,
    /// False when unbound is disabled, so the UI can say the lists are inert.
    pub unbound_enabled: bool,
}

#[derive(Serialize)]
pub struct BlocklistUpdateResult {
    pub ok: bool,
    pub message: String,
    pub state: BlocklistState,
}

#[derive(Deserialize)]
pub struct SourcesRequest {
    pub sources: Vec<BlocklistSource>,
}

#[derive(Deserialize)]
pub struct AllowlistRequest {
    pub allowlist: Vec<String>,
}

#[derive(Deserialize)]
pub struct LookupQuery {
    pub domain: Option<String>,
}

#[derive(Serialize)]
pub struct LookupResult {
    pub domain: String,
    pub blocked: bool,
    pub allowed: bool,
}

/// Merge the stored subscriptions with the last fetch's per-source outcomes.
fn blocklist_state(config: &NetworkConfig, config_path: &std::path::Path) -> BlocklistState {
    let stats = BlocklistStats::load(config_path);
    let sources = config
        .dns
        .blocklists
        .iter()
        .map(|source| {
            let recorded = stats
                .sources
                .iter()
                .find(|recorded| recorded.url == source.url);
            BlocklistRow {
                name: source.name.clone(),
                url: source.url.clone(),
                enabled: source.enabled,
                builtin: source.builtin,
                status: recorded.map(|entry| entry.status.clone()),
                error: recorded.and_then(|entry| entry.error.clone()),
                domains: recorded.map(|entry| entry.domains),
                fetched_at: recorded.and_then(|entry| entry.fetched_at),
            }
        })
        .collect();
    BlocklistState {
        sources,
        allowlist: config.dns.allowlist.clone(),
        total: stats.total,
        updated_at: stats.updated_at,
        compiled_file: blocklists::compiled_path(config_path)
            .to_string_lossy()
            .into_owned(),
        unbound_enabled: config.dns.enabled,
    }
}

pub async fn blocklists_state(
    State(state): State<AppState>,
) -> Result<Json<BlocklistState>, ApiError> {
    let path = state.network.config_path.clone();
    let result = tokio::task::spawn_blocking(move || {
        let (config, _) = store::load(&path);
        blocklist_state(&config, &path)
    })
    .await
    .map_err(|err| ApiError::internal(format!("blocklist task failed: {err}")))?;
    Ok(Json(result))
}

/// Replace the subscription list. Every URL is validated before it is stored, so
/// a saved source can never become a `file://` read or an option to curl.
pub async fn blocklists_sources(
    State(state): State<AppState>,
    Json(request): Json<SourcesRequest>,
) -> Result<Json<BlocklistState>, ApiError> {
    if request.sources.len() > 64 {
        return Err(ApiError::bad_request("at most 64 blocklist sources"));
    }
    let mut sources = Vec::with_capacity(request.sources.len());
    for source in request.sources {
        blocklists::validate_url(&source.url).map_err(ApiError::bad_request)?;
        let name = source.name.trim();
        if name.is_empty() {
            return Err(ApiError::bad_request("every source needs a name"));
        }
        sources.push(BlocklistSource {
            name: name.chars().take(80).collect(),
            url: source.url.trim().to_string(),
            enabled: source.enabled,
            builtin: source.builtin,
        });
    }

    let ctx = state.network.clone();
    let path = ctx.config_path.clone();
    let saved = sources.clone();
    let result = tokio::task::spawn_blocking(move || {
        let (mut config, _) = store::load(&path);
        config.dns.blocklists = saved;
        store::save(&path, &config)?;
        Ok::<_, ApiError>(blocklist_state(&config, &path))
    })
    .await
    .map_err(|err| ApiError::internal(format!("blocklist task failed: {err}")))??;
    Ok(Json(result))
}

/// Replace the allowlist. Entries are normalised here so the stored document
/// only ever holds names that `compile` can match against.
pub async fn blocklists_allowlist(
    State(state): State<AppState>,
    Json(request): Json<AllowlistRequest>,
) -> Result<Json<BlocklistState>, ApiError> {
    if request.allowlist.len() > 4096 {
        return Err(ApiError::bad_request("at most 4096 allowlist entries"));
    }
    let mut cleaned = Vec::with_capacity(request.allowlist.len());
    for entry in request.allowlist {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let domain = blocklists::normalize_domain(entry).ok_or_else(|| {
            ApiError::bad_request(format!("{entry:?} is not a valid domain name"))
        })?;
        cleaned.push(domain);
    }
    cleaned.sort();
    cleaned.dedup();

    let path = state.network.config_path.clone();
    let result = tokio::task::spawn_blocking(move || {
        let (mut config, _) = store::load(&path);
        config.dns.allowlist = cleaned;
        store::save(&path, &config)?;
        Ok::<_, ApiError>(blocklist_state(&config, &path))
    })
    .await
    .map_err(|err| ApiError::internal(format!("blocklist task failed: {err}")))??;
    Ok(Json(result))
}

/// Fetch every enabled list, compile, write the include, reload unbound.
pub async fn blocklists_update(
    State(state): State<AppState>,
) -> Result<Json<BlocklistUpdateResult>, ApiError> {
    let ctx = state.network.clone();
    let path = ctx.config_path.clone();
    let result = tokio::task::spawn_blocking(move || {
        let (mut config, _) = store::load(&path);
        let stats = blocklists::update(&mut config, &path, ctx.runner.as_ref())?;
        store::save(&path, &config)?;
        let failed = stats
            .sources
            .iter()
            .filter(|source| source.status == "failed")
            .count();
        let message = if failed == 0 {
            format!("{} domains compiled", stats.total)
        } else {
            format!("{} domains compiled, {failed} list(s) failed", stats.total)
        };
        Ok::<_, ApiError>((message, blocklist_state(&config, &path)))
    })
    .await
    .map_err(|err| ApiError::internal(format!("blocklist task failed: {err}")))??;

    state.audit("dns.blocklist_update", "unbound", &result.0);
    Ok(Json(BlocklistUpdateResult {
        ok: true,
        message: result.0,
        state: result.1,
    }))
}

/// Is one domain in the compiled blocklist?
pub async fn blocklists_lookup(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<LookupQuery>,
) -> Result<Json<LookupResult>, ApiError> {
    let domain = query.domain.unwrap_or_default();
    let path = state.network.config_path.clone();
    let result = tokio::task::spawn_blocking(move || {
        let (config, _) = store::load(&path);
        let normalized = blocklists::normalize_domain(&domain).unwrap_or_default();
        let allowed =
            normalized.is_empty() || blocklists::is_allowed(&normalized, &config.dns.allowlist);
        let blocked =
            !normalized.is_empty() && blocklists::is_blocked(&path, &config.dns.allowlist, &domain);
        LookupResult {
            domain: normalized,
            blocked,
            allowed,
        }
    })
    .await
    .map_err(|err| ApiError::internal(format!("blocklist task failed: {err}")))?;
    Ok(Json(result))
}

/// Daily refresh of the blocklists. Mirrors the SMART scheduler: one ticker for
/// the life of the process, waking often enough to hit the configured hour
/// without a cron dependency.
pub fn spawn_blocklist_scheduler(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30 * 60));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let ctx = state.network.clone();
            let path = ctx.config_path.clone();
            let result = tokio::task::spawn_blocking(move || {
                let (mut config, _) = store::load(&path);
                let stats = BlocklistStats::load(&path);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0);
                if !blocklists::refresh_due(stats.updated_at, now, blocklists::local_hour()) {
                    return Ok::<_, ApiError>(None);
                }
                if !config.dns.blocklists.iter().any(|source| source.enabled) {
                    return Ok(None);
                }
                let stats = blocklists::update(&mut config, &path, ctx.runner.as_ref())?;
                store::save(&path, &config)?;
                Ok(Some(stats.total))
            })
            .await;

            match result {
                Ok(Ok(Some(total))) => {
                    tracing::info!(domains = total, "scheduled blocklist refresh complete");
                }
                Ok(Ok(None)) => {}
                Ok(Err(err)) => tracing::warn!(%err, "scheduled blocklist refresh failed"),
                Err(err) => tracing::warn!(%err, "blocklist refresh task failed"),
            }
        }
    });
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
