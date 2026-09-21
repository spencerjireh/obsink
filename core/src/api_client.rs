use std::time::Duration;

use reqwest::{RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use thiserror::Error;
use tracing::{debug, warn};

use crate::crypto::{decrypt_path, encrypt_path, path_token, CryptoError, CryptoKeys};
use crate::types::{
    BatchOp, BatchOperationResult, BatchResponse, CreateVaultRequest, CreateVaultResponse,
    Manifest, ServerConflict, VaultConfig, VaultSummary,
};

/// Whole-request budget for the small metadata calls (manifest, vault list,
/// delete). Blob transfers get no total budget: a 50 MB upload on a slow
/// uplink legitimately takes minutes, so they rely on the inactivity timeouts.
const SMALL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Time to establish a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Inactivity timeout between bytes of a response; a stalled transfer fails
/// here instead of hanging.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// Total attempts (1 initial + retries) for transient network failures.
const MAX_ATTEMPTS: u32 = 3;
/// Whole-request ceiling for a batch upload: up to 32 MiB of ciphertext that
/// the server applies operation by operation.
const BATCH_REQUEST_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Clone)]
pub struct ApiClient {
    config: VaultConfig,
    client: reqwest::Client,
}

impl std::fmt::Debug for ApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiClient")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("api conflict on {path}")]
    Conflict {
        path: String,
        conflict: ServerConflict,
    },
    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),
    /// 401: the bearer (a session token or the operator `API_KEY`) was
    /// rejected — the session may have been revoked or expired.
    #[error("unauthorized: sign in again")]
    Unauthorized,
    #[error("unexpected status {status}: {body}")]
    UnexpectedStatus { status: StatusCode, body: String },
}

impl ApiClient {
    pub fn new(config: VaultConfig) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { config, client }
    }

    /// Send a request, retrying transient network failures (timeouts and
    /// connection errors) with exponential backoff. Non-idempotent risk is
    /// acceptable here: the server's parent-hash check makes retried PUT/DELETE
    /// either succeed or surface a 409, never silently duplicate.
    async fn send_with_retry(&self, builder: RequestBuilder) -> Result<Response, ApiError> {
        let mut attempt: u32 = 0;
        loop {
            let Some(clone) = builder.try_clone() else {
                // Non-cloneable body (streaming): a single attempt is all we can do.
                return builder.send().await.map_err(ApiError::from);
            };

            match clone.send().await {
                Ok(response) => return Ok(response),
                Err(error) => {
                    attempt += 1;
                    let transient = error.is_timeout() || error.is_connect();
                    if attempt >= MAX_ATTEMPTS || !transient {
                        return Err(error.into());
                    }
                    let backoff = Duration::from_millis(100 * 2_u64.pow(attempt));
                    warn!(attempt, %error, ?backoff, "transient request failure; retrying");
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }

    pub fn vault_url(&self, suffix: &str) -> String {
        let base = self.config.server_url.trim_end_matches('/');
        let suffix = suffix.trim_start_matches('/');
        if suffix.is_empty() {
            return format!("{base}/vaults/{}", self.config.vault_id);
        }
        format!("{base}/vaults/{}/{suffix}", self.config.vault_id)
    }

    fn root_url(&self, suffix: &str) -> String {
        let base = self.config.server_url.trim_end_matches('/');
        format!("{base}/{}", suffix.trim_start_matches('/'))
    }

    pub async fn list_vaults(&self) -> Result<Vec<VaultSummary>, ApiError> {
        debug!("listing vaults");
        let request = self
            .client
            .get(self.root_url("vaults"))
            .timeout(SMALL_REQUEST_TIMEOUT)
            .bearer_auth(&self.config.api_key);

        parse_json(self.send_with_retry(request).await?).await
    }

    pub async fn create_vault(
        &self,
        request: &CreateVaultRequest,
    ) -> Result<CreateVaultResponse, ApiError> {
        debug!(name = %request.name, "creating vault");
        let http_request = self
            .client
            .post(self.root_url("vaults"))
            .timeout(SMALL_REQUEST_TIMEOUT)
            .bearer_auth(&self.config.api_key)
            .json(request);

        parse_json(self.send_with_retry(http_request).await?).await
    }

    /// Delete the configured vault and every blob it owns on the server.
    pub async fn delete_vault(&self) -> Result<(), ApiError> {
        debug!(vault = %self.config.vault_id, "deleting vault");
        let request = self
            .client
            .delete(self.vault_url(""))
            .timeout(SMALL_REQUEST_TIMEOUT)
            .bearer_auth(&self.config.api_key);
        parse_empty("", self.send_with_retry(request).await?).await
    }

    /// Fetch the server manifest (keyed by opaque path tokens) and re-key it by
    /// real path, decrypting each entry's `encPath`. Entries the caller's key
    /// can't decrypt are skipped (different vault key or corruption).
    pub async fn get_manifest(&self, keys: &CryptoKeys) -> Result<Manifest, ApiError> {
        match self.get_manifest_if_changed(keys, None).await? {
            ManifestFetch::Modified { manifest, .. } => Ok(manifest),
            // Without a validator the server cannot answer 304; treat it as empty.
            ManifestFetch::NotModified => Ok(Manifest::new()),
        }
    }

    /// Conditional manifest fetch. With `if_none_match` set to the ETag of the
    /// last copy, an unchanged manifest comes back as [`ManifestFetch::NotModified`]
    /// with no body. Servers without ETags always return `Modified` with
    /// `etag: None`.
    pub async fn get_manifest_if_changed(
        &self,
        keys: &CryptoKeys,
        if_none_match: Option<&str>,
    ) -> Result<ManifestFetch, ApiError> {
        debug!(conditional = if_none_match.is_some(), "fetching manifest");
        let mut request = self
            .client
            .get(self.vault_url("manifest"))
            .timeout(SMALL_REQUEST_TIMEOUT)
            .bearer_auth(&self.config.api_key);
        if let Some(etag) = if_none_match {
            request = request.header("If-None-Match", etag);
        }
        let response = self.send_with_retry(request).await?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(ManifestFetch::NotModified);
        }
        let etag = response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let raw: Manifest = parse_json(response).await?;
        let mut decoded = Manifest::new();
        for entry in raw.into_values() {
            if entry.enc_path.is_empty() {
                continue;
            }
            let path = decrypt_path(&keys.path_enc, &entry.enc_path)?;
            decoded.insert(path, entry);
        }

        Ok(ManifestFetch::Modified {
            manifest: decoded,
            etag,
        })
    }

    pub async fn get_file(&self, path: &str, keys: &CryptoKeys) -> Result<Vec<u8>, ApiError> {
        let token = path_token(&keys.path_token, path);
        debug!(path, "downloading file");
        let request = self
            .client
            .get(self.vault_url(&format!("files/{token}")))
            .bearer_auth(&self.config.api_key);

        parse_bytes(self.send_with_retry(request).await?).await
    }

    pub async fn put_file(
        &self,
        path: &str,
        parent_hash: Option<&str>,
        content_hash: &str,
        content: Vec<u8>,
        keys: &CryptoKeys,
    ) -> Result<(), ApiError> {
        let token = path_token(&keys.path_token, path);
        let enc_path = encrypt_path(&keys.path_enc, path)?;
        debug!(path, bytes = content.len(), "uploading file");
        let mut request = self
            .client
            .put(self.vault_url(&format!("files/{token}")))
            .bearer_auth(&self.config.api_key)
            .header("X-Content-Hash", content_hash)
            .header("X-Enc-Path", enc_path)
            .body(content);

        if let Some(parent_hash) = parent_hash {
            request = request.header("X-Parent-Hash", parent_hash);
        }

        parse_empty(path, self.send_with_retry(request).await?).await
    }

    pub async fn delete_file(
        &self,
        path: &str,
        parent_hash: Option<&str>,
        keys: &CryptoKeys,
    ) -> Result<(), ApiError> {
        let token = path_token(&keys.path_token, path);
        debug!(path, "deleting file");
        let mut request = self
            .client
            .delete(self.vault_url(&format!("files/{token}")))
            .timeout(SMALL_REQUEST_TIMEOUT)
            .bearer_auth(&self.config.api_key);

        if let Some(parent_hash) = parent_hash {
            request = request.header("X-Parent-Hash", parent_hash);
        }

        parse_empty(path, self.send_with_retry(request).await?).await
    }

    /// Send several puts/deletes in one `multipart/form-data` request: an
    /// `operations` JSON part plus one `content` part per put, named by
    /// operation index. Results come back keyed by real path, one per
    /// operation in order. A multipart body cannot be cloned, so the form is
    /// rebuilt for each attempt of the transient-failure retry; the
    /// per-operation parent-hash gate makes a resend land or 409, never
    /// duplicate.
    pub async fn batch(
        &self,
        operations: &[BatchOp],
        keys: &CryptoKeys,
    ) -> Result<Vec<BatchOperationResult>, ApiError> {
        let mut attempt: u32 = 0;
        let response = loop {
            let (form, _) = self.build_batch_form(operations, keys)?;
            debug!(operations = operations.len(), attempt, "sending batch");
            let request = self
                .client
                .post(self.vault_url("batch"))
                .timeout(BATCH_REQUEST_TIMEOUT)
                .bearer_auth(&self.config.api_key)
                .multipart(form);
            match request.send().await {
                Ok(response) => break response,
                Err(error) => {
                    attempt += 1;
                    let transient = error.is_timeout() || error.is_connect();
                    if attempt >= MAX_ATTEMPTS || !transient {
                        return Err(error.into());
                    }
                    let backoff = Duration::from_millis(100 * 2_u64.pow(attempt));
                    warn!(attempt, %error, ?backoff, "transient batch failure; retrying");
                    tokio::time::sleep(backoff).await;
                }
            }
        };
        let real_paths: Vec<String> = operations
            .iter()
            .map(|op| match op {
                BatchOp::Put { path, .. } | BatchOp::Delete { path, .. } => path.clone(),
            })
            .collect();
        let parsed: BatchResponse = parse_json(response).await?;
        if parsed.results.len() != operations.len() {
            return Err(ApiError::UnexpectedStatus {
                status: StatusCode::BAD_GATEWAY,
                body: format!(
                    "batch answered {} results for {} operations",
                    parsed.results.len(),
                    operations.len()
                ),
            });
        }
        Ok(parsed
            .results
            .into_iter()
            .enumerate()
            .map(|(index, result)| BatchOperationResult {
                path: real_paths.get(index).cloned().unwrap_or(result.path),
                status: result.status,
                conflict: result.conflict,
            })
            .collect())
    }

    /// The multipart form for one batch attempt, plus the real paths in
    /// operation order.
    fn build_batch_form(
        &self,
        operations: &[BatchOp],
        keys: &CryptoKeys,
    ) -> Result<(reqwest::multipart::Form, Vec<String>), ApiError> {
        let mut wire = Vec::with_capacity(operations.len());
        let mut form = reqwest::multipart::Form::new();
        let mut real_paths = Vec::with_capacity(operations.len());
        for (index, op) in operations.iter().enumerate() {
            match op {
                BatchOp::Put {
                    path,
                    parent_hash,
                    content_hash,
                    content,
                } => {
                    wire.push(WireBatchOperation {
                        action: "put",
                        path: path_token(&keys.path_token, path),
                        parent_hash: parent_hash.clone(),
                        content_hash: Some(content_hash.clone()),
                        enc_path: Some(encrypt_path(&keys.path_enc, path)?),
                    });
                    form = form.part(
                        "content",
                        reqwest::multipart::Part::bytes(content.clone())
                            .file_name(index.to_string())
                            .mime_str("application/octet-stream")
                            .map_err(ApiError::Http)?,
                    );
                    real_paths.push(path.clone());
                }
                BatchOp::Delete { path, parent_hash } => {
                    wire.push(WireBatchOperation {
                        action: "delete",
                        path: path_token(&keys.path_token, path),
                        parent_hash: parent_hash.clone(),
                        content_hash: None,
                        enc_path: None,
                    });
                    real_paths.push(path.clone());
                }
            }
        }
        let operations_json = serde_json::to_string(&serde_json::json!({ "operations": wire }))
            .map_err(|error| ApiError::UnexpectedStatus {
                status: StatusCode::BAD_REQUEST,
                body: error.to_string(),
            })?;
        let form = form.part(
            "operations",
            reqwest::multipart::Part::text(operations_json)
                .mime_str("application/json")
                .map_err(ApiError::Http)?,
        );
        Ok((form, real_paths))
    }
}

/// Outcome of a conditional manifest fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestFetch {
    Modified {
        manifest: Manifest,
        etag: Option<String>,
    },
    NotModified,
}

/// One batch operation as the server sees it (path token, no bytes).
#[derive(serde::Serialize)]
struct WireBatchOperation {
    action: &'static str,
    path: String,
    #[serde(rename = "parentHash", skip_serializing_if = "Option::is_none")]
    parent_hash: Option<String>,
    #[serde(rename = "contentHash", skip_serializing_if = "Option::is_none")]
    content_hash: Option<String>,
    #[serde(rename = "encPath", skip_serializing_if = "Option::is_none")]
    enc_path: Option<String>,
}

async fn parse_json<T: DeserializeOwned>(response: Response) -> Result<T, ApiError> {
    let status = response.status();

    if status.is_success() {
        return Ok(response.json().await?);
    }

    if status == StatusCode::UNAUTHORIZED {
        return Err(ApiError::Unauthorized);
    }
    let body = response.text().await.unwrap_or_default();
    Err(ApiError::UnexpectedStatus { status, body })
}

async fn parse_bytes(response: Response) -> Result<Vec<u8>, ApiError> {
    let status = response.status();

    if status.is_success() {
        return Ok(response.bytes().await?.to_vec());
    }

    if status == StatusCode::UNAUTHORIZED {
        return Err(ApiError::Unauthorized);
    }
    let body = response.text().await.unwrap_or_default();
    Err(ApiError::UnexpectedStatus { status, body })
}

async fn parse_empty(path: &str, response: Response) -> Result<(), ApiError> {
    let status = response.status();

    if status.is_success() {
        return Ok(());
    }

    if status == StatusCode::CONFLICT {
        let conflict = response.json::<ServerConflict>().await?;
        return Err(ApiError::Conflict {
            path: path.to_string(),
            conflict,
        });
    }

    if status == StatusCode::UNAUTHORIZED {
        return Err(ApiError::Unauthorized);
    }
    let body = response.text().await.unwrap_or_default();
    Err(ApiError::UnexpectedStatus { status, body })
}

#[cfg(test)]
mod tests {
    #[test]
    fn api_client_debug_redacts_the_bearer() {
        let client = super::ApiClient::new(crate::VaultConfig {
            server_url: "https://s.test".into(),
            api_key: "secret-bearer-xyz".into(),
            vault_id: "vault_1".into(),
            local_path: "/tmp/v".into(),
        });
        let printed = format!("{client:?}");
        assert!(printed.contains("vault_1"));
        assert!(!printed.contains("secret-bearer-xyz"));
    }

    use httpmock::{Method::GET, Method::POST, Method::PUT, MockServer};

    use super::{ApiClient, ApiError, ManifestFetch};
    use crate::crypto::{derive_key, derive_keys, encrypt_path, path_token, CryptoKeys};
    use crate::types::{BatchOp, FileEntry, VaultConfig};

    fn config(base_url: String) -> VaultConfig {
        VaultConfig {
            server_url: base_url,
            api_key: "token".to_string(),
            vault_id: "vault_123".to_string(),
            local_path: ".".to_string(),
        }
    }

    fn test_keys() -> CryptoKeys {
        derive_keys(&derive_key("hunter2", b"obsink-salt").unwrap())
    }

    #[tokio::test]
    async fn gets_manifest_and_recovers_paths() {
        let keys = test_keys();
        // The server stores entries keyed by an opaque token, with the real path
        // recoverable from `encPath`.
        let token = path_token(&keys.path_token, "note.md");
        let enc_path = encrypt_path(&keys.path_enc, "note.md").unwrap();

        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path("/vaults/vault_123/manifest")
                    .header("authorization", "Bearer token");
                then.status(200).json_body_obj(&serde_json::json!({
                    token.clone(): {
                        "hash": "abc",
                        "modified": 1,
                        "size": 5,
                        "deleted": false,
                        "encPath": enc_path.clone()
                    }
                }));
            })
            .await;

        let client = ApiClient::new(config(server.base_url()));
        let manifest = client.get_manifest(&keys).await.unwrap();

        mock.assert_async().await;
        assert_eq!(manifest["note.md"].hash, "abc");
    }

    #[tokio::test]
    async fn maps_conflicts() {
        let keys = test_keys();
        let token = path_token(&keys.path_token, "note.md");

        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(move |when, then| {
                when.method(PUT)
                    .path(format!("/vaults/vault_123/files/{token}"));
                then.status(409).json_body_obj(&serde_json::json!({
                    "path": "note.md",
                    "current": {
                        "hash": "server",
                        "modified": 2,
                        "size": 7,
                        "deleted": false
                    }
                }));
            })
            .await;

        let client = ApiClient::new(config(server.base_url()));
        let error = client
            .put_file(
                "note.md",
                Some("parent"),
                "next",
                b"payload".to_vec(),
                &keys,
            )
            .await
            .unwrap_err();

        mock.assert_async().await;
        match error {
            ApiError::Conflict { path, conflict } => {
                assert_eq!(path, "note.md");
                assert_eq!(
                    conflict.current,
                    Some(FileEntry {
                        hash: "server".to_string(),
                        modified: 2,
                        size: 7,
                        deleted: false,
                        enc_path: String::new(),
                    })
                );
            }
            other => panic!("expected conflict error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn manifest_304_returns_not_modified() {
        let keys = test_keys();
        let server = MockServer::start_async().await;
        let fresh = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/vaults/vault_123/manifest")
                    .matches(|req| {
                        !req.headers.as_ref().is_some_and(|headers| {
                            headers
                                .iter()
                                .any(|(name, _)| name.eq_ignore_ascii_case("if-none-match"))
                        })
                    });
                then.status(200)
                    .header("etag", "\"7\"")
                    .json_body_obj(&serde_json::json!({}));
            })
            .await;
        let cached = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/vaults/vault_123/manifest")
                    .header("if-none-match", "\"7\"");
                then.status(304).header("etag", "\"7\"");
            })
            .await;

        let client = ApiClient::new(config(server.base_url()));
        let first = client.get_manifest_if_changed(&keys, None).await.unwrap();
        assert_eq!(
            first,
            ManifestFetch::Modified {
                manifest: Default::default(),
                etag: Some("\"7\"".to_string())
            }
        );
        let second = client
            .get_manifest_if_changed(&keys, Some("\"7\""))
            .await
            .unwrap();
        assert_eq!(second, ManifestFetch::NotModified);
        fresh.assert_async().await;
        cached.assert_async().await;
    }

    #[tokio::test]
    async fn batch_sends_multipart() {
        let keys = test_keys();
        let token = path_token(&keys.path_token, "note.md");
        let del_token = path_token(&keys.path_token, "old.md");
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/vaults/vault_123/batch")
                    .header("authorization", "Bearer token")
                    .header_exists("content-type")
                    .body_contains("name=\"operations\"")
                    .body_contains("name=\"content\"; filename=\"0\"")
                    .body_contains(format!("\"path\":\"{token}\""))
                    .body_contains("\"contentHash\":\"h1\"")
                    .body_contains("\"action\":\"delete\"")
                    .body_contains("payload-bytes");
                then.status(200).json_body_obj(&serde_json::json!({
                    "results": [
                        { "path": token, "status": 200, "conflict": null },
                        { "path": del_token, "status": 409, "conflict": { "path": del_token, "current": null } }
                    ]
                }));
            })
            .await;

        let client = ApiClient::new(config(server.base_url()));
        let results = client
            .batch(
                &[
                    BatchOp::Put {
                        path: "note.md".to_string(),
                        parent_hash: None,
                        content_hash: "h1".to_string(),
                        content: b"payload-bytes".to_vec(),
                    },
                    BatchOp::Delete {
                        path: "old.md".to_string(),
                        parent_hash: Some("h0".to_string()),
                    },
                ],
                &keys,
            )
            .await
            .unwrap();
        mock.assert_async().await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].path, "note.md");
        assert_eq!(results[0].status, 200);
        assert_eq!(results[1].path, "old.md");
        assert_eq!(results[1].status, 409);
    }

    #[tokio::test]
    async fn batch_retries_a_connection_failure_before_giving_up() {
        // Nothing listens on port 1: each attempt fails to connect, the form
        // is rebuilt and resent after the backoff (200 ms + 400 ms), and the
        // third failure is returned.
        let keys = test_keys();
        let client = ApiClient::new(config("http://127.0.0.1:1".to_string()));
        let started = std::time::Instant::now();
        let error = client
            .batch(
                &[BatchOp::Delete {
                    path: "old.md".to_string(),
                    parent_hash: None,
                }],
                &keys,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ApiError::Http(_)), "{error}");
        assert!(started.elapsed() >= std::time::Duration::from_millis(600));
    }
}
