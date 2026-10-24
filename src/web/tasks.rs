//! Scheduled tasks: list, create/update, delete, run now, and the runner loop.

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::notify::NotificationRequest;
use crate::services::tasks::Task;
use crate::state::AppState;

#[derive(Serialize)]
pub struct TasksState {
    pub tasks: Vec<Task>,
    /// Set when the document exists but could not be parsed.
    pub doc_error: Option<String>,
}

#[derive(Serialize)]
pub struct TaskRunResult {
    pub ok: bool,
    pub status: String,
    pub output: String,
    pub tasks: Vec<Task>,
}

#[derive(Deserialize)]
pub struct TaskRequest {
    /// Derived from the name when omitted.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub schedule: String,
    pub command: String,
}

fn state_of(state: &AppState) -> TasksState {
    let (doc, doc_error) = state.tasks.load();
    TasksState {
        tasks: doc.tasks,
        doc_error,
    }
}

pub async fn list(State(state): State<AppState>) -> Result<Json<TasksState>, ApiError> {
    Ok(Json(state_of(&state)))
}

pub async fn upsert(
    State(state): State<AppState>,
    Json(request): Json<TaskRequest>,
) -> Result<Json<TasksState>, ApiError> {
    let id = match request
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) => id.to_string(),
        // A name with spaces is not a usable id, so fall back to a slug.
        None => slug(&request.name),
    };
    let ctx = state.tasks.clone();
    let (name, schedule, command) = (
        request.name.clone(),
        request.schedule.clone(),
        request.command.clone(),
    );
    let id_for_task = id.clone();
    tokio::task::spawn_blocking(move || ctx.upsert(&id_for_task, &name, &schedule, &command))
        .await
        .map_err(|err| ApiError::internal(format!("task failed: {err}")))??;

    state.audit("task.save", &id, &request.schedule);
    Ok(Json(state_of(&state)))
}

pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TasksState>, ApiError> {
    let ctx = state.tasks.clone();
    let id_for_task = id.clone();
    tokio::task::spawn_blocking(move || ctx.remove(&id_for_task))
        .await
        .map_err(|err| ApiError::internal(format!("task failed: {err}")))??;

    state.audit("task.delete", &id, "");
    Ok(Json(state_of(&state)))
}

/// Run a task immediately, regardless of its schedule.
pub async fn run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TaskRunResult>, ApiError> {
    let ctx = state.tasks.clone();
    let id_for_task = id.clone();
    let (doc, message) = tokio::task::spawn_blocking(move || ctx.run(&id_for_task))
        .await
        .map_err(|err| ApiError::internal(format!("task failed: {err}")))??;

    let status = doc
        .find(&id)
        .and_then(|task| task.last_status.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let output = doc
        .find(&id)
        .and_then(|task| task.last_output.clone())
        .unwrap_or_default();
    let ok = status == "ok";

    state.audit("task.run", &id, &message);
    if !ok {
        state.notify.emit(NotificationRequest::error(
            "task_failed",
            "Scheduled task failed",
            format!("{id}: {output}"),
        ));
    }
    Ok(Json(TaskRunResult {
        ok,
        status,
        output,
        tasks: doc.tasks,
    }))
}

/// Every minute, run whatever is due.
pub fn spawn_scheduler(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;

            let (doc, _) = state.tasks.load();
            let now_epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            let due = crate::services::tasks::due_ids(
                &doc,
                now_epoch,
                crate::services::tasks::local_now(),
            );

            for id in due {
                let ctx = state.tasks.clone();
                let id_for_task = id.clone();
                let result = tokio::task::spawn_blocking(move || ctx.run(&id_for_task)).await;
                match result {
                    Ok(Ok((updated, message))) => {
                        let status = updated
                            .find(&id)
                            .and_then(|task| task.last_status.clone())
                            .unwrap_or_default();
                        tracing::info!(task = %id, %status, "scheduled task ran");
                        state.audit("task.run", &id, "scheduled");
                        if status != "ok" {
                            state.notify.emit(NotificationRequest::error(
                                "task_failed",
                                "Scheduled task failed",
                                &message,
                            ));
                        }
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(task = %id, %err, "scheduled task failed to start");
                        state.notify.emit(NotificationRequest::error(
                            "task_failed",
                            "Scheduled task failed",
                            format!("{id}: {err}"),
                        ));
                    }
                    Err(err) => tracing::warn!(%err, "task join failed"),
                }
            }
        }
    });
}

/// Turn a display name into a task id.
fn slug(name: &str) -> String {
    let mut id: String = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while id.contains("--") {
        id = id.replace("--", "-");
    }
    let id = id.trim_matches('-').to_string();
    if id.is_empty() {
        "task".to_string()
    } else {
        id.chars().take(64).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::slug;

    #[test]
    fn slug_makes_a_usable_id() {
        assert_eq!(slug("Nightly Cleanup"), "nightly-cleanup");
        assert_eq!(slug("  Disk  prune  "), "disk-prune");
        assert_eq!(slug("!!!"), "task");
        assert!(slug(&"x".repeat(100)).len() <= 64);
    }
}
