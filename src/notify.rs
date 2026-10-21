//! Notifications: the central "something happened" bus.
//!
//! Every subsystem reports through here. Delivery is deliberately split in two:
//!
//! * **In-process**: a bounded ring buffer (the feed the UI reads) plus a
//!   `broadcast` channel (the live push the header bell subscribes to). Neither
//!   can block a caller, so an emitter on a hot path never waits on a slow
//!   consumer.
//! * **Out-of-process**: an optional webhook, POSTed with `curl` so both `http`
//!   and `https` targets work without pulling an HTTP client stack into the
//!   binary. The payload goes in on stdin, never argv, so it cannot leak through
//!   `ps`, and the URL is redacted from every log line.
//!
//! The emitter never decides whether it is interesting; that is the document's
//! job (`event_enabled`). A disabled event costs one lock and no allocation
//! beyond the request the caller already built.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::error::ApiError;
use crate::networking::model::Problem;
use crate::networking::store::write_atomic;
use crate::runner::Runner;

/// How many notifications the in-memory feed keeps.
pub const RECENT_CAP: usize = 200;

/// Fan-out capacity for the live stream; a lagging bell resynchronises on the
/// next frame rather than ever blocking an emitter.
pub const LIVE_BACKLOG: usize = 32;

/// `curl` deadline for one webhook POST.
const WEBHOOK_TIMEOUT_S: u64 = 10;

/// Stable event ids. Emitters use these; the document toggles them.
pub mod event {
    pub const FIREWALL_ROLLBACK: &str = "firewall_rollback";
    pub const BACKUP_FAILED: &str = "backup_failed";
    pub const CERT_EXPIRING: &str = "cert_expiring";
    pub const SMART_PREFAIL: &str = "smart_prefail";
    /// A scheduled or manual SMART self-test failed to run.
    pub const SMART_TEST_FAILED: &str = "smart_test_failed";
    pub const DDNS_FAILED: &str = "ddns_failed";
    pub const DDNS_UPDATED: &str = "ddns_updated";
    pub const DISK_USED: &str = "disk_used";
    pub const SERVICE_DOWN: &str = "service_down";
}

/// Severity, rendered as a badge in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warning,
    Error,
}

/// A request to notify. The centre assigns the id and timestamp, then decides
/// whether the event is enabled and whether to POST it upstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotificationRequest {
    pub level: Level,
    pub event: String,
    pub title: String,
    pub message: String,
}

impl NotificationRequest {
    pub fn info(
        event: impl Into<String>,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(Level::Info, event, title, message)
    }

    pub fn warning(
        event: impl Into<String>,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(Level::Warning, event, title, message)
    }

    pub fn error(
        event: impl Into<String>,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(Level::Error, event, title, message)
    }

    fn new(
        level: Level,
        event: impl Into<String>,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            level,
            event: event.into(),
            title: title.into(),
            message: message.into(),
        }
    }
}

/// A stored notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub id: u64,
    pub ts_ms: u64,
    pub level: Level,
    pub event: String,
    pub title: String,
    pub message: String,
}

/// The notification document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyConfig {
    /// Master switch for out-of-process delivery. The in-app feed always works.
    pub enabled: bool,
    pub backup_failed: bool,
    pub cert_expiring: bool,
    /// Warn when a managed certificate has fewer than this many days left.
    pub cert_expiring_days: u32,
    pub firewall_rollback: bool,
    pub smart_prefail: bool,
    /// Warn when a filesystem passes this usage percentage.
    pub disk_used_pct: u32,
    pub service_down: bool,
    /// Optional webhook; `http`/`https` only. POSTed with `curl`.
    pub webhook_url: Option<String>,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            backup_failed: true,
            cert_expiring: true,
            cert_expiring_days: 30,
            firewall_rollback: true,
            smart_prefail: true,
            disk_used_pct: 90,
            service_down: true,
            webhook_url: None,
        }
    }
}

impl NotifyConfig {
    pub fn validate(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        if self.cert_expiring_days == 0 || self.cert_expiring_days > 365 {
            problems.push(Problem {
                field: "cert_expiring_days".to_string(),
                message: "must be between 1 and 365 days".to_string(),
            });
        }
        if self.disk_used_pct == 0 || self.disk_used_pct > 100 {
            problems.push(Problem {
                field: "disk_used_pct".to_string(),
                message: "must be between 1 and 100 percent".to_string(),
            });
        }
        if let Some(url) = self
            .webhook_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            && !is_valid_webhook(url)
        {
            problems.push(Problem {
                field: "webhook_url".to_string(),
                message: "must be an http:// or https:// URL".to_string(),
            });
        }
        problems
    }

    /// Whether an event should be recorded and delivered.
    pub fn event_enabled(&self, event: &str) -> bool {
        match event {
            event::BACKUP_FAILED => self.backup_failed,
            event::CERT_EXPIRING => self.cert_expiring,
            event::FIREWALL_ROLLBACK => self.firewall_rollback,
            event::SMART_PREFAIL => self.smart_prefail,
            event::SERVICE_DOWN => self.service_down,
            // Disk usage is threshold-gated by its emitter, not by a toggle;
            // unknown/info events are always recorded.
            _ => true,
        }
    }

    /// Normalised webhook URL, or `None` when unset/blank.
    pub fn webhook(&self) -> Option<&str> {
        self.webhook_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
    }
}

/// Anything that can receive a notification. Emitters depend on this narrow
/// trait so they do not need the whole centre (or a tokio runtime).
pub trait Notifier: Send + Sync {
    fn notify(&self, request: NotificationRequest);
}

#[derive(Default)]
struct Inner {
    recent: VecDeque<Notification>,
    next_id: u64,
}

/// The notification centre. Cheap to clone (one `Arc`).
#[derive(Clone)]
pub struct NotificationCenter {
    config_path: PathBuf,
    runner: Arc<dyn Runner>,
    inner: Arc<RwLock<Inner>>,
    config: Arc<RwLock<NotifyConfig>>,
    live: broadcast::Sender<Notification>,
}

impl NotificationCenter {
    pub fn new(config_path: PathBuf, runner: Arc<dyn Runner>) -> Self {
        let (config, _) = load_config(&config_path);
        let (live, _) = broadcast::channel(LIVE_BACKLOG);
        Self {
            config_path,
            runner,
            inner: Arc::new(RwLock::new(Inner::default())),
            config: Arc::new(RwLock::new(config)),
            live,
        }
    }

    /// Where the document lives (for the API's parse-error report).
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn config(&self) -> NotifyConfig {
        self.config.read().expect("notify config lock").clone()
    }

    /// Persist a new document and refresh the cache. Validation is the caller's
    /// responsibility (the API rejects an invalid document before calling this).
    pub fn set_config(&self, config: NotifyConfig) -> Result<(), ApiError> {
        save_config(&self.config_path, &config)?;
        *self.config.write().expect("notify config lock") = config;
        Ok(())
    }

    /// Newest-first snapshot of the feed.
    pub fn recent(&self, limit: usize) -> Vec<Notification> {
        let inner = self.inner.read().expect("notify lock");
        inner.recent.iter().rev().take(limit).cloned().collect()
    }

    pub fn clear(&self) {
        self.inner.write().expect("notify lock").recent.clear();
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.live.subscribe()
    }

    /// Emit directly (bypassing the trait) for callers that already hold the
    /// centre; identical semantics to [`Notifier::notify`].
    pub fn emit(&self, request: NotificationRequest) {
        let config = self.config();
        if !config.event_enabled(&request.event) {
            return;
        }

        let notification = {
            let mut inner = self.inner.write().expect("notify lock");
            inner.next_id += 1;
            let notification = Notification {
                id: inner.next_id,
                ts_ms: now_ms(),
                level: request.level,
                event: request.event.clone(),
                title: request.title.clone(),
                message: request.message.clone(),
            };
            inner.recent.push_back(notification.clone());
            while inner.recent.len() > RECENT_CAP {
                inner.recent.pop_front();
            }
            notification
        };

        // A send failure only means nobody is listening right now.
        let _ = self.live.send(notification);

        if config.enabled
            && let Some(url) = config.webhook()
        {
            self.deliver_webhook(url, &request);
        }
    }

    /// POST a webhook for one request. No-op (with a debug log) when there is no
    /// tokio runtime, which is the case in unit tests.
    fn deliver_webhook(&self, url: &str, request: &NotificationRequest) {
        let payload = match serde_json::to_string(request) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::warn!(%err, "cannot serialise webhook payload");
                return;
            }
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::debug!("no runtime; skipping webhook delivery");
            return;
        };
        let runner = Arc::clone(&self.runner);
        let url = url.to_string();
        let redacted = redact_url(&url);
        handle.spawn(async move {
            let result =
                tokio::task::spawn_blocking(move || post_webhook(runner.as_ref(), &url, &payload))
                    .await;
            match result {
                Ok(Ok(())) => tracing::debug!(url = %redacted, "webhook delivered"),
                Ok(Err(err)) => tracing::warn!(url = %redacted, %err, "webhook delivery failed"),
                Err(err) => tracing::warn!(url = %redacted, %err, "webhook task failed"),
            }
        });
    }

    /// Deliver a synthetic event to the configured webhook, for the "Test"
    /// button. Bypasses the toggles so the operator can verify connectivity.
    pub fn test_webhook(&self) -> Result<String, ApiError> {
        let config = self.config();
        let Some(url) = config.webhook() else {
            return Err(ApiError::bad_request("no webhook URL is configured"));
        };
        let request = NotificationRequest::info(
            "webhook_test",
            "Webhook test",
            "If you can read this, stack-stack can reach your webhook.",
        );
        let payload = serde_json::to_string(&request)
            .map_err(|err| ApiError::internal(format!("cannot serialise payload: {err}")))?;
        post_webhook(self.runner.as_ref(), url, &payload)
            .map_err(|err| ApiError::internal(format!("webhook delivery failed: {err}")))?;
        Ok(format!("delivered to {}", redact_url(url)))
    }
}

impl Notifier for NotificationCenter {
    fn notify(&self, request: NotificationRequest) {
        self.emit(request);
    }
}

// ---------------------------------------------------------------------------
// Threshold watcher
// ---------------------------------------------------------------------------

/// Watch the telemetry stream for disk-usage and SMART-health thresholds and
/// raise a notification on the rising edge only: a disk that stays over the
/// line notifies once, not once per sample, and re-arms when it recovers.
///
/// This is deliberately separate from the sampler: the sampler must never wait
/// on notification delivery, and thresholds are policy, not metrics.
pub fn spawn_threshold_watcher(
    center: Arc<NotificationCenter>,
    mut snapshots: broadcast::Receiver<Arc<crate::system::metrics::Snapshot>>,
) {
    tokio::spawn(async move {
        let mut disk_active: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut smart_active: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            match snapshots.recv().await {
                Ok(snapshot) => {
                    let config = center.config();
                    for request in disk_alerts(&config, &snapshot, &mut disk_active) {
                        center.emit(request);
                    }
                    for request in smart_alerts(&config, &snapshot, &mut smart_active) {
                        center.emit(request);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Pure rising-edge evaluator for disk usage.
fn disk_alerts(
    config: &NotifyConfig,
    snapshot: &crate::system::metrics::Snapshot,
    active: &mut std::collections::HashSet<String>,
) -> Vec<NotificationRequest> {
    let mut requests = Vec::new();
    let mut now_over: std::collections::HashSet<String> = std::collections::HashSet::new();

    for disk in &snapshot.disks {
        if disk.used_pct >= config.disk_used_pct as f32 {
            now_over.insert(disk.mount.clone());
            if !active.contains(&disk.mount) {
                requests.push(NotificationRequest::warning(
                    event::DISK_USED,
                    format!("{} is {:.0}% full", disk.mount, disk.used_pct),
                    format!(
                        "{} ({}, {}) is at {:.0}% used, past the {}% threshold.",
                        disk.mount, disk.device, disk.fs, disk.used_pct, config.disk_used_pct
                    ),
                ));
            }
        }
    }

    *active = now_over;
    requests
}

/// Pure rising-edge evaluator for SMART health.
fn smart_alerts(
    config: &NotifyConfig,
    snapshot: &crate::system::metrics::Snapshot,
    active: &mut std::collections::HashSet<String>,
) -> Vec<NotificationRequest> {
    if !config.smart_prefail {
        active.clear();
        return Vec::new();
    }

    let mut requests = Vec::new();
    let mut now_bad: std::collections::HashSet<String> = std::collections::HashSet::new();

    for device in &snapshot.smart {
        if device.healthy == Some(false) {
            now_bad.insert(device.device.clone());
            if !active.contains(&device.device) {
                requests.push(NotificationRequest::error(
                    event::SMART_PREFAIL,
                    format!("{} reports a SMART failure", device.device),
                    format!(
                        "SMART health for {} ({}) is failing. Back up its data and plan a \
                         replacement.",
                        device.device,
                        device.model.as_deref().unwrap_or("unknown model")
                    ),
                ));
            }
        }
    }

    *active = now_bad;
    requests
}

/// Load the document; a missing file is the default, a broken one is reported.
pub fn load_config(path: &Path) -> (NotifyConfig, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<NotifyConfig>(&raw) {
            Ok(config) => (config, None),
            Err(err) => (
                NotifyConfig::default(),
                Some(format!(
                    "{} is not valid notification JSON: {err}",
                    path.display()
                )),
            ),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (NotifyConfig::default(), None),
        Err(err) => (
            NotifyConfig::default(),
            Some(format!("cannot read {}: {err}", path.display())),
        ),
    }
}

fn save_config(path: &Path, config: &NotifyConfig) -> Result<(), ApiError> {
    let mut body = serde_json::to_vec_pretty(config).map_err(|err| {
        ApiError::internal(format!("cannot serialise notification config: {err}"))
    })?;
    body.push(b'\n');
    write_atomic(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))
}

/// POST `payload` to `url` with `curl`. The body goes in on stdin so it can
/// never land in the process argument list.
fn post_webhook(runner: &dyn Runner, url: &str, payload: &str) -> Result<(), String> {
    let args: Vec<String> = [
        "-s",
        "--fail",
        "--max-time",
        &WEBHOOK_TIMEOUT_S.to_string(),
        "-X",
        "POST",
        "-H",
        "content-type: application/json",
        "--data-binary",
        "@-",
        url,
    ]
    .iter()
    .map(|arg| (*arg).to_string())
    .collect();

    let mut input = payload.as_bytes().to_vec();
    input.push(b'\n');

    let output = runner
        .run_with_input(
            "curl",
            &args,
            Duration::from_secs(WEBHOOK_TIMEOUT_S + 5),
            &input,
        )
        .map_err(|err| format!("cannot run curl: {err}"))?;

    if output.timed_out {
        return Err(format!("curl timed out after {WEBHOOK_TIMEOUT_S}s"));
    }
    if !output.success() {
        let detail = output.stderr.trim();
        return Err(if detail.is_empty() {
            format!("curl exited with {:?}", output.code)
        } else {
            detail.chars().take(300).collect()
        });
    }
    Ok(())
}

/// Strip anything that could carry a secret from a URL before it is logged:
/// credentials and the whole query string.
pub fn redact_url(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (scheme, rest),
        None => return "<redacted-url>".to_string(),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = match authority.rsplit_once('@') {
        Some((_, host)) => host,
        None => authority,
    };
    format!("{scheme}://{authority}/…")
}

/// An `http`/`https` URL with a non-empty host.
pub fn is_valid_webhook(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !matches!(scheme, "http" | "https") {
        return false;
    }
    if rest.is_empty() || rest.contains(char::is_whitespace) {
        return false;
    }
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or_else(|| rest.split(['/', '?', '#']).next().unwrap_or(""));
    !host.is_empty()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::fake::FakeRunner;

    fn temp_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stack-stack-notify-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("notify.json")
    }

    fn center(name: &str, runner: FakeRunner) -> NotificationCenter {
        NotificationCenter::new(temp_path(name), Arc::new(runner))
    }

    #[test]
    fn missing_document_is_default_and_valid() {
        let (config, error) = load_config(&temp_path("missing"));
        assert!(error.is_none());
        assert!(config.validate().is_empty());
        assert!(config.enabled);
        assert_eq!(config.disk_used_pct, 90);
    }

    #[test]
    fn round_trips_through_disk() {
        let path = temp_path("roundtrip");
        let runner: Arc<dyn Runner> = Arc::new(FakeRunner::new());
        let center = NotificationCenter::new(path.clone(), runner);

        let mut config = center.config();
        config.disk_used_pct = 75;
        config.webhook_url = Some("https://example.test/hook".to_string());
        center.set_config(config.clone()).expect("save");

        let (loaded, error) = load_config(&path);
        assert!(error.is_none());
        assert_eq!(loaded, config);
    }

    #[test]
    fn ring_buffer_keeps_the_newest_and_caps() {
        let center = center("ring", FakeRunner::new());
        for index in 0..(RECENT_CAP + 25) {
            center.emit(NotificationRequest::info("test", format!("t{index}"), "m"));
        }
        let recent = center.recent(RECENT_CAP + 100);
        assert_eq!(recent.len(), RECENT_CAP);
        // Newest first.
        assert_eq!(recent[0].title, format!("t{}", RECENT_CAP + 24));
        assert_eq!(recent[0].id, (RECENT_CAP + 25) as u64);
    }

    #[test]
    fn disabled_event_is_dropped() {
        let center = center("toggles", FakeRunner::new());
        let mut config = center.config();
        config.firewall_rollback = false;
        center.set_config(config).expect("save");

        center.emit(NotificationRequest::warning(
            event::FIREWALL_ROLLBACK,
            "x",
            "y",
        ));
        assert!(center.recent(10).is_empty());
    }

    #[test]
    fn live_subscribers_receive_emissions() {
        let center = center("live", FakeRunner::new());
        let mut rx = center.subscribe();
        center.emit(NotificationRequest::info("test", "hello", "world"));
        let received = rx.try_recv().expect("a live frame");
        assert_eq!(received.title, "hello");
    }

    #[test]
    fn webhook_validation_and_redaction() {
        assert!(is_valid_webhook("https://hooks.example.test/a/b"));
        assert!(is_valid_webhook("http://10.0.0.5:9000/hook"));
        for bad in [
            "",
            "ftp://x",
            "https://",
            "example.com/hook",
            "http://a b/c",
        ] {
            assert!(!is_valid_webhook(bad), "{bad:?} must be rejected");
        }
        assert_eq!(
            redact_url("https://user:pass@example.test/hook?token=abc"),
            "https://example.test/…"
        );
        assert_eq!(redact_url("not a url"), "<redacted-url>");
    }

    #[test]
    fn test_webhook_requires_a_url_and_reports_delivery() {
        let unset = center("webhook", FakeRunner::new());
        assert!(unset.test_webhook().is_err(), "no url configured");

        let configured = center(
            "webhook-ok",
            FakeRunner::new().respond("curl", Some(0), "", ""),
        );
        let mut config = configured.config();
        config.webhook_url = Some("https://hooks.example.test/x".to_string());
        configured.set_config(config).expect("save");
        let detail = configured.test_webhook().expect("delivered");
        assert!(detail.contains("hooks.example.test"), "{detail}");
    }

    #[test]
    fn disk_alerts_fire_once_per_rising_edge() {
        use crate::system::metrics::{Disk, Snapshot};
        let config = NotifyConfig::default();
        let mut active = std::collections::HashSet::new();

        let over = Snapshot {
            disks: vec![Disk {
                device: "/dev/sda1".to_string(),
                mount: "/".to_string(),
                fs: "ext4".to_string(),
                used_pct: 95.0,
                ..Disk::default()
            }],
            ..Snapshot::default()
        };
        assert_eq!(disk_alerts(&config, &over, &mut active).len(), 1);
        // Still over: no repeat.
        assert!(disk_alerts(&config, &over, &mut active).is_empty());

        // Recovers, then goes over again: fires again.
        let under = Snapshot {
            disks: vec![Disk {
                mount: "/".to_string(),
                used_pct: 10.0,
                ..Disk::default()
            }],
            ..Snapshot::default()
        };
        assert!(disk_alerts(&config, &under, &mut active).is_empty());
        assert_eq!(disk_alerts(&config, &over, &mut active).len(), 1);
    }

    #[test]
    fn smart_alerts_fire_only_for_failing_devices() {
        use crate::system::metrics::{Smart, Snapshot};
        let config = NotifyConfig::default();
        let mut active = std::collections::HashSet::new();

        let snapshot = Snapshot {
            smart: vec![
                Smart {
                    device: "/dev/sda".to_string(),
                    healthy: Some(false),
                    ..Smart::default()
                },
                Smart {
                    device: "/dev/sdb".to_string(),
                    healthy: Some(true),
                    ..Smart::default()
                },
                Smart {
                    device: "/dev/nvme0".to_string(),
                    healthy: None,
                    ..Smart::default()
                },
            ],
            ..Snapshot::default()
        };
        let alerts = smart_alerts(&config, &snapshot, &mut active);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].event, event::SMART_PREFAIL);
        assert!(alerts[0].message.contains("/dev/sda"));
    }

    #[test]
    fn invalid_document_is_reported() {
        let path = temp_path("bad");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "{ not json").expect("write");
        let (config, error) = load_config(&path);
        assert!(error.is_some());
        assert_eq!(config, NotifyConfig::default());
    }
}
