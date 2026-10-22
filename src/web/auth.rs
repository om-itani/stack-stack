//! HTTP surface for the admin gate: setup, login, logout, session, password.
//!
//! The session token travels in an `HttpOnly`, `SameSite=Lax` cookie so it is
//! never readable from JavaScript and never sent cross-site. `Secure` is
//! deliberately **not** set: the same session has to work over the plain
//! `:8553` listener as well as the `:443` TLS front door, and a `Secure` cookie
//! would simply be dropped on the former.
//!
//! The middleware [`gate`] is the single choke point: every `/api/*` route
//! except health and the auth bootstrap endpoints requires a live session.

use axum::Json;
use axum::extract::{Request, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::auth::{AuthContext, COOKIE_NAME};
use crate::error::{ApiError, ErrorResponse};
use crate::state::AppState;

/// Routes reachable without a session. Everything else under `/api/` is gated.
fn is_exempt(path: &str) -> bool {
    matches!(
        path,
        "/api/health" | "/api/auth/setup" | "/api/auth/login" | "/api/auth/session"
    )
}

/// The login gate. Non-`/api/` paths (the embedded SPA) pass through so the
/// login screen itself can load; the API is what is protected.
pub async fn gate(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let is_upgrade = request.headers().get("upgrade").is_some();
    if !path.starts_with("/api/") || is_exempt(&path) {
        if is_upgrade {
            tracing::debug!(path, "websocket upgrade exempt from auth gate");
        }
        return next.run(request).await;
    }

    if let Some(token) = cookie_value(request.headers(), COOKIE_NAME)
        && state.auth.validate_token(&token)
    {
        if is_upgrade {
            tracing::debug!(path, "websocket upgrade authenticated via session cookie");
        }
        return next.run(request).await;
    }

    if is_upgrade {
        tracing::warn!(
            path,
            "websocket upgrade rejected by auth gate (no valid session cookie)"
        );
    }
    let status = state.auth.status(false);
    let message = if status.setup_required {
        "setup required"
    } else if status.error.is_some() {
        "the admin credential document is unusable"
    } else {
        "authentication required"
    };
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorResponse {
            error: message.to_string(),
        }),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PasswordRequest {
    pub password: String,
}

#[derive(Deserialize)]
pub struct ChangeRequest {
    pub current: String,
    pub new_password: String,
}

/// First-run credential creation. On success the operator is logged straight in.
pub async fn setup(
    State(state): State<AppState>,
    Json(request): Json<PasswordRequest>,
) -> Result<Response, ApiError> {
    let auth = state.auth.clone();
    let password = request.password;
    tokio::task::spawn_blocking(move || auth.setup(&password))
        .await
        .map_err(|err| ApiError::internal(format!("auth task failed: {err}")))??;

    Ok(logged_in(&state.auth, state.auth.issue_session()))
}

pub async fn login(
    State(state): State<AppState>,
    Json(request): Json<PasswordRequest>,
) -> Result<Response, ApiError> {
    let auth = state.auth.clone();
    let password = request.password;
    let token = tokio::task::spawn_blocking(move || auth.login(&password))
        .await
        .map_err(|err| ApiError::internal(format!("auth task failed: {err}")))??;

    Ok(logged_in(&state.auth, token))
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie_value(&headers, COOKIE_NAME) {
        state.auth.logout(&token);
    }
    let mut response = Json(state.auth.status(false)).into_response();
    response.headers_mut().insert(SET_COOKIE, clear_cookie());
    response
}

/// Boot check for the SPA. Exempt from the gate, but reports the real session
/// state when a cookie is present.
pub async fn session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Json<crate::auth::AuthStatus> {
    let authenticated =
        cookie_value(&headers, COOKIE_NAME).is_some_and(|token| state.auth.validate_token(&token));
    Json(state.auth.status(authenticated))
}

/// Change the admin password. Drops every session, then re-issues one for the
/// caller so the current tab stays logged in.
pub async fn change_password(
    State(state): State<AppState>,
    Json(request): Json<ChangeRequest>,
) -> Result<Response, ApiError> {
    let auth = state.auth.clone();
    let current = request.current;
    let new_password = request.new_password;
    tokio::task::spawn_blocking(move || auth.change_password(&current, &new_password))
        .await
        .map_err(|err| ApiError::internal(format!("auth task failed: {err}")))??;

    Ok(logged_in(&state.auth, state.auth.issue_session()))
}

// ---------------------------------------------------------------------------
// Cookie helpers
// ---------------------------------------------------------------------------

/// Read one cookie value out of the `Cookie` header.
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(COOKIE)?.to_str().ok()?;
    for pair in raw.split(';') {
        let pair = pair.trim();
        if let Some((key, value)) = pair.split_once('=')
            && key.trim() == name
        {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn session_cookie(auth: &AuthContext, token: String) -> HeaderValue {
    let max_age = auth.ttl.as_secs();
    let value = format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}");
    HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn clear_cookie() -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// A successful auth response: fresh cookie plus the current status.
fn logged_in(auth: &AuthContext, token: String) -> Response {
    let mut response = Json(auth.status(true)).into_response();
    response
        .headers_mut()
        .insert(SET_COOKIE, session_cookie(auth, token));
    response
}

/// Drop expired sessions on a timer so the in-memory map cannot grow without
/// bound between accesses. Runs for the life of the process.
pub fn spawn_session_sweeper(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(600));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await; // consume the immediate first tick
        loop {
            ticker.tick().await;
            let removed = state.auth.sessions.sweep();
            if removed > 0 {
                tracing::debug!(removed, "swept expired admin sessions");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::IntoHeaderName;

    fn header(name: impl IntoHeaderName, value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_str(value).expect("header"));
        headers
    }

    #[test]
    fn exempt_paths_are_exactly_the_bootstrap_set() {
        assert!(is_exempt("/api/health"));
        assert!(is_exempt("/api/auth/setup"));
        assert!(is_exempt("/api/auth/login"));
        assert!(is_exempt("/api/auth/session"));
        assert!(!is_exempt("/api/auth/logout"));
        assert!(!is_exempt("/api/auth/password"));
        assert!(!is_exempt("/api/containers"));
    }

    #[test]
    fn cookie_parsing_picks_the_right_pair() {
        let headers = header(COOKIE, "other=1; stack_stack_session=abc123; theme=dark");
        assert_eq!(
            cookie_value(&headers, COOKIE_NAME).as_deref(),
            Some("abc123")
        );
        assert_eq!(cookie_value(&headers, "theme").as_deref(), Some("dark"));
        assert_eq!(cookie_value(&headers, "missing"), None);
        // An empty value is treated as absent.
        assert_eq!(
            cookie_value(&header(COOKIE, "stack_stack_session="), COOKIE_NAME),
            None
        );
    }

    #[test]
    fn cookie_attributes_are_pinned() {
        let auth = AuthContext::new(
            std::path::PathBuf::from("/tmp/x"),
            std::sync::Arc::new(crate::runner::fake::FakeRunner::new()),
            std::time::Duration::from_secs(3600),
        );
        let value = session_cookie(&auth, "tok".to_string());
        let rendered = value.to_str().expect("ascii");
        assert!(rendered.contains("HttpOnly"));
        assert!(rendered.contains("SameSite=Lax"));
        assert!(rendered.contains("Max-Age=3600"));
        assert!(rendered.contains("Path=/"));
        assert!(!rendered.contains("Secure"), "must work on plain :8553 too");

        assert!(
            clear_cookie()
                .to_str()
                .expect("ascii")
                .contains("Max-Age=0")
        );
    }
}
