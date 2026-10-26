//! HTTP surface and evaluator for container / stack health checks.
//!
//! A single document (one probe per stack) is read and written here. The
//! evaluator probes every enabled check on a fixed cadence; after the check's
//! retry count is reached it restarts the stack through the job runner and
//! raises a notification, then stays quiet for the check's cooldown.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::error::ApiError;
use crate::notify::NotificationRequest;
use crate::services::health::{HealthCheck, HealthDoc, ProbeKind};
use crate::state::AppState;

#[derive(Serialize)]
pub struct HealthState {
    pub checks: Vec<HealthCheck>,
    /// Set when the document exists but could not be parsed.
    pub doc_error: Option<String>,
}

fn state_of(state: &AppState) -> HealthState {
    let (doc, doc_error) = state.health.load();
    HealthState {
        checks: doc.checks,
        doc_error,
    }
}

pub async fn state(State(state): State<AppState>) -> Json<HealthState> {
    Json(state_of(&state))
}

/// Replace the whole document. Validation runs before anything is written.
pub async fn save(
    State(state): State<AppState>,
    Json(doc): Json<HealthDoc>,
) -> Result<Json<HealthState>, ApiError> {
    let problems = doc.validate();
    if !problems.is_empty() {
        return Err(
            ApiError::bad_request("health configuration is not valid").with_details(
                problems
                    .iter()
                    .map(|problem| format!("{}: {}", problem.field, problem.message))
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
        );
    }
    state.health.save(&doc)?;
    state.audit(
        "health.save",
        "health.json",
        &format!("{} checks", doc.checks.len()),
    );
    Ok(Json(state_of(&state)))
}

/// Probe every enabled check on a fixed cadence.
pub fn spawn_evaluator(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(
            crate::services::health::HEALTH_INTERVAL_S,
        ));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Stack -> consecutive failures.
        let mut failures: HashMap<String, u32> = HashMap::new();
        loop {
            ticker.tick().await;
            evaluate_once(&state, &mut failures).await;
        }
    });
}

async fn evaluate_once(state: &AppState, failures: &mut HashMap<String, u32>) {
    let (doc, _) = state.health.load();
    if !doc.checks.iter().any(|check| check.enabled) {
        failures.clear();
        return;
    }

    let now = now_secs();
    for check in doc.checks.iter().filter(|check| check.enabled) {
        let ctx = state.health.clone();
        let probe = check.clone();
        let healthy = tokio::task::spawn_blocking(move || ctx.probe(&probe))
            .await
            .unwrap_or(false);

        if healthy {
            failures.remove(&check.stack);
            continue;
        }

        let count = failures.entry(check.stack.clone()).or_insert(0);
        *count += 1;
        if *count < check.retries.max(1) {
            continue;
        }
        if !crate::services::health::cooldown_elapsed(check.last_restart, check.cooldown_s, now) {
            continue;
        }

        *count = 0;
        restart(state, check);
    }
}

fn restart(state: &AppState, check: &HealthCheck) {
    let kind = match check.kind {
        ProbeKind::Http => "HTTP",
        ProbeKind::Tcp => "TCP",
    };
    state.notify.emit(NotificationRequest::error(
        "container_unhealthy",
        format!("{} failed its health check", check.stack),
        format!(
            "{} consecutive {} probe failures against {}; restarting the stack.",
            check.retries, kind, check.target
        ),
    ));
    state.audit("health.restart", &check.stack, &check.target);

    let cfg = state.config.clone();
    let timeout = state.config.compose_timeout_s;
    let stack = check.stack.clone();
    // Persist the cooldown stamp before the restart is dispatched, so a failure
    // in the restart itself cannot cause a tight loop.
    state.health.record_restart(&stack);

    state
        .jobs
        .spawn_job(format!("Restarting {stack}"), move |ctx| {
            ctx.step(10, "docker compose restart");
            crate::containers::store::restart_stack_blocking(&cfg, &stack, timeout)
                .map_err(|err| err.to_string())
        });
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::services::health::{HealthCheck, ProbeKind};

    fn test_state(name: &str) -> AppState {
        let dir = std::env::temp_dir().join(format!("ss-health-web-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        AppState::new(Config {
            health_path: dir.join("health.json").to_string_lossy().into_owned(),
            ..Config::default()
        })
    }

    #[tokio::test]
    async fn state_round_trips_checks() {
        let app = test_state("state");
        let doc = HealthDoc {
            checks: vec![HealthCheck {
                stack: "web".to_string(),
                enabled: true,
                kind: ProbeKind::Tcp,
                target: "10.0.0.5:8080".to_string(),
                timeout_s: 5,
                retries: 3,
                cooldown_s: 300,
                last_restart: None,
            }],
        };
        app.health.save(&doc).expect("save");
        let Json(view) = state(State(app)).await;
        assert_eq!(view.checks.len(), 1);
        assert!(view.doc_error.is_none());
    }

    #[tokio::test]
    async fn save_rejects_an_invalid_target() {
        let app = test_state("invalid");
        let doc = HealthDoc {
            checks: vec![HealthCheck {
                stack: "web".to_string(),
                enabled: true,
                kind: ProbeKind::Http,
                target: "not-a-url".to_string(),
                timeout_s: 5,
                retries: 3,
                cooldown_s: 300,
                last_restart: None,
            }],
        };
        assert!(save(State(app), Json(doc)).await.is_err());
    }
}
