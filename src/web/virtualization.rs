//! HTTP + WebSocket surface for virtualization.
//!
//! Reads gather the document, the catalog, the host capability probe and live
//! guest status in a single round trip. Writes validate before persisting, and
//! every action that touches the system runs on the blocking pool under the
//! context lock so two lifecycle calls cannot race the same guest.

use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};

use crate::containers::model::{ExecClientMessage, ExecServerMessage};
use crate::error::ApiError;
use crate::state::AppState;
use crate::virtualization::host::{self, HostCapabilities};
use crate::virtualization::manager::{self, Report, SnapshotAction, VmControl};
use crate::virtualization::model::{CtConfig, Problem, VirtualizationConfig, VmConfig};
use crate::virtualization::sources::{self, CachedImage, ImportResult};
use crate::virtualization::status::{self, GuestStatus};
use crate::virtualization::{CtPreview, VirtContext, VmPreview, images, store};

/// Server-side keepalive, matching the telemetry socket.
const HEARTBEAT: Duration = Duration::from_secs(30);

/// How much lifecycle output is echoed back to the browser.
const MAX_OUTPUT: usize = 4000;

/// Everything the virtualization page needs for its first paint.
#[derive(Debug, Clone, Serialize)]
pub struct VirtState {
    pub config: VirtualizationConfig,
    pub problems: Vec<Problem>,
    pub config_error: Option<String>,
    pub catalog: Vec<images::GuestImage>,
    pub host: HostCapabilities,
    pub statuses: Vec<GuestStatus>,
}

#[derive(Serialize)]
struct ValidationError {
    error: String,
    problems: Vec<Problem>,
}

#[derive(Serialize)]
pub struct ActionReport {
    pub ok: bool,
    pub output: String,
    pub warnings: Vec<String>,
    pub disk: Option<String>,
    pub seed: Option<String>,
}

#[derive(Deserialize)]
pub struct SnapshotRequest {
    /// `create`, `apply`, `delete` or `list`.
    pub action: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Deserialize)]
pub struct ResizeRequest {
    pub disk_gb: u32,
}

#[derive(Deserialize)]
pub struct PasswordRequest {
    pub password: String,
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

pub async fn state(State(state): State<AppState>) -> Result<Json<VirtState>, ApiError> {
    Ok(Json(collect(&state).await?))
}

/// Persist the document, refusing an invalid one before it can be written.
// ---------------------------------------------------------------------------
// Custom image sources and the download cache
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct FetchRequest {
    pub url: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Deserialize)]
pub struct ImportRequest {
    /// Absolute path to an image already on this host.
    pub path: String,
}

#[derive(Serialize)]
pub struct ImageActionResult {
    pub ok: bool,
    pub message: String,
    pub image: ImportResult,
}

/// Images currently in the cache.
pub async fn image_list(State(state): State<AppState>) -> Result<Json<Vec<CachedImage>>, ApiError> {
    let dir = std::path::PathBuf::from(&state.config.images_dir);
    let images = tokio::task::spawn_blocking(move || sources::list(&dir))
        .await
        .map_err(|err| ApiError::internal(format!("image task failed: {err}")))?;
    Ok(Json(images))
}

/// Download an image from a URL into the cache.
pub async fn image_fetch(
    State(state): State<AppState>,
    Json(request): Json<FetchRequest>,
) -> Result<Json<ImageActionResult>, ApiError> {
    let dir = std::path::PathBuf::from(&state.config.images_dir);
    let runner = state.virt.runner.clone();
    let url = request.url.clone();
    let name = request.name.clone();
    let label = name.clone().unwrap_or_else(|| url.clone());
    let image = state
        .jobs
        .track_with(format!("Downloading {label}"), move |ctx| async move {
            ctx.step(5, format!("fetching {url}"));
            let image = tokio::task::spawn_blocking(move || {
                sources::fetch(&dir, runner.as_ref(), &url, name.as_deref())
            })
            .await
            .map_err(|err| ApiError::internal(format!("image task failed: {err}")))??;
            ctx.log(format!("{} ready", image.name));
            Ok::<_, ApiError>(image)
        })
        .await?;

    state.audit("virt.image_fetch", &request.url, &image.name);
    Ok(Json(ImageActionResult {
        ok: true,
        message: format!("{} downloaded", image.name),
        image,
    }))
}

/// Copy an image that is already on this host into the cache.
pub async fn image_import(
    State(state): State<AppState>,
    Json(request): Json<ImportRequest>,
) -> Result<Json<ImageActionResult>, ApiError> {
    let dir = std::path::PathBuf::from(&state.config.images_dir);
    let runner = state.virt.runner.clone();
    let path = request.path.clone();
    let image = tokio::task::spawn_blocking(move || sources::import(&dir, runner.as_ref(), &path))
        .await
        .map_err(|err| ApiError::internal(format!("image task failed: {err}")))??;

    state.audit("virt.image_import", &request.path, &image.name);
    Ok(Json(ImageActionResult {
        ok: true,
        message: format!("{} imported", image.name),
        image,
    }))
}

/// Delete one cached image.
pub async fn image_delete(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let dir = std::path::PathBuf::from(&state.config.images_dir);
    let target = name.clone();
    tokio::task::spawn_blocking(move || sources::remove(&dir, &target))
        .await
        .map_err(|err| ApiError::internal(format!("image task failed: {err}")))??;

    state.audit("virt.image_delete", &name, "");
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn update_config(
    State(state): State<AppState>,
    Json(config): Json<VirtualizationConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid(problems));
    }
    persist(&state, &config).await?;
    Ok(Json(collect(&state).await?).into_response())
}

pub async fn preview_vm(
    State(state): State<AppState>,
    Json(vm): Json<VmConfig>,
) -> Json<VmPreview> {
    let caps = probe_host(&state).await.unwrap_or_default();
    Json(crate::virtualization::preview_vm(
        &state.config,
        &vm,
        caps.kvm,
    ))
}

pub async fn preview_ct(
    State(state): State<AppState>,
    Json(ct): Json<CtConfig>,
) -> Json<CtPreview> {
    Json(crate::virtualization::preview_ct(&state.config, &ct))
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

pub async fn create_vm(
    State(state): State<AppState>,
    Json(mut vm): Json<VmConfig>,
) -> Result<Response, ApiError> {
    let (doc, _) = load(&state);
    if doc.vm(&vm.id).is_some() || doc.ct(&vm.id).is_some() {
        return Err(ApiError::bad_request(format!(
            "a guest called {:?} already exists",
            vm.id
        )));
    }

    // Allocate a VNC port before validation so the candidate is complete.
    if vm.vnc_enabled && vm.vnc_port.is_none() {
        vm.vnc_port = Some(manager::allocate_vnc_port(
            &doc,
            doc.defaults.vnc_port_start,
        ));
    }

    let mut candidate = doc.clone();
    candidate.vms.push(vm.clone());
    let problems = candidate.validate();
    if !problems.is_empty() {
        return Ok(invalid(problems));
    }

    let cfg = state.config.clone();
    let runner = state.virt.runner.clone();
    let caps = probe_host(&state).await?;
    let _guard = state.virt.lock.lock().await;

    let mut to_create = vm.clone();
    let report = tokio::task::spawn_blocking(move || {
        manager::create_vm(&cfg, &mut to_create, &caps, runner.as_ref())
    })
    .await
    .map_err(|err| ApiError::internal(format!("create task failed: {err}")))??;

    // Only persist once the guest really exists on disk.
    persist(&state, &candidate).await?;
    tracing::info!(guest = %vm.id, "vm created");
    Ok(Json(report_to_body(report)).into_response())
}

pub async fn create_ct(
    State(state): State<AppState>,
    Json(ct): Json<CtConfig>,
) -> Result<Response, ApiError> {
    let (doc, _) = load(&state);
    if doc.vm(&ct.id).is_some() || doc.ct(&ct.id).is_some() {
        return Err(ApiError::bad_request(format!(
            "a guest called {:?} already exists",
            ct.id
        )));
    }

    let mut candidate = doc.clone();
    candidate.cts.push(ct.clone());
    let problems = candidate.validate();
    if !problems.is_empty() {
        return Ok(invalid(problems));
    }

    let cfg = state.config.clone();
    let runner = state.virt.runner.clone();
    let caps = probe_host(&state).await?;
    let _guard = state.virt.lock.lock().await;

    let to_create = ct.clone();
    let report = tokio::task::spawn_blocking(move || {
        manager::create_ct(&cfg, &to_create, &caps, runner.as_ref())
    })
    .await
    .map_err(|err| ApiError::internal(format!("create task failed: {err}")))??;

    persist(&state, &candidate).await?;
    tracing::info!(guest = %ct.id, "container created");
    Ok(Json(report_to_body(report)).into_response())
}

// ---------------------------------------------------------------------------
// VM lifecycle
// ---------------------------------------------------------------------------

pub async fn vm_start(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionReport>, ApiError> {
    let (doc, _) = load(&state);
    let vm = doc.vm(&id).cloned().ok_or_else(|| not_found(&id))?;
    let cfg = state.config.clone();
    let runner = state.virt.runner.clone();
    let caps = probe_host(&state).await?;
    let _guard = state.virt.lock.lock().await;
    let report = blocking(move || manager::start_vm(&cfg, &vm, &caps, runner.as_ref())).await?;
    Ok(Json(report_to_body(report)))
}

pub async fn vm_stop(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionReport>, ApiError> {
    control(&state, &id, VmControl::Stop).await
}

pub async fn vm_shutdown(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionReport>, ApiError> {
    control(&state, &id, VmControl::Shutdown).await
}

pub async fn vm_reboot(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionReport>, ApiError> {
    control(&state, &id, VmControl::Reboot).await
}

pub async fn vm_destroy(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionReport>, ApiError> {
    control(&state, &id, VmControl::Destroy).await
}

async fn control(
    state: &AppState,
    id: &str,
    control: VmControl,
) -> Result<Json<ActionReport>, ApiError> {
    let (doc, _) = load(state);
    let vm = doc.vm(id).cloned().ok_or_else(|| not_found(id))?;
    let cfg = state.config.clone();
    let runner = state.virt.runner.clone();
    let _guard = state.virt.lock.lock().await;
    let report = blocking(move || manager::control_vm(&cfg, &vm, control, runner.as_ref())).await?;
    Ok(Json(report_to_body(report)))
}

pub async fn vm_delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionReport>, ApiError> {
    let (doc, _) = load(&state);
    let vm = doc.vm(&id).cloned().ok_or_else(|| not_found(&id))?;
    let cfg = state.config.clone();
    let runner = state.virt.runner.clone();
    let _guard = state.virt.lock.lock().await;
    let report = blocking(move || manager::delete_vm(&cfg, &vm, runner.as_ref())).await?;

    let mut updated = doc.clone();
    updated.vms.retain(|entry| entry.id != id);
    persist(&state, &updated).await?;
    Ok(Json(report_to_body(report)))
}

pub async fn vm_snapshot(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<SnapshotRequest>,
) -> Result<Json<ActionReport>, ApiError> {
    let (doc, _) = load(&state);
    let vm = doc.vm(&id).cloned().ok_or_else(|| not_found(&id))?;
    let action = match request.action.as_str() {
        "create" => SnapshotAction::Create(required_name(&request.name)?),
        "apply" => SnapshotAction::Apply(required_name(&request.name)?),
        "delete" => SnapshotAction::Delete(required_name(&request.name)?),
        "list" => SnapshotAction::List,
        other => {
            return Err(ApiError::bad_request(format!(
                "unknown snapshot action {other:?}"
            )));
        }
    };

    let cfg = state.config.clone();
    let runner = state.virt.runner.clone();
    let _guard = state.virt.lock.lock().await;
    let report =
        blocking(move || manager::snapshot_vm(&cfg, &vm, &action, runner.as_ref())).await?;
    Ok(Json(report_to_body(report)))
}

pub async fn vm_resize(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ResizeRequest>,
) -> Result<Json<ActionReport>, ApiError> {
    let (doc, _) = load(&state);
    let mut vm = doc.vm(&id).cloned().ok_or_else(|| not_found(&id))?;
    let cfg = state.config.clone();
    let runner = state.virt.runner.clone();
    let disk_gb = request.disk_gb;
    let _guard = state.virt.lock.lock().await;

    let report =
        blocking(move || manager::resize_vm(&cfg, &mut vm, disk_gb, runner.as_ref())).await?;

    let mut updated = doc.clone();
    if let Some(entry) = updated.vms.iter_mut().find(|entry| entry.id == id) {
        entry.disk_gb = disk_gb;
    }
    persist(&state, &updated).await?;
    Ok(Json(report_to_body(report)))
}

// ---------------------------------------------------------------------------
// Container lifecycle
// ---------------------------------------------------------------------------

pub async fn ct_start(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionReport>, ApiError> {
    let (doc, _) = load(&state);
    let ct = doc.ct(&id).cloned().ok_or_else(|| not_found(&id))?;
    let cfg = state.config.clone();
    let runner = state.virt.runner.clone();
    let _guard = state.virt.lock.lock().await;
    let report = blocking(move || manager::start_ct(&cfg, &ct, runner.as_ref())).await?;
    Ok(Json(report_to_body(report)))
}

pub async fn ct_stop(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionReport>, ApiError> {
    stop_ct(&state, &id, false).await
}

pub async fn ct_kill(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionReport>, ApiError> {
    stop_ct(&state, &id, true).await
}

async fn stop_ct(state: &AppState, id: &str, kill: bool) -> Result<Json<ActionReport>, ApiError> {
    let (doc, _) = load(state);
    let ct = doc.ct(id).cloned().ok_or_else(|| not_found(id))?;
    let cfg = state.config.clone();
    let runner = state.virt.runner.clone();
    let _guard = state.virt.lock.lock().await;
    let report = blocking(move || manager::stop_ct(&cfg, &ct, kill, runner.as_ref())).await?;
    Ok(Json(report_to_body(report)))
}

pub async fn ct_delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionReport>, ApiError> {
    let (doc, _) = load(&state);
    let ct = doc.ct(&id).cloned().ok_or_else(|| not_found(&id))?;
    let cfg = state.config.clone();
    let runner = state.virt.runner.clone();
    let _guard = state.virt.lock.lock().await;
    let report = blocking(move || manager::delete_ct(&cfg, &ct, runner.as_ref())).await?;

    let mut updated = doc.clone();
    updated.cts.retain(|entry| entry.id != id);
    persist(&state, &updated).await?;
    Ok(Json(report_to_body(report)))
}

pub async fn ct_password(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<PasswordRequest>,
) -> Result<Json<ActionReport>, ApiError> {
    let (doc, _) = load(&state);
    let mut ct = doc.ct(&id).cloned().ok_or_else(|| not_found(&id))?;
    let new_password = request.password;
    ct.password = new_password.clone();
    let cfg = state.config.clone();
    let runner = state.virt.runner.clone();
    let ct_for_task = ct.clone();
    let _guard = state.virt.lock.lock().await;
    let report = blocking(move || {
        let mut warnings = Vec::new();
        let outcome = manager::set_ct_password(&cfg, &ct_for_task, runner.as_ref(), &mut warnings)?;
        Ok::<Report, ApiError>(Report {
            warnings,
            ..outcome
        })
    })
    .await?;

    let mut updated = doc.clone();
    if let Some(entry) = updated.cts.iter_mut().find(|entry| entry.id == id) {
        entry.password = new_password;
    }
    persist(&state, &updated).await?;
    Ok(Json(report_to_body(report)))
}

// ---------------------------------------------------------------------------
// Console sockets
// ---------------------------------------------------------------------------

/// VM serial console: a raw byte bridge to QEMU's serial unix socket.
pub async fn vm_console_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let (doc, _) = load(&state);
    let Some(vm) = doc.vm(&id) else {
        return ApiError::not_found(format!("no VM called {id}")).into_response();
    };
    if !vm.serial_console {
        return ApiError::bad_request(format!("{id} has no serial console enabled"))
            .into_response();
    }
    if status::probe_vm(&state.config, vm).state != "running" {
        return ApiError::bad_request(format!("{id} is not running")).into_response();
    }
    let path = store::serial_path(&state.config, &id);
    ws.on_upgrade(move |socket| serial_session(socket, path))
}

/// Container console: an interactive shell via `lxc-attach`.
pub async fn ct_console_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let (doc, _) = load(&state);
    let Some(ct) = doc.ct(&id) else {
        return ApiError::not_found(format!("no container called {id}")).into_response();
    };
    if status::probe_ct(&state.config, ct, state.virt.runner.as_ref()).state != "running" {
        return ApiError::bad_request(format!("{id} is not running")).into_response();
    }
    let args = crate::virtualization::lxc::attach_argv(&state.config, &id, &["/bin/sh"]);
    ws.on_upgrade(move |socket| attach_session(socket, args))
}

#[cfg(unix)]
async fn serial_session(socket: WebSocket, path: std::path::PathBuf) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = match tokio::net::UnixStream::connect(&path).await {
        Ok(stream) => stream,
        Err(err) => {
            fail_socket(
                socket,
                &format!(
                    "cannot reach the serial console at {}: {err}",
                    path.display()
                ),
            )
            .await;
            return;
        }
    };
    let (mut unix_read, mut unix_write) = stream.into_split();
    let (mut sink, mut source) = socket.split();

    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await;
    let mut buffer = vec![0u8; 4096];

    loop {
        tokio::select! {
            read = unix_read.read(&mut buffer) => match read {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let chunk = String::from_utf8_lossy(&buffer[..count]).into_owned();
                    if send_frame(&mut sink, &ExecServerMessage::Data { data: chunk }).await.is_err() {
                        break;
                    }
                }
            },
            incoming = source.next() => match incoming {
                None | Some(Err(_)) => break,
                Some(Ok(Message::Text(text))) => {
                    if let Some(bytes) = decode_client(text.as_str())
                        && unix_write.write_all(&bytes).await.is_err()
                    {
                        break;
                    }
                }
                Some(Ok(Message::Binary(bytes))) => {
                    if unix_write.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
                Some(Ok(Message::Ping(payload))) => {
                    if sink.send(Message::Pong(payload)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(_)) => {}
            },
            _ = heartbeat.tick() => {
                if sink.send(Message::Ping(Bytes::new())).await.is_err() {
                    break;
                }
            }
        }
    }

    let _ = sink.close().await;
}

#[cfg(not(unix))]
async fn serial_session(socket: WebSocket, _path: std::path::PathBuf) {
    fail_socket(socket, "serial consoles require a unix host").await;
}

/// Bridge a WebSocket to an `lxc-attach` shell. The child's stdio is piped and
/// copied on blocking threads, because Tokio's process support is not a
/// dependency of this binary.
fn attach_session(
    socket: WebSocket,
    args: Vec<String>,
) -> impl std::future::Future<Output = ()> + Send {
    use std::process::{Command, Stdio};

    let spawned = Command::new("lxc-attach")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    async move {
        let mut child = match spawned {
            Ok(child) => child,
            Err(err) => {
                fail_socket(socket, &format!("cannot start lxc-attach: {err}")).await;
                return;
            }
        };

        let mut stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Reader threads -> an async channel. stdout and stderr are distinct
        // types, so they are unified behind `Read` before spawning.
        let (output_tx, mut output_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let mut readers: Vec<Box<dyn std::io::Read + Send>> = Vec::new();
        if let Some(out) = stdout {
            readers.push(Box::new(out));
        }
        if let Some(err) = stderr {
            readers.push(Box::new(err));
        }
        for pipe in readers {
            let tx = output_tx.clone();
            std::thread::spawn(move || {
                use std::io::Read;
                let mut reader = pipe;
                let mut buffer = [0u8; 4096];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            if tx.send(buffer[..count].to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
        drop(output_tx);

        let (mut sink, mut source) = socket.split();
        let mut heartbeat = tokio::time::interval(HEARTBEAT);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await;

        loop {
            tokio::select! {
                chunk = output_rx.recv() => match chunk {
                    Some(bytes) => {
                        let frame = ExecServerMessage::Data {
                            data: String::from_utf8_lossy(&bytes).into_owned(),
                        };
                        if send_frame(&mut sink, &frame).await.is_err() {
                            break;
                        }
                    }
                    None => {
                        let _ = send_frame(&mut sink, &ExecServerMessage::Exit).await;
                        break;
                    }
                },
                incoming = source.next() => match incoming {
                    None | Some(Err(_)) => break,
                    Some(Ok(Message::Text(text))) => {
                        if let Some(stdin) = stdin.as_mut()
                            && let Some(bytes) = decode_client(text.as_str())
                            && std::io::Write::write_all(stdin, &bytes).is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        if let Some(stdin) = stdin.as_mut()
                            && std::io::Write::write_all(stdin, &bytes).is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        let _ = sink.send(Message::Close(None)).await;
                        break;
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if sink.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                },
                _ = heartbeat.tick() => {
                    if sink.send(Message::Ping(Bytes::new())).await.is_err() {
                        break;
                    }
                }
            }
        }

        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        let _ = sink.close().await;
    }
}

/// Decode a client frame: `{"type":"data","data":…}` or raw bytes as text.
fn decode_client(text: &str) -> Option<Vec<u8>> {
    match serde_json::from_str::<ExecClientMessage>(text) {
        Ok(ExecClientMessage::Data { data }) => Some(data.into_bytes()),
        // Resize has no meaning for a serial line or a pipe shell.
        Ok(ExecClientMessage::Resize { .. }) => None,
        // A bare keystroke sent as plain text is accepted too.
        Err(_) => Some(text.as_bytes().to_vec()),
    }
}

async fn send_frame(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: &ExecServerMessage,
) -> Result<(), axum::Error> {
    let text = serde_json::to_string(message).unwrap_or_else(|_| "{\"type\":\"exit\"}".to_string());
    sink.send(Message::Text(text.into())).await
}

async fn fail_socket(socket: WebSocket, message: &str) {
    let (mut sink, _source) = socket.split();
    let payload = serde_json::to_string(&ExecServerMessage::Data {
        data: format!("{message}\r\n"),
    })
    .unwrap_or_else(|_| message.to_string());
    let _ = sink.send(Message::Text(payload.into())).await;
    let _ = sink.send(Message::Close(None)).await;
    let _ = sink.close().await;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load(state: &AppState) -> (VirtualizationConfig, Option<String>) {
    store::load(&state.virt.config_path)
}

async fn collect(state: &AppState) -> Result<VirtState, ApiError> {
    let ctx: VirtContext = state.virt.clone();
    let cfg = state.config.clone();
    let runner = ctx.runner.clone();

    tokio::task::spawn_blocking(move || {
        let (config, config_error) = store::load(&ctx.config_path);
        let problems = config.validate();
        let catalog = images::catalog_with_state(&cfg);
        let host = host::probe(runner.as_ref());
        let statuses = status::probe_all(&cfg, &config, runner.as_ref());
        VirtState {
            config,
            problems,
            config_error,
            catalog,
            host,
            statuses,
        }
    })
    .await
    .map_err(|err| ApiError::internal(format!("virtualization probe failed: {err}")))
}

async fn probe_host(state: &AppState) -> Result<HostCapabilities, ApiError> {
    let runner = state.virt.runner.clone();
    tokio::task::spawn_blocking(move || host::probe(runner.as_ref()))
        .await
        .map_err(|err| ApiError::internal(format!("host probe failed: {err}")))
}

async fn persist(state: &AppState, config: &VirtualizationConfig) -> Result<(), ApiError> {
    let path = state.virt.config_path.clone();
    let config = config.clone();
    tokio::task::spawn_blocking(move || store::save(&path, &config))
        .await
        .map_err(|err| ApiError::internal(format!("config save task failed: {err}")))?
}

/// Run a blocking lifecycle action and surface its `ApiError`.
async fn blocking<F>(f: F) -> Result<Report, ApiError>
where
    F: FnOnce() -> Result<Report, ApiError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|err| ApiError::internal(format!("lifecycle task failed: {err}")))?
}

fn report_to_body(report: Report) -> ActionReport {
    ActionReport {
        ok: true,
        output: trim(&report.output),
        warnings: report.warnings,
        disk: report.disk,
        seed: report.seed,
    }
}

fn required_name(name: &Option<String>) -> Result<String, ApiError> {
    name.clone()
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request("a snapshot name is required for this action"))
}

fn not_found(id: &str) -> ApiError {
    ApiError::not_found(format!("no guest called {id}"))
}

fn invalid(problems: Vec<Problem>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ValidationError {
            error: "configuration is not valid".to_string(),
            problems,
        }),
    )
        .into_response()
}

fn trim(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.len() <= MAX_OUTPUT {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - MAX_OUTPUT;
    while start < trimmed.len() && !trimmed.is_char_boundary(start) {
        start += 1;
    }
    trimmed[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_frames_decode_both_shapes() {
        assert_eq!(
            decode_client(r#"{"type":"data","data":"ls\n"}"#),
            Some(b"ls\n".to_vec())
        );
        // A bare text frame is treated as keystrokes.
        assert_eq!(decode_client("x"), Some(b"x".to_vec()));
        // Resize is ignored.
        assert_eq!(
            decode_client(r#"{"type":"resize","cols":80,"rows":24}"#),
            None
        );
    }

    #[test]
    fn trim_bounds_output_on_char_boundaries() {
        let long = "é".repeat(MAX_OUTPUT);
        let trimmed = trim(&long);
        assert!(trimmed.len() <= MAX_OUTPUT);
        assert!(trimmed.is_char_boundary(trimmed.len()));
    }

    #[test]
    fn snapshot_actions_need_a_name() {
        assert!(required_name(&None).is_err());
        assert!(required_name(&Some("  ".to_string())).is_err());
        assert_eq!(required_name(&Some("pre".to_string())).expect("ok"), "pre");
    }
}
