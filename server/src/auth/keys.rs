//! `GET/PUT /auth/keys` and `PUT /auth/keys/rewrap` (spec §4.1, §6.1): the
//! wrapped account key. The server stores what the client sends and can
//! only tell whether a rewrap request knows the account key (the verifier);
//! it never sees a KEK, an account key or a vault key.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use obsink_core::{decode_base64, encode_base64, SALT_LEN, WRAPPED_KEY_LEN};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use subtle::ConstantTimeEq;

use crate::{
    auth::Principal,
    crypto,
    error::{ApiError, AppJson},
    AppState,
};

const VERIFIER_LEN: usize = 32;

#[derive(Debug, Clone, Serialize)]
pub struct AccountKeyBlob {
    pub key_id: String,
    pub wrapped: String,
    pub salt: String,
}

#[derive(Serialize)]
pub struct KeysResponse {
    pub account_key: Option<AccountKeyBlob>,
}

#[derive(Deserialize)]
pub struct SetKeysBody {
    pub wrapped: Option<String>,
    pub salt: Option<String>,
    pub verifier: Option<String>,
}

struct Material {
    wrapped: Vec<u8>,
    salt: Vec<u8>,
    verifier: Vec<u8>,
}

fn decode_field(value: Option<&str>, name: &str, len: usize) -> Result<Vec<u8>, ApiError> {
    let bytes = value
        .map(decode_base64)
        .transpose()
        .map_err(|_| ApiError::bad_request(format!("{name} must be base64")))?
        .ok_or_else(|| ApiError::bad_request(format!("{name} is required")))?;
    if bytes.len() != len {
        return Err(ApiError::bad_request(format!(
            "{name} must decode to {len} bytes"
        )));
    }
    Ok(bytes)
}

impl SetKeysBody {
    fn decode(&self) -> Result<Material, ApiError> {
        Ok(Material {
            wrapped: decode_field(self.wrapped.as_deref(), "wrapped", WRAPPED_KEY_LEN)?,
            salt: decode_field(self.salt.as_deref(), "salt", SALT_LEN)?,
            verifier: decode_field(self.verifier.as_deref(), "verifier", VERIFIER_LEN)?,
        })
    }
}

/// The stored blob, or `None` before the first set.
pub async fn current(
    conn: &mut PgConnection,
    user_id: &str,
) -> Result<Option<AccountKeyBlob>, ApiError> {
    let row = sqlx::query(
        "SELECT account_key_enc, account_key_salt, account_key_id FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| ApiError::unauthorized("unauthorized"))?;
    let wrapped: Option<Vec<u8>> = row.get("account_key_enc");
    let salt: Option<Vec<u8>> = row.get("account_key_salt");
    let key_id: Option<String> = row.get("account_key_id");
    Ok(match (wrapped, salt, key_id) {
        (Some(wrapped), Some(salt), Some(key_id)) => Some(AccountKeyBlob {
            key_id,
            wrapped: encode_base64(&wrapped),
            salt: encode_base64(&salt),
        }),
        _ => None,
    })
}

/// Whether the account has set a passphrase (vault creation needs one).
pub async fn has_key(conn: &mut PgConnection, user_id: &str) -> Result<bool, ApiError> {
    let (set,): (bool,) =
        sqlx::query_as("SELECT account_key_enc IS NOT NULL FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(conn)
            .await?;
    Ok(set)
}

pub async fn get(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<KeysResponse>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let account_key = current(&mut conn, &principal.user_id).await?;
    Ok(Json(KeysResponse { account_key }))
}

#[derive(Serialize)]
pub struct CreatedResponse {
    pub key_id: String,
}

/// Create-only: the first device to set the passphrase wins; a second setter
/// gets `409` with the winner's blob and unlocks with that instead.
pub async fn set(
    State(state): State<AppState>,
    principal: Principal,
    AppJson(body): AppJson<SetKeysBody>,
) -> Result<axum::response::Response, ApiError> {
    let material = body.decode()?;
    let key_id = crypto::new_id("key");
    let mut conn = state.pool.acquire().await?;
    let updated = sqlx::query(
        "UPDATE users SET account_key_enc = $2, account_key_salt = $3, account_key_id = $4,
             account_key_verifier = $5
         WHERE id = $1 AND account_key_enc IS NULL",
    )
    .bind(&principal.user_id)
    .bind(&material.wrapped)
    .bind(&material.salt)
    .bind(&key_id)
    .bind(&material.verifier)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if updated == 1 {
        return Ok((StatusCode::CREATED, Json(CreatedResponse { key_id })).into_response());
    }
    let account_key = current(&mut conn, &principal.user_id).await?;
    Ok((StatusCode::CONFLICT, Json(KeysResponse { account_key })).into_response())
}

/// A passphrase change: the same account key under a new KEK. The verifier
/// proves the caller holds the unwrapped key; the stored verifier never
/// changes.
pub async fn rewrap(
    State(state): State<AppState>,
    principal: Principal,
    AppJson(body): AppJson<SetKeysBody>,
) -> Result<StatusCode, ApiError> {
    let material = body.decode()?;
    let mut tx = state.pool.begin().await?;
    let row = sqlx::query("SELECT account_key_verifier FROM users WHERE id = $1 FOR UPDATE")
        .bind(&principal.user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::unauthorized("unauthorized"))?;
    let stored: Option<Vec<u8>> = row.get("account_key_verifier");
    let Some(stored) = stored else {
        return Err(ApiError::bad_request("set a passphrase first"));
    };
    if !bool::from(stored.as_slice().ct_eq(material.verifier.as_slice())) {
        return Err(ApiError::forbidden(
            "passphrase does not match this account",
        ));
    }
    sqlx::query("UPDATE users SET account_key_enc = $2, account_key_salt = $3 WHERE id = $1")
        .bind(&principal.user_id)
        .bind(&material.wrapped)
        .bind(&material.salt)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
