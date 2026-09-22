//! Devices: the physical machines of an account (spec §4.1). A device is
//! identified by a client-generated id the client keeps for good; the server
//! keeps one session per device and a sealed name.

use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};

use crate::{crypto::ServerKeys, db, error::ApiError};

const MAX_DEVICE_NAME: usize = 80;
const MAX_DEVICE_ID: usize = 64;

/// The platforms a client may report. `unknown` is transitional: a sign-in
/// that carries no `device` object (a v2 client) gets a synthesized device
/// until OBS-143 makes the object required.
pub const PLATFORMS: &[&str] = &["macos", "ios", "browser", "cli", "unknown"];

/// The `device` object of a sign-in body.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceBody {
    pub id: Option<String>,
    pub name: Option<String>,
    pub platform: Option<String>,
}

/// A validated device identity.
#[derive(Debug, Clone)]
pub struct DeviceIdentity {
    pub id: String,
    pub name: String,
    pub platform: String,
}

impl DeviceBody {
    pub fn validate(self) -> Result<DeviceIdentity, ApiError> {
        let id = self.id.as_deref().map(str::trim).unwrap_or("");
        if id.is_empty() || id.len() > MAX_DEVICE_ID || !valid_device_id(id) {
            return Err(ApiError::bad_request(
                "device.id must be 1-64 characters of letters, digits, '-' or '_'",
            ));
        }
        let platform = self
            .platform
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !PLATFORMS.contains(&platform.as_str()) {
            return Err(ApiError::bad_request(
                "device.platform must be one of macos, ios, browser, cli",
            ));
        }
        Ok(DeviceIdentity {
            id: id.to_string(),
            name: clean_device_name(self.name.as_deref()),
            platform,
        })
    }
}

/// The device a v2 client gets: one fresh row per sign-in, so the old
/// "every sign-in is a session" behaviour survives until the client moves.
pub fn legacy_device(device_name: Option<&str>) -> DeviceIdentity {
    DeviceIdentity {
        id: crate::crypto::new_id("legacy"),
        name: clean_device_name(device_name),
        platform: "unknown".to_string(),
    }
}

fn valid_device_id(id: &str) -> bool {
    id.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn clean_device_name(name: Option<&str>) -> String {
    let trimmed = name.map(str::trim).unwrap_or("");
    if trimmed.is_empty() {
        return "device".to_string();
    }
    trimmed.chars().take(MAX_DEVICE_NAME).collect()
}

/// What `GET /auth/me` says about a device.
#[derive(Debug, Serialize)]
pub struct DeviceSummary {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub created: u64,
    pub last_seen: u64,
    pub current: bool,
    pub vault_ids: Vec<String>,
}

/// Register the device at sign-in, or refresh its name and `last_seen`.
pub async fn upsert(
    conn: &mut PgConnection,
    keys: &ServerKeys,
    user_id: &str,
    device: &DeviceIdentity,
    now: u64,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO devices (user_id, id, name_enc, platform, created, last_seen)
         VALUES ($1, $2, $3, $4, $5, $5)
         ON CONFLICT (user_id, id) DO UPDATE SET name_enc = EXCLUDED.name_enc,
             platform = EXCLUDED.platform, last_seen = EXCLUDED.last_seen",
    )
    .bind(user_id)
    .bind(&device.id)
    .bind(seal_name(keys, user_id, &device.id, &device.name))
    .bind(&device.platform)
    .bind(db::to_i64(now))
    .execute(conn)
    .await?;
    Ok(())
}

fn seal_name(keys: &ServerKeys, user_id: &str, device_id: &str, name: &str) -> Vec<u8> {
    keys.seal_field("devices", "name", &format!("{user_id}:{device_id}"), name)
}

/// Every device of the account, oldest first, with the vaults each holds.
pub async fn list(
    conn: &mut PgConnection,
    keys: &ServerKeys,
    user_id: &str,
    current_device_id: &str,
) -> Result<Vec<DeviceSummary>, ApiError> {
    let rows = sqlx::query(
        "SELECT d.id, d.name_enc, d.platform, d.created, d.last_seen,
                COALESCE(array_agg(dv.vault_id ORDER BY dv.attached, dv.vault_id)
                         FILTER (WHERE dv.vault_id IS NOT NULL), '{}') AS vault_ids
         FROM devices d
         LEFT JOIN device_vaults dv ON dv.user_id = d.user_id AND dv.device_id = d.id
         WHERE d.user_id = $1
         GROUP BY d.id, d.name_enc, d.platform, d.created, d.last_seen
         ORDER BY d.created ASC, d.id ASC",
    )
    .bind(user_id)
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|row| {
            let id: String = row.get("id");
            let name = keys
                .open_field(
                    "devices",
                    "name",
                    &format!("{user_id}:{id}"),
                    &row.get::<Vec<u8>, _>("name_enc"),
                )
                .map_err(ApiError::internal)?;
            Ok(DeviceSummary {
                current: id == current_device_id,
                id,
                name,
                platform: row.get("platform"),
                created: db::to_u64(row.get("created")),
                last_seen: db::to_u64(row.get("last_seen")),
                vault_ids: row.get("vault_ids"),
            })
        })
        .collect()
}

/// Rename one of the account's devices. 404 when it is not theirs.
pub async fn rename(
    conn: &mut PgConnection,
    keys: &ServerKeys,
    user_id: &str,
    device_id: &str,
    name: Option<&str>,
) -> Result<(), ApiError> {
    let name = clean_device_name(name);
    let result = sqlx::query("UPDATE devices SET name_enc = $3 WHERE user_id = $1 AND id = $2")
        .bind(user_id)
        .bind(device_id)
        .bind(seal_name(keys, user_id, device_id, &name))
        .execute(conn)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("device not found"));
    }
    Ok(())
}

/// Sign a device out for good: its session, its `device_vaults` rows and the
/// row itself go through the cascades in one statement. 404 when it is not
/// the account's.
pub async fn delete(
    conn: &mut PgConnection,
    user_id: &str,
    device_id: &str,
) -> Result<(), ApiError> {
    let result = sqlx::query("DELETE FROM devices WHERE user_id = $1 AND id = $2")
        .bind(user_id)
        .bind(device_id)
        .execute(conn)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("device not found"));
    }
    Ok(())
}

/// The device a session belongs to (the `/auth/sessions/{id}` alias kept for
/// v2 clients until OBS-143).
pub async fn device_of_session(
    conn: &mut PgConnection,
    user_id: &str,
    session_id: &str,
) -> Result<Option<String>, ApiError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT device_id FROM sessions WHERE id = $1 AND user_id = $2")
            .bind(session_id)
            .bind(user_id)
            .fetch_optional(conn)
            .await?;
    Ok(row.map(|(device_id,)| device_id))
}

/// Bump `last_seen` (sign-in and the checkpoint report, never per request).
pub async fn touch(
    conn: &mut PgConnection,
    user_id: &str,
    device_id: &str,
    now: u64,
) -> Result<(), ApiError> {
    sqlx::query("UPDATE devices SET last_seen = $3 WHERE user_id = $1 AND id = $2")
        .bind(user_id)
        .bind(device_id)
        .bind(db::to_i64(now))
        .execute(conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_bodies_are_validated() {
        let ok = DeviceBody {
            id: Some(" dev-1_A ".into()),
            name: Some("  MacBook  ".into()),
            platform: Some("MacOS".into()),
        }
        .validate()
        .unwrap();
        assert_eq!(
            (ok.id.as_str(), ok.name.as_str(), ok.platform.as_str()),
            ("dev-1_A", "MacBook", "macos")
        );

        let bad_id = DeviceBody {
            id: Some("has space".into()),
            name: None,
            platform: Some("ios".into()),
        };
        assert!(bad_id.validate().is_err());
        let bad_platform = DeviceBody {
            id: Some("x".into()),
            name: None,
            platform: Some("windows".into()),
        };
        assert!(bad_platform.validate().is_err());
        let unnamed = DeviceBody {
            id: Some("x".into()),
            name: None,
            platform: Some("cli".into()),
        }
        .validate()
        .unwrap();
        assert_eq!(unnamed.name, "device");

        let legacy = legacy_device(Some("old phone"));
        assert!(legacy.id.starts_with("legacy_"));
        assert_eq!(legacy.platform, "unknown");
        assert_eq!(legacy.name, "old phone");
    }
}
