//! `GET/POST /vaults`, `DELETE /vaults/{id}` (spec §4.3, §10).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use obsink_core::{CreateVaultResponse, VaultSummary};
use serde::Deserialize;
use sqlx::{PgConnection, Row};

use crate::{
    auth::Principal,
    blobs::valid_vault_id,
    crypto, db,
    error::{ApiError, AppJson},
    AppState,
};

#[derive(Deserialize)]
pub struct CreateVaultBody {
    pub name: Option<String>,
    pub max_file_size: Option<u64>,
}

pub async fn list_for_tenant(
    conn: &mut PgConnection,
    state: &AppState,
    tenant: &str,
) -> Result<Vec<VaultSummary>, ApiError> {
    let rows = sqlx::query("SELECT id, name_enc, created, max_file_size FROM vaults WHERE tenant = $1 ORDER BY created ASC, id ASC")
        .bind(tenant)
        .fetch_all(conn)
        .await?;
    rows.into_iter()
        .map(|row| {
            let id: String = row.get("id");
            let name = state
                .keys
                .open_field("vaults", "name", &id, &row.get::<Vec<u8>, _>("name_enc"))
                .map_err(ApiError::internal)?;
            Ok(VaultSummary {
                id,
                name,
                created: db::to_u64(row.get("created")),
                max_file_size: db::to_u64(row.get("max_file_size")),
            })
        })
        .collect()
}

pub async fn list(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<VaultSummary>>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    Ok(Json(
        list_for_tenant(&mut conn, &state, principal.tenant()).await?,
    ))
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
    let max_file_size = body
        .max_file_size
        .unwrap_or(state.config.max_file_bytes)
        .min(state.config.max_file_bytes);
    let now = db::now();
    let tenant = principal.tenant();

    let mut tx = state.pool.begin().await?;
    // Serialise creates per tenant so the quota check cannot race.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(tenant)
        .execute(&mut *tx)
        .await?;
    if principal.is_user() {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM vaults WHERE tenant = $1")
            .bind(tenant)
            .fetch_one(&mut *tx)
            .await?;
        if count >= i64::from(state.config.max_vaults_per_user) {
            return Err(ApiError::forbidden(format!(
                "vault limit reached ({} per account)",
                state.config.max_vaults_per_user
            )));
        }
    }
    let id = crypto::new_id("vault");
    sqlx::query("INSERT INTO vaults (id, tenant, name_enc, created, max_file_size) VALUES ($1, $2, $3, $4, $5)")
        .bind(&id)
        .bind(tenant)
        .bind(state.keys.seal_field("vaults", "name", &id, name))
        .bind(db::to_i64(now))
        .bind(db::to_i64(max_file_size))
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
            },
        }),
    ))
}

pub async fn delete_vault(
    State(state): State<AppState>,
    principal: Principal,
    Path(vault_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if !valid_vault_id(&vault_id) {
        return Err(ApiError::not_found("vault not found"));
    }
    let deleted = sqlx::query("DELETE FROM vaults WHERE id = $1 AND tenant = $2")
        .bind(&vault_id)
        .bind(principal.tenant())
        .execute(&state.pool)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::not_found("vault not found"));
    }
    let blobs = state.blobs.clone();
    tokio::task::spawn_blocking(move || blobs.delete_vault(&vault_id))
        .await
        .map_err(ApiError::internal)??;
    Ok(StatusCode::NO_CONTENT)
}

/// Delete every vault a tenant owns (account deletion). Returns the ids so the
/// caller can remove blob directories after commit.
pub async fn delete_all_for_tenant(
    conn: &mut PgConnection,
    tenant: &str,
) -> Result<Vec<String>, ApiError> {
    let rows: Vec<(String,)> = sqlx::query_as("DELETE FROM vaults WHERE tenant = $1 RETURNING id")
        .bind(tenant)
        .fetch_all(conn)
        .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}
