//! HTTP surface for the background job registry.
//!
//! `GET /api/jobs` is intentionally a plain poll: the registry is in memory and
//! the read is one short lock, so the activity drawer can refresh every couple
//! of seconds without a socket or any load on the rest of the API.

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;

use crate::error::ApiError;
use crate::jobs::Job;
use crate::state::AppState;

#[derive(Serialize)]
pub struct JobsState {
    /// Newest first; running jobs are interleaved with recent history.
    pub jobs: Vec<Job>,
}

#[derive(Serialize)]
pub struct OkResult {
    pub ok: bool,
}

pub async fn list(State(state): State<AppState>) -> Json<JobsState> {
    Json(JobsState {
        jobs: state.jobs.list(),
    })
}

/// Clear a finished job from the history. A job still running cannot be removed.
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<OkResult>, ApiError> {
    if state.jobs.get(&id).is_none() {
        return Err(ApiError::not_found(format!("no job {id}")));
    }
    state
        .jobs
        .remove(&id)
        .map_err(|_| ApiError::bad_request("a running job cannot be removed"))?;
    Ok(Json(OkResult { ok: true }))
}
