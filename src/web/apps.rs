//! HTTP surface for the app store: catalog, install, remove.
//!
//! Catalog reads and compose runs touch the filesystem and fork processes, so
//! they are pushed onto the blocking pool; the async runtime is never blocked.

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};

use crate::containers::hub::HubRepo;
use crate::containers::model::App;
use crate::containers::store;
use crate::error::ApiError;
use crate::state::AppState;

/// How much compose output is echoed back to the browser.
const MAX_OUTPUT: usize = 4000;

#[derive(Serialize)]
pub struct OpResult {
    pub ok: bool,
    /// Trimmed compose output, useful when an install takes a while.
    pub output: String,
}

pub async fn catalog(State(state): State<AppState>) -> Result<Json<Vec<App>>, ApiError> {
    let config = state.config.clone();
    let apps = tokio::task::spawn_blocking(move || store::catalog_with_state(&config))
        .await
        .map_err(|err| ApiError::internal(format!("catalog task failed: {err}")))?;
    Ok(Json(apps))
}

pub async fn install(
    State(state): State<AppState>,
    Path(app): Path<String>,
) -> Result<Json<OpResult>, ApiError> {
    let output = store::install(&state.config, &app).await?;
    // Compose created containers outside the Docker client; drop the cached list.
    state.docker.invalidate().await;
    Ok(Json(OpResult {
        ok: true,
        output: trim(&output),
    }))
}

pub async fn remove(
    State(state): State<AppState>,
    Path(app): Path<String>,
) -> Result<Json<OpResult>, ApiError> {
    let output = store::remove(&state.config, &app).await?;
    state.docker.invalidate().await;
    Ok(Json(OpResult {
        ok: true,
        output: trim(&output),
    }))
}

#[derive(Deserialize)]
pub struct HubQuery {
    pub q: Option<String>,
}

#[derive(Serialize)]
pub struct HubResults {
    /// False when the transport (`curl`) is missing, so the UI disables the box.
    pub available: bool,
    pub results: Vec<HubRepo>,
}

/// Search Docker Hub for repositories. Cached in-process for five minutes and
/// rate-limit aware, so an operator typing a query does not burn the unauth
/// quota. A missing `curl` is reported as `available: false` rather than an
/// error, so the UI can disable the box with a clear message.
pub async fn hub_search(
    State(state): State<AppState>,
    Query(query): Query<HubQuery>,
) -> Result<Json<HubResults>, ApiError> {
    if !state.hub.available() {
        return Ok(Json(HubResults {
            available: false,
            results: Vec::new(),
        }));
    }
    let results = state.hub.search(query.q.as_deref().unwrap_or("")).await?;
    Ok(Json(HubResults {
        available: true,
        results,
    }))
}

fn trim(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.len() <= MAX_OUTPUT {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - MAX_OUTPUT;
    while start < trimmed.len() && !trimmed.is_char_boundary(start) {
        start += 1;
    }
    trimmed[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_bounds_output_on_char_boundaries() {
        assert_eq!(trim("  done\n"), "done");
        let long = "é".repeat(MAX_OUTPUT);
        let trimmed = trim(&long);
        assert!(trimmed.len() <= MAX_OUTPUT);
        assert!(trimmed.is_char_boundary(trimmed.len()));
    }
}
