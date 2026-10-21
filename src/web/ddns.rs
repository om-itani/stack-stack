//! HTTP surface + scheduler for Dynamic DNS.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::model::Problem;
use crate::notify::{NotificationRequest, event};
use crate::services::ddns::{self, DdnsConfig, DdnsContext, DdnsSecrets, UpdateOutcome};
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct DdnsState {
    pub config: DdnsConfig,
    pub problems: Vec<Problem>,
    pub config_error: Option<String>,
    pub has_token: bool,
    pub wan_ip: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UpdateReport {
    pub outcomes: Vec<UpdateOutcome>,
    pub state: DdnsState,
}

#[derive(Serialize)]
pub struct OkResult {
    pub ok: bool,
}

#[derive(Deserialize)]
pub struct TokenRequest {
    pub api_token: String,
}

#[derive(Serialize)]
struct ValidationError {
    error: String,
    problems: Vec<Problem>,
}

pub async fn state(State(state): State<AppState>) -> Json<DdnsState> {
    Json(collect(&state).await)
}

pub async fn save_config(
    State(state): State<AppState>,
    Json(config): Json<DdnsConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid(problems));
    }
    let path = state.ddns.config_path.clone();
    let for_write = config.clone();
    tokio::task::spawn_blocking(move || ddns::save(&path, &for_write))
        .await
        .map_err(|err| ApiError::internal(format!("ddns task failed: {err}")))??;
    Ok(Json(collect(&state).await).into_response())
}

pub async fn save_token(
    State(state): State<AppState>,
    Json(request): Json<TokenRequest>,
) -> Result<Json<OkResult>, ApiError> {
    let token = request.api_token.trim().to_string();
    if token.len() < 20 {
        return Err(ApiError::bad_request(
            "that does not look like a Cloudflare API token (too short)",
        ));
    }
    let path = state.ddns.secrets_path.clone();
    tokio::task::spawn_blocking(move || {
        ddns::save_secrets(&path, &DdnsSecrets { api_token: token })
    })
    .await
    .map_err(|err| ApiError::internal(format!("ddns task failed: {err}")))??;
    Ok(Json(OkResult { ok: true }))
}

pub async fn update(State(state): State<AppState>) -> Result<Json<UpdateReport>, ApiError> {
    let outcomes = state.ddns.update().await;
    if let Some(failed) = outcomes.iter().find(|outcome| !outcome.ok) {
        state.notify.emit(NotificationRequest::error(
            event::DDNS_FAILED,
            "Dynamic DNS update failed",
            failed.detail.clone(),
        ));
    } else if outcomes.iter().any(|outcome| outcome.changed) {
        state.notify.emit(NotificationRequest::info(
            event::DDNS_UPDATED,
            "Dynamic DNS updated",
            outcomes
                .iter()
                .map(|outcome| outcome.detail.clone())
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    Ok(Json(UpdateReport {
        outcomes,
        state: collect(&state).await,
    }))
}

async fn collect(state: &AppState) -> DdnsState {
    let path = state.ddns.config_path.clone();
    let (config, config_error) = tokio::task::spawn_blocking(move || ddns::load(&path))
        .await
        .unwrap_or((DdnsConfig::default(), None));
    DdnsState {
        problems: config.validate(),
        config_error,
        has_token: state.ddns.has_token(),
        wan_ip: state.ddns.wan_ip(),
        config,
    }
}

fn invalid(problems: Vec<Problem>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ValidationError {
            error: "dynamic DNS configuration is not valid".to_string(),
            problems,
        }),
    )
        .into_response()
}

/// Check and update on the configured interval for the life of the process.
pub fn spawn_scheduler(state: AppState) {
    tokio::spawn(async move {
        let interval = {
            let config = state.ddns.config();
            std::time::Duration::from_secs(u64::from(config.interval_min.max(1)) * 60)
        };
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await; // consume the immediate first tick
        loop {
            ticker.tick().await;
            let config = state.ddns.config();
            if !config.enabled {
                continue;
            }
            // The address is authoritative from the interface document; when it
            // has not changed since the last successful push, skip the API call.
            let wan = state.ddns.wan_ip();
            if wan.is_some() && config.last_seen_ip == wan && state.ddns.has_token() {
                continue;
            }
            let context = DdnsContext::new(
                state.ddns.config_path.clone(),
                state.ddns.secrets_path.clone(),
                state.ddns.network_path.clone(),
                Arc::clone(&state.ddns.runner),
            );
            let outcomes = context.update().await;
            if let Some(failed) = outcomes.iter().find(|outcome| !outcome.ok) {
                state.notify.emit(NotificationRequest::error(
                    event::DDNS_FAILED,
                    "Dynamic DNS update failed",
                    failed.detail.clone(),
                ));
            } else if outcomes.iter().any(|outcome| outcome.changed) {
                state.notify.emit(NotificationRequest::info(
                    event::DDNS_UPDATED,
                    "Dynamic DNS updated",
                    outcomes
                        .iter()
                        .map(|outcome| outcome.detail.clone())
                        .collect::<Vec<_>>()
                        .join("; "),
                ));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn state_reports_the_wan_address() {
        let state = AppState::new(crate::config::Config::default());
        let snapshot = collect(&state).await;
        assert!(!snapshot.config.enabled);
        assert!(!snapshot.has_token);
        assert_eq!(snapshot.wan_ip, None);
    }

    #[tokio::test]
    async fn invalid_config_is_rejected_with_problems() {
        let state = AppState::new(crate::config::Config::default());
        let config = DdnsConfig {
            enabled: true,
            zone_id: "zz".to_string(),
            ..DdnsConfig::default()
        };
        let response = save_config(State(state), Json(config))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
