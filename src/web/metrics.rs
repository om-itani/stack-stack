//! HTTP surface for the minute-resolution telemetry history.

use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AppState;
use crate::system::history::{HistoryPoint, INTERVAL_MS};

#[derive(Deserialize)]
pub struct RangeQuery {
    /// `1h`, `6h` or `24h`. Defaults to `1h`.
    #[serde(default)]
    pub range: String,
}

#[derive(Serialize)]
pub struct HistoryResponse {
    pub range: String,
    pub interval_ms: u64,
    /// How many points the buffer holds in total (not just this range).
    pub total: usize,
    pub points: Vec<HistoryPoint>,
}

/// Minutes of history for each supported range.
fn range_minutes(range: &str) -> Option<u64> {
    match range.trim().to_ascii_lowercase().as_str() {
        "" | "1h" => Some(60),
        "6h" => Some(360),
        "24h" => Some(1440),
        _ => None,
    }
}

pub async fn history(
    State(state): State<AppState>,
    Query(query): Query<RangeQuery>,
) -> Result<Json<HistoryResponse>, ApiError> {
    let minutes = range_minutes(&query.range).ok_or_else(|| {
        ApiError::bad_request(format!(
            "unknown range {:?}; expected 1h, 6h or 24h",
            query.range
        ))
    })?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let from_ms = now_ms.saturating_sub(minutes * 60_000);

    Ok(Json(HistoryResponse {
        range: query.range.trim().to_ascii_lowercase(),
        interval_ms: INTERVAL_MS,
        total: state.history.len(),
        points: state.history.since(from_ms),
    }))
}

#[derive(serde::Deserialize)]
pub struct SetPowerProfileRequest {
    pub profile: crate::system::power::PowerProfile,
}

pub async fn get_power_profile(
    State(state): State<AppState>,
) -> Result<Json<crate::system::power::PowerProfileStatus>, ApiError> {
    let runner = state.samba.runner.clone();
    let status = tokio::task::spawn_blocking(move || {
        crate::system::power::get_power_profile(runner.as_ref())
    })
    .await
    .map_err(|err| ApiError::internal(format!("power profile task failed: {err}")))?;
    Ok(Json(status))
}

pub async fn set_power_profile(
    State(state): State<AppState>,
    Json(request): Json<SetPowerProfileRequest>,
) -> Result<Json<crate::web::storage::StorageResult>, ApiError> {
    let runner = state.samba.runner.clone();
    let profile = request.profile;
    let message = tokio::task::spawn_blocking(move || {
        crate::system::power::set_power_profile(runner.as_ref(), profile)
    })
    .await
    .map_err(|err| ApiError::internal(format!("set power profile task failed: {err}")))??;
    state.audit("system.power_profile", &format!("{profile:?}"), "");
    Ok(Json(crate::web::storage::StorageResult {
        ok: true,
        message,
    }))
}

pub async fn get_hardening_status(
    State(state): State<AppState>,
) -> Result<Json<crate::system::power::HardeningStatus>, ApiError> {
    let runner = state.samba.runner.clone();
    let status = tokio::task::spawn_blocking(move || {
        crate::system::power::get_hardening_status(runner.as_ref())
    })
    .await
    .map_err(|err| ApiError::internal(format!("hardening status task failed: {err}")))?;
    Ok(Json(status))
}

pub async fn apply_hardening(
    State(state): State<AppState>,
) -> Result<Json<crate::web::storage::StorageResult>, ApiError> {
    let runner = state.samba.runner.clone();
    let message =
        tokio::task::spawn_blocking(move || crate::system::power::apply_hardening(runner.as_ref()))
            .await
            .map_err(|err| ApiError::internal(format!("hardening apply task failed: {err}")))??;
    state.audit("system.hardening", "apply", "");
    Ok(Json(crate::web::storage::StorageResult {
        ok: true,
        message,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn ranges_map_to_minutes() {
        assert_eq!(range_minutes(""), Some(60));
        assert_eq!(range_minutes("1h"), Some(60));
        assert_eq!(range_minutes("6h"), Some(360));
        assert_eq!(range_minutes("24H"), Some(1440));
        assert_eq!(range_minutes("7d"), None);
    }

    #[tokio::test]
    async fn history_returns_recorded_points() {
        let state = AppState::new(Config::default());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        state.history.push(HistoryPoint {
            ts_ms: now,
            cpu_pct: 12.0,
            mem_pct: 34.0,
            rx_bps: 1,
            tx_bps: 2,
        });

        let Json(body) = history(
            State(state),
            Query(RangeQuery {
                range: "1h".to_string(),
            }),
        )
        .await
        .expect("ok");
        assert_eq!(body.range, "1h");
        assert_eq!(body.points.len(), 1);
        assert_eq!(body.points[0].cpu_pct, 12.0);
    }

    #[tokio::test]
    async fn an_unknown_range_is_rejected() {
        let state = AppState::new(Config::default());
        assert!(
            history(
                State(state),
                Query(RangeQuery {
                    range: "7d".to_string(),
                }),
            )
            .await
            .is_err()
        );
    }
}
