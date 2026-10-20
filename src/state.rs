//! Shared application state handed to every Axum handler.

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{broadcast, watch};

use crate::config::Config;
use crate::containers::docker::DockerClient;
use crate::networking::NetworkContext;
use crate::system::metrics::Snapshot;

/// Fan-out capacity for the telemetry socket. A slow client that falls more
/// than this many frames behind is told it lagged and resynchronises on the
/// next frame instead of ever blocking the sampler.
pub const TELEMETRY_BACKLOG: usize = 16;

/// Per-request Docker API timeout, in seconds.
pub const DOCKER_TIMEOUT_S: u64 = 60;

#[derive(Clone)]
pub struct AppState {
    /// When the process came up, used by `/api/health`.
    pub started_at: Instant,
    /// Every sample tick is broadcast here; the WS handler subscribes.
    pub telemetry: broadcast::Sender<Arc<Snapshot>>,
    /// Most recent sample, so REST callers get an instant answer without
    /// waiting up to one full tick.
    pub latest: watch::Sender<Option<Arc<Snapshot>>>,
    /// Runtime configuration, shared with the container/app-store handlers.
    pub config: Config,
    /// Lazily-connected Docker daemon client.
    pub docker: DockerClient,
    /// Router state: config document, apply manager and command runner.
    pub network: NetworkContext,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let (telemetry, _) = broadcast::channel(TELEMETRY_BACKLOG);
        let (latest, _) = watch::channel(None);
        let docker = DockerClient::new(config.docker_socket.clone(), DOCKER_TIMEOUT_S);
        let network = NetworkContext::new(std::path::PathBuf::from(&config.network_path));
        Self {
            started_at: Instant::now(),
            telemetry,
            latest,
            config,
            docker,
            network,
        }
    }

    pub fn latest_snapshot(&self) -> Option<Arc<Snapshot>> {
        self.latest.borrow().clone()
    }

    pub fn uptime_s(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}
