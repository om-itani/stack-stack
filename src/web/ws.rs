//! Telemetry transport: one sampling task, one broadcast channel, N sockets.
//!
//! The sampler ticks on a fixed interval and pushes a JSON snapshot into a
//! `broadcast` channel. Socket handlers only *subscribe*, so a stalled browser
//! can never slow the sampler down: it falls behind, gets a `Lagged`, and picks
//! up on the next frame.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;

use crate::config::Config;
use crate::state::AppState;
use crate::system::metrics::{Sampler, Snapshot};

/// Server-side keepalive. Browsers answer pings automatically, so this also
/// flushes dead connections whose TCP state the OS has not noticed yet.
const HEARTBEAT: Duration = Duration::from_secs(30);

/// Start the sampling loop. Never returns; the task lives as long as the process.
pub fn spawn_sampler(state: AppState, cfg: Config) {
    let telemetry = state.telemetry.clone();
    let latest = state.latest.clone();

    tokio::spawn(async move {
        let mut sampler = Sampler::new();
        let mut ticker =
            tokio::time::interval(Duration::from_millis(cfg.sample_interval_ms.max(100)));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;

            // `/proc` reads are cheap and smartctl is cached, but the whole
            // collection is still a blocking fork+parse: keep it off the
            // async worker threads.
            let mut taken = std::mem::take(&mut sampler);
            let cfg = cfg.clone();
            let collected = tokio::task::spawn_blocking(move || {
                let snapshot = taken.sample(&cfg);
                (taken, Arc::new(snapshot))
            })
            .await;

            match collected {
                Ok((restored, snapshot)) => {
                    sampler = restored;
                    // `latest` feeds the REST endpoint; `telemetry` feeds the sockets.
                    latest.send_replace(Some(snapshot.clone()));
                    // Err just means nobody is connected right now.
                    let _ = telemetry.send(snapshot);
                }
                // The runtime is going away, so there is nothing left to serve.
                Err(err) if err.is_cancelled() => {
                    tracing::debug!("sampler task cancelled, stopping telemetry loop");
                    break;
                }
                // A panic in one collection must not permanently kill telemetry
                // (the process is only abort-on-panic in release; in debug this
                // would otherwise silently stop every future frame). `sampler`
                // was already taken, so a fresh one starts cleanly next tick.
                Err(err) => {
                    tracing::error!(%err, "sampler panicked, restarting with a fresh sampler");
                    sampler = Sampler::new();
                }
            }
        }
    });
}

pub async fn telemetry_ws(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    let stream = state.telemetry.subscribe();
    let initial = state.latest_snapshot();
    ws.on_upgrade(move |socket| handler(socket, stream, initial))
}

async fn handler(
    socket: WebSocket,
    mut stream: broadcast::Receiver<Arc<Snapshot>>,
    initial: Option<Arc<Snapshot>>,
) {
    // Splitting gives us independent send/receive halves, which is what lets the
    // loop below both push snapshots and answer an inbound Close in one
    // `select!` without aliasing the socket.
    let (mut sink, mut source) = socket.split();

    // Paint the UI immediately instead of waiting up to a full sample interval.
    if let Some(snapshot) = initial
        && send_snapshot(&mut sink, &snapshot).await.is_err()
    {
        return;
    }

    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            received = stream.recv() => match received {
                Ok(snapshot) => {
                    if send_snapshot(&mut sink, &snapshot).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::debug!(missed, "telemetry subscriber lagged, resynchronising");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = heartbeat.tick() => {
                if sink.send(Message::Ping(Bytes::new())).await.is_err() {
                    break;
                }
            },
            incoming = source.next() => match incoming {
                // Stream ended or transport failed: the peer is gone.
                None | Some(Err(_)) => break,
                // Complete the closing handshake so a polite client (and our own
                // smoke test) is not left waiting for a reply that never comes.
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
    tracing::debug!("telemetry socket closed");
}

async fn send_snapshot(
    sink: &mut SplitSink<WebSocket, Message>,
    snapshot: &Snapshot,
) -> Result<(), axum::Error> {
    // A snapshot that cannot serialise is a bug, not a transport error: send an
    // empty frame rather than killing the client's stream, but never silently —
    // a masked serialisation failure would make the UI degrade with no trace.
    let payload = match serde_json::to_string(snapshot) {
        Ok(payload) => payload,
        Err(err) => {
            tracing::warn!(%err, "failed to serialize snapshot; sending empty frame");
            "{}".to_string()
        }
    };
    sink.send(Message::Text(payload.into())).await
}
