//! `POST /vaults/{id}/batch` as `multipart/form-data`.
//!
//! Parts: one `operations` part holding `{"operations":[...]}` (the Worker's
//! JSON minus the base64 `content`), then one `content` part per put with
//! `filename="<operation index>"`. Operations run sequentially, each in its own
//! transaction, and the response lists a status per operation (409s carry the
//! conflicting entry), so a batch can partly succeed.

use std::collections::HashMap;

use axum::{
    body::Bytes,
    extract::{
        multipart::{MultipartError, MultipartRejection},
        Multipart, Path, State,
    },
    http::StatusCode,
    Json,
};
use obsink_core::{BatchOperationResult, BatchResponse, ServerConflict};
use serde::Deserialize;

use crate::{
    auth::Principal,
    error::ApiError,
    routes::files::{apply_delete, apply_put, PutParams},
    AppState,
};

#[derive(Deserialize)]
struct WireOp {
    action: Option<String>,
    path: Option<String>,
    #[serde(rename = "parentHash")]
    parent_hash: Option<String>,
    #[serde(rename = "contentHash")]
    content_hash: Option<String>,
    #[serde(rename = "encPath")]
    enc_path: Option<String>,
}

fn multipart_error(error: MultipartError) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::status(StatusCode::PAYLOAD_TOO_LARGE, "batch too large")
    } else {
        ApiError::bad_request("malformed multipart body")
    }
}

/// One validated operation: the action is known, a put has its bytes, a
/// delete has none.
struct ParsedOp {
    path: String,
    parent_hash: Option<String>,
    kind: OpKind,
}

enum OpKind {
    Put {
        content_hash: Option<String>,
        enc_path: Option<String>,
        content: Bytes,
    },
    Delete,
}

pub async fn batch(
    State(state): State<AppState>,
    principal: Principal,
    Path(vault_id): Path<String>,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<Json<BatchResponse>, ApiError> {
    let multipart = multipart.map_err(|rejection| match rejection {
        MultipartRejection::InvalidBoundary(_) => ApiError::status(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "batch requires multipart/form-data",
        ),
        _ => ApiError::bad_request("malformed multipart body"),
    })?;
    let ops = parse_batch(multipart).await?;
    let results = apply_operations(&state, &principal, &vault_id, ops).await?;
    Ok(Json(BatchResponse { results }))
}

/// Read the parts, decode the operations JSON and check the whole request
/// before anything is applied: an invalid batch changes nothing.
async fn parse_batch(mut multipart: Multipart) -> Result<Vec<ParsedOp>, ApiError> {
    let mut operations: Option<String> = None;
    let mut contents: HashMap<usize, Bytes> = HashMap::new();
    while let Some(field) = multipart.next_field().await.map_err(multipart_error)? {
        match field.name() {
            Some("operations") => {
                if operations.is_some() {
                    return Err(ApiError::bad_request("duplicate operations part"));
                }
                operations = Some(field.text().await.map_err(multipart_error)?);
            }
            Some("content") => {
                let index: usize = field
                    .file_name()
                    .and_then(|name| name.parse().ok())
                    .ok_or_else(|| {
                        ApiError::bad_request("content part filename must be the operation index")
                    })?;
                let bytes = field.bytes().await.map_err(multipart_error)?;
                if contents.insert(index, bytes).is_some() {
                    return Err(ApiError::bad_request(format!(
                        "duplicate content part for operation {index}"
                    )));
                }
            }
            _ => return Err(ApiError::bad_request("unknown multipart part")),
        }
    }

    let operations =
        operations.ok_or_else(|| ApiError::bad_request("operations must be an array"))?;
    let parsed: serde_json::Value = serde_json::from_str(&operations)
        .map_err(|_| ApiError::bad_request("body must be JSON"))?;
    let ops = parsed
        .get("operations")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| ApiError::bad_request("operations must be an array"))?;
    let ops: Vec<WireOp> = ops
        .iter()
        .map(|op| {
            serde_json::from_value(op.clone())
                .map_err(|_| ApiError::bad_request("operations must be an array"))
        })
        .collect::<Result<_, _>>()?;

    if let Some(stray) = contents.keys().find(|index| **index >= ops.len()) {
        return Err(ApiError::bad_request(format!(
            "content part {stray} has no operation"
        )));
    }
    ops.into_iter()
        .enumerate()
        .map(|(index, op)| {
            // Only the two documented actions; anything else must not fall
            // through to the delete branch.
            let kind = match op.action.as_deref() {
                Some("put") => OpKind::Put {
                    content_hash: op.content_hash,
                    enc_path: op.enc_path,
                    content: contents.remove(&index).ok_or_else(|| {
                        ApiError::bad_request(format!("missing content part for operation {index}"))
                    })?,
                },
                Some("delete") => {
                    if contents.contains_key(&index) {
                        return Err(ApiError::bad_request(format!(
                            "operation {index} is not a put but has a content part"
                        )));
                    }
                    OpKind::Delete
                }
                other => {
                    return Err(ApiError::bad_request(format!(
                        "operation {index}: unknown action {:?}",
                        other.unwrap_or("")
                    )))
                }
            };
            Ok(ParsedOp {
                path: op.path.unwrap_or_default(),
                parent_hash: op.parent_hash,
                kind,
            })
        })
        .collect()
}

/// Run the operations in order, each in its own transaction, and map every
/// outcome to a per-operation status; only an internal error fails the
/// whole request.
async fn apply_operations(
    state: &AppState,
    principal: &Principal,
    vault_id: &str,
    ops: Vec<ParsedOp>,
) -> Result<Vec<BatchOperationResult>, ApiError> {
    let mut results = Vec::with_capacity(ops.len());
    for op in ops {
        let parent_hash = Some(op.parent_hash.as_deref().unwrap_or(""));
        let outcome = match &op.kind {
            OpKind::Put {
                content_hash,
                enc_path,
                content,
            } => {
                apply_put(
                    state,
                    principal,
                    PutParams {
                        vault_id,
                        path: &op.path,
                        parent_hash,
                        content_hash: content_hash.as_deref(),
                        enc_path: enc_path.as_deref(),
                    },
                    content,
                )
                .await
            }
            OpKind::Delete => apply_delete(state, principal, vault_id, &op.path, parent_hash).await,
        };
        let result = match outcome {
            Ok(()) => BatchOperationResult {
                path: op.path,
                status: 200,
                conflict: None,
            },
            Err(ApiError::Conflict { path, current }) => BatchOperationResult {
                path: path.clone(),
                status: 409,
                conflict: Some(ServerConflict { path, current }),
            },
            Err(ApiError::Status(code, _)) => BatchOperationResult {
                path: op.path,
                status: code.as_u16(),
                conflict: None,
            },
            Err(error @ ApiError::Internal(_)) => return Err(error),
        };
        results.push(result);
    }
    Ok(results)
}
