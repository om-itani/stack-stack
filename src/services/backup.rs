//! Backups: restic jobs, an in-process scheduler and run history.
//!
//! Every restic invocation goes through [`crate::runner::Runner`], which keeps
//! the engine testable without restic installed and gives every command one
//! place with a wall-clock deadline. Secrets live in a separate 0600 document
//! and reach restic through the environment (`RESTIC_PASSWORD`, `AWS_*`), never
//! the argument list and never the log.
//!
//! Runs are serialised: a backup is disk-bound, and two jobs pruning the same
//! repository concurrently is how a repository gets corrupted. The scheduler and
//! the "Run now" button share one lock.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::ApiError;
use crate::networking::model::Problem;
use crate::networking::store::write_atomic;
use crate::runner::Runner;

/// How many runs are remembered for the history table.
pub const HISTORY_CAP: usize = 50;

/// Job cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cadence {
    Hourly,
    Daily,
    Weekly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub cadence: Cadence,
    /// `HH:MM` (ignored for hourly).
    pub at: String,
    /// 0 = Sunday for weekly schedules.
    pub weekday: u8,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            cadence: Cadence::Daily,
            at: "03:00".to_string(),
            weekday: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Retention {
    pub keep_last: u32,
    pub keep_daily: u32,
    pub keep_weekly: u32,
    pub keep_monthly: u32,
    pub keep_yearly: u32,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            keep_last: 7,
            keep_daily: 14,
            keep_weekly: 8,
            keep_monthly: 6,
            keep_yearly: 1,
        }
    }
}

/// Which standard source groups a job includes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JobSources {
    pub docker_volumes: bool,
    pub compose_apps: bool,
    pub virt_guests: bool,
    pub documents: bool,
    pub certs_ca: bool,
    pub file_roots: bool,
    pub custom: Vec<String>,
}

impl Default for JobSources {
    fn default() -> Self {
        Self {
            docker_volumes: false,
            compose_apps: true,
            virt_guests: false,
            documents: true,
            certs_ca: true,
            file_roots: false,
            custom: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Backend {
    /// A local directory (including a mounted USB or NFS path).
    Local { path: String },
    /// An S3-compatible endpoint.
    S3 { endpoint: String, bucket: String },
    /// `sftp:user@host:/path`.
    Sftp { target: String },
}

impl Backend {
    /// restic's repository URL for this backend.
    pub fn repo(&self) -> String {
        match self {
            Backend::Local { path } => path.trim().to_string(),
            Backend::S3 { endpoint, bucket } => {
                let endpoint = endpoint.trim().trim_end_matches('/');
                format!("s3:{}/{}", endpoint, bucket.trim())
            }
            Backend::Sftp { target } => {
                let target = target.trim();
                if target.starts_with("sftp:") {
                    target.to_string()
                } else {
                    format!("sftp:{target}")
                }
            }
        }
    }

    pub fn is_s3(&self) -> bool {
        matches!(self, Backend::S3 { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupJob {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub sources: JobSources,
    #[serde(default)]
    pub excludes: Vec<String>,
    #[serde(default)]
    pub schedule: Schedule,
    #[serde(default)]
    pub retention: Retention,
    pub backend: Backend,
    /// Key into the secrets document. Defaults to the job id.
    #[serde(default)]
    pub secret_ref: String,
}

impl BackupJob {
    pub fn secret_key(&self) -> &str {
        if self.secret_ref.trim().is_empty() {
            &self.id
        } else {
            &self.secret_ref
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BackupConfig {
    #[serde(default)]
    pub jobs: Vec<BackupJob>,
}

impl BackupConfig {
    pub fn validate(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        let mut ids: Vec<&str> = Vec::new();
        for (index, job) in self.jobs.iter().enumerate() {
            let field = format!("jobs[{index}]");
            if job.id.trim().is_empty()
                || !job
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            {
                problems.push(Problem {
                    field: format!("{field}.id"),
                    message: "job id may only contain letters, digits, . _ -".to_string(),
                });
            } else if ids.contains(&job.id.as_str()) {
                problems.push(Problem {
                    field: format!("{field}.id"),
                    message: format!("job id {:?} is used twice", job.id),
                });
            }
            ids.push(job.id.as_str());

            if job.name.trim().is_empty() {
                problems.push(Problem {
                    field: format!("{field}.name"),
                    message: "a job name is required".to_string(),
                });
            }
            if job
                .excludes
                .iter()
                .any(|exclude| !exclude.trim().starts_with('/'))
            {
                problems.push(Problem {
                    field: format!("{field}.excludes"),
                    message: "excludes must be absolute paths".to_string(),
                });
            }
            if matches!(job.schedule.cadence, Cadence::Daily | Cadence::Weekly)
                && parse_hhmm(&job.schedule.at).is_none()
            {
                problems.push(Problem {
                    field: format!("{field}.schedule.at"),
                    message: "time must be HH:MM (24h)".to_string(),
                });
            }
            if job.schedule.weekday > 6 {
                problems.push(Problem {
                    field: format!("{field}.schedule.weekday"),
                    message: "weekday must be 0 (Sunday) to 6 (Saturday)".to_string(),
                });
            }
            match &job.backend {
                Backend::Local { path } if !path.trim().starts_with('/') => {
                    problems.push(Problem {
                        field: format!("{field}.backend.path"),
                        message: "local repository path must be absolute".to_string(),
                    })
                }
                Backend::S3 { endpoint, bucket } => {
                    if !endpoint.trim().starts_with("http") {
                        problems.push(Problem {
                            field: format!("{field}.backend.endpoint"),
                            message: "S3 endpoint must be an http(s) URL".to_string(),
                        });
                    }
                    if bucket.trim().is_empty() {
                        problems.push(Problem {
                            field: format!("{field}.backend.bucket"),
                            message: "an S3 bucket is required".to_string(),
                        });
                    }
                }
                Backend::Sftp { target }
                    if !target.trim().contains('@') || !target.trim().contains(':') =>
                {
                    problems.push(Problem {
                        field: format!("{field}.backend.target"),
                        message: "SFTP target must look like user@host:/path".to_string(),
                    });
                }
                _ => {}
            }
        }
        problems
    }

    pub fn find(&self, id: &str) -> Option<&BackupJob> {
        self.jobs.iter().find(|job| job.id == id)
    }
}

/// Parse `HH:MM` into (hour, minute).
pub fn parse_hhmm(value: &str) -> Option<(u32, u32)> {
    let (hour, minute) = value.trim().split_once(':')?;
    let hour: u32 = hour.trim().parse().ok()?;
    let minute: u32 = minute.trim().parse().ok()?;
    (hour < 24 && minute < 60).then_some((hour, minute))
}

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoSecret {
    pub restic_password: String,
    pub aws_access_key_id: Option<String>,
    pub aws_secret_access_key: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupSecrets {
    pub repos: HashMap<String, RepoSecret>,
}

pub fn load_secrets(path: &std::path::Path) -> BackupSecrets {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save_secrets(path: &std::path::Path, secrets: &BackupSecrets) -> Result<(), ApiError> {
    let mut body = serde_json::to_vec_pretty(secrets)
        .map_err(|err| ApiError::internal(format!("cannot serialise secrets: {err}")))?;
    body.push(b'\n');
    write_atomic(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))?;
    restrict_600(path);
    Ok(())
}

/// Environment restic needs for a job. Never logged.
pub fn restic_env(secret: &RepoSecret, backend: &Backend) -> Vec<(String, String)> {
    let mut env = vec![(
        "RESTIC_PASSWORD".to_string(),
        secret.restic_password.clone(),
    )];
    if backend.is_s3() {
        if let Some(key) = secret
            .aws_access_key_id
            .as_deref()
            .filter(|k| !k.is_empty())
        {
            env.push(("AWS_ACCESS_KEY_ID".to_string(), key.to_string()));
        }
        if let Some(key) = secret
            .aws_secret_access_key
            .as_deref()
            .filter(|k| !k.is_empty())
        {
            env.push(("AWS_SECRET_ACCESS_KEY".to_string(), key.to_string()));
        }
    }
    env
}

// ---------------------------------------------------------------------------
// Document persistence
// ---------------------------------------------------------------------------

pub fn load(path: &std::path::Path) -> (BackupConfig, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<BackupConfig>(&raw) {
            Ok(config) => (config, None),
            Err(err) => (
                BackupConfig::default(),
                Some(format!(
                    "{} is not valid backup JSON: {err}",
                    path.display()
                )),
            ),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (BackupConfig::default(), None),
        Err(err) => (
            BackupConfig::default(),
            Some(format!("cannot read {}: {err}", path.display())),
        ),
    }
}

pub fn save(path: &std::path::Path, config: &BackupConfig) -> Result<(), ApiError> {
    let mut body = serde_json::to_vec_pretty(config)
        .map_err(|err| ApiError::internal(format!("cannot serialise backup config: {err}")))?;
    body.push(b'\n');
    write_atomic(path, &body)
        .map_err(|err| ApiError::internal(format!("cannot write {}: {err}", path.display())))
}

// ---------------------------------------------------------------------------
// Source resolution
// ---------------------------------------------------------------------------

/// Concrete directories the standard source groups map to.
#[derive(Debug, Clone, Default)]
pub struct SourceDirs {
    pub docker_volumes: PathBuf,
    pub apps: PathBuf,
    pub guests: PathBuf,
    pub config_dir: PathBuf,
    pub ca: PathBuf,
    pub file_roots: Vec<String>,
}

pub fn resolve_sources(job: &BackupJob, dirs: &SourceDirs) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let mut push = |path: String| {
        if !path.is_empty() && !paths.contains(&path) {
            paths.push(path);
        }
    };
    if job.sources.docker_volumes {
        push(dirs.docker_volumes.to_string_lossy().to_string());
    }
    if job.sources.compose_apps {
        push(dirs.apps.to_string_lossy().to_string());
    }
    if job.sources.virt_guests {
        push(dirs.guests.to_string_lossy().to_string());
    }
    if job.sources.documents {
        push(dirs.config_dir.to_string_lossy().to_string());
    }
    if job.sources.certs_ca {
        push(dirs.ca.to_string_lossy().to_string());
    }
    if job.sources.file_roots {
        for root in &dirs.file_roots {
            push(root.clone());
        }
    }
    for custom in &job.sources.custom {
        push(custom.trim().to_string());
    }
    paths
}

// ---------------------------------------------------------------------------
// restic JSON shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    #[serde(default)]
    pub short_id: String,
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Parse `restic snapshots --json`.
pub fn parse_snapshots(json: &str) -> Result<Vec<Snapshot>, ApiError> {
    serde_json::from_str::<Vec<Snapshot>>(json)
        .map_err(|err| ApiError::internal(format!("cannot read the snapshot list: {err}")))
}

/// The summary line of `restic backup --json`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct BackupSummary {
    pub snapshot_id: String,
    pub bytes_processed: u64,
    pub files_new: u64,
}

/// Parse the newline-delimited JSON `restic backup --json` emits, taking the
/// final `summary` message.
pub fn parse_backup_summary(json_lines: &str) -> BackupSummary {
    let mut summary = BackupSummary::default();
    for line in json_lines.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("message_type").and_then(|v| v.as_str()) != Some("summary") {
            continue;
        }
        if let Some(id) = value.get("snapshot_id").and_then(|v| v.as_str()) {
            summary.snapshot_id = id.to_string();
        }
        if let Some(bytes) = value.get("total_bytes_processed").and_then(|v| v.as_u64()) {
            summary.bytes_processed = bytes;
        }
        if let Some(files) = value.get("files_new").and_then(|v| v.as_u64()) {
            summary.files_new = files;
        }
    }
    summary
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LsEntry {
    pub path: String,
    pub name: String,
    /// `file` or `dir`.
    pub kind: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub mtime: String,
}

/// Parse `restic ls --json`, keeping the immediate children of `prefix`.
pub fn parse_ls(json_lines: &str, prefix: &str) -> Result<Vec<LsEntry>, ApiError> {
    let mut entries: Vec<LsEntry> = Vec::new();
    let prefix = prefix.trim_end_matches('/');

    for line in json_lines.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(path) = value.get("path").and_then(|v| v.as_str()) else {
            continue;
        };
        let kind = match value.get("type").and_then(|v| v.as_str()) {
            Some("dir") => "dir",
            Some("file") => "file",
            _ => continue,
        };
        // Only the immediate children of `prefix`.
        let relative = if prefix.is_empty() {
            path.trim_start_matches('/')
        } else if let Some(rest) = path.strip_prefix(prefix) {
            rest.trim_start_matches('/')
        } else {
            continue;
        };
        if relative.is_empty() || relative.contains('/') {
            continue;
        }
        let name = value
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(relative)
            .to_string();
        entries.push(LsEntry {
            path: path.to_string(),
            name,
            kind: kind.to_string(),
            size: value.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
            mtime: value
                .get("mtime")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    entries.sort_by(|a, b| {
        (b.kind == "dir")
            .cmp(&(a.kind == "dir"))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

// ---------------------------------------------------------------------------
// Run history
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackupRun {
    pub job_id: String,
    pub name: String,
    pub ts_ms: u64,
    pub ok: bool,
    pub duration_ms: u64,
    pub snapshot_id: Option<String>,
    pub bytes: Option<u64>,
    pub detail: String,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct BackupEngine {
    pub config_path: PathBuf,
    pub secrets_path: PathBuf,
    pub runner: Arc<dyn Runner>,
    pub timeout: Duration,
    pub dirs: SourceDirs,
    /// One lock for every repository write, so prune and backup never race.
    lock: Arc<Mutex<()>>,
    history: Arc<Mutex<VecDeque<BackupRun>>>,
}

impl BackupEngine {
    pub fn new(
        config_path: PathBuf,
        secrets_path: PathBuf,
        runner: Arc<dyn Runner>,
        timeout: Duration,
        dirs: SourceDirs,
    ) -> Self {
        Self {
            config_path,
            secrets_path,
            runner,
            timeout,
            dirs,
            lock: Arc::new(Mutex::new(())),
            history: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub async fn history(&self) -> Vec<BackupRun> {
        let guard = self.history.lock().await;
        guard.iter().rev().cloned().collect()
    }

    /// True while any job is running.
    pub async fn busy(&self) -> bool {
        self.lock.try_lock().is_err()
    }

    /// Run one job: init if needed, back up, then prune per its retention.
    pub async fn run_job(&self, job: &BackupJob) -> BackupRun {
        let started = std::time::Instant::now();
        let started_ms = now_ms();
        let outcome = self.run_job_inner(job).await;
        let run = BackupRun {
            job_id: job.id.clone(),
            name: job.name.clone(),
            ts_ms: started_ms,
            ok: outcome.is_ok(),
            duration_ms: started.elapsed().as_millis() as u64,
            snapshot_id: outcome.as_ref().ok().and_then(|summary| {
                (!summary.snapshot_id.is_empty()).then(|| summary.snapshot_id.clone())
            }),
            bytes: outcome.as_ref().ok().map(|summary| summary.bytes_processed),
            detail: match &outcome {
                Ok(summary) => {
                    if summary.snapshot_id.is_empty() {
                        "completed".to_string()
                    } else {
                        format!("snapshot {}", short_id(&summary.snapshot_id))
                    }
                }
                Err(detail) => detail.clone(),
            },
        };
        let mut guard = self.history.lock().await;
        guard.push_back(run.clone());
        while guard.len() > HISTORY_CAP {
            guard.pop_front();
        }
        run
    }

    async fn run_job_inner(&self, job: &BackupJob) -> Result<BackupSummary, String> {
        let sources = resolve_sources(job, &self.dirs);
        if sources.is_empty() {
            return Err("the job has no sources selected".to_string());
        }
        let secrets = load_secrets(&self.secrets_path);
        let secret = secrets
            .repos
            .get(job.secret_key())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "no repository password is stored for this job (expected secret {:?})",
                    job.secret_key()
                )
            })?;
        if secret.restic_password.is_empty() {
            return Err("the repository password is empty".to_string());
        }

        // Serialise all repository access.
        let _guard = self.lock.lock().await;
        let repo = job.backend.repo();
        let env = restic_env(&secret, &job.backend);
        let runner = Arc::clone(&self.runner);
        let timeout = self.timeout;
        let excludes = job.excludes.clone();
        let retention = job.retention.clone();

        tokio::task::spawn_blocking(move || {
            restic_init(runner.as_ref(), &repo, &env, timeout)?;

            let mut args: Vec<String> = vec![
                "-r".to_string(),
                repo.clone(),
                "backup".to_string(),
                "--json".to_string(),
            ];
            for exclude in &excludes {
                args.push("--exclude".to_string());
                args.push(exclude.clone());
            }
            args.extend(sources.iter().cloned());

            let output = runner
                .run_env("restic", &args, &env, timeout)
                .map_err(|err| format!("restic is not available: {err}"))?;
            if output.timed_out {
                return Err("restic backup timed out".to_string());
            }
            if !output.success() {
                return Err(format!(
                    "restic backup failed: {}",
                    first_line(&output.stderr, &output.stdout)
                ));
            }
            let summary = parse_backup_summary(&output.stdout);

            // Retention pruning is best-effort: a failure must not turn a good
            // backup into a failed run.
            if let Err(err) = restic_forget(runner.as_ref(), &repo, &env, &retention, timeout) {
                tracing::warn!(%err, "backup retention pruning failed");
            }
            Ok(summary)
        })
        .await
        .map_err(|err| format!("backup task failed: {err}"))?
    }

    /// Verify a repository (`restic check`).
    pub async fn verify(&self, job: &BackupJob) -> Result<String, ApiError> {
        let secrets = load_secrets(&self.secrets_path);
        let secret = secrets
            .repos
            .get(job.secret_key())
            .cloned()
            .ok_or_else(|| {
                ApiError::bad_request("no repository password is stored for this job")
            })?;
        let repo = job.backend.repo();
        let env = restic_env(&secret, &job.backend);
        let runner = Arc::clone(&self.runner);
        let timeout = self.timeout;
        tokio::task::spawn_blocking(move || restic_check(runner.as_ref(), &repo, &env, timeout))
            .await
            .map_err(|err| ApiError::internal(format!("check task failed: {err}")))?
    }

    /// Test connectivity by listing snapshots.
    pub async fn test(&self, backend: Backend, secret_ref: String) -> Result<usize, ApiError> {
        let secrets = load_secrets(&self.secrets_path);
        let secret =
            secrets.repos.get(&secret_ref).cloned().ok_or_else(|| {
                ApiError::bad_request("no credentials are stored for this job yet")
            })?;
        let repo = backend.repo();
        let env = restic_env(&secret, &backend);
        let runner = Arc::clone(&self.runner);
        let timeout = self.timeout;
        let snapshots = tokio::task::spawn_blocking(move || {
            restic_snapshots(runner.as_ref(), &repo, &env, timeout)
        })
        .await
        .map_err(|err| ApiError::internal(format!("test task failed: {err}")))??;
        Ok(snapshots.len())
    }

    pub async fn snapshots(&self, job: &BackupJob) -> Result<Vec<Snapshot>, ApiError> {
        let secrets = load_secrets(&self.secrets_path);
        let secret = secrets
            .repos
            .get(job.secret_key())
            .cloned()
            .ok_or_else(|| {
                ApiError::bad_request("no repository password is stored for this job")
            })?;
        let repo = job.backend.repo();
        let env = restic_env(&secret, &job.backend);
        let runner = Arc::clone(&self.runner);
        let timeout = self.timeout;
        tokio::task::spawn_blocking(move || restic_snapshots(runner.as_ref(), &repo, &env, timeout))
            .await
            .map_err(|err| ApiError::internal(format!("snapshots task failed: {err}")))?
    }

    pub async fn browse(
        &self,
        job: &BackupJob,
        snapshot: String,
        path: String,
    ) -> Result<Vec<LsEntry>, ApiError> {
        let secrets = load_secrets(&self.secrets_path);
        let secret = secrets
            .repos
            .get(job.secret_key())
            .cloned()
            .ok_or_else(|| {
                ApiError::bad_request("no repository password is stored for this job")
            })?;
        let repo = job.backend.repo();
        let env = restic_env(&secret, &job.backend);
        let runner = Arc::clone(&self.runner);
        let timeout = self.timeout;
        tokio::task::spawn_blocking(move || {
            restic_ls(runner.as_ref(), &repo, &env, &snapshot, &path, timeout)
        })
        .await
        .map_err(|err| ApiError::internal(format!("ls task failed: {err}")))?
    }

    /// Restore a snapshot into `target` (optionally a single `include` path).
    pub async fn restore(
        &self,
        job: &BackupJob,
        snapshot: String,
        target: String,
        include: Option<String>,
    ) -> Result<String, ApiError> {
        let secrets = load_secrets(&self.secrets_path);
        let secret = secrets
            .repos
            .get(job.secret_key())
            .cloned()
            .ok_or_else(|| {
                ApiError::bad_request("no repository password is stored for this job")
            })?;
        let repo = job.backend.repo();
        let env = restic_env(&secret, &job.backend);
        let runner = Arc::clone(&self.runner);
        let timeout = self.timeout;
        tokio::task::spawn_blocking(move || {
            restic_restore(
                runner.as_ref(),
                &repo,
                &env,
                &snapshot,
                &target,
                include.as_deref(),
                timeout,
            )
        })
        .await
        .map_err(|err| ApiError::internal(format!("restore task failed: {err}")))?
    }

    pub fn config(&self) -> BackupConfig {
        load(&self.config_path).0
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn short_id(id: &str) -> &str {
    &id[..id.len().min(8)]
}

fn first_line(stderr: &str, stdout: &str) -> String {
    let text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    text.lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(300).collect())
        .unwrap_or_else(|| "no output".to_string())
}

// ---------------------------------------------------------------------------
// restic command wrappers
// ---------------------------------------------------------------------------

pub fn restic_init(
    runner: &dyn Runner,
    repo: &str,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<(), String> {
    let args = vec!["-r".to_string(), repo.to_string(), "init".to_string()];
    let output = runner
        .run_env("restic", &args, env, timeout)
        .map_err(|err| format!("restic is not available: {err}"))?;
    if output.timed_out {
        return Err("restic init timed out".to_string());
    }
    // "already initialized" is the normal case after the first run.
    if !output.success() && !output.combined().contains("already initialized") {
        return Err(format!(
            "restic init failed: {}",
            first_line(&output.stderr, &output.stdout)
        ));
    }
    Ok(())
}

pub fn restic_snapshots(
    runner: &dyn Runner,
    repo: &str,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<Vec<Snapshot>, ApiError> {
    let args = vec![
        "-r".to_string(),
        repo.to_string(),
        "snapshots".to_string(),
        "--json".to_string(),
    ];
    let output = runner
        .run_env("restic", &args, env, timeout)
        .map_err(|err| ApiError::unavailable(format!("restic is not available: {err}")))?;
    if output.timed_out {
        return Err(ApiError::internal("restic snapshots timed out"));
    }
    if !output.success() {
        return Err(ApiError::internal(format!(
            "restic snapshots failed: {}",
            first_line(&output.stderr, &output.stdout)
        )));
    }
    parse_snapshots(&output.stdout)
}

pub fn restic_forget(
    runner: &dyn Runner,
    repo: &str,
    env: &[(String, String)],
    retention: &Retention,
    timeout: Duration,
) -> Result<(), String> {
    let mut args = vec![
        "-r".to_string(),
        repo.to_string(),
        "forget".to_string(),
        "--prune".to_string(),
    ];
    for (flag, value) in [
        ("--keep-last", retention.keep_last),
        ("--keep-daily", retention.keep_daily),
        ("--keep-weekly", retention.keep_weekly),
        ("--keep-monthly", retention.keep_monthly),
        ("--keep-yearly", retention.keep_yearly),
    ] {
        if value > 0 {
            args.push(flag.to_string());
            args.push(value.to_string());
        }
    }
    let output = runner
        .run_env("restic", &args, env, timeout)
        .map_err(|err| format!("restic is not available: {err}"))?;
    if output.timed_out {
        return Err("restic forget timed out".to_string());
    }
    if !output.success() {
        return Err(format!(
            "restic forget failed: {}",
            first_line(&output.stderr, &output.stdout)
        ));
    }
    Ok(())
}

pub fn restic_check(
    runner: &dyn Runner,
    repo: &str,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<String, ApiError> {
    let args = vec!["-r".to_string(), repo.to_string(), "check".to_string()];
    let output = runner
        .run_env("restic", &args, env, timeout)
        .map_err(|err| ApiError::unavailable(format!("restic is not available: {err}")))?;
    if output.timed_out {
        return Err(ApiError::internal("restic check timed out"));
    }
    if !output.success() {
        return Err(ApiError::internal(format!(
            "restic check failed: {}",
            first_line(&output.stderr, &output.stdout)
        )));
    }
    Ok(first_line(&output.stderr, &output.stdout))
}

pub fn restic_ls(
    runner: &dyn Runner,
    repo: &str,
    env: &[(String, String)],
    snapshot: &str,
    path: &str,
    timeout: Duration,
) -> Result<Vec<LsEntry>, ApiError> {
    let mut args = vec![
        "-r".to_string(),
        repo.to_string(),
        "ls".to_string(),
        "--json".to_string(),
        snapshot.to_string(),
    ];
    if !path.trim().is_empty() && path.trim() != "/" {
        args.push(path.trim().to_string());
    }
    let output = runner
        .run_env("restic", &args, env, timeout)
        .map_err(|err| ApiError::unavailable(format!("restic is not available: {err}")))?;
    if output.timed_out {
        return Err(ApiError::internal("restic ls timed out"));
    }
    if !output.success() {
        return Err(ApiError::internal(format!(
            "restic ls failed: {}",
            first_line(&output.stderr, &output.stdout)
        )));
    }
    parse_ls(&output.stdout, path)
}

pub fn restic_restore(
    runner: &dyn Runner,
    repo: &str,
    env: &[(String, String)],
    snapshot: &str,
    target: &str,
    include: Option<&str>,
    timeout: Duration,
) -> Result<String, ApiError> {
    let mut args = vec![
        "-r".to_string(),
        repo.to_string(),
        "restore".to_string(),
        snapshot.to_string(),
        "--target".to_string(),
        target.to_string(),
    ];
    if let Some(include) = include.map(str::trim).filter(|value| !value.is_empty()) {
        args.push("--include".to_string());
        args.push(include.to_string());
    }
    let output = runner
        .run_env("restic", &args, env, timeout)
        .map_err(|err| ApiError::unavailable(format!("restic is not available: {err}")))?;
    if output.timed_out {
        return Err(ApiError::internal("restic restore timed out"));
    }
    if !output.success() {
        return Err(ApiError::internal(format!(
            "restic restore failed: {}",
            first_line(&output.stderr, &output.stdout)
        )));
    }
    Ok(format!("restored into {target}"))
}

/// Next run time (unix seconds) for a job at or after `from`.
pub fn next_run(schedule: &Schedule, from: u64) -> u64 {
    let day = 86_400;
    match schedule.cadence {
        Cadence::Hourly => {
            // The next whole hour.
            (from / 3600 + 1) * 3600
        }
        Cadence::Daily | Cadence::Weekly => {
            let (hour, minute) = parse_hhmm(&schedule.at).unwrap_or((3, 0));
            let offset = hour as u64 * 3600 + minute as u64 * 60;
            let mut candidate = (from / day) * day + offset;
            if candidate <= from {
                candidate += day;
            }
            if schedule.cadence == Cadence::Weekly {
                // 1970-01-01 was a Thursday (weekday 4); align to the target.
                let mut guard = 0;
                while ((candidate / day + 4) % 7) as u8 != schedule.weekday % 7 && guard < 8 {
                    candidate += day;
                    guard += 1;
                }
            }
            candidate
        }
    }
}

#[cfg(unix)]
fn restrict_600(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_600(_path: &std::path::Path) {}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::fake::FakeRunner;

    fn job(id: &str) -> BackupJob {
        BackupJob {
            id: id.to_string(),
            name: format!("Job {id}"),
            enabled: true,
            sources: JobSources {
                documents: true,
                ..JobSources::default()
            },
            excludes: Vec::new(),
            schedule: Schedule::default(),
            retention: Retention::default(),
            backend: Backend::Local {
                path: "/srv/backups".to_string(),
            },
            secret_ref: String::new(),
        }
    }

    fn dirs() -> SourceDirs {
        SourceDirs {
            config_dir: PathBuf::from("/etc/stack-stack"),
            ..SourceDirs::default()
        }
    }

    #[test]
    fn default_config_validates_clean() {
        assert!(BackupConfig::default().validate().is_empty());
    }

    #[test]
    fn job_validation_catches_ids_schedules_and_backends() {
        let config = BackupConfig {
            jobs: vec![
                job("a"),
                BackupJob {
                    id: "a".to_string(),
                    name: String::new(),
                    schedule: Schedule {
                        cadence: Cadence::Daily,
                        at: "25:00".to_string(),
                        weekday: 9,
                    },
                    backend: Backend::S3 {
                        endpoint: "ftp://x".to_string(),
                        bucket: String::new(),
                    },
                    ..job("a")
                },
            ],
        };
        let problems = config.validate();
        let fields: Vec<&str> = problems.iter().map(|p| p.field.as_str()).collect();
        assert!(fields.iter().any(|f| f.ends_with("id")));
        assert!(fields.iter().any(|f| f.ends_with("name")));
        assert!(fields.iter().any(|f| f.ends_with("schedule.at")));
        assert!(fields.iter().any(|f| f.ends_with("schedule.weekday")));
        assert!(fields.iter().any(|f| f.ends_with("backend.endpoint")));
        assert!(fields.iter().any(|f| f.ends_with("backend.bucket")));
    }

    #[test]
    fn repo_urls_are_built_per_backend() {
        assert_eq!(
            Backend::Local {
                path: "/srv/b".to_string()
            }
            .repo(),
            "/srv/b"
        );
        assert_eq!(
            Backend::S3 {
                endpoint: "https://s3.example.test/".to_string(),
                bucket: "bu".to_string()
            }
            .repo(),
            "s3:https://s3.example.test/bu"
        );
        assert_eq!(
            Backend::Sftp {
                target: "u@h:/p".to_string()
            }
            .repo(),
            "sftp:u@h:/p"
        );
    }

    #[test]
    fn env_carries_the_password_and_s3_credentials_only() {
        let secret = RepoSecret {
            restic_password: "pw".to_string(),
            aws_access_key_id: Some("AK".to_string()),
            aws_secret_access_key: Some("SK".to_string()),
        };
        let local = restic_env(
            &secret,
            &Backend::Local {
                path: "/x".to_string(),
            },
        );
        assert_eq!(
            local,
            vec![("RESTIC_PASSWORD".to_string(), "pw".to_string())]
        );
        let s3 = restic_env(
            &secret,
            &Backend::S3 {
                endpoint: "https://e".to_string(),
                bucket: "b".to_string(),
            },
        );
        assert!(s3.iter().any(|(k, _)| k == "AWS_ACCESS_KEY_ID"));
        assert!(s3.iter().any(|(k, _)| k == "AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn sources_resolve_from_the_selected_groups() {
        let mut job = job("a");
        job.sources.custom = vec!["/opt/data".to_string()];
        let resolved = resolve_sources(&job, &dirs());
        assert!(resolved.contains(&"/etc/stack-stack".to_string()));
        assert!(resolved.contains(&"/opt/data".to_string()));
        assert!(!resolved.contains(&"/var/lib/docker/volumes".to_string()));
    }

    #[test]
    fn snapshot_json_is_parsed() {
        let json = r#"[{"id":"abc12345","short_id":"abc12345","time":"2026-01-01T03:00:00Z","hostname":"box","paths":["/etc"],"tags":[]}]"#;
        let snapshots = parse_snapshots(json).expect("parse");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].short_id, "abc12345");
    }

    #[test]
    fn backup_summary_takes_the_last_summary_line() {
        let lines = "{\"message_type\":\"status\",\"percent_done\":0.5}\n{\"message_type\":\"summary\",\"snapshot_id\":\"deadbeef\",\"total_bytes_processed\":4096,\"files_new\":3}\n";
        let summary = parse_backup_summary(lines);
        assert_eq!(summary.snapshot_id, "deadbeef");
        assert_eq!(summary.bytes_processed, 4096);
        assert_eq!(summary.files_new, 3);
    }

    #[test]
    fn ls_keeps_only_immediate_children() {
        let lines = "{\"path\":\"/etc\",\"name\":\"etc\",\"type\":\"dir\"}\n{\"path\":\"/etc/a.conf\",\"name\":\"a.conf\",\"type\":\"file\",\"size\":10}\n{\"path\":\"/etc/sub/b\",\"name\":\"b\",\"type\":\"file\",\"size\":1}\n";
        let entries = parse_ls(lines, "/etc").expect("parse");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a.conf");
    }

    #[test]
    fn next_run_is_in_the_future_and_respects_weekly() {
        // 2026-01-01T00:00:00Z
        let from = 1_767_225_600;
        let daily = next_run(&Schedule::default(), from);
        assert!(daily > from && daily - from <= 86_400);
        let hourly = next_run(
            &Schedule {
                cadence: Cadence::Hourly,
                ..Schedule::default()
            },
            from,
        );
        assert!(hourly > from && hourly - from <= 3600);
        let weekly = next_run(
            &Schedule {
                cadence: Cadence::Weekly,
                weekday: 1,
                ..Schedule::default()
            },
            from,
        );
        assert!(weekly > from && weekly - from <= 7 * 86_400);
        assert_eq!(((weekly / 86_400 + 4) % 7) as u8, 1);
    }

    #[test]
    fn running_a_job_without_a_secret_is_reported() {
        let dir = std::env::temp_dir().join(format!("ss-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let engine = BackupEngine::new(
            dir.join("backup.json"),
            dir.join("secrets.json"),
            Arc::new(FakeRunner::new()),
            Duration::from_secs(60),
            dirs(),
        );
        let runtime = tokio::runtime::Runtime::new().expect("rt");
        let run = runtime.block_on(engine.run_job(&job("a")));
        assert!(!run.ok);
        assert!(run.detail.contains("password"), "{}", run.detail);
    }

    #[test]
    fn running_a_job_hands_restic_its_password_and_records_history() {
        let dir = std::env::temp_dir().join(format!("ss-backup-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let secrets_path = dir.join("secrets.json");
        let mut secrets = BackupSecrets::default();
        secrets.repos.insert(
            "a".to_string(),
            RepoSecret {
                restic_password: "hunter2".to_string(),
                ..RepoSecret::default()
            },
        );
        save_secrets(&secrets_path, &secrets).expect("secrets");

        let runner = Arc::new(
            FakeRunner::new()
                .respond("restic", Some(0), "", "")
                .respond_when(
                    "restic",
                    "backup",
                    Some(0),
                    "{\"message_type\":\"summary\",\"snapshot_id\":\"cafe1234\",\"total_bytes_processed\":2048,\"files_new\":1}\n",
                    "",
                ),
        );
        let engine = BackupEngine::new(
            dir.join("backup.json"),
            secrets_path,
            runner.clone() as Arc<dyn Runner>,
            Duration::from_secs(60),
            dirs(),
        );
        let runtime = tokio::runtime::Runtime::new().expect("rt");
        let run = runtime.block_on(engine.run_job(&job("a")));
        assert!(run.ok, "{}", run.detail);
        assert_eq!(run.snapshot_id.as_deref(), Some("cafe1234"));
        assert_eq!(run.bytes, Some(2048));
        assert_eq!(runtime.block_on(engine.history()).len(), 1);
        // The repository password must have been handed over in the environment.
        assert!(
            runner
                .env_vars("restic")
                .iter()
                .any(|(key, value)| key == "RESTIC_PASSWORD" && value == "hunter2"),
            "RESTIC_PASSWORD must be set on the child"
        );
    }
}
