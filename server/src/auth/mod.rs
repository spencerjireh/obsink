//! Bearer resolution. The one principal is a user session (`os_…`), which
//! belongs to a device; every vault route is scoped to the vaults that user
//! is a member of (spec §4.1).

pub mod account;
pub mod apple;
pub mod devices;
pub mod email;
pub mod invites;
pub mod keys;
pub mod sessions;
pub mod users;

use axum::{
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts},
};

use crate::{db, error::ApiError, AppState};

#[derive(Debug, Clone)]
pub struct Principal {
    pub user_id: String,
    pub session_id: String,
    pub device_id: String,
}

pub async fn resolve(
    state: &AppState,
    authorization: Option<&str>,
) -> Result<Option<Principal>, ApiError> {
    let Some(token) = authorization
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
    else {
        return Ok(None);
    };
    if !token.starts_with("os_") {
        return Ok(None);
    }
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id, user_id, device_id FROM sessions WHERE token_hash = $1 AND expires > $2",
    )
    .bind(crate::crypto::sha256(token.as_bytes()))
    .bind(db::to_i64(db::now()))
    .fetch_optional(&state.pool)
    .await?;
    Ok(row.map(|(session_id, user_id, device_id)| Principal {
        user_id,
        session_id,
        device_id,
    }))
}

impl FromRequestParts<AppState> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok());
        resolve(state, header)
            .await?
            .ok_or_else(|| ApiError::unauthorized("unauthorized"))
    }
}
