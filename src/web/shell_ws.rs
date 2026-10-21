//! Host shell WebSocket bridge.
//!
//! Mirrors the container exec bridge frame for frame, so the same xterm modal
//! drives both: the client sends `{"type":"data",...}` and
//! `{"type":"resize",...}`, the server replies with `{"type":"data",...}` and a
//! final `{"type":"exit"}`.
//!
//! The PTY is blocking by nature, so reads run on a dedicated blocking task that
//! feeds an unbounded channel; the async side fans frames out with a per-client
//! rate cap. When the socket closes the shell is hung up, and when the shell
//! exits the socket is closed — both directions end cleanly. The endpoint sits
//! under `/api/*`, so the login gate covers it like everything else.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};

use crate::containers::model::{ExecClientMessage, ExecServerMessage};
use crate::error::ApiError;
use crate::services::pty::{HostShell, utf8_split};
use crate::state::AppState;

/// Per-client output cap, matching the container exec bridge.
const OUTPUT_CAP_BYTES_PER_SEC: usize = 64 * 1024;
/// Read chunk size from the master.
const READ_CHUNK: usize = 8 * 1024;

pub async fn shell_ws(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    let hostname = state
        .latest_snapshot()
        .map(|snapshot| snapshot.hostname.clone())
        .unwrap_or_default();
    ws.on_upgrade(move |socket| session(socket, hostname))
}

async fn session(socket: WebSocket, hostname: String) {
    // The fork must happen off the async runtime; it is quick.
    let spawned = tokio::task::spawn_blocking(|| HostShell::spawn(24, 80))
        .await
        .map_err(|err| ApiError::internal(format!("shell task failed: {err}")))
        .and_then(|result| result.map_err(|err| ApiError::internal(err.to_string())));

    let shell = match spawned {
        Ok(shell) => Arc::new(shell),
        Err(err) => {
            return fail(socket, &err.to_string()).await;
        }
    };
    tracing::info!(pid = shell.pid(), "host shell opened");

    let (mut sink, mut source) = socket.split();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

    // Banner so the operator knows which box this shell belongs to.
    let banner = format!(
        "stack-stack host shell{}\r\n",
        if hostname.is_empty() {
            String::new()
        } else {
            format!(" on {hostname}")
        }
    );
    let _ = tx.send(banner.into_bytes());

    // Blocking reader: master -> channel.
    let read_shell = Arc::clone(&shell);
    let reader = tokio::task::spawn_blocking(move || {
        let mut carry: Vec<u8> = Vec::new();
        loop {
            let mut buffer = vec![0u8; READ_CHUNK];
            match read_shell.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    buffer.truncate(read);
                    if carry.is_empty() {
                        let (valid, rest) = utf8_split(&buffer);
                        if !valid.is_empty() && tx.send(valid.to_vec()).is_err() {
                            break;
                        }
                        carry = rest.to_vec();
                    } else {
                        carry.extend_from_slice(&buffer);
                        let (valid, rest) = utf8_split(&carry);
                        if !valid.is_empty() && tx.send(valid.to_vec()).is_err() {
                            break;
                        }
                        carry = rest.to_vec();
                    }
                }
                // EIO is how Linux says "the slave hung up".
                Err(err)
                    if err.kind() == std::io::ErrorKind::Interrupted
                        || err.raw_os_error() == Some(libc::EIO)
                        || err.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(err) => {
                    tracing::debug!(%err, "host shell read failed");
                    break;
                }
            }
        }
        // Reap so the child cannot linger as a zombie.
        let code = read_shell.reap();
        tracing::debug!(code, "host shell exited");
    });

    let mut rate_window = Instant::now();
    let mut sent_in_window = 0usize;
    // Set once the operator's socket goes away; the source must then stop being
    // polled (it would return `None` forever) while the exit frame is delivered.
    let mut closed = false;

    loop {
        tokio::select! {
            chunk = rx.recv() => {
                let Some(chunk) = chunk else {
                    let _ = sink.send(frame(&ExecServerMessage::Exit)).await;
                    break;
                };
                // Rate cap: a runaway `cat` must not saturate the socket.
                if rate_window.elapsed() >= Duration::from_secs(1) {
                    rate_window = Instant::now();
                    sent_in_window = 0;
                }
                sent_in_window += chunk.len();
                if sent_in_window > OUTPUT_CAP_BYTES_PER_SEC {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                let text = String::from_utf8_lossy(&chunk).into_owned();
                if sink.send(frame(&ExecServerMessage::Data { data: text })).await.is_err() {
                    shell.terminate();
                    closed = true;
                }
            }
            incoming = source.next(), if !closed => match incoming {
                None | Some(Err(_)) => {
                    closed = true;
                    shell.terminate();
                }
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ExecClientMessage>(text.as_str()) {
                    Ok(ExecClientMessage::Data { data }) => {
                        let writer = Arc::clone(&shell);
                        let payload = data;
                        let _ = tokio::task::spawn_blocking(move || writer.write(payload.as_bytes())).await;
                    }
                    Ok(ExecClientMessage::Resize { cols, rows }) => {
                        let sheller = Arc::clone(&shell);
                        let _ = tokio::task::spawn_blocking(move || sheller.resize(rows, cols)).await;
                    }
                    Err(err) => tracing::debug!(%err, "bad shell frame"),
                },
                Some(Ok(Message::Close(_))) => {
                    closed = true;
                    shell.terminate();
                }
                Some(Ok(Message::Ping(payload))) => {
                    let _ = sink.send(Message::Pong(payload)).await;
                }
                Some(Ok(_)) => {}
            },
        }
    }

    reader.abort();
    // If the operator disconnected, the child must not linger as a zombie.
    if !shell.poll_exit() {
        shell.kill();
    }
    let _ = shell.reap();
    let _ = sink.close().await;
    tracing::info!("host shell closed");
}

fn frame(message: &ExecServerMessage) -> Message {
    let payload =
        serde_json::to_string(message).unwrap_or_else(|_| r#"{"type":"exit"}"#.to_string());
    Message::Text(Utf8Bytes::from(payload))
}

/// Report an unavailable shell, then close.
async fn fail(socket: WebSocket, message: &str) {
    let (mut sink, _) = socket.split();
    let payload = serde_json::to_string(&ExecServerMessage::Data {
        data: format!("{message}\r\n"),
    })
    .unwrap_or_else(|_| r#"{"type":"exit"}"#.to_string());
    let _ = sink.send(Message::Text(Utf8Bytes::from(payload))).await;
    let _ = sink.send(frame(&ExecServerMessage::Exit)).await;
    let _ = sink.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_match_the_container_exec_protocol() {
        assert_eq!(
            frame(&ExecServerMessage::Exit),
            Message::Text(Utf8Bytes::from(r#"{"type":"exit"}"#))
        );
        match frame(&ExecServerMessage::Data {
            data: "hi".to_string(),
        }) {
            Message::Text(text) => assert_eq!(text.as_str(), r#"{"type":"data","data":"hi"}"#),
            other => panic!("unexpected frame {other:?}"),
        }
    }
}
