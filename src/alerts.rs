//! Alerting rules engine.
//!
//! The notification feed (`notify.rs`) is passive: subsystems push events into
//! it. This is the active half — operator-defined rules that watch telemetry and
//! fire an action when a metric has crossed a line *for long enough*.
//!
//! A rule reads, in the UI, as "if [metric] is [condition] [threshold] for
//! [duration], send to [action]". The evaluator (see `web::alerts`) samples
//! every 30 seconds and keeps, per rule, the instant the condition first became
//! true; it fires once the condition has held for `duration_s`, then stays quiet
//! for `cooldown_s`. Everything in this module is pure so the logic is unit
//! tested without a clock or a daemon.
//!
//! The document (`alerts.json`) holds both the rules and the notification
//! providers they target, so a rule's `action` is just a provider id.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::containers::model::Container;
use crate::error::ApiError;
use crate::networking::model::Problem;
use crate::networking::store::write_atomic;
use crate::notifications;
use crate::runner::Runner;
use crate::system::metrics::Snapshot;

/// Default sustained duration before a rule fires (5 minutes).
const DEFAULT_DURATION_S: u64 = 300;
/// Default quiet period after a rule fires (1 hour).
const DEFAULT_COOLDOWN_S: u64 = 3600;
/// The evaluator's tick, exposed so the web scheduler and docs agree.
pub const EVALUATOR_INTERVAL_S: u64 = 30;

/// Stable event id for the in-app notification a firing rule raises.
pub const ALERT_EVENT: &str = "alert_fired";

/// Message delivered to a provider and echoed into the feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertMessage {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Comparison {
    #[serde(rename = ">")]
    Greater,
    #[serde(rename = "<")]
    Less,
    #[serde(rename = "==")]
    Equal,
    #[serde(rename = "!=")]
    NotEqual,
}

impl Comparison {
    pub fn as_str(self) -> &'static str {
        match self {
            Comparison::Greater => ">",
            Comparison::Less => "<",
            Comparison::Equal => "==",
            Comparison::NotEqual => "!=",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertRule {
    pub id: String,
    pub name: String,
    /// `cpu_usage`, `memory_usage`, `load1`, `disk_usage`, `disk_usage:/mnt`,
    /// or `container_status:<name>`.
    pub metric: String,
    pub condition: Comparison,
    pub threshold: f64,
    /// How long the condition must hold before firing, in seconds.
    #[serde(default = "default_duration")]
    pub duration_s: u64,
    /// Provider id to notify when the rule fires.
    pub action: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Minimum gap between two firings of the same rule, in seconds.
    #[serde(default = "default_cooldown")]
    pub cooldown_s: u64,
    /// Last time the rule fired, Unix seconds. Persisted so a restart does not
    /// immediately re-fire a rule that already fired.
    #[serde(default)]
    pub last_fired: Option<u64>,
}

fn default_duration() -> u64 {
    DEFAULT_DURATION_S
}

fn default_cooldown() -> u64 {
    DEFAULT_COOLDOWN_S
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Discord,
    Slack,
    Email,
    Webhook,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    /// Webhook URL for the HTTP kinds; unused for email.
    #[serde(default)]
    pub url: Option<String>,
    /// Recipient for the email kind; unused otherwise.
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// The alerting document: providers plus rules.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlertsDoc {
    pub providers: Vec<ProviderConfig>,
    pub rules: Vec<AlertRule>,
}

impl AlertsDoc {
    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|provider| provider.id == id)
    }

    pub fn validate(&self) -> Vec<Problem> {
        let mut problems = Vec::new();

        let mut provider_ids: Vec<&str> = Vec::new();
        for provider in &self.providers {
            if !valid_id(&provider.id) {
                problems.push(Problem {
                    field: format!("provider {}", provider.id),
                    message: "id must be 1-64 characters of letters, digits, '-' or '_'"
                        .to_string(),
                });
            }
            if provider_ids.contains(&provider.id.as_str()) {
                problems.push(Problem {
                    field: format!("provider {}", provider.id),
                    message: "duplicate provider id".to_string(),
                });
            }
            provider_ids.push(&provider.id);

            if provider.name.trim().is_empty() {
                problems.push(Problem {
                    field: format!("provider {}", provider.id),
                    message: "needs a name".to_string(),
                });
            }

            let issue = match provider.kind {
                ProviderKind::Discord | ProviderKind::Slack | ProviderKind::Webhook => provider
                    .url
                    .as_deref()
                    .map(str::trim)
                    .filter(|url| !url.is_empty())
                    .map(|url| {
                        if crate::notify::is_valid_webhook(url) {
                            None
                        } else {
                            Some("webhook URL must be a valid http:// or https:// URL")
                        }
                    })
                    .unwrap_or(Some("needs a webhook URL")),
                ProviderKind::Email => provider
                    .to
                    .as_deref()
                    .filter(|to| crate::notifications::valid_email_address(to))
                    .map(|_| None)
                    .unwrap_or(Some(
                        "needs a plain recipient email address (no spaces or control characters)",
                    )),
            };
            if let Some(message) = issue {
                problems.push(Problem {
                    field: format!("provider {}", provider.id),
                    message: message.to_string(),
                });
            }
        }

        let mut rule_ids: Vec<&str> = Vec::new();
        for rule in &self.rules {
            let field = format!("rule {}", rule.id);
            if !valid_id(&rule.id) {
                problems.push(Problem {
                    field: field.clone(),
                    message: "id must be 1-64 characters of letters, digits, '-' or '_'"
                        .to_string(),
                });
            }
            if rule_ids.contains(&rule.id.as_str()) {
                problems.push(Problem {
                    field: field.clone(),
                    message: "duplicate rule id".to_string(),
                });
            }
            rule_ids.push(&rule.id);
            if rule.name.trim().is_empty() {
                problems.push(Problem {
                    field: field.clone(),
                    message: "needs a name".to_string(),
                });
            }
            if !valid_metric(&rule.metric) {
                problems.push(Problem {
                    field: field.clone(),
                    message: "unknown metric; use cpu_usage, memory_usage, load1, disk_usage[:mount] or container_status:<name>".to_string(),
                });
            }
            if !rule.threshold.is_finite() {
                problems.push(Problem {
                    field: field.clone(),
                    message: "threshold must be a number".to_string(),
                });
            }
            // Continuous metrics are sampled floats, so an exact equality test
            // would essentially never match. Only the discrete 1/0
            // container_status metric supports == and !=.
            if matches!(rule.condition, Comparison::Equal | Comparison::NotEqual)
                && metric_base(&rule.metric) != "container_status"
            {
                problems.push(Problem {
                    field: field.clone(),
                    message:
                        "== and != only apply to container_status; use > or < for continuous metrics"
                            .to_string(),
                });
            }
            if rule.duration_s > 24 * 3600 {
                problems.push(Problem {
                    field: field.clone(),
                    message: "duration must be under 24 hours".to_string(),
                });
            }
            if rule.cooldown_s > 7 * 24 * 3600 {
                problems.push(Problem {
                    field: field.clone(),
                    message: "cooldown must be under 7 days".to_string(),
                });
            }
            if rule.action.trim().is_empty() {
                problems.push(Problem {
                    field: field.clone(),
                    message: "needs an action (provider id)".to_string(),
                });
            } else if !self.providers.iter().any(|p| p.id == rule.action) {
                problems.push(Problem {
                    field: field.clone(),
                    message: format!("action {:?} does not match any provider", rule.action),
                });
            }
        }

        problems
    }
}

/// The base name of a metric, without any `:target` suffix.
fn metric_base(metric: &str) -> &str {
    metric.trim().split(':').next().unwrap_or("")
}

/// A rule metric is a known name, optionally with a `:target` suffix.
pub fn valid_metric(metric: &str) -> bool {
    let metric = metric.trim();
    let (base, target) = match metric.split_once(':') {
        Some((base, target)) => (base, target.trim()),
        None => (metric, ""),
    };
    match base {
        "cpu_usage" | "memory_usage" | "load1" | "disk_usage" => {
            target.is_empty() || base == "disk_usage"
        }
        "container_status" => !target.is_empty(),
        _ => false,
    }
}

/// True when `id` is a boring identifier (useful as a JSON key and URL segment).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Tolerance for the equality comparisons. Only `container_status` (0 or 1) is
/// allowed to use `==`/`!=`, but a small epsilon keeps a float representation of
/// an integer exact.
const EQ_TOLERANCE: f64 = 1e-6;

/// The comparison, applied to a sampled value.
pub fn condition_holds(condition: Comparison, value: f64, threshold: f64) -> bool {
    match condition {
        Comparison::Greater => value > threshold,
        Comparison::Less => value < threshold,
        Comparison::Equal => (value - threshold).abs() < EQ_TOLERANCE,
        Comparison::NotEqual => (value - threshold).abs() >= EQ_TOLERANCE,
    }
}

/// Sample one rule's metric from the current snapshot and container list.
///
/// Returns `None` when the metric's target does not exist (an unmounted path, a
/// container that is not present), which the evaluator treats as "not firing"
/// rather than a false alarm.
pub fn metric_value(metric: &str, snapshot: &Snapshot, containers: &[Container]) -> Option<f64> {
    let metric = metric.trim();
    let (base, target) = match metric.split_once(':') {
        Some((base, target)) => (base, target.trim()),
        None => (metric, ""),
    };

    match base {
        "cpu_usage" => Some(snapshot.cpu.usage_pct as f64),
        "memory_usage" => Some(snapshot.memory.used_pct as f64),
        "load1" => Some(snapshot.load[0] as f64),
        "disk_usage" => {
            if target.is_empty() {
                snapshot
                    .disks
                    .iter()
                    .map(|disk| disk.used_pct as f64)
                    .reduce(f64::max)
            } else {
                snapshot
                    .disks
                    .iter()
                    .find(|disk| disk.mount == target)
                    .map(|disk| disk.used_pct as f64)
            }
        }
        "container_status" => Some(
            containers
                .iter()
                .find(|container| container.name == target)
                .map(|container| {
                    if container.state == "running" {
                        1.0
                    } else {
                        0.0
                    }
                })
                // A missing container is reported as not running, which is the
                // answer an operator wants from `container_status:nginx == 1`.
                .unwrap_or(0.0),
        ),
        _ => None,
    }
}

/// Whether a rule may fire again now, given its last firing time.
pub fn cooldown_elapsed(last_fired: Option<u64>, cooldown_s: u64, now: u64) -> bool {
    match last_fired {
        Some(last) => now.saturating_sub(last) >= cooldown_s,
        None => true,
    }
}

/// Human description of a rule, used in notifications and logs.
pub fn describe(rule: &AlertRule) -> String {
    format!(
        "{} is {} {} for {}s",
        rule.metric,
        rule.condition.as_str(),
        format_threshold(rule.threshold),
        rule.duration_s
    )
}

fn format_threshold(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// The alerting document plus the runner used to reach providers.
#[derive(Clone)]
pub struct AlertContext {
    path: PathBuf,
    runner: Arc<dyn Runner>,
}

impl AlertContext {
    pub fn new(path: PathBuf, runner: Arc<dyn Runner>) -> Self {
        Self { path, runner }
    }

    /// Forgiving read: a missing file is an empty document, a malformed one is
    /// reported but does not stop the service.
    pub fn load(&self) -> (AlertsDoc, Option<String>) {
        match std::fs::read_to_string(&self.path) {
            Ok(raw) => match serde_json::from_str::<AlertsDoc>(&raw) {
                Ok(doc) => (doc, None),
                Err(err) => (
                    AlertsDoc::default(),
                    Some(format!("{} is not valid JSON: {err}", self.path.display())),
                ),
            },
            Err(_) => (AlertsDoc::default(), None),
        }
    }

    pub fn save(&self, doc: &AlertsDoc) -> Result<(), ApiError> {
        let mut body = serde_json::to_vec_pretty(doc)
            .map_err(|err| ApiError::internal(format!("cannot serialise alerts: {err}")))?;
        body.push(b'\n');
        write_atomic(&self.path, &body).map_err(|err| {
            ApiError::internal(format!("cannot write {}: {err}", self.path.display()))
        })?;
        // The document holds provider webhook URLs (bearer capabilities), so it
        // is owner-only, like the other secret-bearing documents.
        restrict_600(&self.path);
        Ok(())
    }

    /// Send `message` to one provider, blocking on the shared runner.
    pub fn deliver(&self, provider: &ProviderConfig, message: &AlertMessage) -> Result<(), String> {
        notifications::deliver(self.runner.as_ref(), provider, message)
    }

    /// Remember that a rule just fired, so the cooldown survives a restart.
    pub fn record_fired(&self, rule_id: &str) {
        let (mut doc, _) = self.load();
        if let Some(rule) = doc.rules.iter_mut().find(|rule| rule.id == rule_id) {
            rule.last_fired = Some(now_secs());
            let _ = self.save(&doc);
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(unix)]
fn restrict_600(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_600(_path: &std::path::Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::containers::model::{Container, ContainerSource};
    use crate::system::metrics::{Cpu, Disk, Memory, Snapshot};

    fn rule(id: &str, metric: &str, condition: Comparison, threshold: f64) -> AlertRule {
        AlertRule {
            id: id.to_string(),
            name: id.to_string(),
            metric: metric.to_string(),
            condition,
            threshold,
            duration_s: 300,
            action: "discord".to_string(),
            enabled: true,
            cooldown_s: 3600,
            last_fired: None,
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            cpu: Cpu {
                usage_pct: 95.0,
                ..Cpu::default()
            },
            memory: Memory {
                used_pct: 40.0,
                ..Memory::default()
            },
            load: [2.5, 1.0, 0.5],
            disks: vec![
                Disk {
                    mount: "/".to_string(),
                    used_pct: 80.0,
                    ..Disk::default()
                },
                Disk {
                    mount: "/mnt/data".to_string(),
                    used_pct: 91.0,
                    ..Disk::default()
                },
            ],
            ..Snapshot::default()
        }
    }

    fn container(name: &str, state: &str) -> Container {
        Container {
            id: name.to_string(),
            name: name.to_string(),
            image: "x".to_string(),
            state: state.to_string(),
            status: String::new(),
            created: 0,
            ports: Vec::new(),
            source: ContainerSource::Manual,
            app_id: None,
            stack: None,
            web_urls: Vec::new(),
        }
    }

    #[test]
    fn metrics_read_from_the_snapshot() {
        let snap = snapshot();
        let containers = vec![container("nginx", "running")];
        assert_eq!(metric_value("cpu_usage", &snap, &containers), Some(95.0));
        assert_eq!(metric_value("memory_usage", &snap, &containers), Some(40.0));
        assert_eq!(metric_value("load1", &snap, &containers), Some(2.5));
        // Aggregate disk usage is the worst mount.
        assert_eq!(metric_value("disk_usage", &snap, &containers), Some(91.0));
        assert_eq!(metric_value("disk_usage:/", &snap, &containers), Some(80.0));
        // Unknown mount and unknown metric yield nothing.
        assert_eq!(metric_value("disk_usage:/nope", &snap, &containers), None);
        assert_eq!(metric_value("bogus", &snap, &containers), None);
        // Container status is 1/0 and defaults to 0 when missing.
        assert_eq!(
            metric_value("container_status:nginx", &snap, &containers),
            Some(1.0)
        );
        assert_eq!(
            metric_value("container_status:redis", &snap, &containers),
            Some(0.0)
        );
    }

    #[test]
    fn conditions_compare_correctly() {
        assert!(condition_holds(Comparison::Greater, 95.0, 90.0));
        assert!(!condition_holds(Comparison::Greater, 90.0, 90.0));
        assert!(condition_holds(Comparison::Less, 5.0, 10.0));
        assert!(condition_holds(Comparison::Equal, 1.0, 1.0));
        assert!(condition_holds(Comparison::NotEqual, 0.0, 1.0));
    }

    #[test]
    fn cooldown_gates_refiring() {
        assert!(cooldown_elapsed(None, 3600, 10_000));
        assert!(!cooldown_elapsed(Some(9_000), 3600, 10_000));
        assert!(cooldown_elapsed(Some(1_000), 3600, 10_000));
    }

    #[test]
    fn valid_metric_covers_the_documented_set() {
        for ok in [
            "cpu_usage",
            "memory_usage",
            "load1",
            "disk_usage",
            "disk_usage:/mnt/data",
            "container_status:nginx",
        ] {
            assert!(valid_metric(ok), "{ok} should be valid");
        }
        for bad in ["", "bogus", "container_status", "cpu_usage:x"] {
            assert!(!valid_metric(bad), "{bad} should be invalid");
        }
    }

    #[test]
    fn validation_requires_a_known_provider() {
        let mut doc = AlertsDoc {
            providers: vec![ProviderConfig {
                id: "discord".to_string(),
                name: "Discord".to_string(),
                kind: ProviderKind::Discord,
                url: Some("https://hooks.example.test/x".to_string()),
                to: None,
                enabled: true,
            }],
            rules: vec![rule("cpu", "cpu_usage", Comparison::Greater, 90.0)],
        };
        assert!(doc.validate().is_empty());

        // A rule pointing at a provider that does not exist is rejected.
        doc.rules[0].action = "ghost".to_string();
        assert!(!doc.validate().is_empty());
    }

    #[test]
    fn duplicate_rule_ids_are_rejected() {
        let doc = AlertsDoc {
            providers: vec![ProviderConfig {
                id: "discord".to_string(),
                name: "Discord".to_string(),
                kind: ProviderKind::Discord,
                url: Some("https://hooks.example.test/x".to_string()),
                to: None,
                enabled: true,
            }],
            rules: vec![
                rule("cpu", "cpu_usage", Comparison::Greater, 90.0),
                rule("cpu", "memory_usage", Comparison::Greater, 90.0),
            ],
        };
        assert!(
            doc.validate()
                .iter()
                .any(|p| p.message.contains("duplicate rule id"))
        );
    }

    #[test]
    fn equality_is_only_allowed_for_container_status() {
        let mut doc = AlertsDoc {
            providers: vec![ProviderConfig {
                id: "discord".to_string(),
                name: "Discord".to_string(),
                kind: ProviderKind::Discord,
                url: Some("https://hooks.example.test/x".to_string()),
                to: None,
                enabled: true,
            }],
            rules: vec![rule("cpu", "cpu_usage", Comparison::Equal, 50.0)],
        };
        assert!(
            doc.validate()
                .iter()
                .any(|p| p.field.starts_with("rule cpu"))
        );

        doc.rules[0].metric = "container_status:nginx".to_string();
        doc.rules[0].condition = Comparison::Equal;
        doc.rules[0].threshold = 1.0;
        assert!(doc.validate().is_empty(), "{:?}", doc.validate());
    }

    #[test]
    fn provider_without_url_is_rejected() {
        let doc = AlertsDoc {
            providers: vec![ProviderConfig {
                id: "discord".to_string(),
                name: "Discord".to_string(),
                kind: ProviderKind::Discord,
                url: None,
                to: None,
                enabled: true,
            }],
            rules: Vec::new(),
        };
        let problems = doc.validate();
        assert_eq!(problems.len(), 1);
        assert!(problems[0].message.contains("webhook URL"));
    }

    #[test]
    fn document_round_trips_through_disk() {
        let path = std::env::temp_dir().join(format!(
            "stack-stack-alerts-{}-alerts.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let ctx = AlertContext::new(
            path.clone(),
            Arc::new(crate::runner::SystemRunner::default()),
        );
        let doc = AlertsDoc {
            providers: vec![ProviderConfig {
                id: "discord".to_string(),
                name: "Discord".to_string(),
                kind: ProviderKind::Discord,
                url: Some("https://hooks.example.test/x".to_string()),
                to: None,
                enabled: true,
            }],
            rules: vec![rule("cpu", "cpu_usage", Comparison::Greater, 90.0)],
        };
        ctx.save(&doc).expect("save");
        let (loaded, error) = ctx.load();
        assert!(error.is_none());
        assert_eq!(loaded, doc);
    }
}
