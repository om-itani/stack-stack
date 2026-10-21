//! The router: interfaces, firewall, DHCP, DNS, traffic shaping, tunnels and
//! certificates.
//!
//! Design rules for this whole module:
//!
//! * **Nothing is applied that has not been validated.** `model::validate` runs
//!   before a config is persisted or rendered.
//! * **Generators are pure.** `nftables`, `dnsmasq`, `unbound`, `cloudflared`
//!   and `sqm` turn a [`model::NetworkConfig`] into text or argv, with no I/O,
//!   so they are unit-testable on any host.
//! * **Applying is reversible where it matters.** [`apply::ApplyManager`] holds
//!   a firewall change open for confirmation and restores the previous ruleset
//!   if the operator never confirms.
//! * **Everything degrades.** On a host without `nft`, `ip`, `iw`, `openssl` or
//!   `systemctl`, probes report `unavailable` and the UI still loads.

pub mod apply;
pub mod certs;
pub mod cloudflared;
pub mod dnsmasq;
pub mod interfaces;
pub mod ipmath;
pub mod model;
pub mod nftables;
pub mod runner;
pub mod sqm;
pub mod status;
pub mod store;
pub mod unbound;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use crate::networking::apply::{ApplyManager, ApplyPaths, CONFIRM_WINDOW_S};
use crate::networking::model::NetworkConfig;
use crate::networking::runner::{Runner, SystemRunner};
use crate::notify::Notifier;

/// Every generated file the UI can show side by side.
#[derive(Debug, Clone, Serialize)]
pub struct PreviewBundle {
    pub nftables: String,
    pub dnsmasq: String,
    pub unbound: String,
    pub cloudflared: String,
    pub sqm: String,
}

/// Render every subsystem's configuration without touching the system.
pub fn preview(config: &NetworkConfig) -> PreviewBundle {
    PreviewBundle {
        nftables: nftables::render(config),
        dnsmasq: dnsmasq::render(config),
        unbound: unbound::render(config),
        cloudflared: cloudflared::render(config),
        sqm: sqm::script(config),
    }
}

/// Shared runtime handles for the network API.
#[derive(Clone)]
pub struct NetworkContext {
    /// Where the configuration document lives.
    pub config_path: PathBuf,
    /// Owns the firewall confirmation window.
    pub apply: ApplyManager,
    /// Command executor for probes, apply and certificate work.
    pub runner: Arc<dyn Runner>,
}

impl NetworkContext {
    /// `notifier` is threaded into the firewall safe-mode window so an automatic
    /// rollback reaches the operator; it is `None` in bare unit tests.
    pub fn new(config_path: PathBuf, notifier: Option<Arc<dyn Notifier>>) -> Self {
        let runner: Arc<dyn Runner> = Arc::new(SystemRunner::default());
        let mut apply = ApplyManager::new(
            ApplyPaths::default(),
            Arc::clone(&runner),
            Duration::from_secs(CONFIRM_WINDOW_S),
        );
        if let Some(notifier) = notifier {
            apply = apply.with_notifier(notifier);
        }
        Self {
            config_path,
            apply,
            runner,
        }
    }
}
