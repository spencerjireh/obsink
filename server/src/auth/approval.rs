//! Device key approval (spec §4.1, §12.1, §12.3): a signed-in device without
//! the account key registers an X25519 public key (`PUT /auth/approval`),
//! polls for the wrapped account key (`GET /auth/approval`) and withdraws the
//! request once it has it (`DELETE /auth/approval`); an unlocked device
//! answers with `POST /auth/devices/{device_id}/approval`. The server relays
//! a public key and a ciphertext it cannot open and never computes or
//! returns a fingerprint; the verifier on the approve step proves the caller
//! holds the account key, so a stolen locked session cannot poison a request.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use obsink_core::{encode_base64, APPROVAL_BLOB_LEN, APPROVAL_PUBLIC_KEY_LEN};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use subtle::ConstantTimeEq;

use crate::{
    auth::{
        keys::{self, decode_field},
        Principal,
    },
    db,
    error::{ApiError, AppJson},
    AppState,
};

/// How long a request lives after registration, and again after approval so
/// the poller can collect the blob.
pub const APPROVAL_TTL_SECS: u64 = 600;
const VERIFIER_LEN: usize = 32;

#[derive(Deserialize)]
pub struct RegisterBody {
    pub public_key: Option<String>,
}

#[derive(Serialize)]
pub struct Registered {
    pub requested: u64,
    pub expires: u64,
}

/// `PUT /auth/approval`: register (or replace) this device's request. The
/// account must have a passphrase, else there is no key to approve with.
pub async fn register(
    State(state): State<AppState>,
    principal: Principal,
    AppJson(body): AppJson<RegisterBody>,
) -> Result<(StatusCode, Json<Registered>), ApiError> {
    let public_key = decode_field(
        body.public_key.as_deref(),
        "public_key",
        APPROVAL_PUBLIC_KEY_LEN,
    )?;
    let mut conn = state.pool.acquire().await?;
    if !keys::has_key(&mut conn, &principal.user_id).await? {
        return Err(ApiError::bad_request("set a passphrase first"));
    }
    let now = db::now();
    let expires = now + APPROVAL_TTL_SECS;
    let updated = sqlx::query(
        "UPDATE devices SET approval_public_key = $3, approval_requested = $4,
             approval_expires = $5, approval_wrapped = NULL, approval_approved_by = NULL,
             approval_approved = NULL
         WHERE user_id = $1 AND id = $2",
    )
    .bind(&principal.user_id)
    .bind(&principal.device_id)
    .bind(&public_key)
    .bind(db::to_i64(now))
    .bind(db::to_i64(expires))
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(ApiError::unauthorized("unauthorized"));
    }
    Ok((
        StatusCode::CREATED,
        Json(Registered {
            requested: now,
            expires,
        }),
    ))
}

/// This device's request as `GET /auth/approval` reports it.
#[derive(Serialize)]
pub struct ApprovalStatus {
    pub public_key: String,
    pub requested: u64,
    pub expires: u64,
    /// Null until an unlocked device approved.
    pub wrapped: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved: Option<u64>,
}

#[derive(Serialize)]
pub struct PollResponse {
    pub approval: Option<ApprovalStatus>,
}

/// `GET /auth/approval`: `{ approval: null }` when nothing is live, else the
/// request with the blob once approved (`key_id` travels with it).
pub async fn poll(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<PollResponse>, ApiError> {
    let row = sqlx::query(
        "SELECT d.approval_public_key, d.approval_requested, d.approval_expires,
                d.approval_wrapped, d.approval_approved_by, d.approval_approved,
                u.account_key_id
         FROM devices d JOIN users u ON u.id = d.user_id
         WHERE d.user_id = $1 AND d.id = $2",
    )
    .bind(&principal.user_id)
    .bind(&principal.device_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::unauthorized("unauthorized"))?;
    let public_key: Option<Vec<u8>> = row.get("approval_public_key");
    let expires: Option<i64> = row.get("approval_expires");
    let now = db::now();
    let approval = match (public_key, expires) {
        (Some(public_key), Some(expires)) if db::to_u64(expires) > now => {
            let wrapped: Option<Vec<u8>> = row.get("approval_wrapped");
            let key_id: Option<String> = row.get("account_key_id");
            let approved_by: Option<String> = row.get("approval_approved_by");
            let approved: Option<i64> = row.get("approval_approved");
            let requested: Option<i64> = row.get("approval_requested");
            Some(ApprovalStatus {
                public_key: encode_base64(&public_key),
                requested: db::to_u64(requested.unwrap_or_default()),
                expires: db::to_u64(expires),
                key_id: wrapped.as_ref().and(key_id),
                wrapped: wrapped.as_deref().map(encode_base64),
                approved_by,
                approved: approved.map(db::to_u64),
            })
        }
        _ => None,
    };
    Ok(Json(PollResponse { approval }))
}

/// `DELETE /auth/approval`: withdraw this device's request. Idempotent.
pub async fn cancel(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<StatusCode, ApiError> {
    sqlx::query(
        "UPDATE devices SET approval_public_key = NULL, approval_requested = NULL,
             approval_expires = NULL, approval_wrapped = NULL, approval_approved_by = NULL,
             approval_approved = NULL
         WHERE user_id = $1 AND id = $2",
    )
    .bind(&principal.user_id)
    .bind(&principal.device_id)
    .execute(&state.pool)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ApproveBody {
    pub wrapped: Option<String>,
    pub verifier: Option<String>,
}

/// `POST /auth/devices/{device_id}/approval`: an unlocked device hands the
/// pending one the account key wrapped to its public key. The verifier must
/// match the account's (constant-time); the request must be live and not
/// yet answered. Approval gives the request a fresh 10 minutes.
pub async fn approve(
    State(state): State<AppState>,
    principal: Principal,
    Path(device_id): Path<String>,
    AppJson(body): AppJson<ApproveBody>,
) -> Result<StatusCode, ApiError> {
    if device_id == principal.device_id {
        return Err(ApiError::bad_request("a device cannot approve itself"));
    }
    let wrapped = decode_field(body.wrapped.as_deref(), "wrapped", APPROVAL_BLOB_LEN)?;
    let verifier = decode_field(body.verifier.as_deref(), "verifier", VERIFIER_LEN)?;

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
    if !bool::from(stored.as_slice().ct_eq(verifier.as_slice())) {
        return Err(ApiError::forbidden(
            "passphrase does not match this account",
        ));
    }

    let now = db::now();
    let updated = sqlx::query(
        "UPDATE devices SET approval_wrapped = $3, approval_approved_by = $4,
             approval_approved = $5, approval_expires = $5 + $6
         WHERE user_id = $1 AND id = $2 AND approval_public_key IS NOT NULL
           AND approval_expires > $5 AND approval_wrapped IS NULL",
    )
    .bind(&principal.user_id)
    .bind(&device_id)
    .bind(&wrapped)
    .bind(&principal.device_id)
    .bind(db::to_i64(now))
    .bind(db::to_i64(APPROVAL_TTL_SECS))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated == 0 {
        let live: Option<(bool,)> = sqlx::query_as(
            "SELECT approval_wrapped IS NOT NULL FROM devices
             WHERE user_id = $1 AND id = $2 AND approval_public_key IS NOT NULL
               AND approval_expires > $3",
        )
        .bind(&principal.user_id)
        .bind(&device_id)
        .bind(db::to_i64(now))
        .fetch_optional(&mut *tx)
        .await?;
        return Err(match live {
            Some((true,)) => ApiError::status(StatusCode::CONFLICT, "already approved"),
            _ => ApiError::not_found("no pending request for this device"),
        });
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
