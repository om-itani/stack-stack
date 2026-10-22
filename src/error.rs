//! One error type for every JSON API surface.
//!
//! Handlers return `Result<_, ApiError>` and never have to know about HTTP
//! status codes; the `IntoResponse` impl maps each variant to exactly one
//! status and a `{"error": "..."}` body.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug)]
pub enum ApiError {
    /// Caller sent something malformed (bad id, unknown field, invalid config).
    BadRequest(String),
    /// Caller is not authenticated: missing/expired session, or a bad password.
    Unauthorized(String),
    /// The object exists but we were asked for something it does not have.
    NotFound(String),
    /// A dependency we need is not present: no Docker socket, no `nft` binary.
    Unavailable(String),
    /// An upstream we proxy rate-limited us; the client should retry later.
    TooManyRequests(String),
    /// Anything else; the message is safe to show but not something the caller
    /// can act on beyond retrying.
    Internal(String),
}

/// Wire shape of every error response; mirrored in the frontend client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::Unauthorized(message.into())
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::Unavailable(message.into())
    }

    pub fn too_many_requests(message: impl Into<String>) -> Self {
        Self::TooManyRequests(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    fn parts(&self) -> (StatusCode, &str) {
        match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::Unauthorized(message) => (StatusCode::UNAUTHORIZED, message),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message),
            Self::Unavailable(message) => (StatusCode::SERVICE_UNAVAILABLE, message),
            Self::TooManyRequests(message) => (StatusCode::TOO_MANY_REQUESTS, message),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping_is_stable() {
        assert_eq!(
            ApiError::bad_request("x").parts().0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiError::unauthorized("x").parts().0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(ApiError::not_found("x").parts().0, StatusCode::NOT_FOUND);
        assert_eq!(
            ApiError::unavailable("x").parts().0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            ApiError::too_many_requests("x").parts().0,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            ApiError::internal("x").parts().0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
