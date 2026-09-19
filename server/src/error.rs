//! One error type for every handler. Bodies are `{ "error": "<message>" }`
//! except the 409, which carries the conflicting manifest entry so clients can
//! show it (spec §5.1).

use axum::{
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use obsink_core::FileEntry;
use serde::Serialize;

#[derive(Debug)]
pub enum ApiError {
    Status(StatusCode, String),
    Conflict {
        path: String,
        current: Option<FileEntry>,
    },
    Internal(String),
}

impl ApiError {
    pub fn status(code: StatusCode, message: impl Into<String>) -> Self {
        Self::Status(code, message.into())
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::status(StatusCode::BAD_REQUEST, message)
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::status(StatusCode::UNAUTHORIZED, message)
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::status(StatusCode::FORBIDDEN, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::status(StatusCode::NOT_FOUND, message)
    }
    pub fn internal(error: impl std::fmt::Display) -> Self {
        Self::Internal(error.to_string())
    }

    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::Status(code, _) => *code,
            Self::Conflict { .. } => StatusCode::CONFLICT,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

#[derive(Serialize)]
struct ConflictBody {
    path: String,
    current: Option<FileEntry>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Status(code, message) => {
                (code, Json(ErrorBody { error: &message })).into_response()
            }
            Self::Conflict { path, current } => {
                (StatusCode::CONFLICT, Json(ConflictBody { path, current })).into_response()
            }
            Self::Internal(message) => {
                tracing::error!(error = %message, "internal server error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorBody {
                        error: "internal server error",
                    }),
                )
                    .into_response()
            }
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(format!("database: {error}"))
    }
}

impl From<std::io::Error> for ApiError {
    fn from(error: std::io::Error) -> Self {
        Self::Internal(format!("io: {error}"))
    }
}

impl From<JsonRejection> for ApiError {
    fn from(_: JsonRejection) -> Self {
        Self::bad_request("body must be JSON")
    }
}

/// `axum::Json` with the Worker's 400 message on any parse failure.
#[derive(axum::extract::FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct AppJson<T>(pub T);
