//! Developer / diagnostics surface.
//!
//! Backs the hidden Developer page: enumerate the read routes, ping them all
//! from the server (re-using the live router so the answer is the real handler,
//! including the auth gate), and read back the stored configuration documents
//! so an operator can confirm what the backend actually saved.
//!
//! `ping` re-enters this router with the caller's own session cookie, so it
//! reports the same statuses the browser would see.

use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Request, header};
use serde::{Deserialize, Serialize};
use tower::ServiceExt;

use crate::config::Config;
use crate::error::ApiError;
use crate::state::AppState;

/// Per-route ceiling for a ping, so one wedged dependency cannot stall the
/// whole report.
const PING_TIMEOUT: Duration = Duration::from_secs(8);

/// Read routes the ping exercises, in a stable display order. Websocket routes
/// and routes needing a path parameter are excluded.
const GET_ROUTES: &[&str] = &[
    "/api/health",
    "/api/system/snapshot",
    "/api/host/ip",
    "/api/auth/session",
    "/api/notifications",
    "/api/jobs",
    "/api/alerts",
    "/api/healthchecks",
    "/api/containers",
    "/api/containers/stacks",
    "/api/apps/catalog",
    "/api/network",
    "/api/network/blocklists",
    "/api/proxy",
    "/api/ca",
    "/api/files",
    "/api/files/accounts",
    "/api/samba",
    "/api/nfs",
    "/api/backup",
    "/api/wireguard",
    "/api/tasks",
    "/api/updates",
    "/api/storage",
    "/api/smart",
    "/api/smart/progress",
    "/api/logs",
    "/api/ddns",
    "/api/virt",
    "/api/virt/images",
];

/// Configuration documents an operator may inspect, excluding secret material
/// (`auth.json` and the `*.secrets.json` files).
fn config_paths(cfg: &Config) -> Vec<(&'static str, String)> {
    vec![
        ("network", cfg.network_path.clone()),
        ("tasks", cfg.tasks_path.clone()),
        ("alerts", cfg.alerts_path.clone()),
        ("health", cfg.health_path.clone()),
        ("notify", cfg.notify_path.clone()),
        ("files", cfg.files_path.clone()),
        ("proxy", cfg.proxy_path.clone()),
        ("updates", cfg.updates_path.clone()),
        ("virtualization", cfg.virt_path.clone()),
        ("samba", cfg.samba_path.clone()),
        ("nfs", cfg.nfs_path.clone()),
        ("backup", cfg.backup_path.clone()),
        ("ddns", cfg.ddns_path.clone()),
        ("wireguard", cfg.wg_path.clone()),
        ("smart", cfg.smart_path.clone()),
        ("guest_catalog", cfg.guest_catalog_path.clone()),
    ]
}

#[derive(Serialize)]
pub struct RouteRef {
    pub method: &'static str,
    pub path: &'static str,
}

#[derive(Serialize)]
pub struct RoutesState {
    pub routes: Vec<RouteRef>,
}

/// The list of read routes the ping covers.
pub async fn routes() -> Json<RoutesState> {
    Json(RoutesState {
        routes: GET_ROUTES
            .iter()
            .map(|path| RouteRef {
                method: "GET",
                path,
            })
            .collect(),
    })
}

#[derive(Serialize)]
pub struct PingResult {
    pub method: &'static str,
    pub path: &'static str,
    /// HTTP status, or 0 when the request could not be completed.
    pub status: u16,
    pub ok: bool,
    pub ms: u64,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct PingReport {
    pub results: Vec<PingResult>,
    pub ok_count: usize,
    pub fail_count: usize,
}

/// Hit every read route through the live router and report the statuses.
pub async fn ping(State(state): State<AppState>, headers: HeaderMap) -> Json<PingReport> {
    let cookie = headers.get(header::COOKIE).cloned();
    let router = crate::web::router(state.clone());
    let mut results = Vec::with_capacity(GET_ROUTES.len());

    for path in GET_ROUTES {
        let started = Instant::now();
        let mut builder = Request::builder().method("GET").uri(*path);
        if let Some(cookie) = &cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        let request = match builder.body(Body::empty()) {
            Ok(request) => request,
            Err(err) => {
                results.push(PingResult {
                    method: "GET",
                    path,
                    status: 0,
                    ok: false,
                    ms: 0,
                    error: Some(err.to_string()),
                });
                continue;
            }
        };

        match tokio::time::timeout(PING_TIMEOUT, router.clone().oneshot(request)).await {
            Ok(Ok(response)) => {
                let status = response.status();
                results.push(PingResult {
                    method: "GET",
                    path,
                    status: status.as_u16(),
                    ok: status.is_success(),
                    ms: started.elapsed().as_millis() as u64,
                    error: None,
                });
            }
            Ok(Err(err)) => results.push(PingResult {
                method: "GET",
                path,
                status: 0,
                ok: false,
                ms: started.elapsed().as_millis() as u64,
                error: Some(err.to_string()),
            }),
            Err(_) => results.push(PingResult {
                method: "GET",
                path,
                status: 0,
                ok: false,
                ms: started.elapsed().as_millis() as u64,
                error: Some("timed out".to_string()),
            }),
        }
    }

    let ok_count = results.iter().filter(|result| result.ok).count();
    let fail_count = results.len() - ok_count;
    Json(PingReport {
        results,
        ok_count,
        fail_count,
    })
}

#[derive(Serialize)]
pub struct ConfigRef {
    pub name: &'static str,
    pub path: String,
    pub exists: bool,
    pub bytes: Option<u64>,
    pub modified_ms: Option<u64>,
}

#[derive(Serialize)]
pub struct ConfigsState {
    pub configs: Vec<ConfigRef>,
}

/// The inspectable configuration documents and whether each exists.
pub async fn configs(State(state): State<AppState>) -> Json<ConfigsState> {
    let configs = config_paths(&state.config)
        .into_iter()
        .map(|(name, path)| {
            let metadata = std::fs::metadata(&path).ok();
            ConfigRef {
                name,
                exists: metadata.is_some(),
                bytes: metadata.as_ref().map(|meta| meta.len()),
                modified_ms: metadata
                    .and_then(|meta| meta.modified().ok())
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_millis() as u64),
                path,
            }
        })
        .collect();
    Json(ConfigsState { configs })
}

#[derive(Deserialize)]
pub struct ConfigQuery {
    pub name: String,
}

#[derive(Serialize)]
pub struct ConfigBody {
    pub name: String,
    pub path: String,
    /// The parsed document, so a malformed file is an error rather than a blob.
    pub json: serde_json::Value,
}

/// The raw (but parsed) JSON of one configuration document.
pub async fn config(
    State(state): State<AppState>,
    Query(query): Query<ConfigQuery>,
) -> Result<Json<ConfigBody>, ApiError> {
    let Some((_, path)) = config_paths(&state.config)
        .into_iter()
        .find(|(name, _)| *name == query.name)
    else {
        return Err(ApiError::not_found(format!(
            "no inspectable config named {}",
            query.name
        )));
    };

    let raw = std::fs::read_to_string(&path)
        .map_err(|err| ApiError::not_found(format!("cannot read {path}: {err}")))?;
    let json: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|err| ApiError::internal(format!("{path} is not valid JSON: {err}")))?;
    Ok(Json(ConfigBody {
        name: query.name,
        path,
        json,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_list_excludes_secrets() {
        let names: Vec<&str> = config_paths(&Config::default())
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert!(names.contains(&"network"));
        assert!(names.contains(&"tasks"));
        assert!(!names.contains(&"auth"), "auth.json must not be exposed");
        assert!(!names.iter().any(|name| name.contains("secrets")));
    }

    #[tokio::test]
    async fn ping_reaches_the_health_route_through_the_router() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let router = crate::web::router(AppState::new(Config::default()));
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn ping_route_list_is_unique_and_prefixed() {
        let mut seen = GET_ROUTES.to_vec();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "ping routes must be unique");
        for path in GET_ROUTES {
            assert!(path.starts_with("/api/"), "{path} must be an API path");
            assert!(!path.contains('{'), "{path} must not need a parameter");
        }
    }
}
