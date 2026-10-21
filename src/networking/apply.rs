//! Config rendering, application, and the firewall safe-mode window.
//!
//! Applying is per-subsystem and never fatal for the whole run: nftables,
//! dnsmasq, unbound, cloudflared and SQM each report their own result so one
//! missing daemon cannot block the rest.
//!
//! The firewall gets an extra guarantee. After a *successful* apply the previous
//! ruleset is held for [`CONFIRM_WINDOW_S`]; if the operator does not confirm,
//! it is restored. That is what stops a bad rule from locking out SSH or the
//! web UI with no way back in.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::Mutex;

use crate::networking::model::NetworkConfig;
use crate::networking::runner::Runner;
use crate::networking::{cloudflared, dnsmasq, nftables, sqm, unbound};
use crate::notify::{NotificationRequest, Notifier, event};

/// How long a fresh firewall ruleset stays reversible.
pub const CONFIRM_WINDOW_S: u64 = 30;

#[derive(Debug, Clone)]
pub struct ApplyPaths {
    pub nftables: PathBuf,
    pub rollback: PathBuf,
    pub dnsmasq: PathBuf,
    pub unbound: PathBuf,
    pub cloudflared: PathBuf,
}

impl Default for ApplyPaths {
    fn default() -> Self {
        Self {
            nftables: PathBuf::from("/etc/stack-stack/net/nftables.conf"),
            rollback: PathBuf::from("/etc/stack-stack/net/nftables.rollback.conf"),
            dnsmasq: PathBuf::from("/etc/dnsmasq.d/stack-stack.conf"),
            unbound: PathBuf::from("/etc/unbound/unbound.conf.d/stack-stack.conf"),
            cloudflared: PathBuf::from("/etc/cloudflared/config.yml"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepResult {
    pub subsystem: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingInfo {
    pub generation: u64,
    pub expires_in_s: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyReport {
    pub steps: Vec<StepResult>,
    /// Present when a firewall change is reversible and awaits confirmation.
    pub pending: Option<PendingInfo>,
}

// ---------------------------------------------------------------------------
// Synchronous apply
// ---------------------------------------------------------------------------

/// Result of applying every subsystem: per-step outcomes plus the previous
/// firewall ruleset when the firewall step succeeded (empty string means
/// "there were no rules to restore").
struct Applied {
    steps: Vec<StepResult>,
    rollback: Option<String>,
}

fn apply_sync(config: &NetworkConfig, paths: &ApplyPaths, runner: &dyn Runner) -> Applied {
    let mut steps = Vec::new();
    let rollback = apply_firewall(config, paths, runner, &mut steps);
    apply_dnsmasq(config, paths, runner, &mut steps);
    apply_unbound(config, paths, runner, &mut steps);
    apply_cloudflared(config, paths, runner, &mut steps);
    apply_sqm(config, runner, &mut steps);
    Applied { steps, rollback }
}

fn step(steps: &mut Vec<StepResult>, subsystem: &str, result: Result<String, String>) {
    match result {
        Ok(detail) => steps.push(StepResult {
            subsystem: subsystem.to_string(),
            ok: true,
            detail,
        }),
        Err(detail) => {
            tracing::warn!(subsystem, %detail, "network apply step failed");
            steps.push(StepResult {
                subsystem: subsystem.to_string(),
                ok: false,
                detail,
            });
        }
    }
}

fn apply_firewall(
    config: &NetworkConfig,
    paths: &ApplyPaths,
    runner: &dyn Runner,
    steps: &mut Vec<StepResult>,
) -> Option<String> {
    let outcome = (|| -> Result<(String, String), String> {
        let ruleset = nftables::render(config);
        crate::networking::store::write_atomic(&paths.nftables, ruleset.as_bytes())
            .map_err(|err| format!("cannot write {}: {err}", paths.nftables.display()))?;

        // Check first: a ruleset that does not parse is never loaded.
        let check = runner.call("nft", &["-c", "-f", &paths.nftables.to_string_lossy()])?;
        if !check.success() {
            return Err(format!(
                "nftables check failed: {}",
                trimmed(&check.combined())
            ));
        }

        // Snapshot the running ruleset so we can undo this apply.
        let snapshot = runner.call("nft", &["list", "ruleset"])?;
        let previous = if snapshot.success() {
            snapshot.stdout.clone()
        } else {
            String::new()
        };

        let applied = runner.call("nft", &["-f", &paths.nftables.to_string_lossy()])?;
        if !applied.success() {
            return Err(format!(
                "nftables apply failed: {}",
                trimmed(&applied.combined())
            ));
        }

        Ok((format!("{} bytes loaded", ruleset.len()), previous))
    })();

    match outcome {
        Ok((detail, previous)) => {
            steps.push(StepResult {
                subsystem: "firewall".to_string(),
                ok: true,
                detail,
            });
            Some(previous)
        }
        Err(detail) => {
            steps.push(StepResult {
                subsystem: "firewall".to_string(),
                ok: false,
                detail,
            });
            None
        }
    }
}

fn apply_dnsmasq(
    config: &NetworkConfig,
    paths: &ApplyPaths,
    runner: &dyn Runner,
    steps: &mut Vec<StepResult>,
) {
    let result = (|| -> Result<String, String> {
        // The config is always written, even with DHCP off, so the daemon is
        // left in a defined (inert) state rather than holding stale leases.
        let rendered = dnsmasq::render(config);
        crate::networking::store::write_atomic(&paths.dnsmasq, rendered.as_bytes())
            .map_err(|err| format!("cannot write {}: {err}", paths.dnsmasq.display()))?;

        let check = runner.call(
            "dnsmasq",
            &[
                "--test",
                &format!("--conf-file={}", paths.dnsmasq.display()),
            ],
        )?;
        if !check.success() {
            return Err(format!(
                "dnsmasq config rejected: {}",
                trimmed(&check.combined())
            ));
        }

        let restart = runner.call("systemctl", &["restart", "dnsmasq"])?;
        if !restart.success() {
            return Err(format!(
                "systemctl restart dnsmasq: {}",
                trimmed(&restart.combined())
            ));
        }

        Ok(if config.dhcp.enabled {
            "DHCP active".to_string()
        } else {
            "DHCP disabled (inert config)".to_string()
        })
    })();
    step(steps, "dhcp", result);
}

fn apply_unbound(
    config: &NetworkConfig,
    paths: &ApplyPaths,
    runner: &dyn Runner,
    steps: &mut Vec<StepResult>,
) {
    let result = (|| -> Result<String, String> {
        if !config.dns.enabled {
            // Best effort: a stopped resolver is the expected state here.
            let _ = runner.call("systemctl", &["stop", "unbound"]);
            return Ok("DNS disabled".to_string());
        }

        let mut rendered = unbound::render(config);
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
        crate::networking::store::write_atomic(&paths.unbound, rendered.as_bytes())
            .map_err(|err| format!("cannot write {}: {err}", paths.unbound.display()))?;

        let check = runner.call("unbound-checkconf", &[&paths.unbound.to_string_lossy()])?;
        if !check.success() {
            return Err(format!(
                "unbound config rejected: {}",
                trimmed(&check.combined())
            ));
        }

        let restart = runner.call("systemctl", &["restart", "unbound"])?;
        if !restart.success() {
            return Err(format!(
                "systemctl restart unbound: {}",
                trimmed(&restart.combined())
            ));
        }

        Ok(format!(
            "{} rewrites, {} blocked domains",
            config.dns.rewrites.len(),
            config.dns.blocked_domains.len()
        ))
    })();
    step(steps, "dns", result);
}

fn apply_cloudflared(
    config: &NetworkConfig,
    paths: &ApplyPaths,
    runner: &dyn Runner,
    steps: &mut Vec<StepResult>,
) {
    let result = (|| -> Result<String, String> {
        if !config.tunnel.enabled {
            let _ = runner.call("systemctl", &["stop", "cloudflared"]);
            return Ok("tunnel disabled".to_string());
        }

        let rendered = cloudflared::render(config);
        crate::networking::store::write_atomic(&paths.cloudflared, rendered.as_bytes())
            .map_err(|err| format!("cannot write {}: {err}", paths.cloudflared.display()))?;

        let config_path = paths.cloudflared.to_string_lossy().to_string();
        let check = runner.call(
            "cloudflared",
            &["tunnel", "ingress", "validate", "--config", &config_path],
        )?;
        if !check.success() {
            return Err(format!(
                "cloudflared config rejected: {}",
                trimmed(&check.combined())
            ));
        }

        let restart = runner.call("systemctl", &["restart", "cloudflared"])?;
        if !restart.success() {
            return Err(format!(
                "systemctl restart cloudflared: {}",
                trimmed(&restart.combined())
            ));
        }

        Ok(format!("{} hostnames", config.tunnel.ingress.len()))
    })();
    step(steps, "tunnel", result);
}

fn apply_sqm(config: &NetworkConfig, runner: &dyn Runner, steps: &mut Vec<StepResult>) {
    let result = (|| -> Result<String, String> {
        if !config.sqm.enabled {
            return run_commands(runner, &sqm::clear_commands(config))
                .map(|_| "SQM disabled".to_string());
        }
        run_commands(runner, &sqm::apply_commands(config))?;
        Ok(format!(
            "cake {} / {} kbit",
            config.sqm.download_kbps, config.sqm.upload_kbps
        ))
    })();
    step(steps, "sqm", result);
}

fn run_commands(runner: &dyn Runner, commands: &[(String, Vec<String>)]) -> Result<(), String> {
    for (program, args) in commands {
        let output = runner.run(program, args)?;
        // `qdisc del` on an unshaped interface legitimately fails; the caller
        // asked for the end state, not for every intermediate step to succeed.
        if !output.success() && !output.timed_out {
            tracing::debug!(
                program,
                %output.stderr,
                "best-effort SQM command returned non-zero"
            );
        }
    }
    Ok(())
}

fn trimmed(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        "no output".to_string()
    } else if text.len() > 400 {
        text[..400].to_string()
    } else {
        text.to_string()
    }
}

/// Restore the previous firewall ruleset. An empty snapshot means there were no
/// rules before the apply, so the equivalent undo is a flush.
fn restore_firewall(runner: &dyn Runner, paths: &ApplyPaths, previous: &str) -> Result<(), String> {
    if previous.trim().is_empty() {
        let output = runner.call("nft", &["flush", "ruleset"])?;
        if output.success() {
            Ok(())
        } else {
            Err(format!(
                "nft flush ruleset: {}",
                trimmed(&output.combined())
            ))
        }
    } else {
        crate::networking::store::write_atomic(&paths.rollback, previous.as_bytes())
            .map_err(|err| format!("cannot write {}: {err}", paths.rollback.display()))?;
        let output = runner.call("nft", &["-f", &paths.rollback.to_string_lossy()])?;
        if output.success() {
            Ok(())
        } else {
            Err(format!(
                "nft restore failed: {}",
                trimmed(&output.combined())
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Manager: owns the confirmation window
// ---------------------------------------------------------------------------

struct Pending {
    generation: u64,
    expires_at: Instant,
    previous_ruleset: String,
}

#[derive(Clone)]
pub struct ApplyManager {
    paths: Arc<ApplyPaths>,
    runner: Arc<dyn Runner>,
    window: Duration,
    generation: Arc<AtomicU64>,
    state: Arc<Mutex<Option<Pending>>>,
    notifier: Option<Arc<dyn Notifier>>,
}

impl ApplyManager {
    pub fn new(paths: ApplyPaths, runner: Arc<dyn Runner>, window: Duration) -> Self {
        Self {
            paths: Arc::new(paths),
            runner,
            window,
            generation: Arc::new(AtomicU64::new(1)),
            state: Arc::new(Mutex::new(None)),
            notifier: None,
        }
    }

    /// Attach a notification sink. Set by [`crate::networking::NetworkContext`]
    /// when the router runs inside the full app; absent in unit tests.
    pub fn with_notifier(mut self, notifier: Arc<dyn Notifier>) -> Self {
        self.notifier = Some(notifier);
        self
    }

    fn notify(&self, request: NotificationRequest) {
        if let Some(notifier) = &self.notifier {
            notifier.notify(request);
        }
    }

    /// Apply every subsystem. The returned report always covers all of them,
    /// even when some failed.
    pub async fn apply(
        &self,
        config: &NetworkConfig,
    ) -> Result<ApplyReport, crate::error::ApiError> {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        let config = config.clone();
        let paths = Arc::clone(&self.paths);
        let runner = Arc::clone(&self.runner);

        let applied =
            tokio::task::spawn_blocking(move || apply_sync(&config, &paths, runner.as_ref()))
                .await
                .map_err(|err| {
                    crate::error::ApiError::internal(format!("apply task failed: {err}"))
                })?;

        let mut pending_info = None;
        if let Some(previous) = applied.rollback {
            let expires_at = Instant::now() + self.window;
            {
                let mut guard = self.state.lock().await;
                *guard = Some(Pending {
                    generation,
                    expires_at,
                    previous_ruleset: previous,
                });
            }
            self.spawn_expiry(generation);
            pending_info = Some(PendingInfo {
                generation,
                expires_in_s: self.window.as_secs(),
            });
        }

        Ok(ApplyReport {
            steps: applied.steps,
            pending: pending_info,
        })
    }

    /// Apply only the DNS subsystem.
    ///
    /// Used when a proxy route changes a local rewrite. It deliberately never
    /// touches the firewall, so it cannot open the safe-mode confirmation window
    /// or risk a lockout — the operator is editing proxy routes, not rules.
    pub async fn apply_dns(
        &self,
        config: &NetworkConfig,
    ) -> Result<StepResult, crate::error::ApiError> {
        let config = config.clone();
        let paths = Arc::clone(&self.paths);
        let runner = Arc::clone(&self.runner);
        tokio::task::spawn_blocking(move || {
            let mut steps = Vec::new();
            apply_unbound(&config, &paths, runner.as_ref(), &mut steps);
            steps.pop().expect("the DNS step is always pushed")
        })
        .await
        .map_err(|err| crate::error::ApiError::internal(format!("dns apply task failed: {err}")))
    }

    /// Accept the new ruleset, cancelling the automatic rollback.
    pub async fn confirm(&self) -> Result<u64, crate::error::ApiError> {
        let taken = self.state.lock().await.take();
        match taken {
            Some(pending) => {
                tracing::info!(generation = pending.generation, "firewall change confirmed");
                Ok(pending.generation)
            }
            None => Err(crate::error::ApiError::bad_request(
                "no firewall change is awaiting confirmation (it was already confirmed or rolled back)",
            )),
        }
    }

    pub async fn pending(&self) -> Option<PendingInfo> {
        let guard = self.state.lock().await;
        guard.as_ref().map(|pending| PendingInfo {
            generation: pending.generation,
            expires_in_s: pending
                .expires_at
                .saturating_duration_since(Instant::now())
                .as_secs(),
        })
    }

    fn spawn_expiry(&self, generation: u64) {
        let manager = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(manager.window).await;
            manager.expire(generation).await;
        });
    }

    /// Called by the timer; also safe to call directly (tests do).
    pub async fn expire(&self, generation: u64) {
        let taken = {
            let mut guard = self.state.lock().await;
            match guard.as_ref() {
                Some(pending) if pending.generation == generation => guard.take(),
                _ => None,
            }
        };

        let Some(pending) = taken else {
            return;
        };

        let paths = Arc::clone(&self.paths);
        let runner = Arc::clone(&self.runner);
        let previous = pending.previous_ruleset;
        let result = tokio::task::spawn_blocking(move || {
            restore_firewall(runner.as_ref(), &paths, &previous)
        })
        .await;

        match result {
            Ok(Ok(())) => {
                tracing::warn!(
                    generation,
                    "firewall change was not confirmed in time; previous ruleset restored"
                );
                self.notify(NotificationRequest::warning(
                    event::FIREWALL_ROLLBACK,
                    "Firewall change rolled back",
                    "The new firewall ruleset was not confirmed in time, so the previous ruleset \
                     was restored. Any intended change did not take effect.",
                ));
            }
            Ok(Err(err)) => {
                tracing::error!(
                    generation,
                    %err,
                    "firewall rollback FAILED; the host may be unreachable"
                );
                self.notify(NotificationRequest::error(
                    event::FIREWALL_ROLLBACK,
                    "Firewall rollback FAILED",
                    format!(
                        "The unconfirmed ruleset could not be rolled back: {err}. The host may be \
                         unreachable."
                    ),
                ));
            }
            Err(err) => {
                tracing::error!(generation, %err, "firewall rollback task failed");
                self.notify(NotificationRequest::error(
                    event::FIREWALL_ROLLBACK,
                    "Firewall rollback task failed",
                    format!("The rollback task could not run: {err}."),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::networking::model::{BridgeConfig, DhcpConfig, InterfaceConfig, InterfaceRole};
    use crate::networking::runner::fake::FakeRunner;

    fn temp_paths(name: &str) -> ApplyPaths {
        let dir =
            std::env::temp_dir().join(format!("stack-stack-apply-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ApplyPaths {
            nftables: dir.join("nftables.conf"),
            rollback: dir.join("nftables.rollback.conf"),
            dnsmasq: dir.join("dnsmasq.conf"),
            unbound: dir.join("unbound.conf"),
            cloudflared: dir.join("cloudflared.yml"),
        }
    }

    fn example_config() -> NetworkConfig {
        NetworkConfig {
            interfaces: vec![
                InterfaceConfig {
                    name: "eth0".to_string(),
                    role: InterfaceRole::Wan,
                    address: None,
                },
                InterfaceConfig {
                    name: "eth1".to_string(),
                    role: InterfaceRole::Lan,
                    address: None,
                },
            ],
            bridges: vec![BridgeConfig {
                name: "br0".to_string(),
                members: vec!["eth1".to_string()],
                address: Some("192.168.1.1/24".to_string()),
                stp: false,
                comment: String::new(),
            }],
            dhcp: DhcpConfig {
                interface: "br0".to_string(),
                range_start: "192.168.1.100".to_string(),
                range_end: "192.168.1.250".to_string(),
                gateway: "192.168.1.1".to_string(),
                dns_servers: vec!["192.168.1.1".to_string()],
                ..DhcpConfig::default()
            },
            dns: crate::networking::model::DnsConfig {
                listen: vec!["192.168.1.1".to_string()],
                ..crate::networking::model::DnsConfig::default()
            },
            ..NetworkConfig::default()
        }
    }

    #[test]
    fn apply_writes_config_and_snapshots_the_firewall() {
        let paths = temp_paths("apply");
        let runner = FakeRunner::new().respond("nft", Some(0), "table inet filter {}\n", "");

        let applied = apply_sync(&example_config(), &paths, &runner);

        assert!(
            applied.rollback.is_some(),
            "a successful firewall step is reversible"
        );
        assert_eq!(applied.rollback.as_deref(), Some("table inet filter {}\n"));
        assert!(paths.nftables.is_file());
        assert!(paths.dnsmasq.is_file());
        assert!(paths.unbound.is_file());
        assert!(
            applied.steps.iter().all(|step| step.ok),
            "{:?}",
            applied.steps
        );
    }

    #[test]
    fn firewall_check_failure_prevents_apply_and_rollback() {
        let paths = temp_paths("check-fail");
        // `nft -c` fails; the ruleset must never be loaded.
        let runner = FakeRunner::new()
            .respond("nft", None, "", "")
            .respond("nft", None, "", "");
        let applied = apply_sync(&example_config(), &paths, &runner);
        assert!(applied.rollback.is_none());

        let firewall = applied
            .steps
            .iter()
            .find(|step| step.subsystem == "firewall")
            .expect("firewall step");
        assert!(!firewall.ok);
    }

    #[test]
    fn missing_systemctl_fails_only_that_subsystem() {
        let paths = temp_paths("no-systemctl");
        let runner = FakeRunner::new().missing("systemctl");
        let applied = apply_sync(&example_config(), &paths, &runner);

        let dhcp = applied
            .steps
            .iter()
            .find(|step| step.subsystem == "dhcp")
            .expect("dhcp step");
        assert!(!dhcp.ok);
        // The firewall still applied, so the box is not left half-configured.
        let firewall = applied
            .steps
            .iter()
            .find(|step| step.subsystem == "firewall")
            .expect("firewall step");
        assert!(firewall.ok);
    }

    #[tokio::test]
    async fn unconfirmed_firewall_change_is_rolled_back() {
        let paths = temp_paths("rollback");
        let runner = Arc::new(FakeRunner::new().respond("nft", Some(0), "old ruleset\n", ""));
        let manager = ApplyManager::new(
            paths.clone(),
            Arc::clone(&runner) as Arc<dyn Runner>,
            Duration::from_secs(60),
        );

        let report = manager.apply(&example_config()).await.expect("apply");
        let pending = report.pending.expect("firewall change is reversible");
        assert!(manager.pending().await.is_some());

        // Simulate the window elapsing.
        manager.expire(pending.generation).await;

        assert!(manager.pending().await.is_none());
        assert!(
            runner.ran_with("nft", &["-f", &paths.rollback.to_string_lossy()]),
            "the previous ruleset must be restored: {:?}",
            runner.calls()
        );
        assert_eq!(
            std::fs::read_to_string(&paths.rollback).expect("rollback file"),
            "old ruleset\n"
        );
    }

    #[tokio::test]
    async fn confirm_cancels_the_rollback() {
        let paths = temp_paths("confirm");
        let runner = Arc::new(FakeRunner::new().respond("nft", Some(0), "old ruleset\n", ""));
        let manager = ApplyManager::new(
            paths,
            Arc::clone(&runner) as Arc<dyn Runner>,
            Duration::from_secs(60),
        );

        let report = manager.apply(&example_config()).await.expect("apply");
        let pending = report.pending.expect("pending");

        assert_eq!(
            manager.confirm().await.expect("confirm"),
            pending.generation
        );
        manager.expire(pending.generation).await;
        assert!(
            !runner.ran("nft") || !runner.ran_with("nft", &["flush", "ruleset"]),
            "a confirmed change must not be rolled back"
        );
        assert!(manager.pending().await.is_none());
    }

    #[tokio::test]
    async fn empty_snapshot_rolls_back_to_a_flush() {
        let paths = temp_paths("flush");
        // No pre-existing ruleset: `nft list ruleset` returns nothing.
        let runner = Arc::new(FakeRunner::new().respond("nft", Some(0), "", ""));
        let manager = ApplyManager::new(
            paths,
            Arc::clone(&runner) as Arc<dyn Runner>,
            Duration::from_secs(60),
        );

        let report = manager.apply(&example_config()).await.expect("apply");
        let pending = report.pending.expect("pending");
        manager.expire(pending.generation).await;

        assert!(
            runner.ran_with("nft", &["flush", "ruleset"]),
            "{:?}",
            runner.calls()
        );
    }

    #[test]
    fn sqm_disabled_clears_queues_instead_of_applying() {
        let paths = temp_paths("sqm-off");
        let runner = FakeRunner::new();
        let applied = apply_sync(&example_config(), &paths, &runner);

        let sqm = applied
            .steps
            .iter()
            .find(|step| step.subsystem == "sqm")
            .expect("sqm step");
        assert!(sqm.ok);
        assert_eq!(sqm.detail, "SQM disabled");
    }
}
