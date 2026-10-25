//! Shared application state handed to every Axum handler.

use std::sync::Arc;
use std::time::Instant;

use std::time::Duration;

use tokio::sync::{broadcast, watch};

use crate::alerts::AlertContext;
use crate::auth::AuthContext;
use crate::ca::CaContext;
use crate::config::Config;
use crate::containers::docker::DockerClient;
use crate::containers::hub::{CurlHttp, HubSearch};
use crate::files::FilesContext;
use crate::jobs::JobManager;
use crate::networking::NetworkContext;
use crate::notify::{NotificationCenter, Notifier};
use crate::runner::SystemRunner;
use crate::services::backup::{BackupEngine, SourceDirs};
use crate::services::ddns::DdnsContext;
use crate::services::logs::LogsContext;
use crate::services::nfs::NfsContext;
use crate::services::samba::SambaContext;
use crate::services::smart::SmartContext;
use crate::services::tasks::TaskContext;
use crate::services::updates::UpdateContext;
use crate::services::wireguard::WgContext;
use crate::system::metrics::Snapshot;
use crate::system::storage::StorageContext;
use crate::tls::TlsRuntime;
use crate::virtualization::VirtContext;

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
    /// Docker Hub search with a process-wide result cache.
    pub hub: HubSearch,
    /// Router state: config document, apply manager and command runner.
    pub network: NetworkContext,
    /// Virtualization state: guest document, catalog and lifecycle lock.
    pub virt: VirtContext,
    /// Single-admin login gate: credential document and in-memory sessions.
    pub auth: AuthContext,
    /// Notification bus: the in-memory feed, live stream and optional webhook.
    pub notify: Arc<NotificationCenter>,
    /// Universal background job registry: the activity drawer's data source.
    pub jobs: JobManager,
    /// Alerting rules document and the runner that reaches providers.
    pub alerts: AlertContext,
    /// TLS front door: internal CA, SNI certificate map and listener ports.
    pub tls: TlsRuntime,
    /// File manager: guarded filesystem access and its roots document.
    pub files: FilesContext,
    /// Samba share document and apply surface.
    pub samba: SambaContext,
    /// NFS export document and apply surface.
    pub nfs: NfsContext,
    /// Backup engine: restic jobs, secrets, history and the scheduler's lock.
    pub backup: BackupEngine,
    /// WireGuard interface document and apply surface.
    pub wireguard: WgContext,
    /// SMART self-test scheduling and history.
    pub smart: SmartContext,
    /// Physical disks and the format/mount operations behind the Storage page.
    pub storage: StorageContext,
    /// Container and stack auto-update, with the digest history rollback uses.
    pub updates: UpdateContext,
    /// Operator-defined scheduled shell tasks.
    pub tasks: TaskContext,

    /// journald reader surface.
    pub logs: LogsContext,
    /// Dynamic DNS document, token and update surface.
    pub ddns: DdnsContext,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let (telemetry, _) = broadcast::channel(TELEMETRY_BACKLOG);
        let (latest, _) = watch::channel(None);
        let docker = DockerClient::new(config.docker_socket.clone(), DOCKER_TIMEOUT_S);
        let hub = HubSearch::new(Arc::new(CurlHttp::new()));
        if !hub.available() {
            tracing::warn!(
                "curl was not found on PATH; Docker Hub search in the App Store is disabled"
            );
        }
        let notify = Arc::new(NotificationCenter::new(
            std::path::PathBuf::from(&config.notify_path),
            Arc::new(SystemRunner::new(Duration::from_secs(15))),
        ));
        let notifier: Arc<dyn Notifier> = notify.clone();
        let jobs = JobManager::new(Some(notify.clone()));
        let alerts = AlertContext::new(
            std::path::PathBuf::from(&config.alerts_path),
            Arc::new(SystemRunner::new(Duration::from_secs(20))),
        );
        let network = NetworkContext::new(
            std::path::PathBuf::from(&config.network_path),
            Some(notifier),
        );
        let virt = VirtContext::new(std::path::PathBuf::from(&config.virt_path));
        let auth = AuthContext::new(
            std::path::PathBuf::from(&config.auth_path),
            Arc::new(SystemRunner::new(Duration::from_secs(30))),
            Duration::from_secs(config.session_ttl_h.max(1) * 3600),
        );
        let ca = CaContext::new(
            std::path::PathBuf::from(&config.ca_dir),
            Arc::new(SystemRunner::new(Duration::from_secs(60))),
        );
        let tls = TlsRuntime::new(&config, ca);
        let files = FilesContext::new(
            std::path::PathBuf::from(&config.files_path),
            Arc::new(SystemRunner::new(Duration::from_secs(1800))),
        );
        let samba = SambaContext::new(
            std::path::PathBuf::from(&config.samba_path),
            std::path::PathBuf::from(crate::services::samba::SMB_CONF),
            Arc::new(SystemRunner::new(Duration::from_secs(60))),
        );
        let nfs = NfsContext::new(
            std::path::PathBuf::from(&config.nfs_path),
            std::path::PathBuf::from(crate::services::nfs::EXPORTS),
            Arc::new(SystemRunner::new(Duration::from_secs(60))),
        );
        let config_dir = std::path::PathBuf::from(&config.auth_path)
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from("/etc/stack-stack"));
        let (files_config, _) = crate::files::load(std::path::Path::new(&config.files_path));
        let restic_timeout = Duration::from_secs(config.restic_timeout_s.max(60));
        let backup = BackupEngine::new(
            std::path::PathBuf::from(&config.backup_path),
            std::path::PathBuf::from(&config.backup_secrets_path),
            Arc::new(SystemRunner::new(restic_timeout)),
            restic_timeout,
            SourceDirs {
                docker_volumes: std::path::PathBuf::from("/var/lib/docker/volumes"),
                apps: std::path::PathBuf::from(&config.apps_dir),
                guests: std::path::PathBuf::from(&config.guests_dir),
                config_dir,
                ca: std::path::PathBuf::from(&config.ca_dir),
                file_roots: files_config.roots,
            },
        );
        let wireguard = WgContext::new(
            std::path::PathBuf::from(&config.wg_path),
            std::path::PathBuf::from(&config.wg_secrets_path),
            Arc::new(SystemRunner::new(Duration::from_secs(60))),
        );
        let logs = LogsContext::new(Arc::new(SystemRunner::new(Duration::from_secs(30))));
        let ddns = DdnsContext::new(
            std::path::PathBuf::from(&config.ddns_path),
            std::path::PathBuf::from(&config.ddns_secrets_path),
            std::path::PathBuf::from(&config.network_path),
            Arc::new(SystemRunner::new(Duration::from_secs(60))),
        );

        let smart = SmartContext::new(
            std::path::PathBuf::from(&config.smart_path),
            Arc::new(SystemRunner::new(Duration::from_secs(120))),
            Duration::from_secs(120),
        );
        // mkfs on a large disk can take minutes; mount/umount are instant.
        let storage = StorageContext::new(Arc::new(SystemRunner::new(Duration::from_secs(600))));
        let tasks = TaskContext::new(
            std::path::PathBuf::from(&config.tasks_path),
            Arc::new(SystemRunner::new(Duration::from_secs(
                crate::services::tasks::TASK_TIMEOUT_S + 30,
            ))),
        );
        // A pull can be slow; the compose timeout already covers it.
        let updates = UpdateContext::new(
            std::path::PathBuf::from(&config.updates_path),
            std::path::PathBuf::from(&config.stacks_dir),
            config.compose_timeout_s,
            Arc::new(SystemRunner::new(Duration::from_secs(
                config.compose_timeout_s.clamp(30, 1800),
            ))),
        );
        Self {
            started_at: Instant::now(),
            telemetry,
            latest,
            config,
            docker,
            hub,
            network,
            virt,
            auth,
            notify,
            jobs,
            alerts,
            tls,
            files,
            samba,
            nfs,
            backup,
            wireguard,
            smart,
            storage,
            updates,
            tasks,
            logs,
            ddns,
        }
    }

    pub fn latest_snapshot(&self) -> Option<Arc<Snapshot>> {
        self.latest.borrow().clone()
    }

    pub fn uptime_s(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Record a destructive operation in the audit trail. Fire-and-forget on the
    /// blocking pool so the file write never slows a handler.
    pub fn audit(&self, action: &str, target: &str, detail: &str) {
        let path = self.config.audit_path.clone();
        let (action, target, detail) = (action.to_string(), target.to_string(), detail.to_string());
        // Detached: the handle is dropped on purpose so the write never gates the
        // response.
        drop(tokio::task::spawn_blocking(move || {
            crate::audit::record(&path, "admin", &action, &target, &detail);
        }));
    }
}
