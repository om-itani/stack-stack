//! HTTP surface. Everything the browser talks to lives behind `/api`; the
//! fallback serves the embedded frontend.

pub mod apps;
pub mod assets;
pub mod containers;
pub mod network;
pub mod virtualization;
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
        .route(
            "/api/network",
            get(network::state).put(network::update_config),
        )
        .route("/api/network/preview", post(network::preview))
        .route("/api/network/apply", post(network::apply))
        .route("/api/network/confirm", post(network::confirm))
        .route(
            "/api/network/certificates",
            get(network::certificate_list).post(network::certificate_import),
        )
        .route(
            "/api/network/certificates/self-signed",
            post(network::certificate_generate),
        )
        .route(
            "/api/network/certificates/{name}",
            delete(network::certificate_remove),
        )
        .route(
            "/api/virt",
            get(virtualization::state).put(virtualization::update_config),
        )
        .route("/api/virt/preview/vm", post(virtualization::preview_vm))
        .route("/api/virt/preview/ct", post(virtualization::preview_ct))
        .route("/api/virt/vms", post(virtualization::create_vm))
        .route("/api/virt/vms/{id}/start", post(virtualization::vm_start))
        .route("/api/virt/vms/{id}/stop", post(virtualization::vm_stop))
        .route(
            "/api/virt/vms/{id}/shutdown",
            post(virtualization::vm_shutdown),
        )
        .route("/api/virt/vms/{id}/reboot", post(virtualization::vm_reboot))
        .route(
            "/api/virt/vms/{id}/destroy",
            post(virtualization::vm_destroy),
        )
        .route(
            "/api/virt/vms/{id}/snapshot",
            post(virtualization::vm_snapshot),
        )
        .route("/api/virt/vms/{id}/resize", post(virtualization::vm_resize))
        .route("/api/virt/vms/{id}", delete(virtualization::vm_delete))
        .route(
            "/api/virt/vms/{id}/console/ws",
            get(virtualization::vm_console_ws),
        )
        .route("/api/virt/cts", post(virtualization::create_ct))
        .route("/api/virt/cts/{id}/start", post(virtualization::ct_start))
        .route("/api/virt/cts/{id}/stop", post(virtualization::ct_stop))
        .route("/api/virt/cts/{id}/kill", post(virtualization::ct_kill))
        .route(
            "/api/virt/cts/{id}/password",
            post(virtualization::ct_password),
        )
        .route("/api/virt/cts/{id}", delete(virtualization::ct_delete))
        .route(
            "/api/virt/cts/{id}/console/ws",
            get(virtualization::ct_console_ws),
        )
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
