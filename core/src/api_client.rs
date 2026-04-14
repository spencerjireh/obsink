use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;
use thiserror::Error;
use urlencoding::encode;

use crate::types::{
    BatchRequest, BatchResponse, CreateVaultRequest, CreateVaultResponse, Manifest, ServerConflict,
    VaultConfig, VaultSummary,
};

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
    #[error("unexpected status {status}: {body}")]
    UnexpectedStatus { status: StatusCode, body: String },
}

impl ApiClient {
    pub fn new(config: VaultConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
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
        let response = self
            .client
            .get(self.root_url("vaults"))
            .bearer_auth(&self.config.api_key)
            .send()
            .await?;

        parse_json(response).await
    }

    pub async fn create_vault(
        &self,
        request: &CreateVaultRequest,
    ) -> Result<CreateVaultResponse, ApiError> {
        let response = self
            .client
            .post(self.root_url("vaults"))
            .bearer_auth(&self.config.api_key)
            .json(request)
            .send()
            .await?;

        parse_json(response).await
    }

    pub async fn get_manifest(&self) -> Result<Manifest, ApiError> {
        let response = self
            .client
            .get(self.vault_url("manifest"))
            .bearer_auth(&self.config.api_key)
            .send()
            .await?;

        parse_json(response).await
    }

    pub async fn get_file(&self, path: &str) -> Result<Vec<u8>, ApiError> {
        let response = self
            .client
            .get(self.vault_url(&format!("files/{}", encode(path))))
            .bearer_auth(&self.config.api_key)
            .send()
            .await?;

        parse_bytes(response).await
    }

    pub async fn put_file(
        &self,
        path: &str,
        parent_hash: Option<&str>,
        content_hash: &str,
        content: Vec<u8>,
    ) -> Result<(), ApiError> {
        let mut request = self
            .client
            .put(self.vault_url(&format!("files/{}", encode(path))))
            .bearer_auth(&self.config.api_key)
            .header("X-Content-Hash", content_hash)
            .body(content);

        if let Some(parent_hash) = parent_hash {
            request = request.header("X-Parent-Hash", parent_hash);
        }

        parse_empty(path, request.send().await?).await
    }

    pub async fn delete_file(&self, path: &str, parent_hash: Option<&str>) -> Result<(), ApiError> {
        let mut request = self
            .client
            .delete(self.vault_url(&format!("files/{}", encode(path))))
            .bearer_auth(&self.config.api_key);

        if let Some(parent_hash) = parent_hash {
            request = request.header("X-Parent-Hash", parent_hash);
        }

        parse_empty(path, request.send().await?).await
    }

    pub async fn batch(&self, request: &BatchRequest) -> Result<BatchResponse, ApiError> {
        let response = self
            .client
            .post(self.vault_url("batch"))
            .bearer_auth(&self.config.api_key)
            .json(request)
            .send()
            .await?;

        parse_json(response).await
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
    use crate::types::{FileEntry, VaultConfig};

    fn config(base_url: String) -> VaultConfig {
        VaultConfig {
            worker_url: base_url,
            api_key: "token".to_string(),
            vault_id: "vault_123".to_string(),
            local_path: ".".to_string(),
        }
    }

    #[tokio::test]
    async fn gets_manifest() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/vaults/vault_123/manifest")
                    .header("authorization", "Bearer token");
                then.status(200).json_body_obj(&serde_json::json!({
                    "note.md": {
                        "hash": "abc",
                        "modified": 1,
                        "size": 5,
                        "deleted": false
                    }
                }));
            })
            .await;

        let client = ApiClient::new(config(server.base_url()));
        let manifest = client.get_manifest().await.unwrap();

        mock.assert_async().await;
        assert_eq!(manifest["note.md"].hash, "abc");
    }

    #[tokio::test]
    async fn maps_conflicts() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(PUT).path("/vaults/vault_123/files/note.md");
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
            .put_file("note.md", Some("parent"), "next", b"payload".to_vec())
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
                    })
                );
            }
            other => panic!("expected conflict error, got {other:?}"),
        }
    }
}
