//! The read side of file history (spec §4.3, §8.2, §9.3): a file's archived
//! versions and the vault's trash. Restore is a client operation (only a
//! client can compute the manifest `hash` of a restored file), so there are
//! no write routes here.
//!
//! Route shapes: axum's `{*path}` must be the last segment and one prefix
//! holds one wildcard, so the version list lives under `/history/` and a
//! version blob under `/versions/{name}/`.

use axum::{
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use sqlx::Row;

use crate::{
    auth::Principal,
    blobs::{valid_path, Tier},
    db,
    error::ApiError,
    routes::vaults::require_member,
    AppState,
};

#[derive(Serialize)]
pub struct VersionInfo {
    /// The entry to pass to the blob route (`<unix>[-n]`).
    pub name: String,
    pub ts: u64,
    /// The sealed size on the server, a few bytes over the client blob.
    pub size: u64,
}

#[derive(Serialize)]
pub struct VersionsResponse {
    pub versions: Vec<VersionInfo>,
}

#[derive(Serialize)]
pub struct TrashEntry {
    pub path: String,
    #[serde(rename = "encPath")]
    pub enc_path: String,
    pub hash: String,
    pub size: u64,
    pub deleted_at: u64,
}

#[derive(Serialize)]
pub struct TrashResponse {
    pub entries: Vec<TrashEntry>,
}

async fn check(state: &AppState, principal: &Principal, vault_id: &str) -> Result<(), ApiError> {
    let mut conn = state.pool.acquire().await?;
    require_member(&mut conn, vault_id, &principal.user_id).await?;
    Ok(())
}

fn blob_response(bytes: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        bytes,
    )
        .into_response()
}

/// `GET /vaults/{id}/history/{*path}`: the archived versions of one path token.
pub async fn list_versions(
    State(state): State<AppState>,
    principal: Principal,
    Path((vault_id, path)): Path<(String, String)>,
) -> Result<Json<VersionsResponse>, ApiError> {
    check(&state, &principal, &vault_id).await?;
    if !valid_path(&path) {
        return Err(ApiError::not_found("not_found"));
    }
    let blobs = state.blobs.clone();
    let entries = tokio::task::spawn_blocking(move || {
        blobs.history_entries(Tier::Versions, &vault_id, &path)
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(Json(VersionsResponse {
        versions: entries
            .into_iter()
            .map(|entry| VersionInfo {
                name: entry.name,
                ts: entry.ts,
                size: entry.size,
            })
            .collect(),
    }))
}

/// `GET /vaults/{id}/versions/{name}/{*path}`: one archived version's blob.
pub async fn get_version(
    State(state): State<AppState>,
    principal: Principal,
    Path((vault_id, name, path)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    check(&state, &principal, &vault_id).await?;
    if !valid_path(&path) {
        return Err(ApiError::not_found("not_found"));
    }
    let blobs = state.blobs.clone();
    let (vid, p) = (vault_id.clone(), path.clone());
    let sealed =
        tokio::task::spawn_blocking(move || blobs.get_history(Tier::Versions, &vid, &p, &name))
            .await
            .map_err(ApiError::internal)??
            .ok_or_else(|| ApiError::not_found("not_found"))?;
    let bytes = state
        .keys
        .open_blob(&vault_id, &path, &sealed)
        .map_err(ApiError::internal)?;
    Ok(blob_response(bytes))
}

/// `GET /vaults/{id}/trash`: the manifest's tombstones. The volume's names
/// are one-way hashes, so the listing comes from `files WHERE deleted`; the
/// tombstone's `modified` is the server's receipt time of the delete.
pub async fn list_trash(
    State(state): State<AppState>,
    principal: Principal,
    Path(vault_id): Path<String>,
) -> Result<Json<TrashResponse>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    require_member(&mut conn, &vault_id, &principal.user_id).await?;
    let rows = sqlx::query(
        "SELECT path, hash, modified, size, enc_path FROM files
         WHERE vault_id = $1 AND deleted AND enc_path <> ''
         ORDER BY modified DESC, path ASC",
    )
    .bind(&vault_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Json(TrashResponse {
        entries: rows
            .into_iter()
            .map(|row| TrashEntry {
                path: row.get("path"),
                enc_path: row.get("enc_path"),
                hash: row.get("hash"),
                size: db::to_u64(row.get("size")),
                deleted_at: db::to_u64(row.get("modified")),
            })
            .collect(),
    }))
}

/// `GET /vaults/{id}/trash/{*path}`: the newest trashed blob of a path token.
pub async fn get_trash(
    State(state): State<AppState>,
    principal: Principal,
    Path((vault_id, path)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    check(&state, &principal, &vault_id).await?;
    if !valid_path(&path) {
        return Err(ApiError::not_found("not_found"));
    }
    let blobs = state.blobs.clone();
    let (vid, p) = (vault_id.clone(), path.clone());
    let sealed = tokio::task::spawn_blocking(move || {
        let newest = blobs
            .list_history(Tier::Trash, &vid, &p)?
            .into_iter()
            .next();
        match newest {
            Some(name) => blobs.get_history(Tier::Trash, &vid, &p, &name),
            None => Ok(None),
        }
    })
    .await
    .map_err(ApiError::internal)??
    .ok_or_else(|| ApiError::not_found("not_found"))?;
    let bytes = state
        .keys
        .open_blob(&vault_id, &path, &sealed)
        .map_err(ApiError::internal)?;
    Ok(blob_response(bytes))
}
