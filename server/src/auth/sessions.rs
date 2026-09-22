//! Session bearers (`os_…`), 180-day absolute expiry, stored as SHA-256. One
//! session per device: a new sign-in from a known device replaces its session.

use serde::Serialize;
use sqlx::PgConnection;

use crate::{crypto, db, error::ApiError};

pub const SESSION_TTL_SECS: u64 = 180 * 24 * 60 * 60;

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub token: String,
    pub session: SessionInfo,
    pub user: UserInfo,
}

#[derive(Debug, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub expires: u64,
    pub device_id: String,
}

#[derive(Debug, Serialize)]
pub struct UserInfo {
    pub id: String,
    pub email: Option<String>,
}

/// Mint the device's session, replacing any it had.
pub async fn create(
    conn: &mut PgConnection,
    user_id: &str,
    email: Option<String>,
    device_id: &str,
    now: u64,
) -> Result<SessionResponse, ApiError> {
    let token = crypto::session_token();
    let id = crypto::new_id("ses");
    let expires = now + SESSION_TTL_SECS;
    sqlx::query("DELETE FROM sessions WHERE user_id = $1 AND device_id = $2")
        .bind(user_id)
        .bind(device_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO sessions (id, user_id, device_id, token_hash, created, expires)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(device_id)
    .bind(crypto::sha256(token.as_bytes()))
    .bind(db::to_i64(now))
    .bind(db::to_i64(expires))
    .execute(conn)
    .await?;
    Ok(SessionResponse {
        token,
        session: SessionInfo {
            id,
            expires,
            device_id: device_id.to_string(),
        },
        user: UserInfo {
            id: user_id.to_string(),
            email,
        },
    })
}
