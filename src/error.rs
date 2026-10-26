//! One error type for every JSON API surface.
//!
//! Handlers return `Result<_, ApiError>` and never have to know about HTTP
//! status codes; the `IntoResponse` impl maps each variant to exactly one
//! status and a `{"error", "message", "details"}` body, where `error` is a
//! stable machine code, `message` is safe to show, and `details` carries extra
//! diagnostic context when the error has any.

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
    /// Any of the above with extra diagnostic context attached for the UI and
    /// the browser console.
    WithDetails(Box<ApiError>, String),
}

/// Wire shape of every error response; mirrored in the frontend client.
///
/// `error` is the machine code (the `ApiError` variant name, or a short label
/// like `Timeout`), `message` is the human text, and `details` is optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

impl ErrorResponse {
    /// Build a response from a machine code and a human message.
    pub fn new(error: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            message: message.into(),
            details: None,
        }
    }

    /// Attach extra diagnostic context.
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }
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

    /// Attach diagnostic context, preserved through `parts`/`code`.
    pub fn with_details(self, details: impl Into<String>) -> Self {
        Self::WithDetails(Box::new(self), details.into())
    }

    /// The stable machine code reported to clients.
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "BadRequest",
            Self::Unauthorized(_) => "Unauthorized",
            Self::NotFound(_) => "NotFound",
            Self::Unavailable(_) => "Unavailable",
            Self::TooManyRequests(_) => "TooManyRequests",
            Self::Internal(_) => "Internal",
            Self::WithDetails(inner, _) => inner.code(),
        }
    }

    fn parts(&self) -> (StatusCode, &str) {
        match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::Unauthorized(message) => (StatusCode::UNAUTHORIZED, message),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message),
            Self::Unavailable(message) => (StatusCode::SERVICE_UNAVAILABLE, message),
            Self::TooManyRequests(message) => (StatusCode::TOO_MANY_REQUESTS, message),
            Self::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, message),
            Self::WithDetails(inner, _) => inner.parts(),
        }
    }

    fn details(&self) -> Option<&str> {
        match self {
            Self::WithDetails(_, details) => Some(details.as_str()),
            _ => None,
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
        let mut body = ErrorResponse::new(self.code(), message.to_string());
        if let Some(details) = self.details() {
            body = body.with_details(details);
        }
        (status, Json(body)).into_response()
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

    #[test]
    fn codes_are_stable_machine_names() {
        assert_eq!(ApiError::bad_request("x").code(), "BadRequest");
        assert_eq!(ApiError::unauthorized("x").code(), "Unauthorized");
        assert_eq!(ApiError::not_found("x").code(), "NotFound");
        assert_eq!(ApiError::unavailable("x").code(), "Unavailable");
        assert_eq!(ApiError::too_many_requests("x").code(), "TooManyRequests");
        assert_eq!(ApiError::internal("x").code(), "Internal");
    }

    #[test]
    fn details_are_preserved_through_the_wrapper() {
        let err = ApiError::bad_request("invalid").with_details("field a: bad");
        assert_eq!(err.code(), "BadRequest");
        assert_eq!(err.parts(), (StatusCode::BAD_REQUEST, "invalid"));
        assert_eq!(err.details(), Some("field a: bad"));
    }

    #[test]
    fn error_response_serialises_the_three_fields() {
        let body = ErrorResponse::new("PermissionDenied", "device is busy");
        let json = serde_json::to_value(&body).expect("json");
        assert_eq!(json["error"], "PermissionDenied");
        assert_eq!(json["message"], "device is busy");
        assert!(json.get("details").is_none(), "omitted when empty");

        let with = ErrorResponse::new("BadRequest", "nope").with_details("why");
        let json = serde_json::to_value(&with).expect("json");
        assert_eq!(json["details"], "why");
    }
}
