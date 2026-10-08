//! API errors: a status, a stable machine-readable code, and a message.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tracing::error;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{1}")]
    BadRequest(&'static str, String),
    #[error("not signed in")]
    Unauthorized,
    #[error("{1}")]
    Forbidden(&'static str, String),
    #[error("{0}")]
    NotFound(String),
    /// Not found, with a code the client can tell apart from a plain 404.
    #[error("{1}")]
    NotFoundCode(&'static str, String),
    #[error("{1}")]
    Conflict(&'static str, String),
    /// A node the portal depends on failed or couldn't be reached.
    #[error("{1}")]
    BadGateway(&'static str, String),
    /// A node the portal waited on didn't finish in time.
    #[error("{1}")]
    GatewayTimeout(&'static str, String),
    /// Too many requests from one address.
    #[error("{1}")]
    TooManyRequests(&'static str, String),
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::BadRequest(code, message.into())
    }

    pub fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        Self::Forbidden(code, message.into())
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::Conflict(code, message.into())
    }

    /// A node's failure as a 502: what [`crate::nodes::NodeHub`] reports as
    /// conflicts (`node_error`, `node_offline`, `node_timeout`) when the
    /// request isn't the user's doing but the node's.
    pub fn node_failure(self) -> Self {
        match self {
            Self::Conflict(code @ ("node_error" | "node_offline" | "node_timeout"), message) => {
                Self::BadGateway(code, message)
            }
            other => other,
        }
    }

    fn parts(&self) -> (StatusCode, &'static str) {
        match self {
            Self::BadRequest(code, _) => (StatusCode::BAD_REQUEST, code),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden(code, _) => (StatusCode::FORBIDDEN, code),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Self::NotFoundCode(code, _) => (StatusCode::NOT_FOUND, code),
            Self::Conflict(code, _) => (StatusCode::CONFLICT, code),
            Self::TooManyRequests(code, _) => (StatusCode::TOO_MANY_REQUESTS, code),
            Self::BadGateway(code, _) => (StatusCode::BAD_GATEWAY, code),
            Self::GatewayTimeout(code, _) => (StatusCode::GATEWAY_TIMEOUT, code),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        Self::Internal(err.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = self.parts();
        if let Self::Internal(err) = &self {
            // Details go to the log, never to the client.
            error!("{err:#}");
        }
        (
            status,
            Json(json!({ "error": code, "message": self.to_string() })),
        )
            .into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
