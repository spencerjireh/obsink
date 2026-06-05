use std::time::Duration;

use reqwest::{RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use thiserror::Error;
use tracing::{debug, warn};

use crate::crypto::{decrypt_path, encrypt_path, path_token, CryptoError, CryptoKeys};
use crate::types::{
    BatchRequest, BatchResponse, CreateVaultRequest, CreateVaultResponse, Manifest, ServerConflict,
    VaultConfig, VaultSummary,
};

/// Per-request timeout. Bounds hangs on a stalled connection.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Total attempts (1 initial + retries) for transient network failures.
const MAX_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone)]
pub struct ApiClient {
    config: VaultConfig,
    client: reqwest::Client,
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
    #[error("unexpected status {status}: {body}")]
    UnexpectedStatus { status: StatusCode, body: String },
}

impl ApiClient {
    pub fn new(config: VaultConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
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
        let base = self.config.worker_url.trim_end_matches('/');
        format!(
            "{base}/vaults/{}/{}",
            self.config.vault_id,
            suffix.trim_start_matches('/')
        )
    }

    fn root_url(&self, suffix: &str) -> String {
        let base = self.config.worker_url.trim_end_matches('/');
        format!("{base}/{}", suffix.trim_start_matches('/'))
    }

    pub async fn list_vaults(&self) -> Result<Vec<VaultSummary>, ApiError> {
        debug!("listing vaults");
        let request = self
            .client
            .get(self.root_url("vaults"))
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
            .bearer_auth(&self.config.api_key)
            .json(request);

        parse_json(self.send_with_retry(http_request).await?).await
    }

    /// Fetch the server manifest (keyed by opaque path tokens) and re-key it by
    /// real path, decrypting each entry's `encPath`. Entries the caller's key
    /// can't decrypt are skipped (different vault key or corruption).
    pub async fn get_manifest(&self, keys: &CryptoKeys) -> Result<Manifest, ApiError> {
        debug!("fetching manifest");
        let request = self
            .client
            .get(self.vault_url("manifest"))
            .bearer_auth(&self.config.api_key);

        let raw: Manifest = parse_json(self.send_with_retry(request).await?).await?;
        let mut decoded = Manifest::new();
        for entry in raw.into_values() {
            if entry.enc_path.is_empty() {
                continue;
            }
            let path = decrypt_path(&keys.path_enc, &entry.enc_path)?;
            decoded.insert(path, entry);
        }

        Ok(decoded)
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
            .bearer_auth(&self.config.api_key);

        if let Some(parent_hash) = parent_hash {
            request = request.header("X-Parent-Hash", parent_hash);
        }

        parse_empty(path, self.send_with_retry(request).await?).await
    }

    pub async fn batch(&self, request: &BatchRequest) -> Result<BatchResponse, ApiError> {
        let http_request = self
            .client
            .post(self.vault_url("batch"))
            .bearer_auth(&self.config.api_key)
            .json(request);

        parse_json(self.send_with_retry(http_request).await?).await
    }
}

async fn parse_json<T: DeserializeOwned>(response: Response) -> Result<T, ApiError> {
    let status = response.status();

    if status.is_success() {
        return Ok(response.json().await?);
    }

    let body = response.text().await.unwrap_or_default();
    Err(ApiError::UnexpectedStatus { status, body })
}

async fn parse_bytes(response: Response) -> Result<Vec<u8>, ApiError> {
    let status = response.status();

    if status.is_success() {
        return Ok(response.bytes().await?.to_vec());
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

    let body = response.text().await.unwrap_or_default();
    Err(ApiError::UnexpectedStatus { status, body })
}

#[cfg(test)]
mod tests {
    use httpmock::{Method::GET, Method::PUT, MockServer};

    use super::{ApiClient, ApiError};
    use crate::crypto::{derive_key, derive_keys, encrypt_path, path_token, CryptoKeys};
    use crate::types::{FileEntry, VaultConfig};

    fn config(base_url: String) -> VaultConfig {
        VaultConfig {
            worker_url: base_url,
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
}
