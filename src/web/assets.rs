//! Embedded frontend.
//!
//! `frontend/dist` is built on Windows and committed, then baked into the binary
//! at `cargo build` time, so the Fujitsu needs neither Node nor a network fetch
//! to serve the UI.

use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "frontend/dist"]
pub struct Assets;

/// Fingerprint of the embedded `frontend/dist` tree, produced by `build.rs`.
///
/// Logged at startup so a deploy can be tied to an exact bundle, and it is what
/// forces a recompile when the frontend is rebuilt (see `build.rs`).
pub const FINGERPRINT: &str = env!("STACK_STACK_ASSETS_FINGERPRINT");

/// Vite fingerprints everything under `assets/`, so it can be cached forever.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const NO_CACHE: &str = "no-cache";

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    // An unknown API route must 404 as text, never fall through to the SPA
    // shell: a silent HTML answer to a JSON call is a debugging nightmare.
    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "unknown api route").into_response();
    }

    if let Some(response) = asset(path) {
        return response;
    }

    // Client-side routes resolve to the shell; a real 404 only if dist is empty.
    asset("index.html").unwrap_or_else(|| (StatusCode::NOT_FOUND, "not found").into_response())
}

fn asset(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if path.starts_with("assets/") {
        IMMUTABLE
    } else {
        NO_CACHE
    };

    Some(
        (
            [
                (header::CONTENT_TYPE, mime.as_ref()),
                (header::CACHE_CONTROL, cache),
            ],
            file.data.into_owned(),
        )
            .into_response(),
    )
}
