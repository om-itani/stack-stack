//! Embedded frontend.
//!
//! `frontend/dist` is built on Windows and committed, then baked into the binary
//! at `cargo build` time, so the Fujitsu needs neither Node nor a network fetch
//! to serve the UI.

use std::borrow::Cow;

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode, Uri, header};
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

pub async fn serve(headers: HeaderMap, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    // An unknown API route must 404 as text, never fall through to the SPA
    // shell: a silent HTML answer to a JSON call is a debugging nightmare.
    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "unknown api route").into_response();
    }

    if let Some(response) = asset(&headers, path) {
        return response;
    }

    // Client-side routes resolve to the shell; a real 404 only if dist is empty.
    asset(&headers, "index.html")
        .unwrap_or_else(|| (StatusCode::NOT_FOUND, "not found").into_response())
}

fn asset(headers: &HeaderMap, path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();

    // Vite fingerprints assets by hash, so they are immutable forever.
    if path.starts_with("assets/") {
        let body = match file.data {
            Cow::Borrowed(slice) => Bytes::from_static(slice),
            Cow::Owned(vec) => Bytes::from(vec),
        };
        return Some(
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref()),
                    (header::CACHE_CONTROL, IMMUTABLE),
                ],
                body,
            )
                .into_response(),
        );
    }

    // For index.html and root resources, use ETag revalidation with the dist fingerprint.
    // If client has matching ETag, return 304 Not Modified without sending bytes.
    let etag = format!("\"{}\"", FINGERPRINT);
    if let Some(if_none_match) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        && (if_none_match == etag
            || if_none_match == format!("W/\"{}\"", FINGERPRINT)
            || if_none_match == FINGERPRINT)
    {
        return Some(
            (
                StatusCode::NOT_MODIFIED,
                [
                    (header::ETAG, etag.as_str()),
                    (header::CACHE_CONTROL, NO_CACHE),
                ],
            )
                .into_response(),
        );
    }

    let body = match file.data {
        Cow::Borrowed(slice) => Bytes::from_static(slice),
        Cow::Owned(vec) => Bytes::from(vec),
    };

    Some(
        (
            [
                (header::CONTENT_TYPE, mime.as_ref()),
                (header::CACHE_CONTROL, NO_CACHE),
                (header::ETAG, etag.as_str()),
            ],
            body,
        )
            .into_response(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[tokio::test]
    async fn serve_index_returns_ok_with_etag() {
        let headers = HeaderMap::new();
        let uri: Uri = "/".parse().unwrap();
        let res = serve(headers, uri).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers().contains_key(header::ETAG));
        assert_eq!(res.headers().get(header::CACHE_CONTROL).unwrap(), NO_CACHE);
    }

    #[tokio::test]
    async fn serve_index_revalidation_returns_304() {
        let mut headers = HeaderMap::new();
        let etag = format!("\"{}\"", FINGERPRINT);
        headers.insert(header::IF_NONE_MATCH, HeaderValue::from_str(&etag).unwrap());
        let uri: Uri = "/".parse().unwrap();
        let res = serve(headers, uri).await;
        assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn unknown_api_route_returns_404_not_spa() {
        let headers = HeaderMap::new();
        let uri: Uri = "/api/unknown_endpoint".parse().unwrap();
        let res = serve(headers, uri).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn client_side_routes_fallback_to_spa_index() {
        let headers = HeaderMap::new();
        let uri: Uri = "/containers".parse().unwrap();
        let res = serve(headers, uri).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html"
        );
    }
}
