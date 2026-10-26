//! HTTP surface and evaluator for the alerting engine.
//!
//! The page reads one document (rules plus providers); a single `PUT` replaces
//! it, mirroring the other document-backed pages. The evaluator is a 30-second
//! loop that samples telemetry against each enabled rule and fires the action
//! once the condition has held for the rule's duration, honouring its cooldown.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;

use crate::alerts::{
    ALERT_EVENT, AlertMessage, AlertRule, AlertsDoc, EVALUATOR_INTERVAL_S, condition_holds,
    cooldown_elapsed, describe, metric_value,
};
use crate::error::ApiError;
use crate::notify::NotificationRequest;
use crate::state::AppState;

#[derive(Serialize)]
pub struct AlertsState {
    pub providers: Vec<crate::alerts::ProviderConfig>,
    pub rules: Vec<AlertRule>,
    /// Set when the document exists but could not be parsed.
    pub doc_error: Option<String>,
}

#[derive(Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub detail: String,
}

fn state_of(state: &AppState) -> AlertsState {
    let (doc, doc_error) = state.alerts.load();
    AlertsState {
        providers: doc.providers,
        rules: doc.rules,
        doc_error,
    }
}

pub async fn state(State(state): State<AppState>) -> Json<AlertsState> {
    Json(state_of(&state))
}

/// Replace the whole document. Validation runs before anything is written, so a
/// rule that points at a missing provider is refused rather than silently dead.
pub async fn save(
    State(state): State<AppState>,
    Json(doc): Json<AlertsDoc>,
) -> Result<Json<AlertsState>, ApiError> {
    let problems = doc.validate();
    if !problems.is_empty() {
        return Err(
            ApiError::bad_request("alert configuration is not valid").with_details(
                problems
                    .iter()
                    .map(|problem| format!("{}: {}", problem.field, problem.message))
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
        );
    }
    state.alerts.save(&doc)?;
    state.audit(
        "alerts.save",
        "alerts.json",
        &format!(
            "{} rules, {} providers",
            doc.rules.len(),
            doc.providers.len()
        ),
    );
    Ok(Json(state_of(&state)))
}

/// Send a synthetic message through one provider, for the "Test" button.
pub async fn test(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TestResult>, ApiError> {
    let (doc, _) = state.alerts.load();
    let provider = doc
        .provider(&id)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("no provider {id}")))?;

    let message = AlertMessage {
        title: "stack-stack alert test".to_string(),
        body: "If you can read this, the provider is configured correctly.".to_string(),
    };
    let ctx = state.alerts.clone();
    let provider_for_log = provider.clone();
    tokio::task::spawn_blocking(move || ctx.deliver(&provider, &message))
        .await
        .map_err(|err| ApiError::internal(format!("alert task failed: {err}")))?
        .map_err(|err| ApiError::internal(format!("delivery failed: {err}")))?;

    Ok(Json(TestResult {
        ok: true,
        detail: format!(
            "delivered via {}",
            crate::notifications::describe(&provider_for_log)
        ),
    }))
}

/// Sample every enabled rule on a fixed cadence.
pub fn spawn_evaluator(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(EVALUATOR_INTERVAL_S));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Rule id -> the instant (ms) the condition first became true.
        let mut active: HashMap<String, u64> = HashMap::new();
        loop {
            ticker.tick().await;
            evaluate_once(&state, &mut active).await;
        }
    });
}

async fn evaluate_once(state: &AppState, active: &mut HashMap<String, u64>) {
    let (doc, _) = state.alerts.load();
    if !doc.rules.iter().any(|rule| rule.enabled) {
        active.clear();
        return;
    }

    let Some(snapshot) = state.latest_snapshot() else {
        return;
    };

    let needs_containers = doc
        .rules
        .iter()
        .any(|rule| rule.enabled && rule.metric.starts_with("container_status"));
    let containers = if needs_containers {
        state.docker.list("", &[], &[]).await.unwrap_or_default()
    } else {
        Vec::new()
    };

    let now_ms = now_ms();
    let now_s = now_ms / 1000;

    for rule in doc.rules.iter().filter(|rule| rule.enabled) {
        let Some(value) = metric_value(&rule.metric, &snapshot, &containers) else {
            active.remove(&rule.id);
            continue;
        };
        if !condition_holds(rule.condition, value, rule.threshold) {
            active.remove(&rule.id);
            continue;
        }

        match active.get(&rule.id) {
            Some(started) if now_ms.saturating_sub(*started) >= rule.duration_s * 1000 => {}
            Some(_) => continue,
            None => {
                active.insert(rule.id.clone(), now_ms);
                continue;
            }
        }

        if !cooldown_elapsed(rule.last_fired, rule.cooldown_s, now_s) {
            continue;
        }

        fire(state, &doc, rule, value);
        // Re-arm: the condition must hold for another full duration before a
        // second firing, on top of the cooldown.
        active.remove(&rule.id);
    }
}

fn fire(state: &AppState, doc: &AlertsDoc, rule: &AlertRule, value: f64) {
    let title = format!("{}: {}", rule.name, rule.metric);
    let body = format!(
        "{} is {} ({} {} threshold) for at least {}s.",
        rule.metric,
        format_value(value),
        rule.condition.as_str(),
        format_value(rule.threshold),
        rule.duration_s
    );

    state.notify.emit(NotificationRequest::warning(
        ALERT_EVENT,
        title.clone(),
        body.clone(),
    ));
    state.audit("alert.fire", &rule.id, &describe(rule));

    let provider = doc.provider(&rule.action).cloned();
    let ctx = state.alerts.clone();
    let rule_id = rule.id.clone();
    let action = rule.action.clone();
    let message = AlertMessage { title, body };

    // Delivery and the cooldown stamp both touch the filesystem/network, so they
    // run detached on the blocking pool and never stall the evaluator.
    drop(tokio::task::spawn_blocking(move || {
        match provider {
            Some(provider) => {
                if let Err(err) = ctx.deliver(&provider, &message) {
                    tracing::warn!(provider = %provider.id, %err, "alert delivery failed");
                }
            }
            None => tracing::warn!(action = %action, "alert has no matching provider"),
        }
        ctx.record_fired(&rule_id);
    }));
}

fn format_value(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::{Comparison, ProviderConfig, ProviderKind};
    use crate::config::Config;

    fn test_state(name: &str) -> AppState {
        let dir = std::env::temp_dir().join(format!("ss-alerts-web-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        AppState::new(Config {
            alerts_path: dir.join("alerts.json").to_string_lossy().into_owned(),
            notify_path: dir.join("notify.json").to_string_lossy().into_owned(),
            ..Config::default()
        })
    }

    #[tokio::test]
    async fn state_reports_rules_and_providers() {
        let app = test_state("state");
        let doc = AlertsDoc {
            providers: vec![ProviderConfig {
                id: "discord".to_string(),
                name: "Discord".to_string(),
                kind: ProviderKind::Discord,
                url: Some("https://hooks.example.test/x".to_string()),
                to: None,
                enabled: true,
            }],
            rules: vec![AlertRule {
                id: "cpu".to_string(),
                name: "CPU".to_string(),
                metric: "cpu_usage".to_string(),
                condition: Comparison::Greater,
                threshold: 90.0,
                duration_s: 300,
                action: "discord".to_string(),
                enabled: true,
                cooldown_s: 3600,
                last_fired: None,
            }],
        };
        app.alerts.save(&doc).expect("save");

        let Json(view) = state(State(app)).await;
        assert_eq!(view.rules.len(), 1);
        assert_eq!(view.providers.len(), 1);
        assert!(view.doc_error.is_none());
    }

    #[tokio::test]
    async fn save_rejects_a_rule_with_no_provider() {
        let state = test_state("invalid");
        let doc = AlertsDoc {
            providers: Vec::new(),
            rules: vec![AlertRule {
                id: "cpu".to_string(),
                name: "CPU".to_string(),
                metric: "cpu_usage".to_string(),
                condition: Comparison::Greater,
                threshold: 90.0,
                duration_s: 300,
                action: "discord".to_string(),
                enabled: true,
                cooldown_s: 3600,
                last_fired: None,
            }],
        };
        assert!(save(State(state), Json(doc)).await.is_err());
    }

    #[tokio::test]
    async fn test_unknown_provider_is_not_found() {
        let state = test_state("missing");
        assert!(test(State(state), Path("ghost".to_string())).await.is_err());
    }
}
