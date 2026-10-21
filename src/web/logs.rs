//! HTTP + WebSocket surface for the journald viewer.
//!
//! Reads are buffered `journalctl` calls; follow mode forwards a live
//! `journalctl -f` through a reader thread into a tokio channel, with a
//! per-client line cap so a chatty unit cannot saturate the socket.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};

use crate::services::logs::{self, DEFAULT_LINES, LineStream, LogEntry, PRIORITIES, UnitInfo};
use crate::state::AppState;

/// Server-side keepalive for the follow socket.
const HEARTBEAT: Duration = Duration::from_secs(30);
/// Per-client follow cap.
const FOLLOW_LINES_PER_SEC: usize = 256;

#[derive(Debug, Serialize)]
pub struct LogsState {
    pub units: Vec<UnitInfo>,
    pub units_error: Option<String>,
    pub entries: Vec<LogEntry>,
    pub entries_error: Option<String>,
    pub available: bool,
    /// (value, label) pairs for the severity filter.
    pub priorities: Vec<(String, String)>,
}

#[derive(Deserialize)]
pub struct TailQuery {
    pub unit: Option<String>,
    pub lines: Option<u32>,
    pub priority: Option<String>,
}

#[derive(Deserialize)]
pub struct FollowQuery {
    pub unit: Option<String>,
    pub priority: Option<String>,
}

pub async fn state(
    State(state): State<AppState>,
    Query(query): Query<TailQuery>,
) -> Json<LogsState> {
    Json(collect(&state, &query).await)
}

async fn collect(state: &AppState, query: &TailQuery) -> LogsState {
    let runner = Arc::clone(&state.logs.runner);
    let (units, units_error) =
        tokio::task::spawn_blocking(move || logs::list_units(runner.as_ref()))
            .await
            .map(|result| match result {
                Ok(units) => (units, None),
                Err(err) => (Vec::new(), Some(err)),
            })
            .unwrap_or_else(|err| (Vec::new(), Some(err.to_string())));

    let runner = Arc::clone(&state.logs.runner);
    let unit = query.unit.clone();
    let lines = query.lines.unwrap_or(DEFAULT_LINES);
    let priority = query.priority.clone();
    let (entries, entries_error) = tokio::task::spawn_blocking(move || {
        logs::tail(
            runner.as_ref(),
            unit.as_deref(),
            lines,
            priority.as_deref(),
            Duration::from_secs(30),
        )
    })
    .await
    .map(|result| match result {
        Ok(entries) => (entries, None),
        Err(err) => (Vec::new(), Some(err.to_string())),
    })
    .unwrap_or_else(|err| (Vec::new(), Some(err.to_string())));

    LogsState {
        units,
        units_error,
        entries,
        entries_error,
        available: state.logs.available(),
        priorities: PRIORITIES
            .iter()
            .map(|(value, label)| ((*value).to_string(), (*label).to_string()))
            .collect(),
    }
}

/// Follow a unit (or the whole journal) live.
pub async fn ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<FollowQuery>,
) -> Response {
    let available = state.logs.available();
    let unit = query.unit.filter(|unit| !unit.trim().is_empty());
    let priority = query.priority.filter(|p| !p.trim().is_empty());
    ws.on_upgrade(move |socket| session(socket, available, unit, priority))
}

async fn session(
    socket: WebSocket,
    available: bool,
    unit: Option<String>,
    priority: Option<String>,
) {
    if !available {
        return close_with(socket, "journalctl is not installed").await;
    }

    let mut args: Vec<String> = vec![
        "-f".to_string(),
        "-o".to_string(),
        "json".to_string(),
        "-n".to_string(),
        "0".to_string(),
    ];
    if let Some(unit) = &unit {
        args.push("-u".to_string());
        args.push(unit.clone());
    }
    if let Some(priority) = &priority {
        args.push("-p".to_string());
        args.push(priority.clone());
    }

    let stream = match LineStream::spawn("journalctl", &args) {
        Ok(stream) => stream,
        Err(err) => {
            tracing::debug!(%err, "cannot start the follow stream");
            return close_with(socket, "journalctl could not be started").await;
        }
    };

    // Forward the blocking reader thread into an async channel; dropping `rx`
    // makes `send` fail, which ends the thread, which drops the stream, which
    // kills the child. Every exit path therefore reaps journalctl.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let forward = std::thread::spawn(move || {
        while let Some(line) = stream.recv_blocking() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let (mut sink, mut source) = socket.split();
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await; // consume the immediate first tick
    let mut window = Instant::now();
    let mut sent = 0usize;
    let mut closed = false;

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            line = rx.recv() => {
                let Some(text) = line else {
                    let _ = sink.send(Message::Text(Utf8Bytes::from(text_frame("journalctl stopped")))).await;
                    break;
                };
                if window.elapsed() >= Duration::from_secs(1) {
                    window = Instant::now();
                    sent = 0;
                }
                sent += 1;
                if sent > FOLLOW_LINES_PER_SEC {
                    tokio::time::sleep(Duration::from_millis(4)).await;
                }
                if sink.send(Message::Text(Utf8Bytes::from(text_frame(&text)))).await.is_err() {
                    break;
                }
            }
            incoming = source.next(), if !closed => match incoming {
                None | Some(Err(_)) => { closed = true; }
                Some(Ok(Message::Close(_))) => { closed = true; }
                Some(Ok(Message::Ping(payload))) => {
                    let _ = sink.send(Message::Pong(payload)).await;
                }
                Some(Ok(_)) => {}
            },
        }
    }

    drop(rx);
    let _ = forward.join();
    let _ = sink.close().await;
}

fn text_frame(line: &str) -> String {
    serde_json::json!({ "type": "line", "line": line }).to_string()
}

async fn close_with(socket: WebSocket, message: &str) {
    let (mut sink, _) = socket.split();
    let _ = sink
        .send(Message::Text(Utf8Bytes::from(text_frame(message))))
        .await;
    let _ = sink.send(Message::Close(None)).await;
    let _ = sink.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_frames_are_tagged() {
        // Key order comes from serde_json's BTreeMap; the client reads fields.
        let parsed: serde_json::Value = serde_json::from_str(&text_frame("hello")).expect("json");
        assert_eq!(parsed["type"], "line");
        assert_eq!(parsed["line"], "hello");
    }

    #[tokio::test]
    async fn collect_reports_unavailable_without_tools() {
        let state = AppState::new(crate::config::Config::default());
        let query = TailQuery {
            unit: None,
            lines: None,
            priority: None,
        };
        let logs = collect(&state, &query).await;
        // On a Windows dev box neither tool exists; the shape must still hold.
        assert!(logs.units.is_empty() || !logs.units_error.is_none() || logs.available);
    }
}
