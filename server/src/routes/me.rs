//! Account routes: `/auth/me`, session revocation, account deletion, invites.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Serialize;
use sqlx::{PgConnection, Row};

use crate::{
    auth::{
        invites::{self, InviteInfo, INVITE_TTL_SECS},
        sessions::{self, SessionSummary},
        users, Principal,
    },
    db,
    error::ApiError,
    routes::vaults,
    AppState,
};

#[derive(Serialize)]
pub struct MeResponse {
    pub kind: &'static str,
    pub user: Option<MeUser>,
    pub sessions: Vec<SessionSummary>,
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
    /// `null` for the operator: limits apply to accounts only.
    pub max_vault_bytes: Option<u64>,
    pub max_vaults: Option<u32>,
}

#[derive(Serialize)]
pub struct VaultUsage {
    pub id: String,
    pub bytes: u64,
}

async fn usage_for(
    conn: &mut PgConnection,
    state: &AppState,
    principal: &Principal,
) -> Result<Usage, ApiError> {
    let rows = sqlx::query(
        "SELECT v.id, COALESCE(SUM(f.size) FILTER (WHERE NOT f.deleted), 0)::BIGINT AS bytes
         FROM vaults v LEFT JOIN files f ON f.vault_id = v.id
         WHERE v.tenant = $1 GROUP BY v.id, v.created ORDER BY v.created ASC, v.id ASC",
    )
    .bind(principal.tenant())
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
        max_vault_bytes: principal.is_user().then_some(state.config.max_vault_bytes),
        max_vaults: principal
            .is_user()
            .then_some(state.config.max_vaults_per_user),
    })
}

pub async fn me(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<MeResponse>, ApiError> {
    let now = db::now();
    let mut conn = state.pool.acquire().await?;
    let usage = usage_for(&mut conn, &state, &principal).await?;
    match &principal {
        Principal::Operator => Ok(Json(MeResponse {
            kind: "operator",
            user: None,
            sessions: Vec::new(),
            usage,
        })),
        Principal::User {
            user_id,
            session_id,
        } => {
            let user = users::find_by_id(&mut conn, &state.keys, user_id)
                .await?
                .map(|user| MeUser {
                    id: user.id,
                    email: user.email,
                    created: user.created,
                });
            let sessions = sessions::list(&mut conn, &state.keys, user_id, session_id, now).await?;
            Ok(Json(MeResponse {
                kind: "user",
                user,
                sessions,
                usage,
            }))
        }
    }
}

pub async fn sign_out(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<StatusCode, ApiError> {
    let Principal::User {
        user_id,
        session_id,
    } = &principal
    else {
        return Err(ApiError::bad_request("operator bearer has no session"));
    };
    let mut conn = state.pool.acquire().await?;
    sessions::revoke(&mut conn, user_id, session_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn revoke_session(
    State(state): State<AppState>,
    principal: Principal,
    Path(target): Path<String>,
) -> Result<StatusCode, ApiError> {
    let Principal::User { user_id, .. } = &principal else {
        return Err(ApiError::bad_request("operator bearer has no session"));
    };
    let mut conn = state.pool.acquire().await?;
    sessions::revoke(&mut conn, user_id, &target).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// App Store guideline 5.1.1(v): the account, its sessions, invites, and every
/// vault it owns go in one transaction; blob directories follow after commit.
pub async fn delete_account(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<StatusCode, ApiError> {
    let Principal::User { user_id, .. } = &principal else {
        return Err(ApiError::bad_request("operator bearer has no account"));
    };
    let mut tx = state.pool.begin().await?;
    let vault_ids = vaults::delete_all_for_tenant(&mut tx, user_id).await?;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
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
    let invite =
        invites::create(&mut conn, principal.user_id(), db::now(), INVITE_TTL_SECS).await?;
    Ok((StatusCode::CREATED, Json(InviteResponse { invite })))
}

pub async fn list_invites(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<InviteList>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let invites = invites::list(&mut conn, principal.user_id(), db::now()).await?;
    Ok(Json(InviteList { invites }))
}
