//! Bearer resolution. Every vault route is scoped to the principal's tenant:
//! `default` for the operator `OBSINK_API_KEY`, the user id for sessions.

pub mod account;
pub mod apple;
pub mod email;
pub mod invites;
pub mod sessions;
pub mod users;

use axum::{
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts},
};
use subtle::ConstantTimeEq;

use crate::{db, error::ApiError, AppState};

pub const OPERATOR_TENANT: &str = "default";

#[derive(Debug, Clone)]
pub enum Principal {
    Operator,
    User { user_id: String, session_id: String },
}

impl Principal {
    pub fn tenant(&self) -> &str {
        match self {
            Principal::Operator => OPERATOR_TENANT,
            Principal::User { user_id, .. } => user_id,
        }
    }

    pub fn is_user(&self) -> bool {
        matches!(self, Principal::User { .. })
    }

    pub fn user_id(&self) -> Option<&str> {
        match self {
            Principal::Operator => None,
            Principal::User { user_id, .. } => Some(user_id),
        }
    }
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
    if let Some(api_key) = &state.config.api_key {
        if bool::from(token.as_bytes().ct_eq(api_key.as_bytes())) {
            return Ok(Some(Principal::Operator));
        }
    }
    if !token.starts_with("os_") {
        return Ok(None);
    }
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT id, user_id FROM sessions WHERE token_hash = $1 AND expires > $2")
            .bind(crate::crypto::sha256(token.as_bytes()))
            .bind(db::to_i64(db::now()))
            .fetch_optional(&state.pool)
            .await?;
    Ok(row.map(|(session_id, user_id)| Principal::User {
        user_id,
        session_id,
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
