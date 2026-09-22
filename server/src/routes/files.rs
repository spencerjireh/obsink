//! Manifest and per-file routes with the conflict gate (spec §4.3, §5.1).
//!
//! Every write runs in one Postgres transaction that locks the vault row, so
//! two devices racing on the same path get exactly one 200 and one 409, and
//! the per-vault byte budget is checked under the same lock.

use axum::{
    body::{to_bytes, Body},
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use obsink_core::{FileEntry, Manifest};
use sqlx::{PgConnection, Row};

use crate::{
    auth::Principal,
    blobs::{valid_path, valid_vault_id},
    db,
    error::ApiError,
    AppState,
};

pub const QUOTA_MESSAGE: &str = "vault storage limit reached";

/// Vault row fields a write needs, read under `FOR UPDATE`.
struct LockedVault {
    max_file_size: u64,
}

async fn lock_vault(
    conn: &mut PgConnection,
    vault_id: &str,
    user_id: &str,
) -> Result<LockedVault, ApiError> {
    if !valid_vault_id(vault_id) {
        return Err(ApiError::not_found("vault not found"));
    }
    let row =
        sqlx::query("SELECT max_file_size FROM vaults WHERE id = $1 AND owner = $2 FOR UPDATE")
            .bind(vault_id)
            .bind(user_id)
            .fetch_optional(conn)
            .await?
            .ok_or_else(|| ApiError::not_found("vault not found"))?;
    Ok(LockedVault {
        max_file_size: db::to_u64(row.get("max_file_size")),
    })
}

async fn require_vault(
    conn: &mut PgConnection,
    vault_id: &str,
    user_id: &str,
) -> Result<(), ApiError> {
    if !valid_vault_id(vault_id) {
        return Err(ApiError::not_found("vault not found"));
    }
    let exists: Option<(i32,)> =
        sqlx::query_as("SELECT 1 FROM vaults WHERE id = $1 AND owner = $2")
            .bind(vault_id)
            .bind(user_id)
            .fetch_optional(conn)
            .await?;
    exists
        .map(|_| ())
        .ok_or_else(|| ApiError::not_found("vault not found"))
}

fn entry_from_row(row: &sqlx::postgres::PgRow) -> FileEntry {
    FileEntry {
        hash: row.get("hash"),
        modified: db::to_u64(row.get("modified")),
        size: db::to_u64(row.get("size")),
        deleted: row.get("deleted"),
        enc_path: row.get("enc_path"),
    }
}

async fn current_entry(
    conn: &mut PgConnection,
    vault_id: &str,
    path: &str,
) -> Result<Option<FileEntry>, ApiError> {
    let row = sqlx::query("SELECT hash, modified, size, deleted, enc_path FROM files WHERE vault_id = $1 AND path = $2")
        .bind(vault_id)
        .bind(path)
        .fetch_optional(conn)
        .await?;
    Ok(row.as_ref().map(entry_from_row))
}

async fn upsert_entry(
    conn: &mut PgConnection,
    vault_id: &str,
    path: &str,
    entry: &FileEntry,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO files (vault_id, path, hash, modified, size, deleted, enc_path)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (vault_id, path) DO UPDATE SET hash = EXCLUDED.hash, modified = EXCLUDED.modified,
             size = EXCLUDED.size, deleted = EXCLUDED.deleted, enc_path = EXCLUDED.enc_path",
    )
    .bind(vault_id)
    .bind(path)
    .bind(&entry.hash)
    .bind(db::to_i64(entry.modified))
    .bind(db::to_i64(entry.size))
    .bind(entry.deleted)
    .bind(&entry.enc_path)
    .execute(conn)
    .await?;
    Ok(())
}

async fn bump_revision(conn: &mut PgConnection, vault_id: &str) -> Result<(), ApiError> {
    sqlx::query("UPDATE vaults SET revision = revision + 1 WHERE id = $1")
        .bind(vault_id)
        .execute(conn)
        .await?;
    Ok(())
}

pub struct PutParams<'a> {
    pub vault_id: &'a str,
    pub path: &'a str,
    pub parent_hash: Option<&'a str>,
    pub content_hash: Option<&'a str>,
    pub enc_path: Option<&'a str>,
}

/// The conflict-gated upload, shared by `PUT` and the batch endpoint.
pub async fn apply_put(
    state: &AppState,
    principal: &Principal,
    params: PutParams<'_>,
    body: &[u8],
) -> Result<(), ApiError> {
    if !valid_path(params.path) {
        return Err(ApiError::bad_request("invalid path"));
    }
    let content_hash = params
        .content_hash
        .map(str::trim)
        .filter(|hash| !hash.is_empty())
        .ok_or_else(|| ApiError::bad_request("missing X-Content-Hash header"))?;
    let now = db::now();
    let size = body.len() as u64;

    let mut tx = state.pool.begin().await?;
    let vault = lock_vault(&mut tx, params.vault_id, &principal.user_id).await?;
    let current = current_entry(&mut tx, params.vault_id, params.path).await?;
    if size > vault.max_file_size {
        return Err(ApiError::status(
            StatusCode::PAYLOAD_TOO_LARGE,
            "file too large",
        ));
    }
    let (used,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(size), 0)::BIGINT FROM files WHERE vault_id = $1 AND NOT deleted AND path <> $2",
    )
    .bind(params.vault_id)
    .bind(params.path)
    .fetch_one(&mut *tx)
    .await?;
    if db::to_u64(used) + size > state.config.max_vault_bytes {
        return Err(ApiError::status(
            StatusCode::INSUFFICIENT_STORAGE,
            QUOTA_MESSAGE,
        ));
    }
    if let Some(current) = &current {
        if current.hash != params.parent_hash.unwrap_or("") {
            // A retried upload whose first attempt landed: same bytes are
            // already there, so report success instead of a spurious conflict.
            if !current.deleted && current.hash == content_hash {
                return Ok(());
            }
            return Err(ApiError::Conflict {
                path: params.path.to_string(),
                current: Some(current.clone()),
            });
        }
    }

    let enc_path = params
        .enc_path
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| current.as_ref().map(|entry| entry.enc_path.clone()))
        .unwrap_or_default();
    let sealed = state.keys.seal_blob(params.vault_id, params.path, body);
    // The blob changes under the vault row lock so concurrent writers to the
    // same path serialise; if the metadata then fails to commit, the blob is
    // put back so the row and the file never disagree.
    let undo = {
        let blobs = state.blobs.clone();
        let vault_id = params.vault_id.to_string();
        let path = params.path.to_string();
        let archive = current.as_ref().is_some_and(|entry| !entry.deleted);
        tokio::task::spawn_blocking(move || {
            blobs.replace_live(&vault_id, &path, &sealed, archive, now)
        })
        .await
        .map_err(ApiError::internal)??
    };
    let committed = async {
        upsert_entry(
            &mut tx,
            params.vault_id,
            params.path,
            &FileEntry {
                hash: content_hash.to_string(),
                modified: now,
                size,
                deleted: false,
                enc_path,
            },
        )
        .await?;
        bump_revision(&mut tx, params.vault_id).await?;
        tx.commit().await?;
        Ok::<(), ApiError>(())
    }
    .await;
    if let Err(error) = committed {
        let blobs = state.blobs.clone();
        let _ = tokio::task::spawn_blocking(move || blobs.undo(undo)).await;
        return Err(error);
    }
    Ok(())
}

/// The conflict-gated soft delete, shared by `DELETE` and the batch endpoint.
pub async fn apply_delete(
    state: &AppState,
    principal: &Principal,
    vault_id: &str,
    path: &str,
    parent_hash: Option<&str>,
) -> Result<(), ApiError> {
    if !valid_path(path) {
        return Err(ApiError::bad_request("invalid path"));
    }
    let now = db::now();
    let mut tx = state.pool.begin().await?;
    lock_vault(&mut tx, vault_id, &principal.user_id).await?;
    let current = current_entry(&mut tx, vault_id, path).await?;
    if let Some(current) = &current {
        if current.hash != parent_hash.unwrap_or("") {
            return Err(ApiError::Conflict {
                path: path.to_string(),
                current: Some(current.clone()),
            });
        }
    }
    let undo = {
        let blobs = state.blobs.clone();
        let vault_id = vault_id.to_string();
        let path = path.to_string();
        tokio::task::spawn_blocking(move || blobs.trash_live(&vault_id, &path, now))
            .await
            .map_err(ApiError::internal)??
    };
    // Deleting a never-uploaded path still leaves a tombstone, as the Worker did.
    let tombstone = FileEntry {
        hash: current
            .as_ref()
            .map(|entry| entry.hash.clone())
            .unwrap_or_default(),
        modified: now,
        size: current.as_ref().map(|entry| entry.size).unwrap_or(0),
        deleted: true,
        enc_path: current
            .as_ref()
            .map(|entry| entry.enc_path.clone())
            .unwrap_or_default(),
    };
    let committed = async {
        upsert_entry(&mut tx, vault_id, path, &tombstone).await?;
        bump_revision(&mut tx, vault_id).await?;
        tx.commit().await?;
        Ok::<(), ApiError>(())
    }
    .await;
    if let Err(error) = committed {
        let blobs = state.blobs.clone();
        let _ = tokio::task::spawn_blocking(move || blobs.undo(undo)).await;
        return Err(error);
    }
    Ok(())
}

// --- Handlers ------------------------------------------------------------------

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn if_none_match_hits(headers: &HeaderMap, etag: &str) -> bool {
    header_str(headers, "if-none-match")
        .map(|value| {
            value.split(',').any(|candidate| {
                let candidate = candidate.trim();
                let candidate = candidate.strip_prefix("W/").unwrap_or(candidate);
                candidate == "*" || candidate.trim_matches('"') == etag.trim_matches('"')
            })
        })
        .unwrap_or(false)
}

pub async fn get_manifest(
    State(state): State<AppState>,
    principal: Principal,
    Path(vault_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if !valid_vault_id(&vault_id) {
        return Err(ApiError::not_found("vault not found"));
    }
    let mut tx = state.pool.begin().await?;
    // Snapshot: the revision read here must not be newer than the rows below.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let revision: Option<(i64,)> =
        sqlx::query_as("SELECT revision FROM vaults WHERE id = $1 AND owner = $2")
            .bind(&vault_id)
            .bind(&principal.user_id)
            .fetch_optional(&mut *tx)
            .await?;
    let (revision,) = revision.ok_or_else(|| ApiError::not_found("vault not found"))?;
    let etag = format!("\"{revision}\"");
    let etag_value = HeaderValue::from_str(&etag).map_err(ApiError::internal)?;
    let cache_control = HeaderValue::from_static("private, no-cache");
    if if_none_match_hits(&headers, &etag) {
        return Ok((
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag_value),
                (header::CACHE_CONTROL, cache_control),
            ],
        )
            .into_response());
    }
    let rows = sqlx::query(
        "SELECT path, hash, modified, size, deleted, enc_path FROM files WHERE vault_id = $1",
    )
    .bind(&vault_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let manifest: Manifest = rows
        .iter()
        .map(|row| (row.get::<String, _>("path"), entry_from_row(row)))
        .collect();
    Ok((
        StatusCode::OK,
        [
            (header::ETAG, etag_value),
            (header::CACHE_CONTROL, cache_control),
        ],
        Json(manifest),
    )
        .into_response())
}

pub async fn get_file(
    State(state): State<AppState>,
    principal: Principal,
    Path((vault_id, path)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    {
        let mut conn = state.pool.acquire().await?;
        require_vault(&mut conn, &vault_id, &principal.user_id).await?;
    }
    if !valid_path(&path) {
        return Err(ApiError::not_found("not_found"));
    }
    let blobs = state.blobs.clone();
    let (vid, p) = (vault_id.clone(), path.clone());
    let sealed = tokio::task::spawn_blocking(move || blobs.get_live(&vid, &p))
        .await
        .map_err(ApiError::internal)??
        .ok_or_else(|| ApiError::not_found("not_found"))?;
    let bytes = state
        .keys
        .open_blob(&vault_id, &path, &sealed)
        .map_err(ApiError::internal)?;
    Ok((
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
        .into_response())
}

pub async fn put_file(
    State(state): State<AppState>,
    principal: Principal,
    Path((vault_id, path)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, ApiError> {
    let limit = usize::try_from(state.config.max_file_bytes).unwrap_or(usize::MAX);
    let bytes = to_bytes(body, limit)
        .await
        .map_err(|_| ApiError::status(StatusCode::PAYLOAD_TOO_LARGE, "file too large"))?;
    apply_put(
        &state,
        &principal,
        PutParams {
            vault_id: &vault_id,
            path: &path,
            parent_hash: header_str(&headers, "x-parent-hash"),
            content_hash: header_str(&headers, "x-content-hash"),
            enc_path: header_str(&headers, "x-enc-path"),
        },
        &bytes,
    )
    .await?;
    Ok(StatusCode::OK)
}

pub async fn delete_file(
    State(state): State<AppState>,
    principal: Principal,
    Path((vault_id, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    apply_delete(
        &state,
        &principal,
        &vault_id,
        &path,
        header_str(&headers, "x-parent-hash"),
    )
    .await?;
    Ok(StatusCode::OK)
}
