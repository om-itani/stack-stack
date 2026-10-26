//! Container / stack health checks.
//!
//! An operator can attach an optional probe (an HTTP GET or a TCP connect) to a
//! compose stack. A 30-second evaluator runs each probe; after a configurable
//! number of consecutive failures it restarts the stack through the job runner
//! and raises a notification. A per-stack cooldown keeps a genuinely broken
//! stack from being restarted in a loop.
//!
//! The HTTP probe shells out to `curl` (the shared command seam); the TCP probe
//! is a plain `std::net` connect, so neither pulls in a network stack.

use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::model::Problem;
use crate::networking::store::write_atomic;
use crate::runner::Runner;

/// How often every enabled probe is evaluated.
pub const HEALTH_INTERVAL_S: u64 = 30;
/// Consecutive failures before a restart, unless the check overrides it.
pub const DEFAULT_RETRIES: u32 = 3;
/// Quiet period after a restart, unless the check overrides it.
pub const DEFAULT_COOLDOWN_S: u64 = 300;

fn default_enabled() -> bool {
    true
}

fn default_timeout() -> u64 {
    5
}

fn default_retries() -> u32 {
    DEFAULT_RETRIES
}

fn default_cooldown() -> u64 {
    DEFAULT_COOLDOWN_S
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeKind {
    Http,
    Tcp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthCheck {
    /// Compose project / stack name this probe guards.
    pub stack: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub kind: ProbeKind,
    /// An `http(s)://` URL for an HTTP probe, or `host:port` for a TCP connect.
    pub target: String,
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
    /// Consecutive failures before the stack is restarted.
    #[serde(default = "default_retries")]
    pub retries: u32,
    /// Minimum gap between two restarts of the same stack, in seconds.
    #[serde(default = "default_cooldown")]
    pub cooldown_s: u64,
    /// Last restart, Unix seconds, persisted so a restart survives a reload.
    #[serde(default)]
    pub last_restart: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthDoc {
    pub checks: Vec<HealthCheck>,
}

impl HealthDoc {
    pub fn validate(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for check in &self.checks {
            let field = format!("check {}", check.stack);
            if crate::containers::store::sanitize_stack_name(&check.stack).is_err() {
                problems.push(Problem {
                    field: field.clone(),
                    message: "stack name must be a valid compose project name".to_string(),
                });
            }
            if seen.contains(&check.stack.as_str()) {
                problems.push(Problem {
                    field: field.clone(),
                    message: "duplicate check for this stack".to_string(),
                });
            }
            seen.push(&check.stack);

            let target_ok = match check.kind {
                ProbeKind::Http => crate::notify::is_valid_webhook(check.target.trim()),
                ProbeKind::Tcp => parse_tcp_target(&check.target).is_some(),
            };
            if !target_ok {
                problems.push(Problem {
                    field: field.clone(),
                    message: match check.kind {
                        ProbeKind::Http => "target must be an http:// or https:// URL".to_string(),
                        ProbeKind::Tcp => "target must be host:port".to_string(),
                    },
                });
            }
            if check.timeout_s == 0 || check.timeout_s > 60 {
                problems.push(Problem {
                    field: field.clone(),
                    message: "timeout must be between 1 and 60 seconds".to_string(),
                });
            }
            if check.retries == 0 || check.retries > 10 {
                problems.push(Problem {
                    field: field.clone(),
                    message: "retries must be between 1 and 10".to_string(),
                });
            }
            if check.cooldown_s > 24 * 3600 {
                problems.push(Problem {
                    field: field.clone(),
                    message: "cooldown must be under 24 hours".to_string(),
                });
            }
        }
        problems
    }
}

/// `host:port` for a TCP probe. IPv6 literals must be bracketed.
pub fn parse_tcp_target(target: &str) -> Option<(String, u16)> {
    let target = target.trim();
    let (host, port) = target.rsplit_once(':')?;
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return None;
    }
    let port: u16 = port.trim().parse().ok()?;
    if port == 0 {
        return None;
    }
    Some((host.to_string(), port))
}

/// Whether a check may fire a restart now, given its last restart time.
pub fn cooldown_elapsed(last_restart: Option<u64>, cooldown_s: u64, now: u64) -> bool {
    match last_restart {
        Some(last) => now.saturating_sub(last) >= cooldown_s,
        None => true,
    }
}

/// A TCP connect with a deadline. Resolves the host first so a DNS failure is a
/// probe failure rather than a hang.
pub fn probe_tcp(host: &str, port: u16, timeout: Duration) -> bool {
    let Ok(addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    for addr in addrs {
        if TcpStream::connect_timeout(&addr, timeout).is_ok() {
            return true;
        }
    }
    false
}

/// An HTTP probe through `curl`. Any 2xx/3xx response counts as healthy; a
/// connection error, a 4xx/5xx, or a timeout is a failure.
pub fn probe_http(runner: &dyn Runner, url: &str, timeout_s: u64) -> bool {
    let timeout = timeout_s.clamp(1, 60);
    let args: Vec<String> = [
        "-sS",
        "-o",
        "/dev/null",
        "-w",
        "%{http_code}",
        "--max-time",
        &timeout.to_string(),
        "--",
        url,
    ]
    .iter()
    .map(|arg| (*arg).to_string())
    .collect();
    match runner.run_timeout("curl", &args, Duration::from_secs(timeout + 5)) {
        Ok(output) if !output.timed_out => {
            let code = output.stdout.trim();
            // curl -f is not used, so a 4xx/5xx still exits 0; check the code.
            code.starts_with('2') || code.starts_with('3')
        }
        _ => false,
    }
}

/// The health document plus the runner used for HTTP probes.
#[derive(Clone)]
pub struct HealthContext {
    path: PathBuf,
    runner: Arc<dyn Runner>,
}

impl HealthContext {
    pub fn new(path: PathBuf, runner: Arc<dyn Runner>) -> Self {
        Self { path, runner }
    }

    pub fn load(&self) -> (HealthDoc, Option<String>) {
        match std::fs::read_to_string(&self.path) {
            Ok(raw) => match serde_json::from_str::<HealthDoc>(&raw) {
                Ok(doc) => (doc, None),
                Err(err) => (
                    HealthDoc::default(),
                    Some(format!("{} is not valid JSON: {err}", self.path.display())),
                ),
            },
            Err(_) => (HealthDoc::default(), None),
        }
    }

    pub fn save(&self, doc: &HealthDoc) -> Result<(), ApiError> {
        let mut body = serde_json::to_vec_pretty(doc)
            .map_err(|err| ApiError::internal(format!("cannot serialise health checks: {err}")))?;
        body.push(b'\n');
        write_atomic(&self.path, &body).map_err(|err| {
            ApiError::internal(format!("cannot write {}: {err}", self.path.display()))
        })
    }

    /// Run one check. Blocking; the caller runs it on the blocking pool.
    pub fn probe(&self, check: &HealthCheck) -> bool {
        match check.kind {
            ProbeKind::Http => {
                probe_http(self.runner.as_ref(), check.target.trim(), check.timeout_s)
            }
            ProbeKind::Tcp => match parse_tcp_target(&check.target) {
                Some((host, port)) => probe_tcp(
                    &host,
                    port,
                    Duration::from_secs(check.timeout_s.clamp(1, 60)),
                ),
                None => false,
            },
        }
    }

    /// Persist a restart timestamp so the cooldown survives a reload.
    pub fn record_restart(&self, stack: &str) {
        let (mut doc, _) = self.load();
        if let Some(check) = doc.checks.iter_mut().find(|check| check.stack == stack) {
            check.last_restart = Some(now_secs());
            let _ = self.save(&doc);
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::fake::FakeRunner;

    fn check(kind: ProbeKind, target: &str) -> HealthCheck {
        HealthCheck {
            stack: "web".to_string(),
            enabled: true,
            kind,
            target: target.to_string(),
            timeout_s: 5,
            retries: 3,
            cooldown_s: 300,
            last_restart: None,
        }
    }

    #[test]
    fn tcp_targets_parse() {
        assert_eq!(
            parse_tcp_target("10.0.0.5:8080"),
            Some(("10.0.0.5".to_string(), 8080))
        );
        assert_eq!(
            parse_tcp_target("[::1]:443"),
            Some(("::1".to_string(), 443))
        );
        assert_eq!(parse_tcp_target("hostonly"), None);
        assert_eq!(parse_tcp_target(":80"), None);
        assert_eq!(parse_tcp_target("h:0"), None);
    }

    #[test]
    fn http_probe_checks_the_status_code() {
        let ok = FakeRunner::new().respond("curl", Some(0), "200", "");
        assert!(probe_http(&ok, "http://x/health", 5));

        let not_found = FakeRunner::new().respond("curl", Some(0), "404", "");
        assert!(!probe_http(&not_found, "http://x/health", 5));

        let err = FakeRunner::new().respond("curl", Some(7), "", "connection refused");
        assert!(!probe_http(&err, "http://x/health", 5));
    }

    #[test]
    fn cooldown_gates_restarts() {
        assert!(cooldown_elapsed(None, 300, 1000));
        assert!(!cooldown_elapsed(Some(900), 300, 1000));
        assert!(cooldown_elapsed(Some(100), 300, 1000));
    }

    #[test]
    fn validation_covers_both_kinds() {
        let mut doc = HealthDoc {
            checks: vec![check(ProbeKind::Http, "http://10.0.0.5/health")],
        };
        assert!(doc.validate().is_empty());

        doc.checks[0].kind = ProbeKind::Tcp;
        doc.checks[0].target = "10.0.0.5:9000".to_string();
        assert!(doc.validate().is_empty());

        doc.checks[0].target = "not a target".to_string();
        assert!(!doc.validate().is_empty());
    }
}
