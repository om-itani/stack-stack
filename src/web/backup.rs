//! HTTP surface and scheduler for backups.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::model::Problem;
use crate::notify::{NotificationRequest, event};
use crate::services::backup::{
    self, Backend, BackupConfig, BackupJob, BackupRun, LsEntry, RepoSecret, Snapshot,
};
use crate::state::AppState;

const DOWNLOAD_CHUNK: usize = 64 * 1024;

#[derive(Debug, Serialize)]
pub struct BackupState {
    pub config: BackupConfig,
    pub problems: Vec<Problem>,
    pub config_error: Option<String>,
    pub history: Vec<BackupRun>,
    /// Job ids whose repository password is stored.
    pub secrets_configured: Vec<String>,
    /// Next scheduled run per job (unix seconds).
    pub next_runs: HashMap<String, u64>,
    pub running: bool,
}

#[derive(Serialize)]
pub struct RunReport {
    pub run: BackupRun,
    pub history: Vec<BackupRun>,
}

#[derive(Serialize)]
pub struct OkResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
}

#[derive(Deserialize)]
pub struct SecretsRequest {
    pub restic_password: String,
    #[serde(default)]
    pub aws_access_key_id: Option<String>,
    #[serde(default)]
    pub aws_secret_access_key: Option<String>,
    /// Overwrite an existing password (the UI asks before replacing).
    #[serde(default)]
    pub replace: bool,
}

#[derive(Deserialize)]
pub struct TestRequest {
    pub backend: Backend,
    pub secret_ref: String,
}

#[derive(Deserialize)]
pub struct RestoreRequest {
    pub snapshot: String,
    pub target: String,
    #[serde(default)]
    pub include: Option<String>,
}

#[derive(Deserialize)]
pub struct BrowseQuery {
    pub path: String,
}

#[derive(Deserialize)]
pub struct DownloadQuery {
    pub snapshot: String,
    pub path: String,
}

#[derive(Serialize)]
struct ValidationError {
    error: String,
    problems: Vec<Problem>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub async fn state(State(state): State<AppState>) -> Json<BackupState> {
    Json(collect(&state).await)
}

async fn collect(state: &AppState) -> BackupState {
    let path = Path::new(&state.backup.config_path);
    let (config, config_error) = backup::load(path);
    let problems = config.validate();
    let history = state.backup.history().await;
    let secrets = backup::load_secrets(&state.backup.secrets_path);
    let secrets_configured = secrets
        .repos
        .iter()
        .filter(|(_, secret)| !secret.restic_password.is_empty())
        .map(|(key, _)| key.clone())
        .collect();
    let now = now_secs();
    let next_runs = config
        .jobs
        .iter()
        .filter(|job| job.enabled)
        .map(|job| (job.id.clone(), backup::next_run(&job.schedule, now)))
        .collect();
    BackupState {
        config,
        problems,
        config_error,
        history,
        secrets_configured,
        next_runs,
        running: state.backup.busy().await,
    }
}

pub async fn save_config(
    State(state): State<AppState>,
    Json(config): Json<BackupConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid(problems));
    }
    let path = state.backup.config_path.clone();
    let for_write = config.clone();
    tokio::task::spawn_blocking(move || backup::save(&path, &for_write))
        .await
        .map_err(|err| ApiError::internal(format!("backup task failed: {err}")))??;
    Ok(Json(collect(&state).await).into_response())
}

pub async fn save_secrets(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<SecretsRequest>,
) -> Result<Json<OkResult>, ApiError> {
    let config = state.backup.config();
    let job = config
        .find(&id)
        .ok_or_else(|| ApiError::not_found(format!("no backup job with id {id:?}")))?;
    let key = job.secret_key().to_string();

    if request.restic_password.len() < 8 {
        return Err(ApiError::bad_request(
            "the repository password must be at least 8 characters",
        ));
    }

    let path = state.backup.secrets_path.clone();
    let mut secrets = backup::load_secrets(&path);
    if secrets
        .repos
        .get(&key)
        .is_some_and(|secret| !secret.restic_password.is_empty())
        && !request.replace
    {
        return Err(ApiError::bad_request(
            "a repository password is already stored; confirm that you want to replace it",
        ));
    }
    secrets.repos.insert(
        key,
        RepoSecret {
            restic_password: request.restic_password,
            aws_access_key_id: request.aws_access_key_id.filter(|v| !v.trim().is_empty()),
            aws_secret_access_key: request
                .aws_secret_access_key
                .filter(|v| !v.trim().is_empty()),
        },
    );
    tokio::task::spawn_blocking(move || backup::save_secrets(&path, &secrets))
        .await
        .map_err(|err| ApiError::internal(format!("backup task failed: {err}")))??;
    Ok(Json(OkResult {
        ok: true,
        detail: Some("credentials stored".to_string()),
        count: None,
    }))
}

pub async fn run_job(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<RunReport>, ApiError> {
    let job = job_or_404(&state, &id)?;
    let run = state.backup.run_job(&job).await;
    if !run.ok {
        state.notify.emit(NotificationRequest::error(
            event::BACKUP_FAILED,
            format!("Backup {} failed", run.name),
            run.detail.clone(),
        ));
    }
    Ok(Json(RunReport {
        run,
        history: state.backup.history().await,
    }))
}

pub async fn verify(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<OkResult>, ApiError> {
    let job = job_or_404(&state, &id)?;
    let detail = state.backup.verify(&job).await?;
    Ok(Json(OkResult {
        ok: true,
        detail: Some(detail),
        count: None,
    }))
}

pub async fn test(
    State(state): State<AppState>,
    Json(request): Json<TestRequest>,
) -> Result<Json<OkResult>, ApiError> {
    let count = state
        .backup
        .test(request.backend, request.secret_ref)
        .await?;
    Ok(Json(OkResult {
        ok: true,
        detail: Some(format!("repository reachable, {count} snapshot(s)")),
        count: Some(count),
    }))
}

pub async fn snapshots(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Vec<Snapshot>>, ApiError> {
    let job = job_or_404(&state, &id)?;
    Ok(Json(state.backup.snapshots(&job).await?))
}

pub async fn browse(
    State(state): State<AppState>,
    AxumPath((id, snapshot)): AxumPath<(String, String)>,
    Query(query): Query<BrowseQuery>,
) -> Result<Json<Vec<LsEntry>>, ApiError> {
    let job = job_or_404(&state, &id)?;
    Ok(Json(state.backup.browse(&job, snapshot, query.path).await?))
}

pub async fn restore(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<RestoreRequest>,
) -> Result<Json<OkResult>, ApiError> {
    let job = job_or_404(&state, &id)?;
    let detail = state
        .backup
        .restore(&job, request.snapshot, request.target, request.include)
        .await?;
    Ok(Json(OkResult {
        ok: true,
        detail: Some(detail),
        count: None,
    }))
}

/// Restore one file into a temporary directory and stream it to the browser.
pub async fn download(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<DownloadQuery>,
) -> Result<Response, ApiError> {
    let job = job_or_404(&state, &id)?;
    let temp = std::env::temp_dir().join(format!(
        "ss-restic-{}-{}",
        std::process::id(),
        crate::rand::random_hex(6)
    ));
    std::fs::create_dir_all(&temp)
        .map_err(|err| ApiError::internal(format!("cannot create a temporary directory: {err}")))?;

    let target = temp.to_string_lossy().to_string();
    let include = query.path.clone();
    if let Err(err) = state
        .backup
        .restore(&job, query.snapshot, target, Some(include.clone()))
        .await
    {
        let _ = std::fs::remove_dir_all(&temp);
        return Err(err);
    }

    // restic restores an absolute path under the target directory.
    let restored = temp.join(include.trim_start_matches('/'));
    if !restored.is_file() {
        let _ = std::fs::remove_dir_all(&temp);
        return Err(ApiError::not_found(format!(
            "{} was not found in that snapshot",
            query.path
        )));
    }
    stream_file(restored, TempTree(temp)).await
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

/// Run due jobs every minute for the life of the process.
pub fn spawn_scheduler(state: AppState) {
    tokio::spawn(async move {
        let mut last: HashMap<String, u64> = HashMap::new();
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Seed `last` so a restart does not fire every job immediately.
        let now = now_secs();
        for job in state.backup.config().jobs {
            last.insert(job.id.clone(), now);
        }
        loop {
            ticker.tick().await;
            let config = state.backup.config();
            let now = now_secs();
            for job in config.jobs {
                if !job.enabled {
                    continue;
                }
                let anchor = *last.entry(job.id.clone()).or_insert(now);
                let due = backup::next_run(&job.schedule, anchor);
                if now >= due && !state.backup.busy().await {
                    tracing::info!(job = %job.id, "running scheduled backup");
                    let run = state.backup.run_job(&job).await;
                    if !run.ok {
                        state.notify.emit(NotificationRequest::error(
                            event::BACKUP_FAILED,
                            format!("Backup {} failed", run.name),
                            run.detail.clone(),
                        ));
                    }
                    last.insert(job.id.clone(), now);
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn job_or_404(state: &AppState, id: &str) -> Result<BackupJob, ApiError> {
    state
        .backup
        .config()
        .find(id)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("no backup job with id {id:?}")))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn invalid(problems: Vec<Problem>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ValidationError {
            error: "backup configuration is not valid".to_string(),
            problems,
        }),
    )
        .into_response()
}

/// Stream a file with a compensating cleanup.
async fn stream_file(path: PathBuf, cleanup: TempTree) -> Result<Response, ApiError> {
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|err| ApiError::internal(format!("cannot open {}: {err}", path.display())))?;
    let size = file.metadata().await.map(|meta| meta.len()).unwrap_or(0);

    let stream =
        futures_util::stream::unfold((file, cleanup), move |(mut file, cleanup)| async move {
            let mut buffer = vec![0u8; DOWNLOAD_CHUNK];
            match tokio::io::AsyncReadExt::read(&mut file, &mut buffer).await {
                Ok(0) => None,
                Ok(read) => {
                    buffer.truncate(read);
                    Some((
                        Ok::<_, std::io::Error>(Bytes::from(buffer)),
                        (file, cleanup),
                    ))
                }
                Err(err) => Some((Err(err), (file, cleanup))),
            }
        });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, size.to_string())
        .header(
            header::CONTENT_DISPOSITION,
            format!(
                "attachment; filename=\"{}\"",
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "restored".to_string())
                    .replace('"', "")
            ),
        )
        .body(Body::from_stream(stream))
        .map_err(|err| ApiError::internal(format!("cannot build the response: {err}")))
}

/// Removes a restored temporary tree when the response finishes.
struct TempTree(PathBuf);

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
