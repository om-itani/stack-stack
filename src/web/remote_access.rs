//! HTTP surface for multi-protocol zero-config remote access (Tailscale, NetBird, Cloudflare).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;

use crate::services::remote_access::{
    self, NetBirdUpRequest, RemoteAccessOverview, TailscaleUpRequest,
};
use crate::state::AppState;

#[derive(Serialize)]
pub struct ActionResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ActionResult {
    pub fn ok(msg: impl Into<String>) -> Self {
        Self {
            ok: true,
            job_id: None,
            message: Some(msg.into()),
            error: None,
        }
    }

    pub fn err(err: impl Into<String>) -> Self {
        Self {
            ok: false,
            job_id: None,
            message: None,
            error: Some(err.into()),
        }
    }

    pub fn job(job_id: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            ok: true,
            job_id: Some(job_id.into()),
            message: Some(msg.into()),
            error: None,
        }
    }
}

pub async fn status() -> Json<RemoteAccessOverview> {
    Json(remote_access::get_overview())
}

pub async fn tailscale_install(State(state): State<AppState>) -> Json<ActionResult> {
    let job_id = state
        .jobs
        .spawn("Installing Tailscale", move |ctx| async move {
            ctx.step(15, "running official Tailscale package installation");
            tokio::task::spawn_blocking(remote_access::tailscale_install)
                .await
                .map_err(|e| e.to_string())?
        });
    state.audit(
        "remote_access.tailscale_install",
        "tailscale",
        "1-click installation triggered (background)",
    );
    Json(ActionResult::job(
        job_id,
        "Tailscale installation initiated in the background",
    ))
}

pub async fn netbird_install(State(state): State<AppState>) -> Json<ActionResult> {
    let job_id = state
        .jobs
        .spawn("Installing NetBird", move |ctx| async move {
            ctx.step(15, "running official NetBird package installation");
            tokio::task::spawn_blocking(remote_access::netbird_install)
                .await
                .map_err(|e| e.to_string())?
        });
    state.audit(
        "remote_access.netbird_install",
        "netbird",
        "1-click installation triggered (background)",
    );
    Json(ActionResult::job(
        job_id,
        "NetBird installation initiated in the background",
    ))
}

pub async fn tailscale_up(Json(payload): Json<TailscaleUpRequest>) -> impl IntoResponse {
    match remote_access::tailscale_up(&payload) {
        Ok(msg) => (StatusCode::OK, Json(ActionResult::ok(msg))),
        Err(err) => (StatusCode::BAD_REQUEST, Json(ActionResult::err(err))),
    }
}

pub async fn tailscale_down() -> impl IntoResponse {
    match remote_access::tailscale_down() {
        Ok(msg) => (StatusCode::OK, Json(ActionResult::ok(msg))),
        Err(err) => (StatusCode::BAD_REQUEST, Json(ActionResult::err(err))),
    }
}

pub async fn netbird_up(Json(payload): Json<NetBirdUpRequest>) -> impl IntoResponse {
    match remote_access::netbird_up(&payload) {
        Ok(msg) => (StatusCode::OK, Json(ActionResult::ok(msg))),
        Err(err) => (StatusCode::BAD_REQUEST, Json(ActionResult::err(err))),
    }
}

pub async fn netbird_down() -> impl IntoResponse {
    match remote_access::netbird_down() {
        Ok(msg) => (StatusCode::OK, Json(ActionResult::ok(msg))),
        Err(err) => (StatusCode::BAD_REQUEST, Json(ActionResult::err(err))),
    }
}
