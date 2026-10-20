//! HTTP surface. Everything the browser talks to lives behind `/api`; the
//! fallback serves the embedded frontend.

pub mod apps;
pub mod assets;
pub mod containers;
pub mod ws;

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Serialize;

use crate::containers::host;
use crate::containers::model::HostIp;
use crate::state::AppState;
use crate::system::metrics::Snapshot;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/system/snapshot", get(system_snapshot))
        .route("/api/ws/telemetry", get(ws::telemetry_ws))
        .route("/api/host/ip", get(host_ip))
        .route("/api/containers", get(containers::list))
        .route("/api/containers/{id}/start", post(containers::start))
        .route("/api/containers/{id}/stop", post(containers::stop))
        .route("/api/containers/{id}/restart", post(containers::restart))
        .route("/api/containers/{id}", delete(containers::remove))
        .route("/api/containers/{id}/logs/ws", get(containers::logs_ws))
        .route("/api/containers/{id}/exec/ws", get(containers::exec_ws))
        .route("/api/apps/catalog", get(apps::catalog))
        .route("/api/apps/{app}/install", post(apps::install))
        .route("/api/apps/{app}", delete(apps::remove))
        .fallback(assets::serve)
        .with_state(state)
}

/// Best-effort primary IPv4 of this host, used by the UI to build links.
async fn host_ip() -> Json<HostIp> {
    // Routes are cheap but the fallback reads /proc, so keep it off the workers.
    match tokio::task::spawn_blocking(host::detect).await {
        Ok(host) => Json(host),
        Err(err) => {
            tracing::warn!(%err, "host ip detection failed");
            Json(HostIp {
                ip: "0.0.0.0".to_string(),
            })
        }
    }
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
    uptime_s: u64,
    /// Connected telemetry sockets; a quick "is the fan-out alive" signal.
    subscribers: usize,
    has_snapshot: bool,
}

/// Liveness probe. Used by the deploy script and by `systemctl` smoke tests:
/// `curl -fsS http://127.0.0.1:8553/api/health`.
async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        uptime_s: state.uptime_s(),
        subscribers: state.telemetry.receiver_count(),
        has_snapshot: state.latest_snapshot().is_some(),
    })
}

/// Latest sample as plain JSON, for the frontend's first paint and for curl.
/// Null until the first tick has completed. Returns the shared `Arc` rather than
/// deep-cloning the snapshot: `Arc<Snapshot>` serialises transparently.
async fn system_snapshot(State(state): State<AppState>) -> Json<Option<Arc<Snapshot>>> {
    Json(state.latest_snapshot())
}
