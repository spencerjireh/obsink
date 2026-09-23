//! `GET/POST /vaults`, `PATCH/DELETE /vaults/{id}`, `PUT/DELETE
//! /vaults/{id}/devices/self` (spec §4.3, §10). Every vault route is scoped
//! through `vault_members`: an account sees the vaults it is a member of and
//! only the owner may rename or delete.

use std::collections::HashMap;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use obsink_core::{
    decode_base64, encode_base64, CreateVaultResponse, ListVaultsResponse, VaultDevice,
    VaultSummary, WRAPPED_KEY_LEN,
};
use serde::Deserialize;
use sqlx::{PgConnection, Row};

use crate::{
    auth::{devices, keys, Principal},
    blobs::valid_vault_id,
    crypto, db,
    error::{ApiError, AppJson},
    AppState,
};

#[derive(Deserialize)]
pub struct CreateVaultBody {
    /// A client-minted id (`vault_` + 36 hex/dash characters) so the wrapped
    /// key's AAD is the real id. The server mints one when absent.
    pub id: Option<String>,
    pub name: Option<String>,
    pub max_file_size: Option<u64>,
    /// The vault key wrapped under the caller's account key (base64); required.
    pub wrapped_key: Option<String>,
}

#[derive(Deserialize)]
pub struct RenameVaultBody {
    pub name: Option<String>,
}

#[derive(Deserialize)]
pub struct AttachBody {
    /// The manifest revision a checkpoint just saved; absent on attach.
    pub revision: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Owner,
    Member,
}

/// The caller's role on a vault, or 404 when they are not a member (a vault
/// of another account is indistinguishable from a missing one).
pub async fn require_member(
    conn: &mut PgConnection,
    vault_id: &str,
    user_id: &str,
) -> Result<Role, ApiError> {
    if !valid_vault_id(vault_id) {
        return Err(ApiError::not_found("vault not found"));
    }
    let row: Option<(String,)> =
        sqlx::query_as("SELECT role FROM vault_members WHERE vault_id = $1 AND user_id = $2")
            .bind(vault_id)
            .bind(user_id)
            .fetch_optional(conn)
            .await?;
    match row.as_ref().map(|(role,)| role.as_str()) {
        Some("owner") => Ok(Role::Owner),
        Some(_) => Ok(Role::Member),
        None => Err(ApiError::not_found("vault not found")),
    }
}

async fn require_owner(
    conn: &mut PgConnection,
    vault_id: &str,
    user_id: &str,
) -> Result<(), ApiError> {
    match require_member(conn, vault_id, user_id).await? {
        Role::Owner => Ok(()),
        Role::Member => Err(ApiError::forbidden("only the owner can do that")),
    }
}

fn decode_wrapped_key(value: Option<&str>) -> Result<Option<Vec<u8>>, ApiError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let bytes =
        decode_base64(value).map_err(|_| ApiError::bad_request("wrapped_key must be base64"))?;
    if bytes.len() != WRAPPED_KEY_LEN {
        return Err(ApiError::bad_request(format!(
            "wrapped_key must decode to {WRAPPED_KEY_LEN} bytes"
        )));
    }
    Ok(Some(bytes))
}

/// Every vault the account is a member of, with the caller's wrapped key and
/// the devices (of any member) that hold it.
pub async fn list_for_member(
    conn: &mut PgConnection,
    state: &AppState,
    user_id: &str,
) -> Result<Vec<VaultSummary>, ApiError> {
    let rows = sqlx::query(
        "SELECT v.id, v.name_enc, v.created, v.max_file_size, v.revision, v.last_write,
                m.wrapped_key,
                COALESCE((SELECT SUM(f.size) FROM files f WHERE f.vault_id = v.id AND NOT f.deleted), 0)::BIGINT AS bytes
         FROM vaults v JOIN vault_members m ON m.vault_id = v.id
         WHERE m.user_id = $1 ORDER BY v.created ASC, v.id ASC",
    )
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await?;
    let device_rows = sqlx::query(
        "SELECT dv.vault_id, d.user_id, d.id, d.name_enc, d.platform, dv.last_synced, dv.last_revision, dv.attached
         FROM device_vaults dv
         JOIN vault_members m ON m.vault_id = dv.vault_id AND m.user_id = $1
         JOIN devices d ON d.user_id = dv.user_id AND d.id = dv.device_id
         ORDER BY dv.attached ASC, d.id ASC",
    )
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut devices_by_vault: HashMap<String, Vec<VaultDevice>> = HashMap::new();
    for row in device_rows {
        let vault_id: String = row.get("vault_id");
        let device_user: String = row.get("user_id");
        let id: String = row.get("id");
        let name = state
            .keys
            .open_field(
                "devices",
                "name",
                &format!("{device_user}:{id}"),
                &row.get::<Vec<u8>, _>("name_enc"),
            )
            .map_err(ApiError::internal)?;
        devices_by_vault
            .entry(vault_id)
            .or_default()
            .push(VaultDevice {
                id,
                name,
                platform: row.get("platform"),
                last_synced: row.get::<Option<i64>, _>("last_synced").map(db::to_u64),
                last_revision: row.get::<Option<i64>, _>("last_revision").map(db::to_u64),
            });
    }
    rows.into_iter()
        .map(|row| {
            let id: String = row.get("id");
            let name = state
                .keys
                .open_field("vaults", "name", &id, &row.get::<Vec<u8>, _>("name_enc"))
                .map_err(ApiError::internal)?;
            let devices = devices_by_vault.remove(&id).unwrap_or_default();
            Ok(VaultSummary {
                name,
                created: db::to_u64(row.get("created")),
                max_file_size: db::to_u64(row.get("max_file_size")),
                revision: db::to_u64(row.get("revision")),
                last_write: db::to_u64(row.get("last_write")),
                bytes: db::to_u64(row.get("bytes")),
                wrapped_key: row
                    .get::<Option<Vec<u8>>, _>("wrapped_key")
                    .map(|bytes| encode_base64(&bytes)),
                devices,
                id,
            })
        })
        .collect()
}

pub async fn list(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<ListVaultsResponse>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let vaults = list_for_member(&mut conn, &state, &principal.user_id).await?;
    Ok(Json(ListVaultsResponse { vaults }))
}

pub async fn create(
    State(state): State<AppState>,
    principal: Principal,
    AppJson(body): AppJson<CreateVaultBody>,
) -> Result<(StatusCode, Json<CreateVaultResponse>), ApiError> {
    let name = body.name.as_deref().map(str::trim).unwrap_or("");
    if name.is_empty() {
        return Err(ApiError::bad_request("vault name is required"));
    }
    let wrapped_key = decode_wrapped_key(body.wrapped_key.as_deref())?
        .ok_or_else(|| ApiError::bad_request("wrapped_key is required"))?;
    let id = match body
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) if valid_vault_id(id) => id.to_string(),
        Some(_) => {
            return Err(ApiError::bad_request(
                "id must be vault_ followed by a UUID",
            ))
        }
        None => crypto::new_id("vault"),
    };
    let max_file_size = body
        .max_file_size
        .unwrap_or(state.config.max_file_bytes)
        .min(state.config.max_file_bytes);
    let now = db::now();
    let owner = principal.user_id.as_str();

    let mut tx = state.pool.begin().await?;
    // Serialise creates per owner so the quota check cannot race.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(owner)
        .execute(&mut *tx)
        .await?;
    // The vault key is wrapped under the account key (spec §6.1): without a
    // passphrase there is nothing it could be wrapped under.
    if !keys::has_key(&mut tx, owner).await? {
        return Err(ApiError::bad_request("set a passphrase first"));
    }
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM vaults WHERE owner = $1")
        .bind(owner)
        .fetch_one(&mut *tx)
        .await?;
    if count >= i64::from(state.config.max_vaults_per_user) {
        return Err(ApiError::forbidden(format!(
            "vault limit reached ({} per account)",
            state.config.max_vaults_per_user
        )));
    }
    let taken: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM vaults WHERE id = $1")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?;
    if taken.is_some() {
        return Err(ApiError::status(
            StatusCode::CONFLICT,
            "a vault with that id already exists",
        ));
    }
    sqlx::query("INSERT INTO vaults (id, owner, name_enc, created, max_file_size, last_write) VALUES ($1, $2, $3, $4, $5, $4)")
        .bind(&id)
        .bind(owner)
        .bind(state.keys.seal_field("vaults", "name", &id, name))
        .bind(db::to_i64(now))
        .bind(db::to_i64(max_file_size))
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO vault_members (vault_id, user_id, role, wrapped_key, created) VALUES ($1, $2, 'owner', $3, $4)",
    )
    .bind(&id)
    .bind(owner)
    .bind(&wrapped_key)
    .bind(db::to_i64(now))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(CreateVaultResponse {
            vault: VaultSummary {
                id,
                name: name.to_string(),
                created: now,
                max_file_size,
                revision: 0,
                last_write: now,
                bytes: 0,
                wrapped_key: Some(encode_base64(&wrapped_key)),
                devices: Vec::new(),
            },
        }),
    ))
}

/// `PATCH /vaults/{id}`: rename for every device (owner only).
pub async fn rename(
    State(state): State<AppState>,
    principal: Principal,
    Path(vault_id): Path<String>,
    AppJson(body): AppJson<RenameVaultBody>,
) -> Result<StatusCode, ApiError> {
    let name = body.name.as_deref().map(str::trim).unwrap_or("");
    if name.is_empty() {
        return Err(ApiError::bad_request("vault name is required"));
    }
    let mut conn = state.pool.acquire().await?;
    require_owner(&mut conn, &vault_id, &principal.user_id).await?;
    sqlx::query("UPDATE vaults SET name_enc = $2 WHERE id = $1")
        .bind(&vault_id)
        .bind(state.keys.seal_field("vaults", "name", &vault_id, name))
        .execute(&mut *conn)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_vault(
    State(state): State<AppState>,
    principal: Principal,
    Path(vault_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let mut conn = state.pool.acquire().await?;
    require_owner(&mut conn, &vault_id, &principal.user_id).await?;
    let deleted = sqlx::query("DELETE FROM vaults WHERE id = $1 AND owner = $2")
        .bind(&vault_id)
        .bind(&principal.user_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::not_found("vault not found"));
    }
    drop(conn);
    let blobs = state.blobs.clone();
    tokio::task::spawn_blocking(move || blobs.delete_vault(&vault_id))
        .await
        .map_err(ApiError::internal)??;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /vaults/{id}/devices/self`: this device holds the vault (attach), or
/// just synced it to `revision` (the checkpoint report). Idempotent.
pub async fn attach_self(
    State(state): State<AppState>,
    principal: Principal,
    Path(vault_id): Path<String>,
    AppJson(body): AppJson<AttachBody>,
) -> Result<StatusCode, ApiError> {
    let now = db::now();
    let mut tx = state.pool.begin().await?;
    require_member(&mut tx, &vault_id, &principal.user_id).await?;
    match body.revision {
        Some(revision) => {
            sqlx::query(
                "INSERT INTO device_vaults (user_id, device_id, vault_id, attached, last_synced, last_revision)
                 VALUES ($1, $2, $3, $4, $4, $5)
                 ON CONFLICT (user_id, device_id, vault_id)
                 DO UPDATE SET last_synced = EXCLUDED.last_synced, last_revision = EXCLUDED.last_revision",
            )
            .bind(&principal.user_id)
            .bind(&principal.device_id)
            .bind(&vault_id)
            .bind(db::to_i64(now))
            .bind(db::to_i64(revision))
            .execute(&mut *tx)
            .await?;
        }
        None => {
            sqlx::query(
                "INSERT INTO device_vaults (user_id, device_id, vault_id, attached)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (user_id, device_id, vault_id) DO NOTHING",
            )
            .bind(&principal.user_id)
            .bind(&principal.device_id)
            .bind(&vault_id)
            .bind(db::to_i64(now))
            .execute(&mut *tx)
            .await?;
        }
    }
    devices::touch(&mut tx, &principal.user_id, &principal.device_id, now).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /vaults/{id}/devices/self`: this device no longer holds the vault.
pub async fn detach_self(
    State(state): State<AppState>,
    principal: Principal,
    Path(vault_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let mut conn = state.pool.acquire().await?;
    require_member(&mut conn, &vault_id, &principal.user_id).await?;
    sqlx::query(
        "DELETE FROM device_vaults WHERE user_id = $1 AND device_id = $2 AND vault_id = $3",
    )
    .bind(&principal.user_id)
    .bind(&principal.device_id)
    .bind(&vault_id)
    .execute(&mut *conn)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Delete every vault an account owns (account deletion). Returns the ids so
/// the caller can remove blob directories after commit.
pub async fn delete_all_for_owner(
    conn: &mut PgConnection,
    user_id: &str,
) -> Result<Vec<String>, ApiError> {
    let rows: Vec<(String,)> = sqlx::query_as("DELETE FROM vaults WHERE owner = $1 RETURNING id")
        .bind(user_id)
        .fetch_all(conn)
        .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}
