//! HTTP surface for SMART self-tests.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::model::Problem;
use crate::notify::{NotificationRequest, event};
use crate::services::smart::{self, SmartConfig, TestKind, TestProgress, TestRecord};
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct SmartState {
    pub config: SmartConfig,
    pub problems: Vec<Problem>,
    pub config_error: Option<String>,
    pub history: Vec<TestRecord>,
    pub progress: Vec<DeviceProgress>,
    pub available: bool,
    pub running: bool,
    /// Disks `smartctl` can see but that are not scheduled yet, so the UI can
    /// offer a picker instead of guessing a path.
    pub detected: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DeviceProgress {
    pub device: String,
    #[serde(flatten)]
    pub progress: TestProgress,
}

#[derive(Deserialize)]
pub struct DeviceQuery {
    /// Optional: with no device, `progress` reports an idle result instead of
    /// failing to deserialize the query.
    #[serde(default)]
    pub device: String,
}

#[derive(Serialize)]
pub struct StartResult {
    pub accepted: bool,
    pub detail: String,
    #[serde(flatten)]
    pub status: TestProgress,
}

#[derive(Deserialize)]
pub struct AbortRequest {
    pub device: String,
}

#[derive(Serialize)]
pub struct AbortResult {
    pub ok: bool,
    pub detail: String,
    #[serde(flatten)]
    pub status: TestProgress,
}

pub async fn state(State(state): State<AppState>) -> Json<SmartState> {
    Json(collect(&state).await)
}

pub async fn save_config(
    State(state): State<AppState>,
    Json(config): Json<SmartConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid(problems));
    }
    let path = state.smart.config_path.clone();
    let for_write = config.clone();
    tokio::task::spawn_blocking(move || smart::save(&path, &for_write))
        .await
        .map_err(|err| ApiError::internal(format!("smart task failed: {err}")))??;
    Ok(Json(collect(&state).await).into_response())
}

pub async fn start(
    State(state): State<AppState>,
    AxumPath(device): AxumPath<String>,
    Json(kind): Json<TestKind>,
) -> Result<Json<StartResult>, ApiError> {
    if !state.smart.available() {
        return Err(ApiError::unavailable(
            "smartctl is not installed; install smartmontools to run self-tests",
        ));
    }
    let config = state.smart.config();
    if config.plan_for(&device).is_none() {
        return Err(ApiError::bad_request(format!(
            "{device} is not in the SMART schedule; add it first"
        )));
    }

    let record = state.smart.run_test(&device, kind).await;
    // Read the state back: if a test was already running, that is the useful
    // answer, not the raw "cannot start" error smartctl printed.
    let status = state
        .smart
        .progress(&device)
        .await
        .unwrap_or_else(|err| TestProgress::idle(err.to_string()));
    let already_running = status.running;
    let detail = if already_running {
        status.detail.clone()
    } else if smart::is_running_hint(&record.detail) {
        format!("a self-test is already running on {device}")
    } else {
        record.detail.clone()
    };
    Ok(Json(StartResult {
        accepted: record.accepted || already_running,
        detail,
        status,
    }))
}

/// The running self-test's state and the last completed one.
pub async fn selftest(
    State(state): State<AppState>,
    Query(query): Query<DeviceQuery>,
) -> Result<Json<TestProgress>, ApiError> {
    // A device-less request (e.g. the Developer ping) is answered as "idle"
    // rather than probing an empty device name.
    if query.device.trim().is_empty() {
        return Ok(Json(TestProgress::idle("no device specified")));
    }
    Ok(Json(state.smart.progress(&query.device).await?))
}

/// Abort a running self-test.
pub async fn abort(
    State(state): State<AppState>,
    Json(request): Json<AbortRequest>,
) -> Result<Json<AbortResult>, ApiError> {
    let device = request.device.trim();
    if device.is_empty() {
        return Err(ApiError::bad_request("device is required"));
    }
    if !state.smart.available() {
        return Err(ApiError::unavailable(
            "smartctl is not installed; install smartmontools to run self-tests",
        ));
    }
    let detail = state.smart.abort(device).await?;
    state.audit("smart.selftest_abort", device, "aborted");
    let status = state
        .smart
        .progress(device)
        .await
        .unwrap_or_else(|err| TestProgress::idle(err.to_string()));
    Ok(Json(AbortResult {
        ok: true,
        detail,
        status,
    }))
}

pub async fn progress(
    State(state): State<AppState>,
    Query(query): Query<DeviceQuery>,
) -> Result<Json<TestProgress>, ApiError> {
    selftest(State(state), Query(query)).await
}

async fn collect(state: &AppState) -> SmartState {
    let path = state.smart.config_path.clone();
    let (config, config_error) = tokio::task::spawn_blocking(move || smart::load(&path))
        .await
        .map_err(|err| ApiError::internal(format!("smart task failed: {err}")))
        .unwrap_or_else(|err| (SmartConfig::default(), Some(err.to_string())));
    let problems = config.validate();

    let mut progress = Vec::new();
    for plan in &config.devices {
        let progress_value = state
            .smart
            .progress(&plan.device)
            .await
            .unwrap_or_else(|err| TestProgress::idle(err.to_string()));
        progress.push(DeviceProgress {
            device: plan.device.clone(),
            progress: progress_value,
        });
    }

    let detected = {
        let smart = state.smart.clone();
        tokio::task::spawn_blocking(move || smart.detect_devices())
            .await
            .unwrap_or_default()
    };

    SmartState {
        config,
        problems,
        config_error,
        history: state.smart.history().await,
        progress,
        available: state.smart.available(),
        running: state.smart.busy().await,
        detected,
    }
}

/// Run due SMART self-tests every minute for the life of the process.
pub fn spawn_scheduler(state: AppState) {
    tokio::spawn(async move {
        // A fresh install has no schedule at all, which means no self-tests ever
        // run — the silent-failure risk. Discover the disks once and schedule the
        // default weekly-short/monthly-long pair for each of them. This is a
        // no-op once the document names any device, so an opt-out survives.
        if state.smart.available() {
            let scheduled = tokio::task::spawn_blocking({
                let smart = state.smart.clone();
                move || smart.ensure_default_schedule()
            })
            .await
            .unwrap_or_default();
            if !scheduled.is_empty() {
                tracing::info!(
                    devices = scheduled.len(),
                    "scheduled default SMART self-tests for every detected disk"
                );
            }
        }

        // key "device:kind" -> the time the schedule was last considered.
        let mut last: HashMap<String, u64> = HashMap::new();
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let now = now_secs();
        for plan in state.smart.config().devices {
            last.insert(format!("{}:short", plan.device), now);
            last.insert(format!("{}:long", plan.device), now);
        }

        loop {
            ticker.tick().await;
            if !state.smart.available() || state.smart.busy().await {
                continue;
            }
            let config = state.smart.config();
            let now = now_secs();
            for plan in &config.devices {
                for (kind, label) in [(TestKind::Short, "short"), (TestKind::Long, "long")] {
                    let key = format!("{}:{label}", plan.device);
                    let anchor = *last.entry(key.clone()).or_insert(now);
                    let Some(due) = smart::next_due(plan, kind, anchor) else {
                        continue;
                    };
                    if now < due {
                        continue;
                    }
                    tracing::info!(device = %plan.device, kind = label, "starting scheduled SMART self-test");
                    let record = state.smart.run_test(&plan.device, kind).await;
                    if !record.accepted {
                        state.notify.emit(NotificationRequest::error(
                            event::SMART_TEST_FAILED,
                            format!("SMART test on {} failed to start", plan.device),
                            record.detail.clone(),
                        ));
                    }
                    last.insert(key, now);
                }
            }
        }
    });
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn invalid(problems: Vec<Problem>) -> Response {
    // Name the fields in `message`/`details` so the UI can say *what* is wrong
    // even when it only surfaces the flattened error string.
    let details = problems
        .iter()
        .map(|problem| format!("{}: {}", problem.field, problem.message))
        .collect::<Vec<_>>()
        .join("; ");
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": "BadRequest",
            "message": "SMART configuration is not valid",
            "details": details,
            "problems": problems,
        })),
    )
        .into_response()
}
