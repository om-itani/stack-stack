//! One error type for the whole container / app-store API.
//!
//! Handlers return `Result<_, ApiError>` and never have to know about HTTP
//! status codes; the `IntoResponse` impl maps each variant to exactly one
//! status and a `{"error": "..."}` body.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::containers::model::ErrorResponse;

#[derive(Debug)]
pub enum ApiError {
    /// Caller sent something malformed (bad id, unknown field, …).
    BadRequest(String),
    /// The object exists but we were asked for something it does not have.
    NotFound(String),
    /// A dependency we need is not present: no Docker socket, no compose CLI.
    Unavailable(String),
    /// Anything else; the message is safe to show but not something the caller
    /// can act on beyond retrying.
    Internal(String),
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::Unavailable(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    fn parts(&self) -> (StatusCode, &str) {
        match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message),
            Self::Unavailable(message) => (StatusCode::SERVICE_UNAVAILABLE, message),
            Self::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, message),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (status, message) = self.parts();
        write!(f, "{status}: {message}")
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = self.parts();
        let body = Json(ErrorResponse {
            error: message.to_string(),
        });
        (status, body).into_response()
    }
}

/// Classify a bollard failure. A dead or missing socket must surface as 503 so
/// the UI can say "Docker is not running" instead of a generic 500; a 404 from
/// the daemon is a genuine not-found.
impl From<bollard::errors::Error> for ApiError {
    fn from(err: bollard::errors::Error) -> Self {
        use bollard::errors::Error;

        let rendered = err.to_string();
        match err {
            Error::DockerResponseServerError {
                status_code: 404, ..
            } => Self::NotFound(rendered),
            Error::DockerResponseServerError { status_code, .. } => {
                Self::Internal(format!("docker daemon returned {status_code}: {rendered}"))
            }
            // Connecting to a socket that is not there / not accepting.
            Error::SocketNotFoundError(_)
            | Error::HyperLegacyError { .. }
            | Error::HttpClientError { .. }
            | Error::IOError { .. } => Self::Unavailable(format!("docker unavailable: {rendered}")),
            _ => Self::Internal(rendered),
        }
    }
}
