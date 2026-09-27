//! User rows. The email is sealed; lookups use the keyed index. The two
//! `apple_sub_*` columns of 0.4 remain in the schema, unused.

use sqlx::{PgConnection, Row};

use crate::{crypto::ServerKeys, db, error::ApiError};

#[derive(Debug, Clone)]
pub struct UserRecord {
    pub id: String,
    pub email: Option<String>,
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
    Ok(UserRecord {
        id,
        email,
        created: db::to_u64(row.get("created")),
    })
}

const COLUMNS: &str = "id, email_enc, created";

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

pub async fn count(conn: &mut PgConnection) -> Result<i64, ApiError> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(conn)
        .await?;
    Ok(count)
}

pub async fn insert(
    conn: &mut PgConnection,
    keys: &ServerKeys,
    email: &str,
    now: u64,
) -> Result<UserRecord, ApiError> {
    let id = crate::crypto::new_id("usr");
    sqlx::query(
        "INSERT INTO users (id, email_enc, email_hmac, created)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(&id)
    .bind(keys.seal_field("users", "email", &id, email))
    .bind(keys.index("email", email))
    .bind(db::to_i64(now))
    .execute(conn)
    .await?;
    Ok(UserRecord {
        id,
        email: Some(email.to_string()),
        created: now,
    })
}
