//! HTTP surface for multi-protocol zero-config remote access (Tailscale, NetBird, Cloudflare).

use axum::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;

use crate::services::remote_access::{
    self, NetBirdUpRequest, RemoteAccessOverview, TailscaleUpRequest,
};

#[derive(Serialize)]
pub struct ActionResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub async fn status() -> Json<RemoteAccessOverview> {
    Json(remote_access::get_overview())
}

pub async fn tailscale_up(Json(payload): Json<TailscaleUpRequest>) -> impl IntoResponse {
    match remote_access::tailscale_up(&payload) {
        Ok(msg) => (
            StatusCode::OK,
            Json(ActionResult {
                ok: true,
                message: Some(msg),
                error: None,
            }),
        ),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(ActionResult {
                ok: false,
                message: None,
                error: Some(err),
            }),
        ),
    }
}

pub async fn tailscale_down() -> impl IntoResponse {
    match remote_access::tailscale_down() {
        Ok(msg) => (
            StatusCode::OK,
            Json(ActionResult {
                ok: true,
                message: Some(msg),
                error: None,
            }),
        ),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(ActionResult {
                ok: false,
                message: None,
                error: Some(err),
            }),
        ),
    }
}

pub async fn netbird_up(Json(payload): Json<NetBirdUpRequest>) -> impl IntoResponse {
    match remote_access::netbird_up(&payload) {
        Ok(msg) => (
            StatusCode::OK,
            Json(ActionResult {
                ok: true,
                message: Some(msg),
                error: None,
            }),
        ),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(ActionResult {
                ok: false,
                message: None,
                error: Some(err),
            }),
        ),
    }
}

pub async fn netbird_down() -> impl IntoResponse {
    match remote_access::netbird_down() {
        Ok(msg) => (
            StatusCode::OK,
            Json(ActionResult {
                ok: true,
                message: Some(msg),
                error: None,
            }),
        ),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(ActionResult {
                ok: false,
                message: None,
                error: Some(err),
            }),
        ),
    }
}
