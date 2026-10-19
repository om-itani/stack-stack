//! HTTP surface. Everything the browser talks to lives behind `/api`; the
//! fallback serves the embedded frontend.

pub mod assets;
pub mod ws;

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::state::AppState;
use crate::system::metrics::Snapshot;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/system/snapshot", get(system_snapshot))
        .route("/api/ws/telemetry", get(ws::telemetry_ws))
        .fallback(assets::serve)
        .with_state(state)
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
