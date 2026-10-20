//! HTTP surface for the app store: catalog, install, remove.
//!
//! Catalog reads and compose runs touch the filesystem and fork processes, so
//! they are pushed onto the blocking pool; the async runtime is never blocked.

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;

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
    Ok(Json(OpResult {
        ok: true,
        output: trim(&output),
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
