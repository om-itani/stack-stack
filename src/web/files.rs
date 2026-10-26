//! HTTP surface for the file manager.
//!
//! Reads are cheap and inline; downloads and archive creation stream so a large
//! file never lands in memory, and uploads stream to a `.part` file that is only
//! renamed into place once the whole body has arrived.

use std::path::{Path, PathBuf};

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Multipart, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::error::ApiError;
use crate::files::{self, AccountTable, DOWNLOAD_CHUNK, FileText, FilesConfig, Guard, Listing};
use crate::networking::model::Problem;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct FilesState {
    pub config: FilesConfig,
    pub problems: Vec<Problem>,
    pub config_error: Option<String>,
    /// Roots that actually exist and resolve; the sidebar list.
    pub roots: Vec<String>,
    pub listing: Listing,
}

#[derive(Serialize)]
pub struct OkResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

#[derive(Deserialize)]
pub struct PathQuery {
    pub path: String,
}

#[derive(Deserialize)]
pub struct PathBody {
    pub path: String,
}

#[derive(Deserialize)]
pub struct SaveBody {
    pub path: String,
    pub content: String,
}

#[derive(Deserialize)]
pub struct RenameBody {
    pub from: String,
    pub to: String,
}

#[derive(Deserialize)]
pub struct DeleteBody {
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
}

#[derive(Deserialize)]
pub struct ChmodBody {
    pub path: String,
    pub mode: String,
}

#[derive(Deserialize)]
pub struct ChownBody {
    pub path: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
}

#[derive(Serialize)]
pub struct Accounts {
    pub users: Vec<String>,
    pub groups: Vec<String>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub async fn state(
    State(state): State<AppState>,
    Query(query): Query<PathQuery>,
) -> Result<Json<FilesState>, ApiError> {
    let (config, guard, config_error) = context(&state).await?;
    let default = guard
        .roots()
        .first()
        .cloned()
        .ok_or_else(|| ApiError::unavailable("no file root exists on this host"))?;
    let wanted = if query.path.trim().is_empty() {
        default
    } else {
        query.path.clone()
    };
    let problems = config.validate();
    let roots = guard.roots();
    let config_for_list = config.clone();
    let listing = blocking(move || files::list(&config_for_list, &guard, &wanted)).await?;

    Ok(Json(FilesState {
        config,
        problems,
        config_error,
        roots,
        listing,
    }))
}

pub async fn save_config(
    State(state): State<AppState>,
    Json(config): Json<FilesConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "BadRequest",
                "message": "file manager configuration is not valid",
                "problems": problems,
            })),
        )
            .into_response());
    }
    let path = state.files.config_path.clone();
    let for_write = config.clone();
    blocking(move || files::save(&path, &for_write)).await?;

    let (_, guard, _) = context(&state).await?;
    let roots = guard.roots();
    Ok(Json(serde_json::json!({ "ok": true, "roots": roots })).into_response())
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

pub async fn read(
    State(state): State<AppState>,
    Query(query): Query<PathQuery>,
) -> Result<Json<FileText>, ApiError> {
    let (_, guard, _) = context(&state).await?;
    Ok(Json(
        blocking(move || files::read_text(&guard, &query.path)).await?,
    ))
}

pub async fn download(
    State(state): State<AppState>,
    Query(query): Query<PathQuery>,
) -> Result<Response, ApiError> {
    let (_, guard, _) = context(&state).await?;
    let path = blocking(move || guard.read(&query.path)).await?;
    stream_file(path, false).await
}

pub async fn accounts(State(state): State<AppState>) -> Result<Json<Accounts>, ApiError> {
    let _ = state;
    Ok(Json(
        blocking(|| {
            let table = AccountTable::load();
            Ok(Accounts {
                users: table.usernames(),
                groups: table.groupnames(),
            })
        })
        .await?,
    ))
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

pub async fn mkdir(
    State(state): State<AppState>,
    Json(body): Json<PathBody>,
) -> Result<Json<OkResult>, ApiError> {
    let (_, guard, _) = context(&state).await?;
    let path = blocking(move || files::mkdir(&guard, &body.path)).await?;
    Ok(Json(OkResult {
        ok: true,
        path: Some(path),
        bytes: None,
    }))
}

pub async fn create_file(
    State(state): State<AppState>,
    Json(body): Json<PathBody>,
) -> Result<Json<OkResult>, ApiError> {
    let (_, guard, _) = context(&state).await?;
    let path = blocking(move || files::create_file(&guard, &body.path)).await?;
    Ok(Json(OkResult {
        ok: true,
        path: Some(path),
        bytes: None,
    }))
}

pub async fn save(
    State(state): State<AppState>,
    Json(body): Json<SaveBody>,
) -> Result<Json<OkResult>, ApiError> {
    let (_, guard, _) = context(&state).await?;
    let path = body.path.clone();
    let content = body.content;
    let bytes = blocking(move || files::save_file(&guard, &path, &content)).await?;
    Ok(Json(OkResult {
        ok: true,
        path: Some(body.path),
        bytes: Some(bytes),
    }))
}

pub async fn rename(
    State(state): State<AppState>,
    Json(body): Json<RenameBody>,
) -> Result<Json<OkResult>, ApiError> {
    let (_, guard, _) = context(&state).await?;
    let path = blocking(move || files::rename(&guard, &body.from, &body.to)).await?;
    Ok(Json(OkResult {
        ok: true,
        path: Some(path),
        bytes: None,
    }))
}

pub async fn delete(
    State(state): State<AppState>,
    Json(body): Json<DeleteBody>,
) -> Result<Json<OkResult>, ApiError> {
    let (_, guard, _) = context(&state).await?;
    let recursive = body.recursive;
    blocking(move || files::delete(&guard, &body.path, recursive)).await?;
    Ok(Json(OkResult {
        ok: true,
        path: None,
        bytes: None,
    }))
}

pub async fn chmod(
    State(state): State<AppState>,
    Json(body): Json<ChmodBody>,
) -> Result<Json<OkResult>, ApiError> {
    let (_, guard, _) = context(&state).await?;
    blocking(move || files::chmod(&guard, &body.path, &body.mode)).await?;
    Ok(Json(OkResult {
        ok: true,
        path: None,
        bytes: None,
    }))
}

pub async fn chown(
    State(state): State<AppState>,
    Json(body): Json<ChownBody>,
) -> Result<Json<OkResult>, ApiError> {
    let (_, guard, _) = context(&state).await?;
    blocking(move || {
        files::chown(
            &guard,
            &body.path,
            body.user.as_deref(),
            body.group.as_deref(),
        )
    })
    .await?;
    Ok(Json(OkResult {
        ok: true,
        path: None,
        bytes: None,
    }))
}

pub async fn extract(
    State(state): State<AppState>,
    Json(body): Json<PathBody>,
) -> Result<Json<OkResult>, ApiError> {
    let (_, guard, _) = context(&state).await?;
    let runner = state.files.runner.clone();
    let path =
        tokio::task::spawn_blocking(move || files::extract(&guard, runner.as_ref(), &body.path))
            .await
            .map_err(|err| ApiError::internal(format!("extract task failed: {err}")))??;
    Ok(Json(OkResult {
        ok: true,
        path: Some(path),
        bytes: None,
    }))
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

pub async fn upload(
    State(state): State<AppState>,
    Query(query): Query<PathQuery>,
    mut multipart: Multipart,
) -> Result<Json<OkResult>, ApiError> {
    let (config, guard, _) = context(&state).await?;
    let dest = query.path.clone();
    let dest_dir = blocking(move || guard.read(&dest)).await?;
    if !dest_dir.is_dir() {
        return Err(ApiError::bad_request(
            "the upload target must be a directory",
        ));
    }

    let mut last: Option<String> = None;
    let mut total = 0u64;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|err| ApiError::bad_request(format!("malformed upload: {err}")))?
    {
        let Some(raw_name) = field.file_name().map(str::to_string) else {
            continue;
        };
        let name = files::sanitize_filename(&raw_name)?;
        let part = dest_dir.join(format!(".{name}.part"));

        let mut file = tokio::fs::File::create(&part).await.map_err(|err| {
            ApiError::internal(format!("cannot create {}: {err}", part.display()))
        })?;

        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|err| ApiError::bad_request(format!("upload interrupted: {err}")))?
        {
            total += chunk.len() as u64;
            if total > config.upload_max_bytes {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(ApiError::bad_request(format!(
                    "upload exceeds the {} byte limit",
                    config.upload_max_bytes
                )));
            }
            if let Err(err) = file.write_all(&chunk).await {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(ApiError::internal(format!("cannot write upload: {err}")));
            }
        }
        file.flush()
            .await
            .map_err(|err| ApiError::internal(format!("cannot flush upload: {err}")))?;
        file.sync_all()
            .await
            .map_err(|err| ApiError::internal(format!("cannot flush upload: {err}")))?;
        drop(file);

        let dir = dest_dir.clone();
        let name_for_finish = name.clone();
        let part_for_finish = part.clone();
        let finished = tokio::task::spawn_blocking(move || {
            files::finish_upload(&dir, &name_for_finish, &part_for_finish)
        })
        .await
        .map_err(|err| ApiError::internal(format!("upload task failed: {err}")))??;
        last = Some(finished);
    }

    Ok(Json(OkResult {
        ok: true,
        path: last,
        bytes: Some(total),
    }))
}

// ---------------------------------------------------------------------------
// Archive download
// ---------------------------------------------------------------------------

/// Create a `.tar.zst` of a directory in a temporary file and stream it.
pub async fn archive(
    State(state): State<AppState>,
    Query(query): Query<PathQuery>,
) -> Result<Response, ApiError> {
    let (_, guard, _) = context(&state).await?;
    let runner = state.files.runner.clone();
    let target = query.path.clone();

    let temp = tokio::task::spawn_blocking(move || -> Result<PathBuf, ApiError> {
        let source = guard.read(&target)?;
        if !source.is_dir() {
            return Err(ApiError::bad_request("only directories can be archived"));
        }
        let name = files_archive_name(&source);
        let temp_path =
            std::env::temp_dir().join(format!("ss-archive-{}-{name}.tar.zst", std::process::id()));
        let base = source
            .parent()
            .map(|parent| parent.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".to_string());
        let leaf = source
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let args = files::create_tar_args(&base, &[leaf], &temp_path.to_string_lossy());
        let output = runner
            .run_timeout("tar", &args, std::time::Duration::from_secs(1800))
            .map_err(|err| ApiError::unavailable(format!("tar is not available: {err}")))?;
        if !output.success() {
            let _ = std::fs::remove_file(&temp_path);
            return Err(ApiError::internal(format!(
                "archiving failed: {}",
                output.combined().trim()
            )));
        }
        Ok(temp_path)
    })
    .await
    .map_err(|err| ApiError::internal(format!("archive task failed: {err}")))??;

    stream_file(temp, true).await
}

fn files_archive_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "archive".to_string())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn context(state: &AppState) -> Result<(FilesConfig, Guard, Option<String>), ApiError> {
    let path = state.files.config_path.clone();
    let (config, error) = tokio::task::spawn_blocking(move || files::load(&path))
        .await
        .map_err(|err| ApiError::internal(format!("files task failed: {err}")))?;
    let guard = Guard::new(&config.roots);
    Ok((config, guard, error))
}

async fn blocking<T, F>(work: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| ApiError::internal(format!("files worker failed: {err}")))?
}

/// Stream a file, optionally deleting it once the last byte has been sent.
async fn stream_file(path: PathBuf, delete_after: bool) -> Result<Response, ApiError> {
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|err| ApiError::internal(format!("cannot open {}: {err}", path.display())))?;
    let size = file.metadata().await.map(|meta| meta.len()).unwrap_or(0);
    let name = files_archive_name(&path);
    let mime = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string();

    let guard = delete_after.then(|| TempGuard(path.clone()));
    let stream = futures_util::stream::unfold((file, guard), move |(mut file, guard)| async move {
        let mut buffer = vec![0u8; DOWNLOAD_CHUNK];
        match tokio::io::AsyncReadExt::read(&mut file, &mut buffer).await {
            Ok(0) => None,
            Ok(read) => {
                buffer.truncate(read);
                Some((Ok::<_, std::io::Error>(Bytes::from(buffer)), (file, guard)))
            }
            Err(err) => Some((Err(err), (file, guard))),
        }
    });

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, size.to_string())
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", name.replace('"', "")),
        )
        .body(Body::from_stream(stream))
        .map_err(|err| ApiError::internal(format!("cannot build the response: {err}")))?;
    Ok(response)
}

/// Deletes its path when dropped, which happens when the response body finishes
/// (or the client disconnects).
struct TempGuard(PathBuf);

impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
