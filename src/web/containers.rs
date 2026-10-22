//! HTTP + WebSocket surface for Docker containers.
//!
//! Handlers stay thin: validation and daemon calls live in `containers::*`.
//! Every upgrade path establishes its upstream *before* accepting the socket, so
//! a client never ends up with a half-open session it cannot diagnose.

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::{Json, extract::ws::Utf8Bytes};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::containers::docker::{ContainerAction, ExecSession};
use crate::containers::model::{Container, ExecClientMessage, ExecServerMessage, StackInfo};
use crate::containers::{host, store};
use crate::error::ApiError;
use crate::state::AppState;

/// How much compose output is echoed back to the browser.
const MAX_OUTPUT: usize = 4000;

/// Server-side keepalive, matching the telemetry socket.
const HEARTBEAT: Duration = Duration::from_secs(30);

/// Default number of historical log lines and the hard cap a client may ask for.
const DEFAULT_LOG_TAIL: u32 = 200;
const MAX_LOG_TAIL: u32 = 5000;

#[derive(Serialize)]
pub struct ActionResult {
    pub ok: bool,
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<Container>>, ApiError> {
    let installed = store::installed_ids(&state.config);
    let apps = store::catalog(&state.config);
    let host_ip = host::detect().ip;
    let containers = state.docker.list(&host_ip, &installed, &apps).await?;
    Ok(Json(containers))
}

pub async fn start(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionResult>, ApiError> {
    state.docker.action(&id, ContainerAction::Start).await?;
    Ok(Json(ActionResult { ok: true }))
}

pub async fn stop(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionResult>, ApiError> {
    state.docker.action(&id, ContainerAction::Stop).await?;
    state.audit("container.stop", &id, "");
    Ok(Json(ActionResult { ok: true }))
}

pub async fn restart(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionResult>, ApiError> {
    state.docker.action(&id, ContainerAction::Restart).await?;
    Ok(Json(ActionResult { ok: true }))
}

pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionResult>, ApiError> {
    state.docker.remove(&id).await?;
    state.audit("container.remove", &id, "");
    Ok(Json(ActionResult { ok: true }))
}

// ---------------------------------------------------------------------------
// Operator-authored stacks
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct StackResult {
    pub ok: bool,
    /// Trimmed compose output, useful when an image pull takes a while.
    pub output: String,
}

#[derive(Deserialize)]
pub struct CreateStackRequest {
    pub name: String,
    pub compose: String,
}

/// Every stack on disk with its compose text. Container status is derived on the
/// client from the containers list (each container carries its `stack`).
pub async fn list_stacks(State(state): State<AppState>) -> Result<Json<Vec<StackInfo>>, ApiError> {
    let config = state.config.clone();
    let stacks = tokio::task::spawn_blocking(move || store::stack_infos(&config))
        .await
        .map_err(|err| ApiError::internal(format!("stack task failed: {err}")))?;
    Ok(Json(stacks))
}

pub async fn create_stack(
    State(state): State<AppState>,
    Json(request): Json<CreateStackRequest>,
) -> Result<Json<StackResult>, ApiError> {
    let output = store::deploy_stack(&state.config, &request.name, &request.compose).await?;
    state.audit("stack.deploy", &request.name, "compose up -d");
    Ok(Json(StackResult {
        ok: true,
        output: trim(&output),
    }))
}

#[derive(Deserialize)]
pub struct RemoveStackQuery {
    /// Remove the stack directory even if `docker compose down` failed.
    pub force: Option<bool>,
}

pub async fn delete_stack(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<RemoveStackQuery>,
) -> Result<Json<StackResult>, ApiError> {
    let force = query.force.unwrap_or(false);
    let output = store::remove_stack(&state.config, &name, force).await?;
    state.audit(
        "stack.remove",
        &name,
        if force {
            "force (down skipped)"
        } else {
            "compose down"
        },
    );
    Ok(Json(StackResult {
        ok: true,
        output: trim(&output),
    }))
}

/// Bound the compose output echoed to the browser, cutting on a char boundary.
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

// ---------------------------------------------------------------------------
// Logs socket
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LogsQuery {
    pub tail: Option<String>,
}

pub async fn logs_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Response {
    if let Err(err) = store::sanitize_container_id(&id) {
        return err.into_response();
    }
    let tail = normalize_tail(query.tail.as_deref());
    ws.on_upgrade(move |socket| logs_session(socket, state, id, tail))
}

async fn logs_session(socket: WebSocket, state: AppState, id: String, tail: String) {
    let mut stream = match state.docker.logs(&id, &tail).await {
        Ok(stream) => stream,
        Err(err) => {
            fail_socket(socket, &err.to_string()).await;
            return;
        }
    };

    let (mut sink, mut source) = socket.split();
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            next = stream.next() => match next {
                Some(Ok(chunk)) => {
                    if send_text(&mut sink, crate::containers::docker::log_text(chunk)).await.is_err() {
                        break;
                    }
                }
                Some(Err(err)) => {
                    // The log stream is lazy, so a dead daemon only shows up
                    // here. Surface it instead of closing a blank pane.
                    let api = ApiError::from(err);
                    tracing::debug!(%api, "log stream ended with an error");
                    let _ = send_text(&mut sink, format!("{api}\n")).await;
                    break;
                }
                None => break,
            },
            _ = heartbeat.tick() => {
                if sink.send(Message::Ping(Bytes::new())).await.is_err() {
                    break;
                }
            },
            incoming = source.next() => match incoming {
                None | Some(Err(_)) => break,
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
        }
    }

    let _ = sink.close().await;
}

// ---------------------------------------------------------------------------
// Exec socket
// ---------------------------------------------------------------------------

pub async fn exec_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    if let Err(err) = store::sanitize_container_id(&id) {
        return err.into_response();
    }
    ws.on_upgrade(move |socket| exec_session(socket, state, id))
}

async fn exec_session(socket: WebSocket, state: AppState, id: String) {
    let session = match state.docker.exec_attach(&id).await {
        Ok(session) => session,
        Err(err) => {
            fail_socket(socket, &err.to_string()).await;
            return;
        }
    };

    let ExecSession {
        exec_id,
        mut output,
        mut input,
    } = session;
    let resize_client = state.docker.clone();

    let (mut sink, mut source) = socket.split();
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await;

    loop {
        tokio::select! {
            next = output.next() => match next {
                Some(Ok(chunk)) => {
                    let frame = frame(&ExecServerMessage::Data {
                        data: crate::containers::docker::log_text(chunk),
                    });
                    if send_text(&mut sink, frame).await.is_err() {
                        break;
                    }
                }
                Some(Err(err)) => {
                    let api = ApiError::from(err);
                    tracing::debug!(%api, "exec output ended with an error");
                    let _ = send_text(
                        &mut sink,
                        frame(&ExecServerMessage::Data {
                            data: format!("{api}\r\n"),
                        }),
                    )
                    .await;
                    break;
                }
                None => {
                    let _ = send_text(&mut sink, frame(&ExecServerMessage::Exit)).await;
                    break;
                }
            },
            incoming = source.next() => match incoming {
                None | Some(Err(_)) => break,
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ExecClientMessage>(text.as_str()) {
                        Ok(ExecClientMessage::Data { data }) => {
                            if input.write_all(data.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                        Ok(ExecClientMessage::Resize { cols, rows }) => {
                            if let Err(err) = resize_client.resize(&exec_id, cols, rows).await {
                                tracing::debug!(%err, "exec resize failed");
                            }
                        }
                        // An unknown frame type is a client bug, not a reason to
                        // drop a working terminal.
                        Err(_) => {}
                    }
                }
                Some(Ok(Message::Binary(bytes))) => {
                    if input.write_all(&bytes).await.is_err() {
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
            },
        }
    }

    // Dropping stdin lets the shell see EOF and exit; then close the socket.
    let _ = input.shutdown().await;
    let _ = sink.close().await;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn send_text(
    sink: &mut SplitSink<WebSocket, Message>,
    text: String,
) -> Result<(), axum::Error> {
    sink.send(Message::Text(Utf8Bytes::from(text))).await
}

fn frame(message: &ExecServerMessage) -> String {
    serde_json::to_string(message).unwrap_or_else(|_| "{\"type\":\"exit\"}".to_string())
}

/// Tell the client why the session never started, then close politely.
async fn fail_socket(socket: WebSocket, message: &str) {
    let (mut sink, _source) = socket.split();
    let payload = serde_json::to_string(&ExecServerMessage::Data {
        data: format!("{message}\r\n"),
    })
    .unwrap_or_else(|_| message.to_string());
    let _ = sink.send(Message::Text(Utf8Bytes::from(payload))).await;
    let _ = sink.send(Message::Close(None)).await;
    let _ = sink.close().await;
}

/// Clamp a client-supplied `tail` to a sane range; anything unparseable falls
/// back to the default rather than being passed through to the daemon.
fn normalize_tail(raw: Option<&str>) -> String {
    match raw {
        Some("all") => "all".to_string(),
        Some(value) if !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()) => value
            .parse::<u32>()
            .map(|lines| lines.min(MAX_LOG_TAIL))
            .unwrap_or(DEFAULT_LOG_TAIL)
            .to_string(),
        _ => DEFAULT_LOG_TAIL.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_defaults_and_clamps() {
        assert_eq!(normalize_tail(None), "200");
        assert_eq!(normalize_tail(Some("all")), "all");
        assert_eq!(normalize_tail(Some("50")), "50");
        assert_eq!(normalize_tail(Some("999999")), "5000");
        assert_eq!(normalize_tail(Some("100;rm -rf /")), "200");
        assert_eq!(normalize_tail(Some("")), "200");
    }

    #[test]
    fn exec_frames_are_tagged_json() {
        assert_eq!(frame(&ExecServerMessage::Exit), r#"{"type":"exit"}"#);
        assert_eq!(
            frame(&ExecServerMessage::Data {
                data: "hi".to_string()
            }),
            r#"{"type":"data","data":"hi"}"#
        );
    }
}
