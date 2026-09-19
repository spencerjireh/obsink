//! User rows. Email and Apple subject are sealed; lookups use the keyed index.

use sqlx::{PgConnection, Row};

use crate::{crypto::ServerKeys, db, error::ApiError};

#[derive(Debug, Clone)]
pub struct UserRecord {
    pub id: String,
    pub email: Option<String>,
    pub apple_sub: Option<String>,
    pub created: u64,
}

fn decode(keys: &ServerKeys, row: &sqlx::postgres::PgRow) -> Result<UserRecord, ApiError> {
    let id: String = row.get("id");
    let email = match row.get::<Option<Vec<u8>>, _>("email_enc") {
        Some(sealed) => Some(
            keys.open_field("users", "email", &id, &sealed)
                .map_err(ApiError::internal)?,
        ),
        None => None,
    };
    let apple_sub = match row.get::<Option<Vec<u8>>, _>("apple_sub_enc") {
        Some(sealed) => Some(
            keys.open_field("users", "apple_sub", &id, &sealed)
                .map_err(ApiError::internal)?,
        ),
        None => None,
    };
    Ok(UserRecord {
        id,
        email,
        apple_sub,
        created: db::to_u64(row.get("created")),
    })
}

const COLUMNS: &str = "id, email_enc, apple_sub_enc, created";

pub async fn find_by_id(
    conn: &mut PgConnection,
    keys: &ServerKeys,
    id: &str,
) -> Result<Option<UserRecord>, ApiError> {
    let row = sqlx::query(&format!("SELECT {COLUMNS} FROM users WHERE id = $1"))
        .bind(id)
        .fetch_optional(conn)
        .await?;
    row.map(|row| decode(keys, &row)).transpose()
}

pub async fn find_by_email(
    conn: &mut PgConnection,
    keys: &ServerKeys,
    email: &str,
) -> Result<Option<UserRecord>, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM users WHERE email_hmac = $1"
    ))
    .bind(keys.index("email", email))
    .fetch_optional(conn)
    .await?;
    row.map(|row| decode(keys, &row)).transpose()
}

pub async fn find_by_apple_sub(
    conn: &mut PgConnection,
    keys: &ServerKeys,
    sub: &str,
) -> Result<Option<UserRecord>, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM users WHERE apple_sub_hmac = $1"
    ))
    .bind(keys.index("apple_sub", sub))
    .fetch_optional(conn)
    .await?;
    row.map(|row| decode(keys, &row)).transpose()
}

pub async fn count(conn: &mut PgConnection) -> Result<i64, ApiError> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(conn)
        .await?;
    Ok(count)
}

pub async fn insert(
    conn: &mut PgConnection,
    keys: &ServerKeys,
    email: Option<&str>,
    apple_sub: Option<&str>,
    now: u64,
) -> Result<UserRecord, ApiError> {
    let id = crate::crypto::new_id("usr");
    sqlx::query(
        "INSERT INTO users (id, email_enc, email_hmac, apple_sub_enc, apple_sub_hmac, created)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&id)
    .bind(email.map(|value| keys.seal_field("users", "email", &id, value)))
    .bind(email.map(|value| keys.index("email", value)))
    .bind(apple_sub.map(|value| keys.seal_field("users", "apple_sub", &id, value)))
    .bind(apple_sub.map(|value| keys.index("apple_sub", value)))
    .bind(db::to_i64(now))
    .execute(conn)
    .await?;
    Ok(UserRecord {
        id,
        email: email.map(str::to_string),
        apple_sub: apple_sub.map(str::to_string),
        created: now,
    })
}

/// Attach an Apple subject to an existing (email) account.
pub async fn link_apple(
    conn: &mut PgConnection,
    keys: &ServerKeys,
    user_id: &str,
    sub: &str,
) -> Result<(), ApiError> {
    sqlx::query("UPDATE users SET apple_sub_enc = $2, apple_sub_hmac = $3 WHERE id = $1")
        .bind(user_id)
        .bind(keys.seal_field("users", "apple_sub", user_id, sub))
        .bind(keys.index("apple_sub", sub))
        .execute(conn)
        .await?;
    Ok(())
}
