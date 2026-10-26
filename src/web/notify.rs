//! HTTP + WebSocket surface for notifications.
//!
//! One round trip (`GET /api/notifications`) hands the header bell both the
//! current feed and the delivery config. The WebSocket at
//! `/api/notifications/ws` pushes new frames as they happen.

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, extract::ws::Utf8Bytes};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::notify::{Notification, NotifyConfig, RECENT_CAP, load_config};
use crate::state::AppState;

/// Server-side keepalive, matching the other sockets.
const HEARTBEAT: Duration = Duration::from_secs(30);

#[derive(Serialize)]
pub struct NotifyState {
    /// Newest first.
    pub items: Vec<Notification>,
    pub config: NotifyConfig,
    /// Set when `notify.json` exists but could not be parsed.
    pub config_error: Option<String>,
}

#[derive(Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub detail: String,
}

#[derive(Serialize)]
pub struct OkResult {
    pub ok: bool,
}

#[derive(Deserialize)]
pub struct FeedQuery {
    pub limit: Option<u32>,
}

pub async fn feed(
    State(state): State<AppState>,
    Query(query): Query<FeedQuery>,
) -> Json<NotifyState> {
    let limit = query
        .limit
        .map(|value| value.clamp(1, RECENT_CAP as u32) as usize)
        .unwrap_or(RECENT_CAP);
    let (_, config_error) = load_config(state.notify.config_path());
    Json(NotifyState {
        items: state.notify.recent(limit),
        config: state.notify.config(),
        config_error,
    })
}

pub async fn clear(State(state): State<AppState>) -> Json<OkResult> {
    state.notify.clear();
    Json(OkResult { ok: true })
}

pub async fn update_config(
    State(state): State<AppState>,
    Json(config): Json<NotifyConfig>,
) -> Result<Response, ApiError> {
    let problems = config.validate();
    if !problems.is_empty() {
        return Ok(invalid(problems));
    }
    state.notify.set_config(config)?;

    let (_, config_error) = load_config(state.notify.config_path());
    Ok(Json(NotifyState {
        items: state.notify.recent(RECENT_CAP),
        config: state.notify.config(),
        config_error,
    })
    .into_response())
}

pub async fn test(State(state): State<AppState>) -> Result<Json<TestResult>, ApiError> {
    let center = state.notify.clone();
    let detail = tokio::task::spawn_blocking(move || center.test_webhook())
        .await
        .map_err(|err| ApiError::internal(format!("webhook task failed: {err}")))??;
    Ok(Json(TestResult { ok: true, detail }))
}

pub async fn ws(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    let stream = state.notify.subscribe();
    ws.on_upgrade(move |socket| handler(socket, stream))
}

async fn handler(socket: WebSocket, mut stream: tokio::sync::broadcast::Receiver<Notification>) {
    let (mut sink, mut source) = socket.split();
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            received = stream.recv() => match received {
                Ok(notification) => {
                    let payload = serde_json::to_string(&notification)
                        .unwrap_or_else(|_| "{}".to_string());
                    if sink.send(Message::Text(Utf8Bytes::from(payload))).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
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

fn invalid(problems: Vec<crate::networking::model::Problem>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": "BadRequest",
            "message": "notification configuration is not valid",
            "problems": problems,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::NotificationRequest;

    fn test_state(name: &str) -> AppState {
        let dir = std::env::temp_dir().join(format!("ss-notify-web-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        AppState::new(crate::config::Config {
            notify_path: dir.join("notify.json").to_string_lossy().into_owned(),
            ..crate::config::Config::default()
        })
    }

    #[tokio::test]
    async fn feed_reports_config_and_items() {
        let state = test_state("feed");
        state
            .notify
            .emit(NotificationRequest::info("test", "hi", "there"));

        let Json(feed) = feed(State(state), Query(FeedQuery { limit: None })).await;
        assert_eq!(feed.items.len(), 1);
        assert_eq!(feed.items[0].title, "hi");
        assert!(feed.config_error.is_none());
    }

    #[tokio::test]
    async fn clearing_empties_the_feed() {
        let state = test_state("clear");
        state
            .notify
            .emit(NotificationRequest::info("test", "hi", "there"));
        assert_eq!(state.notify.recent(10).len(), 1);
        let Json(result) = clear(State(state.clone())).await;
        assert!(result.ok);
        assert!(state.notify.recent(10).is_empty());
    }

    #[tokio::test]
    async fn invalid_config_is_rejected_with_problems() {
        let state = test_state("invalid");
        let mut config = state.notify.config();
        config.webhook_url = Some("ftp://nope".to_string());
        let response = update_config(State(state), Json(config))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
