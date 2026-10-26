//! HTTP surface for the background job registry.
//!
//! `GET /api/jobs` is intentionally a plain poll: the registry is in memory and
//! the read is one short lock, so the activity drawer can refresh every couple
//! of seconds without a socket or any load on the rest of the API.

use axum::Json;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;

use crate::error::ApiError;
use crate::jobs::Job;
use crate::state::AppState;

#[derive(Serialize)]
pub struct JobsState {
    /// Newest first; running jobs are interleaved with recent history.
    pub jobs: Vec<Job>,
}

#[derive(Serialize)]
pub struct OkResult {
    pub ok: bool,
}

pub async fn list(State(state): State<AppState>) -> Json<JobsState> {
    Json(JobsState {
        jobs: state.jobs.list(),
    })
}

/// Clear a finished job from the history. A job still running cannot be removed.
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<OkResult>, ApiError> {
    if state.jobs.get(&id).is_none() {
        return Err(ApiError::not_found(format!("no job {id}")));
    }
    state
        .jobs
        .remove(&id)
        .map_err(|_| ApiError::bad_request("a running job cannot be removed"))?;
    Ok(Json(OkResult { ok: true }))
}

/// Live job updates for the activity drawer: an initial snapshot, then one
/// frame per change.
pub async fn ws(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| session(socket, state))
}

async fn session(socket: WebSocket, state: AppState) {
    let mut updates = state.jobs.subscribe();
    let (mut sink, mut source) = socket.split();

    let snapshot = serde_json::json!({ "type": "snapshot", "jobs": state.jobs.list() });
    if sink
        .send(Message::Text(Utf8Bytes::from(snapshot.to_string())))
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            update = updates.recv() => match update {
                Ok(job) => {
                    let frame = serde_json::json!({ "type": "job", "job": job }).to_string();
                    if sink.send(Message::Text(Utf8Bytes::from(frame))).await.is_err() {
                        break;
                    }
                }
                // The client fell behind; ask it to resynchronise.
                Err(RecvError::Lagged(_)) => {
                    let frame = serde_json::json!({ "type": "resync" }).to_string();
                    if sink.send(Message::Text(Utf8Bytes::from(frame))).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
            incoming = source.next() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(Message::Ping(payload))) => {
                    let _ = sink.send(Message::Pong(payload)).await;
                }
                Some(Ok(_)) => {}
            },
        }
    }

    let _ = sink.close().await;
}
