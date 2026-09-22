//! Account routes: `/auth/me`, devices, account deletion, invites.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};

use crate::{
    auth::{
        devices::{self, DeviceSummary},
        invites::{self, InviteInfo, INVITE_TTL_SECS},
        users, Principal,
    },
    db,
    error::{ApiError, AppJson},
    routes::vaults,
    AppState,
};

#[derive(Serialize)]
pub struct MeResponse {
    pub user: MeUser,
    pub devices: Vec<DeviceSummary>,
    pub usage: Usage,
}

#[derive(Serialize)]
pub struct MeUser {
    pub id: String,
    pub email: Option<String>,
    pub created: u64,
}

#[derive(Serialize)]
pub struct Usage {
    pub vaults: Vec<VaultUsage>,
    pub total_bytes: u64,
    pub max_vault_bytes: Option<u64>,
    pub max_vaults: Option<u32>,
}

#[derive(Serialize)]
pub struct VaultUsage {
    pub id: String,
    pub bytes: u64,
}

/// Bytes per vault the account owns (quotas count against the owner).
pub async fn usage_for(
    conn: &mut PgConnection,
    state: &AppState,
    user_id: &str,
) -> Result<Usage, ApiError> {
    let rows = sqlx::query(
        "SELECT v.id, COALESCE(SUM(f.size) FILTER (WHERE NOT f.deleted), 0)::BIGINT AS bytes
         FROM vaults v LEFT JOIN files f ON f.vault_id = v.id
         WHERE v.owner = $1 GROUP BY v.id, v.created ORDER BY v.created ASC, v.id ASC",
    )
    .bind(user_id)
    .fetch_all(conn)
    .await?;
    let vaults: Vec<VaultUsage> = rows
        .into_iter()
        .map(|row| VaultUsage {
            id: row.get("id"),
            bytes: db::to_u64(row.get("bytes")),
        })
        .collect();
    Ok(Usage {
        total_bytes: vaults.iter().map(|vault| vault.bytes).sum(),
        vaults,
        max_vault_bytes: Some(state.config.max_vault_bytes),
        max_vaults: Some(state.config.max_vaults_per_user),
    })
}

pub async fn me(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<MeResponse>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let usage = usage_for(&mut conn, &state, &principal.user_id).await?;
    let user = users::find_by_id(&mut conn, &state.keys, &principal.user_id)
        .await?
        .map(|user| MeUser {
            id: user.id,
            email: user.email,
            created: user.created,
        })
        .ok_or_else(|| ApiError::unauthorized("unauthorized"))?;
    let devices = devices::list(
        &mut conn,
        &state.keys,
        &principal.user_id,
        &principal.device_id,
    )
    .await?;
    Ok(Json(MeResponse {
        user,
        devices,
        usage,
    }))
}

/// Sign this device out: its session and its vault attachments go with it.
pub async fn sign_out(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<StatusCode, ApiError> {
    let mut conn = state.pool.acquire().await?;
    devices::delete(&mut conn, &principal.user_id, &principal.device_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /auth/devices/{device_id}`: sign another device out for good.
pub async fn revoke_device(
    State(state): State<AppState>,
    principal: Principal,
    Path(device_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let mut conn = state.pool.acquire().await?;
    devices::delete(&mut conn, &principal.user_id, &device_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /auth/sessions/{session_id}`: the v2 spelling of revoking a
/// device, resolved through the session. Removed in OBS-143.
pub async fn revoke_session(
    State(state): State<AppState>,
    principal: Principal,
    Path(session_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let device_id = devices::device_of_session(&mut conn, &principal.user_id, &session_id)
        .await?
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    devices::delete(&mut conn, &principal.user_id, &device_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct RenameDeviceBody {
    pub name: Option<String>,
}

/// `PATCH /auth/devices/{device_id}`: rename from any device.
pub async fn rename_device(
    State(state): State<AppState>,
    principal: Principal,
    Path(device_id): Path<String>,
    AppJson(body): AppJson<RenameDeviceBody>,
) -> Result<StatusCode, ApiError> {
    let mut conn = state.pool.acquire().await?;
    devices::rename(
        &mut conn,
        &state.keys,
        &principal.user_id,
        &device_id,
        body.name.as_deref(),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// App Store guideline 5.1.1(v): the account, its devices and sessions,
/// invites, wrapped keys and every vault it owns go in one transaction; blob
/// directories follow after commit.
pub async fn delete_account(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.pool.begin().await?;
    let vault_ids = vaults::delete_all_for_owner(&mut tx, &principal.user_id).await?;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(&principal.user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let blobs = state.blobs.clone();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        for id in &vault_ids {
            blobs.delete_vault(id)?;
        }
        Ok(())
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct InviteResponse {
    pub invite: InviteInfo,
}

#[derive(Serialize)]
pub struct InviteList {
    pub invites: Vec<InviteInfo>,
}

pub async fn create_invite(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<(StatusCode, Json<InviteResponse>), ApiError> {
    let mut conn = state.pool.acquire().await?;
    let invite = invites::create(
        &mut conn,
        Some(&principal.user_id),
        db::now(),
        INVITE_TTL_SECS,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(InviteResponse { invite })))
}

pub async fn list_invites(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<InviteList>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let invites = invites::list(&mut conn, Some(&principal.user_id), db::now()).await?;
    Ok(Json(InviteList { invites }))
}
