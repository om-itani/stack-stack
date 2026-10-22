//! HTTP surface. Everything the browser talks to lives behind `/api`; the
//! fallback serves the embedded frontend.

pub mod apps;
pub mod assets;
pub mod auth;
pub mod backup;
pub mod containers;
pub mod ddns;
pub mod files;
pub mod logs;
pub mod network;
pub mod notify;
pub mod proxy;
pub mod shares;
pub mod shell_ws;
pub mod smart;
pub mod virtualization;
pub mod wireguard;
pub mod ws;

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router, middleware};
use serde::Serialize;

use crate::containers::host;
use crate::containers::model::HostIp;
use crate::state::AppState;
use crate::system::metrics::Snapshot;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/auth/setup", post(auth::setup))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/session", get(auth::session))
        .route("/api/auth/password", post(auth::change_password))
        .route("/api/notifications", get(notify::feed))
        .route("/api/notifications/clear", post(notify::clear))
        .route("/api/notifications/config", put(notify::update_config))
        .route("/api/notifications/test", post(notify::test))
        .route("/api/notifications/ws", get(notify::ws))
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
        // Static `stacks` wins over the `{id}` parameter route above in matchit,
        // so both can coexist.
        .route(
            "/api/containers/stacks",
            get(containers::list_stacks).post(containers::create_stack),
        )
        .route(
            "/api/containers/stacks/{name}",
            delete(containers::delete_stack),
        )
        .route("/api/apps/catalog", get(apps::catalog))
        .route("/api/apps/hub-search", get(apps::hub_search))
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
        .route("/api/proxy", get(proxy::state).put(proxy::save))
        .route("/api/proxy/routes", post(proxy::create_route))
        .route(
            "/api/proxy/routes/{id}",
            put(proxy::update_route).delete(proxy::delete_route),
        )
        .route("/api/proxy/preflight", post(proxy::preflight))
        .route("/api/proxy/enable", post(proxy::enable))
        .route("/api/ca", get(proxy::ca_state))
        .route("/api/ca/init", post(proxy::ca_init))
        .route("/api/ca/download", get(proxy::ca_download))
        .route("/api/ca/issue", post(proxy::ca_issue))
        .route("/api/ca/leaves/{name}", delete(proxy::ca_remove))
        .route("/api/files", get(files::state))
        .route("/api/files/config", put(files::save_config))
        .route("/api/files/read", get(files::read))
        .route("/api/files/download", get(files::download))
        .route("/api/files/archive", get(files::archive))
        .route("/api/files/accounts", get(files::accounts))
        .route("/api/files/mkdir", post(files::mkdir))
        .route("/api/files/file", post(files::create_file))
        .route("/api/files/save", post(files::save))
        .route("/api/files/rename", post(files::rename))
        .route("/api/files/delete", post(files::delete))
        .route("/api/files/chmod", post(files::chmod))
        .route("/api/files/chown", post(files::chown))
        .route("/api/files/extract", post(files::extract))
        .route("/api/files/upload", post(files::upload))
        .route("/api/samba", get(shares::samba_state))
        .route("/api/samba/config", put(shares::samba_save))
        .route("/api/samba/apply", post(shares::samba_apply))
        .route("/api/samba/users", post(shares::samba_set_user))
        .route("/api/samba/users/{name}", delete(shares::samba_remove_user))
        .route("/api/nfs", get(shares::nfs_state))
        .route("/api/nfs/config", put(shares::nfs_save))
        .route("/api/nfs/apply", post(shares::nfs_apply))
        .route("/api/backup", get(backup::state))
        .route("/api/backup/config", put(backup::save_config))
        .route("/api/backup/test", post(backup::test))
        .route("/api/backup/jobs/{id}/run", post(backup::run_job))
        .route("/api/backup/jobs/{id}/verify", post(backup::verify))
        .route("/api/backup/jobs/{id}/secrets", post(backup::save_secrets))
        .route("/api/backup/jobs/{id}/snapshots", get(backup::snapshots))
        .route(
            "/api/backup/jobs/{id}/snapshots/{snapshot}/ls",
            get(backup::browse),
        )
        .route("/api/backup/jobs/{id}/restore", post(backup::restore))
        .route("/api/backup/jobs/{id}/download", get(backup::download))
        .route("/api/wireguard", get(wireguard::state))
        .route("/api/wireguard/config", put(wireguard::save))
        .route("/api/wireguard/apply", post(wireguard::apply))
        .route("/api/wireguard/key", post(wireguard::generate_key))
        .route("/api/wireguard/key/server", post(wireguard::set_server_key))
        .route("/api/wireguard/export", post(wireguard::export_client))
        .route("/api/wireguard/peers", post(wireguard::add_peer))
        .route(
            "/api/wireguard/peers/{name}",
            delete(wireguard::remove_peer),
        )
        .route("/api/shell/ws", get(shell_ws::shell_ws))
        .route("/api/smart", get(smart::state))
        .route("/api/smart/config", put(smart::save_config))
        .route("/api/smart/{device}", post(smart::start))
        .route("/api/smart/progress", get(smart::progress))
        .route("/api/logs", get(logs::state))
        .route("/api/logs/units", get(logs::state))
        .route("/api/logs/ws", get(logs::ws))
        .route("/api/ddns", get(ddns::state))
        .route("/api/ddns/config", put(ddns::save_config))
        .route("/api/ddns/token", post(ddns::save_token))
        .route("/api/ddns/update", post(ddns::update))
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
        // The login gate wraps the whole router: non-`/api/` paths (the SPA)
        // pass straight through, everything else is checked against a session.
        .layer(middleware::from_fn_with_state(state.clone(), auth::gate))
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
    /// Live admin sessions; never more than one or two by design.
    sessions: usize,
}

/// Liveness probe. Used by the deploy script and by `systemctl` smoke tests:
/// `curl -fsS http://127.0.0.1:8553/api/health`. Deliberately exempt from the
/// auth gate so the deploy health check needs no credentials.
async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        uptime_s: state.uptime_s(),
        subscribers: state.telemetry.receiver_count(),
        has_snapshot: state.latest_snapshot().is_some(),
        sessions: state.auth.sessions.len(),
    })
}

/// Latest sample as plain JSON, for the frontend's first paint and for curl.
/// Null until the first tick has completed. Returns the shared `Arc` rather than
/// deep-cloning the snapshot: `Arc<Snapshot>` serialises transparently.
async fn system_snapshot(State(state): State<AppState>) -> Json<Option<Arc<Snapshot>>> {
    Json(state.latest_snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// Building the router registers every path with `matchit`, which panics on
    /// a conflicting route (e.g. a static segment colliding with a parameter at
    /// the same position). This test fails loudly if that ever happens, instead
    /// of the process aborting on boot.
    #[test]
    fn router_registers_all_routes_without_conflict() {
        let _router = router(AppState::new(Config::default()));
    }
}
