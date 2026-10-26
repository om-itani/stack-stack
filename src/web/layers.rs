//! Cross-cutting request layers: structured tracing and an API-wide deadline.
//!
//! Both are plain `axum::middleware::from_fn` middleware, so no `tower-http`
//! dependency is pulled in. The deadline deliberately skips WebSocket upgrades,
//! which are long-lived by design.

use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::ErrorResponse;

/// Deadline for one API request: long enough for a slow Docker or `compose` call,
/// short enough that a wedged dependency cannot hold a connection open.
const API_TIMEOUT: Duration = Duration::from_secs(30);

/// True when the request is a WebSocket upgrade. Those outlive the request
/// deadline by design (terminals, logs, telemetry) and must be exempt.
fn is_websocket_upgrade(request: &Request) -> bool {
    request
        .headers()
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
}

/// Log each API request with method, path, status and duration. Failures are
/// logged at `warn` so a single request can be traced through a multi-step
/// operation (create stack → pull → up) without raising the level globally.
pub async fn trace_requests(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started = Instant::now();

    let response = next.run(request).await;

    let status = response.status();
    let elapsed_ms = started.elapsed().as_millis() as u64;
    if status.is_server_error() {
        tracing::warn!(%method, %path, status = status.as_u16(), elapsed_ms, "request failed");
    } else if path.starts_with("/api/") {
        tracing::debug!(%method, %path, status = status.as_u16(), elapsed_ms, "request");
    }
    response
}

/// Fail an API request that runs past [`API_TIMEOUT`] with a 504, instead of
/// leaving the client waiting on a hung dependency.
pub async fn timeout_requests(request: Request, next: Next) -> Response {
    if is_websocket_upgrade(&request) {
        return next.run(request).await;
    }

    let path = request.uri().path().to_string();
    match tokio::time::timeout(API_TIMEOUT, next.run(request)).await {
        Ok(response) => response,
        Err(_) => {
            tracing::warn!(%path, timeout_s = API_TIMEOUT.as_secs(), "api request timed out");
            (
                StatusCode::GATEWAY_TIMEOUT,
                Json(ErrorResponse::new("Timeout", "request timed out")),
            )
                .into_response()
        }
    }
}
