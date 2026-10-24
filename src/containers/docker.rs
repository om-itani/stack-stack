//! Docker daemon client.
//!
//! One lazily-established connection is cached for the process; a missing or
//! dead socket yields [`ApiError::Unavailable`] and every read path degrades to
//! an empty result instead of an error so the UI still renders.
//!
//! Bollard's streams own their transport (they clone an `Arc` internally), so
//! the boxed streams returned here do not borrow the client.

use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(unix)]
use bollard::API_DEFAULT_VERSION;
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::models::{ContainerSummary, PortSummary};
use bollard::query_parameters::{
    ListContainersOptionsBuilder, LogsOptionsBuilder, RemoveContainerOptionsBuilder,
    ResizeExecOptions, RestartContainerOptionsBuilder, StartContainerOptions, StatsOptionsBuilder,
    StopContainerOptionsBuilder,
};
use futures_util::{Stream, StreamExt};
use tokio::io::AsyncWrite;
use tokio::sync::RwLock;

use crate::containers::model::{App, Container, ContainerPort, ContainerSource, ContainerStats};
use crate::containers::store;
use crate::error::ApiError;

/// Docker label that marks a container as belonging to an app-store template.
pub const APP_LABEL: &str = "stack-stack.app";

/// Docker Compose's own project label. Present on every container raised by
/// `docker compose`, which is how a stack's containers are identified.
pub const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";

/// Grace period before `stop`/`restart` escalates to SIGKILL.
const STOP_GRACE_S: i32 = 10;

/// PTY read buffer handed to the exec stream; bounds per-client memory.
const EXEC_OUTPUT_CAPACITY: usize = 64 * 1024;

pub type LogStream = Pin<Box<dyn Stream<Item = Result<LogOutput, bollard::errors::Error>> + Send>>;

/// Flatten any log/console frame to text. Terminal and container output is
/// text; invalid UTF-8 is replaced rather than failing the stream.
pub fn log_text(output: LogOutput) -> String {
    let message = match output {
        LogOutput::StdErr { message }
        | LogOutput::StdOut { message }
        | LogOutput::StdIn { message }
        | LogOutput::Console { message } => message,
    };
    String::from_utf8_lossy(&message).into_owned()
}

/// An attached `docker exec` session: output to relay to the browser, and a
/// handle to write keystrokes back into the container's PTY.
pub struct ExecSession {
    pub exec_id: String,
    pub output: Pin<Box<dyn Stream<Item = Result<LogOutput, bollard::errors::Error>> + Send>>,
    pub input: Pin<Box<dyn AsyncWrite + Send>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
}

/// How long a container listing is reused. The UI polls every few seconds; this
/// collapses overlapping polls (and multiple tabs) onto one daemon call without
/// being stale enough to notice.
const LIST_TTL: Duration = Duration::from_secs(2);

/// `(fetched_at, containers)` for the last successful listing.
type ListCache = Arc<RwLock<Option<(Instant, Vec<Container>)>>>;

#[derive(Clone)]
pub struct DockerClient {
    #[cfg_attr(not(unix), allow(dead_code))]
    socket: Arc<str>,
    timeout_s: u64,
    cached: Arc<RwLock<Option<Docker>>>,
    list_cache: ListCache,
}

impl DockerClient {
    pub fn new(socket: String, timeout_s: u64) -> Self {
        Self {
            socket: Arc::from(socket),
            // A `docker ps` against a wedged daemon must not hang a handler.
            timeout_s: timeout_s.clamp(1, 600),
            cached: Arc::new(RwLock::new(None)),
            list_cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Drop the cached listing; called after any mutation so the next read is
    /// fresh.
    pub async fn invalidate(&self) {
        *self.list_cache.write().await = None;
    }

    /// Cached connection, established on first use. A previously failed attempt
    /// is retried on the next call, so Docker coming up late is picked up.
    pub async fn get(&self) -> Result<Docker, ApiError> {
        if let Some(docker) = self.cached.read().await.clone() {
            return Ok(docker);
        }

        let mut guard = self.cached.write().await;
        if let Some(docker) = guard.clone() {
            return Ok(docker);
        }

        let docker = connect(&self.socket, self.timeout_s)?;
        *guard = Some(docker.clone());
        Ok(docker)
    }

    /// Containers, running and stopped. Missing socket -> empty list.
    pub async fn list(
        &self,
        host_ip: &str,
        installed: &[String],
        apps: &[App],
    ) -> Result<Vec<Container>, ApiError> {
        if let Some((at, containers)) = self.list_cache.read().await.as_ref()
            && at.elapsed() < LIST_TTL
        {
            return Ok(containers.clone());
        }

        // A read must never fail because Docker is missing: report an empty
        // fleet and let the UI say "Docker is not running". Mutations still
        // surface the 503.
        let docker = match self.get().await {
            Ok(docker) => docker,
            Err(ApiError::Unavailable(err)) => {
                tracing::debug!(%err, "docker unavailable, reporting no containers");
                return Ok(Vec::new());
            }
            Err(err) => return Err(err),
        };

        let options = ListContainersOptionsBuilder::new().all(true).build();
        let summaries = match docker.list_containers(Some(options)).await {
            Ok(summaries) => summaries,
            // The socket can exist while the daemon is down; the connect step
            // only proves the file is there.
            Err(err) => match ApiError::from(err) {
                ApiError::Unavailable(err) => {
                    tracing::debug!(%err, "docker daemon unreachable, reporting no containers");
                    return Ok(Vec::new());
                }
                err => return Err(err),
            },
        };

        let mut containers: Vec<Container> = summaries
            .into_iter()
            .map(|summary| summary_to_container(summary, host_ip, installed, apps))
            .collect();
        containers.sort_by(|a, b| a.name.cmp(&b.name));
        *self.list_cache.write().await = Some((Instant::now(), containers.clone()));
        Ok(containers)
    }

    pub async fn action(&self, id: &str, action: ContainerAction) -> Result<(), ApiError> {
        let id = store::sanitize_container_id(id)?;
        let docker = self.get().await?;

        let result = match action {
            ContainerAction::Start => {
                docker
                    .start_container(id, None::<StartContainerOptions>)
                    .await
            }
            ContainerAction::Stop => {
                let options = StopContainerOptionsBuilder::new().t(STOP_GRACE_S).build();
                docker.stop_container(id, Some(options)).await
            }
            ContainerAction::Restart => {
                let options = RestartContainerOptionsBuilder::new()
                    .t(STOP_GRACE_S)
                    .build();
                docker.restart_container(id, Some(options)).await
            }
        };
        result.map_err(ApiError::from)?;
        self.invalidate().await;
        Ok(())
    }

    /// Force-remove the container. Anonymous volumes are kept (`v: false`) so a
    /// delete cannot silently destroy data.
    pub async fn remove(&self, id: &str) -> Result<(), ApiError> {
        let id = store::sanitize_container_id(id)?;
        let docker = self.get().await?;
        let options = RemoveContainerOptionsBuilder::new()
            .force(true)
            .v(false)
            .build();
        docker
            .remove_container(id, Some(options))
            .await
            .map_err(ApiError::from)?;
        self.invalidate().await;
        Ok(())
    }

    /// One live sample for a container. `one_shot` makes Docker answer with a
    /// single reading instead of a stream, so the modal can poll it.
    pub async fn stats(&self, id: &str) -> Result<ContainerStats, ApiError> {
        let id = store::sanitize_container_id(id)?.to_string();
        let docker = self.get().await?;
        let options = StatsOptionsBuilder::new()
            .stream(false)
            .one_shot(true)
            .build();
        let mut stream = docker.stats(&id, Some(options));
        let sample = stream
            .next()
            .await
            .transpose()
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::unavailable("docker returned no stats for this container"))?;
        Ok(compute_stats(&id, &sample))
    }

    /// Follow logs from `tail` historical lines onward.
    pub async fn logs(&self, id: &str, tail: &str) -> Result<LogStream, ApiError> {
        let id = store::sanitize_container_id(id)?;
        let docker = self.get().await?;
        let options = LogsOptionsBuilder::new()
            .follow(true)
            .stdout(true)
            .stderr(true)
            .tail(tail)
            .build();
        Ok(Box::pin(docker.logs(id, Some(options))))
    }

    /// Create and attach an interactive `/bin/sh` on the container's TTY.
    pub async fn exec_attach(&self, id: &str) -> Result<ExecSession, ApiError> {
        let id = store::sanitize_container_id(id)?;
        let docker = self.get().await?;

        let created = docker
            .create_exec(
                id,
                CreateExecOptions {
                    cmd: Some(vec!["/bin/sh".to_string()]),
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(true),
                    ..Default::default()
                },
            )
            .await?;

        let started = docker
            .start_exec(
                &created.id,
                Some(StartExecOptions {
                    detach: false,
                    tty: true,
                    output_capacity: Some(EXEC_OUTPUT_CAPACITY),
                }),
            )
            .await?;

        match started {
            StartExecResults::Attached { output, input } => Ok(ExecSession {
                exec_id: created.id,
                output,
                input,
            }),
            StartExecResults::Detached => Err(ApiError::internal(
                "docker returned a detached exec for an attached request",
            )),
        }
    }

    /// Resize an attached exec's PTY. Best effort: a resize on a closed session
    /// is not worth tearing the socket down for.
    pub async fn resize(&self, exec_id: &str, cols: u16, rows: u16) -> Result<(), ApiError> {
        let docker = self.get().await?;
        docker
            .resize_exec(
                exec_id,
                ResizeExecOptions {
                    w: i32::from(cols),
                    h: i32::from(rows),
                },
            )
            .await
            .map_err(ApiError::from)
    }
}

#[cfg(unix)]
fn connect(socket: &str, timeout_s: u64) -> Result<Docker, ApiError> {
    Docker::connect_with_unix(socket, timeout_s, API_DEFAULT_VERSION).map_err(ApiError::from)
}

#[cfg(not(unix))]
fn connect(_socket: &str, _timeout_s: u64) -> Result<Docker, ApiError> {
    Docker::connect_with_local_defaults().map_err(ApiError::from)
}

// ---------------------------------------------------------------------------
// Model mapping
// ---------------------------------------------------------------------------

fn summary_to_container(
    summary: ContainerSummary,
    host_ip: &str,
    installed: &[String],
    apps: &[App],
) -> Container {
    let id = summary.id.unwrap_or_default();
    let name = summary
        .names
        .unwrap_or_default()
        .into_iter()
        .next()
        .map(|name| name.trim_start_matches('/').to_string())
        .unwrap_or_default();
    let image = summary.image.unwrap_or_default();
    let state = summary
        .state
        .map(|state| state.to_string())
        .filter(|state| !state.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let status = summary.status.unwrap_or_default();
    let created = summary.created.unwrap_or(0);
    let ports: Vec<ContainerPort> = summary
        .ports
        .unwrap_or_default()
        .into_iter()
        .map(to_port)
        .collect();

    let labels = summary.labels.unwrap_or_default();
    let app_id = labels
        .get(APP_LABEL)
        .cloned()
        .or_else(|| installed.iter().find(|app| app.as_str() == name).cloned());
    let source = if app_id.is_some() {
        ContainerSource::AppStore
    } else {
        ContainerSource::Manual
    };
    let stack = labels
        .get(COMPOSE_PROJECT_LABEL)
        .map(|project| project.trim())
        .filter(|project| !project.is_empty())
        .map(str::to_string);

    let app_web_port = app_id
        .as_deref()
        .and_then(|app_id| apps.iter().find(|app| app.id == app_id))
        .and_then(|app| app.web_port);

    let web_urls = web_urls(app_web_port, &ports, host_ip);

    Container {
        id,
        name,
        image,
        state,
        status,
        created,
        ports,
        source,
        app_id,
        stack,
        web_urls,
    }
}

/// Turn one Docker stats sample into the numbers the modal shows.
///
/// Docker reports cumulative counters, so CPU is a delta against the previous
/// sample, scaled by the core count. Memory is the working set (usage minus page
/// cache) where the daemon reports a cache figure.
pub fn compute_stats(id: &str, stats: &bollard::models::ContainerStatsResponse) -> ContainerStats {
    let cpu = stats.cpu_stats.as_ref();
    let pre = stats.precpu_stats.as_ref();

    let cpu_total = cpu
        .and_then(|c| c.cpu_usage.as_ref())
        .and_then(|u| u.total_usage);
    let pre_total = pre
        .and_then(|c| c.cpu_usage.as_ref())
        .and_then(|u| u.total_usage);
    let cpu_delta = cpu_total
        .unwrap_or(0)
        .saturating_sub(pre_total.unwrap_or(0));

    let system_delta = cpu
        .and_then(|c| c.system_cpu_usage)
        .unwrap_or(0)
        .saturating_sub(pre.and_then(|c| c.system_cpu_usage).unwrap_or(0));

    let online = cpu
        .and_then(|c| c.online_cpus)
        .map(u64::from)
        .or_else(|| {
            cpu.and_then(|c| c.cpu_usage.as_ref())
                .and_then(|u| u.percpu_usage.as_ref())
                .map(|cores| cores.len() as u64)
        })
        .unwrap_or(1)
        .max(1);

    let cpu_pct = if system_delta > 0 {
        ((cpu_delta as f64 / system_delta as f64) * online as f64 * 100.0) as f32
    } else {
        0.0
    };

    let memory = stats.memory_stats.as_ref();
    let usage = memory.and_then(|m| m.usage).unwrap_or(0);
    let cache = memory
        .and_then(|m| m.stats.as_ref())
        .and_then(|values| values.get("cache").copied())
        .unwrap_or(0);
    let used = usage.saturating_sub(cache);
    let limit = memory.and_then(|m| m.limit).unwrap_or(0);
    let memory_pct = if limit > 0 {
        (used as f64 / limit as f64 * 100.0) as f32
    } else {
        0.0
    };

    let mut rx = 0u64;
    let mut tx = 0u64;
    if let Some(networks) = stats.networks.as_ref() {
        for interface in networks.values() {
            rx = rx.saturating_add(interface.rx_bytes.unwrap_or(0));
            tx = tx.saturating_add(interface.tx_bytes.unwrap_or(0));
        }
    }

    ContainerStats {
        id: id.to_string(),
        cpu_pct,
        memory_used_bytes: used,
        memory_limit_bytes: limit,
        memory_pct,
        network_rx_bytes: rx,
        network_tx_bytes: tx,
    }
}

fn to_port(port: PortSummary) -> ContainerPort {
    let protocol = port
        .typ
        .map(|typ| typ.to_string())
        .filter(|typ| !typ.is_empty())
        .unwrap_or_else(|| "tcp".to_string());
    ContainerPort {
        private: port.private_port,
        public: port.public_port,
        protocol,
        ip: port.ip,
    }
}

/// Candidate `http://host:port` links for published TCP ports, with the app's
/// declared web port (if any) first so the primary "Open" button is correct.
fn web_urls(app_web_port: Option<u16>, ports: &[ContainerPort], host_ip: &str) -> Vec<String> {
    if host_ip.is_empty() || host_ip == "0.0.0.0" {
        return Vec::new();
    }

    let published: Vec<(u16, u16)> = ports
        .iter()
        .filter(|port| port.protocol == "tcp")
        .filter_map(|port| port.public.map(|public| (port.private, public)))
        .collect();

    let mut urls: Vec<String> = Vec::new();
    if let Some(web_port) = app_web_port
        && let Some((_, public)) = published.iter().find(|(private, _)| *private == web_port)
    {
        push_unique(&mut urls, format!("http://{host_ip}:{public}"));
    }
    for (_, public) in &published {
        push_unique(&mut urls, format!("http://{host_ip}:{public}"));
    }

    urls
}

fn push_unique(urls: &mut Vec<String>, url: String) {
    if !urls.contains(&url) {
        urls.push(url);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::PortSummaryTypeEnum;

    fn port(private: u16, public: Option<u16>, protocol: PortSummaryTypeEnum) -> PortSummary {
        PortSummary {
            ip: None,
            private_port: private,
            public_port: public,
            typ: Some(protocol),
        }
    }

    #[test]
    fn computes_cpu_memory_and_network_from_a_sample() {
        use bollard::models::{
            ContainerCpuStats, ContainerCpuUsage, ContainerMemoryStats, ContainerNetworkStats,
        };
        use std::collections::HashMap;

        let mut networks = HashMap::new();
        networks.insert(
            "eth0".to_string(),
            ContainerNetworkStats {
                rx_bytes: Some(1000),
                tx_bytes: Some(2000),
                ..Default::default()
            },
        );

        let sample = bollard::models::ContainerStatsResponse {
            cpu_stats: Some(ContainerCpuStats {
                cpu_usage: Some(ContainerCpuUsage {
                    total_usage: Some(200),
                    ..Default::default()
                }),
                system_cpu_usage: Some(1000),
                online_cpus: Some(2),
                ..Default::default()
            }),
            precpu_stats: Some(ContainerCpuStats {
                cpu_usage: Some(ContainerCpuUsage {
                    total_usage: Some(100),
                    ..Default::default()
                }),
                system_cpu_usage: Some(500),
                ..Default::default()
            }),
            memory_stats: Some(ContainerMemoryStats {
                usage: Some(1000),
                limit: Some(4000),
                ..Default::default()
            }),
            networks: Some(networks),
            ..Default::default()
        };

        let stats = compute_stats("abc", &sample);
        // cpu delta 100 over system delta 500, across 2 cores, is 40%.
        assert!((stats.cpu_pct - 40.0).abs() < 0.01, "cpu {}", stats.cpu_pct);
        assert_eq!(stats.memory_used_bytes, 1000);
        assert_eq!(stats.memory_limit_bytes, 4000);
        assert!(
            (stats.memory_pct - 25.0).abs() < 0.01,
            "mem {}",
            stats.memory_pct
        );
        assert_eq!(stats.network_rx_bytes, 1000);
        assert_eq!(stats.network_tx_bytes, 2000);
    }

    #[test]
    fn idle_container_reports_zero_everywhere() {
        // No movement between the two samples: no CPU, no network, no memory.
        let cpu = |total: u64| {
            Some(bollard::models::ContainerCpuStats {
                cpu_usage: Some(bollard::models::ContainerCpuUsage {
                    total_usage: Some(total),
                    ..Default::default()
                }),
                system_cpu_usage: Some(100),
                ..Default::default()
            })
        };
        let sample = bollard::models::ContainerStatsResponse {
            cpu_stats: cpu(10),
            precpu_stats: cpu(10),
            ..Default::default()
        };
        let stats = compute_stats("abc", &sample);
        assert_eq!(stats.cpu_pct, 0.0);
        assert_eq!(stats.memory_used_bytes, 0);
        assert_eq!(stats.memory_pct, 0.0);
        assert_eq!(stats.network_rx_bytes, 0);
        assert_eq!(stats.network_tx_bytes, 0);
    }

    #[test]
    fn maps_summary_fields_and_label() {
        let summary = ContainerSummary {
            id: Some("abc123".to_string()),
            names: Some(vec!["/jellyfin".to_string()]),
            image: Some("jellyfin/jellyfin:latest".to_string()),
            state: Some(bollard::models::ContainerSummaryStateEnum::RUNNING),
            status: Some("Up 2 hours".to_string()),
            created: Some(1_700_000_000),
            ports: Some(vec![port(8096, Some(8096), PortSummaryTypeEnum::TCP)]),
            labels: Some(
                [("stack-stack.app".to_string(), "jellyfin".to_string())]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        };

        let apps = vec![App {
            id: "jellyfin".to_string(),
            name: "Jellyfin".to_string(),
            description: String::new(),
            category: "media".to_string(),
            tags: Vec::new(),
            icon: String::new(),
            ports: Vec::new(),
            web_port: Some(8096),
            compose: String::new(),
            installed: false,
        }];

        let container = summary_to_container(summary, "192.168.1.10", &[], &apps);
        assert_eq!(container.name, "jellyfin");
        assert_eq!(container.state, "running");
        assert_eq!(container.source, ContainerSource::AppStore);
        assert_eq!(container.app_id.as_deref(), Some("jellyfin"));
        assert_eq!(
            container.web_urls,
            vec!["http://192.168.1.10:8096".to_string()]
        );
    }

    #[test]
    fn manual_container_falls_back_to_name_lookup() {
        let summary = ContainerSummary {
            id: Some("deadbeef".to_string()),
            names: Some(vec!["sonarr".to_string()]),
            state: Some(bollard::models::ContainerSummaryStateEnum::EXITED),
            ..Default::default()
        };
        let installed = vec!["sonarr".to_string()];
        let container = summary_to_container(summary, "0.0.0.0", &installed, &[]);
        assert_eq!(container.source, ContainerSource::AppStore);
        assert_eq!(container.app_id.as_deref(), Some("sonarr"));
        // Unknown host IP means no clickable links rather than a bogus 0.0.0.0.
        assert!(container.web_urls.is_empty());
    }

    #[test]
    fn empty_state_is_reported_as_unknown() {
        let summary = ContainerSummary {
            id: Some("x".to_string()),
            ..Default::default()
        };
        let container = summary_to_container(summary, "127.0.0.1", &[], &[]);
        assert_eq!(container.state, "unknown");
        assert_eq!(container.source, ContainerSource::Manual);
        assert!(container.app_id.is_none());
    }

    #[test]
    fn web_urls_prefers_declared_port_and_dedups() {
        let ports = vec![
            ContainerPort {
                private: 6881,
                public: Some(6881),
                protocol: "tcp".to_string(),
                ip: None,
            },
            ContainerPort {
                private: 8080,
                public: Some(8080),
                protocol: "tcp".to_string(),
                ip: None,
            },
            ContainerPort {
                private: 53,
                public: Some(53),
                protocol: "udp".to_string(),
                ip: None,
            },
        ];
        let urls = web_urls(Some(8080), &ports, "10.0.0.2");
        assert_eq!(
            urls,
            vec![
                "http://10.0.0.2:8080".to_string(),
                "http://10.0.0.2:6881".to_string(),
            ],
            "web port first, udp excluded"
        );
    }
}
