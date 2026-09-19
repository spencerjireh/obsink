//! Session bearers (`os_…`), 180-day absolute expiry, stored as SHA-256.

use serde::Serialize;
use sqlx::{PgConnection, Row};

use crate::{crypto, crypto::ServerKeys, db, error::ApiError};

pub const SESSION_TTL_SECS: u64 = 180 * 24 * 60 * 60;
const MAX_DEVICE_NAME: usize = 80;

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
}

#[derive(Debug, Serialize)]
pub struct UserInfo {
    pub id: String,
    pub email: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub id: String,
    #[serde(rename = "deviceName")]
    pub device_name: String,
    pub created: u64,
    pub current: bool,
}

pub fn clean_device_name(name: Option<&str>) -> String {
    let trimmed = name.map(str::trim).unwrap_or("");
    if trimmed.is_empty() {
        return "device".to_string();
    }
    trimmed.chars().take(MAX_DEVICE_NAME).collect()
}

pub async fn create(
    conn: &mut PgConnection,
    keys: &ServerKeys,
    user_id: &str,
    email: Option<String>,
    device_name: &str,
    now: u64,
) -> Result<SessionResponse, ApiError> {
    let token = crypto::session_token();
    let id = crypto::new_id("ses");
    let expires = now + SESSION_TTL_SECS;
    sqlx::query(
        "INSERT INTO sessions (id, user_id, token_hash, device_name_enc, created, expires)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(crypto::sha256(token.as_bytes()))
    .bind(keys.seal_field("sessions", "device_name", &id, device_name))
    .bind(db::to_i64(now))
    .bind(db::to_i64(expires))
    .execute(conn)
    .await?;
    Ok(SessionResponse {
        token,
        session: SessionInfo { id, expires },
        user: UserInfo {
            id: user_id.to_string(),
            email,
        },
    })
}

/// Live sessions for an account, oldest first.
pub async fn list(
    conn: &mut PgConnection,
    keys: &ServerKeys,
    user_id: &str,
    current_session_id: &str,
    now: u64,
) -> Result<Vec<SessionSummary>, ApiError> {
    let rows = sqlx::query(
        "SELECT id, device_name_enc, created FROM sessions
         WHERE user_id = $1 AND expires > $2 ORDER BY created ASC, id ASC",
    )
    .bind(user_id)
    .bind(db::to_i64(now))
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|row| {
            let id: String = row.get("id");
            let device_name = keys
                .open_field(
                    "sessions",
                    "device_name",
                    &id,
                    &row.get::<Vec<u8>, _>("device_name_enc"),
                )
                .map_err(ApiError::internal)?;
            Ok(SessionSummary {
                current: id == current_session_id,
                id,
                device_name,
                created: db::to_u64(row.get("created")),
            })
        })
        .collect()
}

/// Revoke one of the account's own sessions. 404 when it is not theirs.
pub async fn revoke(
    conn: &mut PgConnection,
    user_id: &str,
    session_id: &str,
) -> Result<(), ApiError> {
    let result = sqlx::query("DELETE FROM sessions WHERE id = $1 AND user_id = $2")
        .bind(session_id)
        .bind(user_id)
        .execute(conn)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("session not found"));
    }
    Ok(())
}
