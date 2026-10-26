//! Auto-update surface: opt-in per stack, manual check, redeploy and rollback.
//!
//! Every operation shells out through the shared runner, so the handlers only
//! marshal JSON and move the blocking work onto the pool.

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::notify::NotificationRequest;
use crate::services::updates::{Schedule, UpdateEntry};
use crate::state::AppState;

#[derive(Serialize)]
pub struct UpdatesState {
    pub entries: Vec<UpdateEntry>,
    /// Set when the document exists but could not be parsed.
    pub doc_error: Option<String>,
}

#[derive(Serialize)]
pub struct UpdateResult {
    pub ok: bool,
    pub message: String,
    pub entries: Vec<UpdateEntry>,
}

#[derive(Deserialize)]
pub struct ConfigureRequest {
    pub auto_update: bool,
    #[serde(default)]
    pub schedule: Schedule,
    /// `stack` (default) or `container`; only a stack can be redeployed.
    #[serde(default)]
    pub kind: Option<String>,
}

fn entries_of(state: &AppState) -> (Vec<UpdateEntry>, Option<String>) {
    let (doc, error) = state.updates.load();
    (doc.entries, error)
}

pub async fn state(State(state): State<AppState>) -> Result<Json<UpdatesState>, ApiError> {
    let (entries, doc_error) = entries_of(&state);
    Ok(Json(UpdatesState { entries, doc_error }))
}

/// Turn auto-update on or off and set the cadence. Both default to off/manual.
pub async fn configure(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ConfigureRequest>,
) -> Result<Json<UpdatesState>, ApiError> {
    let kind = request.kind.unwrap_or_else(|| "stack".to_string());
    if kind != "stack" && kind != "container" {
        return Err(ApiError::bad_request("kind must be 'stack' or 'container'"));
    }
    let updates = state.updates.clone();
    let id_for_task = id.clone();
    let (auto_update, schedule) = (request.auto_update, request.schedule);
    tokio::task::spawn_blocking(move || {
        updates.configure(&id_for_task, &kind, auto_update, schedule)
    })
    .await
    .map_err(|err| ApiError::internal(format!("update task failed: {err}")))??;

    state.audit(
        "container.autoupdate",
        &id,
        &format!("{auto_update} ({schedule:?})"),
    );
    let (entries, doc_error) = entries_of(&state);
    Ok(Json(UpdatesState { entries, doc_error }))
}

/// Pull now and report whether the digest moved.
pub async fn check(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<UpdateResult>, ApiError> {
    let updates = state.updates.clone();
    let id_for_task = id.clone();
    let outcome = state
        .jobs
        .track_with(format!("Checking {id}"), move |ctx| async move {
            ctx.step(10, "docker compose pull");
            tokio::task::spawn_blocking(move || updates.check(&id_for_task))
                .await
                .map_err(|err| ApiError::internal(format!("update task failed: {err}")))?
        })
        .await?;

    state.audit("container.update_check", &id, &outcome.message);
    let (entries, _) = entries_of(&state);
    Ok(Json(UpdateResult {
        ok: true,
        message: outcome.message,
        entries,
    }))
}

/// Bring the stack up again on whatever the compose file points at now.
pub async fn apply(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<UpdateResult>, ApiError> {
    let updates = state.updates.clone();
    let id_for_task = id.clone();
    let output = state
        .jobs
        .track_with(format!("Redeploying {id}"), move |ctx| async move {
            ctx.step(10, "docker compose up -d");
            tokio::task::spawn_blocking(move || updates.apply(&id_for_task))
                .await
                .map_err(|err| ApiError::internal(format!("update task failed: {err}")))?
        })
        .await?;

    state.audit("container.update", &id, "redeployed");
    state.docker.invalidate().await;
    let (entries, _) = entries_of(&state);
    Ok(Json(UpdateResult {
        ok: true,
        message: if output.is_empty() {
            "redeployed".to_string()
        } else {
            output
        },
        entries,
    }))
}

/// Pin the previous digest into the compose file and redeploy.
pub async fn rollback(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<UpdateResult>, ApiError> {
    let updates = state.updates.clone();
    let id_for_task = id.clone();
    let output = state
        .jobs
        .track_with(format!("Rolling back {id}"), move |ctx| async move {
            ctx.step(10, "restoring the previous digest");
            tokio::task::spawn_blocking(move || updates.rollback(&id_for_task))
                .await
                .map_err(|err| ApiError::internal(format!("update task failed: {err}")))?
        })
        .await?;

    state.audit("container.rollback", &id, "previous digest restored");
    state.docker.invalidate().await;
    let (entries, _) = entries_of(&state);
    Ok(Json(UpdateResult {
        ok: true,
        message: if output.is_empty() {
            "rolled back".to_string()
        } else {
            output
        },
        entries,
    }))
}

/// Scheduled checks. Wakes often and lets the per-entry schedule decide, the
/// same shape as the SMART and blocklist schedulers.
pub fn spawn_scheduler(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30 * 60));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;

            let (doc, _) = state.updates.load();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            let due = crate::services::updates::due_ids(&doc, now);

            for id in due {
                let updates = state.updates.clone();
                let id_for_task = id.clone();
                let id_label = id.clone();
                let audit_path = state.config.audit_path.clone();
                // Sequential on purpose: several stacks can come due on the same
                // tick, and concurrent pulls/redeploys contend for IO on a small
                // box. `track_with` keeps the drawer updated while the loop waits
                // for each stack in turn, and only a stack that actually moved is
                // audited as an update.
                let result = state
                    .jobs
                    .track_with(format!("Auto-update {id}"), move |ctx| async move {
                        ctx.step(10, "docker compose pull");
                        let updated =
                            tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
                                let outcome = updates.check(&id_for_task)?;
                                if !outcome.changed {
                                    // An up-to-date stack is never bounced.
                                    return Ok(None);
                                }
                                updates.apply(&id_for_task)?;
                                Ok(Some(outcome.id))
                            })
                            .await
                            .map_err(|err| {
                                ApiError::internal(format!("update task failed: {err}"))
                            })??;

                        match updated {
                            Some(updated) => {
                                ctx.step(80, "redeployed");
                                crate::audit::record(
                                    &audit_path,
                                    "system",
                                    "container.update",
                                    &updated,
                                    "scheduled",
                                );
                                Ok::<String, ApiError>(format!("{updated} updated"))
                            }
                            None => {
                                Ok::<String, ApiError>(format!("{id_label} already up to date"))
                            }
                        }
                    })
                    .await;

                match result {
                    Ok(message) => {
                        tracing::info!(stack = %id, %message, "scheduled container update");
                        state.notify.emit(NotificationRequest::info(
                            "container_update",
                            "Container updated",
                            &message,
                        ));
                    }
                    Err(err) => {
                        tracing::warn!(stack = %id, %err, "scheduled container update failed");
                        state.notify.emit(NotificationRequest::error(
                            "container_update",
                            "Container update failed",
                            format!("{id}: {err}"),
                        ));
                    }
                }
            }
        }
    });
}
