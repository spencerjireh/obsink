//! Postgres pool, migrations, and small shared helpers.

use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::{postgres::PgPoolOptions, PgPool};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
}

pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}

/// Unix seconds. All timestamps on the wire and in the database are seconds.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Postgres has no unsigned integers; sizes and timestamps are stored as
/// BIGINT and clamped on the way back.
pub fn to_u64(value: i64) -> u64 {
    value.max(0) as u64
}

pub fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
