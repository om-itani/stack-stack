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
    /// Detached operations return their activity-drawer job id immediately.
    pub job_id: Option<String>,
    /// Trimmed compose output, useful for completed synchronous operations.
    pub output: String,
}

pub async fn catalog(State(state): State<AppState>) -> Result<Json<Vec<App>>, ApiError> {
    let config = state.config.clone();
    let mut apps = tokio::task::spawn_blocking(move || store::catalog(&config))
        .await
        .map_err(|err| ApiError::internal(format!("catalog task failed: {err}")))?;
    let installed = store::installed_ids(&state.config);
    let containers = state
        .docker
        .list("", &installed, &apps)
        .await
        .unwrap_or_default();
    for app in &mut apps {
        let members: Vec<_> = containers
            .iter()
            .filter(|container| {
                container.app_id.as_deref() == Some(&app.id)
                    || container.stack.as_deref() == Some(&app.id)
            })
            .collect();
        app.installed = installed.iter().any(|id| id == &app.id) && !members.is_empty();
        app.container_state = members
            .iter()
            .map(|container| container.state.as_str())
            .find(|state| *state != "running")
            .or_else(|| members.first().map(|container| container.state.as_str()))
            .map(str::to_string);
    }
    Ok(Json(apps))
}

pub async fn reset(
    State(state): State<AppState>,
    Path(app): Path<String>,
) -> Result<Json<OpResult>, ApiError> {
    if !store::compose_path(&state.config, &app).is_file() {
        return Err(ApiError::not_found(format!("app {app} is not installed")));
    }
    let config = state.config.clone();
    let app_for_job = app.clone();
    let docker = state.docker.clone();
    let job_id = state
        .jobs
        .spawn(format!("Resetting {app}"), move |ctx| async move {
            ctx.step(10, format!("removing data for {app_for_job}"));
            match store::reset(&config, &app_for_job).await {
                Ok(output) => {
                    docker.invalidate().await;
                    Ok(trim(&output))
                }
                Err(err) => Err(err.to_string()),
            }
        });
    state.audit(
        "app.reset",
        &app,
        "compose down -v and reinstall (background)",
    );
    Ok(Json(OpResult {
        ok: true,
        job_id: Some(job_id),
        output: String::new(),
    }))
}

pub async fn install(
    State(state): State<AppState>,
    Path(app): Path<String>,
) -> Result<Json<OpResult>, ApiError> {
    let config = state.config.clone();
    let app_for_job = app.clone();
    let docker = state.docker.clone();
    let job_id = state
        .jobs
        .spawn(format!("Installing {app}"), move |ctx| async move {
            ctx.step(10, format!("deploying {app_for_job}"));
            match store::install(&config, &app_for_job).await {
                Ok(output) => {
                    docker.invalidate().await;
                    Ok(trim(&output))
                }
                Err(err) => Err(err.to_string()),
            }
        });
    state.audit("app.install", &app, "compose up -d (background)");
    Ok(Json(OpResult {
        ok: true,
        job_id: Some(job_id),
        output: String::new(),
    }))
}

pub async fn remove(
    State(state): State<AppState>,
    Path(app): Path<String>,
) -> Result<Json<OpResult>, ApiError> {
    if !store::compose_path(&state.config, &app).is_file() {
        return Err(ApiError::not_found(format!("app {app} is not installed")));
    }
    let config = state.config.clone();
    let app_for_job = app.clone();
    let docker = state.docker.clone();
    let job_id = state
        .jobs
        .spawn(format!("Removing {app}"), move |ctx| async move {
            ctx.step(10, format!("removing {app_for_job}"));
            match store::remove(&config, &app_for_job).await {
                Ok(output) => {
                    docker.invalidate().await;
                    Ok(trim(&output))
                }
                Err(err) => Err(err.to_string()),
            }
        });
    state.audit("app.remove", &app, "compose down (background)");
    Ok(Json(OpResult {
        ok: true,
        job_id: Some(job_id),
        output: String::new(),
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
