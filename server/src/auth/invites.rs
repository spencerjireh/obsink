//! Invite-only signup. The first account on a server needs no invite; every
//! later account must redeem an unused, unexpired code. Any signed-in user
//! (and the operator) can mint codes.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::Serialize;
use sqlx::{PgConnection, Row};

use crate::{
    auth::users,
    crypto::{self, INVITE_ALPHABET, INVITE_CODE_LEN},
    db,
    error::ApiError,
};

pub const INVITE_TTL_SECS: u64 = 7 * 24 * 60 * 60;
const REDEEM_WINDOW: Duration = Duration::from_secs(60);
const REDEEM_MAX_FAILURES: u32 = 20;

pub const REQUIRED_MESSAGE: &str = "an invite code is required to create an account";
pub const INVALID_MESSAGE: &str = "invite code is invalid, used, or expired";

#[derive(Debug, Serialize)]
pub struct InviteInfo {
    pub code: String,
    pub created: u64,
    pub expires: u64,
    pub status: &'static str,
    pub used_at: Option<u64>,
}

/// Process-wide brake on guessing: after N failed redemptions per minute the
/// endpoint answers 429 for the rest of the window.
#[derive(Debug, Default)]
pub struct RedeemLimiter {
    inner: Mutex<Option<(Instant, u32)>>,
}

impl RedeemLimiter {
    pub fn check(&self) -> Result<(), ApiError> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((start, count)) = *guard {
            if start.elapsed() > REDEEM_WINDOW {
                *guard = None;
            } else if count >= REDEEM_MAX_FAILURES {
                return Err(ApiError::status(
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    "too many invite attempts; try again in a minute",
                ));
            }
        }
        Ok(())
    }

    pub fn record_failure(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *guard = match *guard {
            Some((start, count)) if start.elapsed() <= REDEEM_WINDOW => Some((start, count + 1)),
            _ => Some((Instant::now(), 1)),
        };
    }
}

pub fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

fn well_formed(code: &str) -> bool {
    code.len() == INVITE_CODE_LEN && code.bytes().all(|b| INVITE_ALPHABET.contains(&b))
}

/// Decide whether a NEW account may be created. Returns the locked invite code
/// to mark used after the user row exists, or `None` when signup is open
/// (zero users). Must run inside the transaction that inserts the user.
pub async fn authorize_signup(
    conn: &mut PgConnection,
    limiter: &RedeemLimiter,
    invite_code: Option<&str>,
    now: u64,
) -> Result<Option<String>, ApiError> {
    if users::count(conn).await? == 0 {
        return Ok(None);
    }
    let code = invite_code
        .map(normalize_code)
        .filter(|code| !code.is_empty());
    let Some(code) = code else {
        return Err(ApiError::forbidden(REQUIRED_MESSAGE));
    };
    limiter.check()?;
    if !well_formed(&code) {
        limiter.record_failure();
        return Err(ApiError::forbidden(INVALID_MESSAGE));
    }
    let row =
        sqlx::query("SELECT expires, used_by, used_at FROM invites WHERE code = $1 FOR UPDATE")
            .bind(&code)
            .fetch_optional(conn)
            .await?;
    // `used_by` is nulled when the redeemer deletes their account (FK ON
    // DELETE SET NULL); `used_at` survives, so it is what makes a code spent.
    let usable = match row {
        Some(row) => {
            row.get::<Option<String>, _>("used_by").is_none()
                && row.get::<Option<i64>, _>("used_at").is_none()
                && db::to_u64(row.get("expires")) > now
        }
        None => false,
    };
    if !usable {
        limiter.record_failure();
        return Err(ApiError::forbidden(INVALID_MESSAGE));
    }
    Ok(Some(code))
}

pub async fn mark_used(
    conn: &mut PgConnection,
    code: &str,
    user_id: &str,
    now: u64,
) -> Result<(), ApiError> {
    sqlx::query("UPDATE invites SET used_by = $2, used_at = $3 WHERE code = $1")
        .bind(code)
        .bind(user_id)
        .bind(db::to_i64(now))
        .execute(conn)
        .await?;
    Ok(())
}

/// Mint a fresh code. `created_by = None` is the operator / admin CLI.
pub async fn create(
    conn: &mut PgConnection,
    created_by: Option<&str>,
    now: u64,
    ttl_secs: u64,
) -> Result<InviteInfo, ApiError> {
    for _ in 0..8 {
        let code = crypto::random_invite_code();
        let expires = now + ttl_secs;
        let inserted = sqlx::query(
            "INSERT INTO invites (code, created_by, created, expires) VALUES ($1, $2, $3, $4)
             ON CONFLICT (code) DO NOTHING",
        )
        .bind(&code)
        .bind(created_by)
        .bind(db::to_i64(now))
        .bind(db::to_i64(expires))
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if inserted == 1 {
            return Ok(InviteInfo {
                code,
                created: now,
                expires,
                status: "active",
                used_at: None,
            });
        }
    }
    Err(ApiError::internal(
        "could not allocate an unused invite code",
    ))
}

/// Invites minted by a principal (operator: `created_by IS NULL`), newest first.
pub async fn list(
    conn: &mut PgConnection,
    created_by: Option<&str>,
    now: u64,
) -> Result<Vec<InviteInfo>, ApiError> {
    let rows = match created_by {
        Some(user_id) => {
            sqlx::query("SELECT code, created, expires, used_at FROM invites WHERE created_by = $1 ORDER BY created DESC")
                .bind(user_id)
                .fetch_all(conn)
                .await?
        }
        None => {
            sqlx::query("SELECT code, created, expires, used_at FROM invites WHERE created_by IS NULL ORDER BY created DESC")
                .fetch_all(conn)
                .await?
        }
    };
    Ok(rows
        .into_iter()
        .map(|row| {
            let expires = db::to_u64(row.get("expires"));
            let used_at = row.get::<Option<i64>, _>("used_at").map(db::to_u64);
            InviteInfo {
                code: row.get("code"),
                created: db::to_u64(row.get("created")),
                expires,
                status: if used_at.is_some() {
                    "used"
                } else if expires <= now {
                    "expired"
                } else {
                    "active"
                },
                used_at,
            }
        })
        .collect())
}
