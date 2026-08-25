//! Account sign-in for the hosted ("ObSink Cloud") backend.
//!
//! The Worker offers two ways to obtain a session bearer: an emailed one-time
//! code (all platforms) and Sign in with Apple (iOS). The resulting `os_…`
//! token is stored by the client in the OS keychain and used as
//! [`VaultConfig::api_key`](crate::VaultConfig) — the sync engine does not
//! distinguish a session from a self-hosted `API_KEY`.

use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The operator-run Worker every client offers as "ObSink Cloud". Overridable
/// per process with `OBSINK_HOSTED_URL` (harnesses, staging).
pub const HOSTED_WORKER_URL: &str = "https://obsink-worker.spencer-080.workers.dev";

/// Resolve the hosted Worker URL, honouring `OBSINK_HOSTED_URL`.
pub fn hosted_worker_url() -> String {
    std::env::var("OBSINK_HOSTED_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| HOSTED_WORKER_URL.to_string())
}

/// Normalise a Worker URL so the same server always maps to the same
/// keychain entry: trimmed, no trailing slash, lowercase scheme+host.
pub fn normalize_worker_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    match trimmed.split_once("://") {
        Some((scheme, rest)) => {
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            let mut out = format!("{}://{}", scheme.to_ascii_lowercase(), host.to_ascii_lowercase());
            if !path.is_empty() {
                out.push('/');
                out.push_str(path);
            }
            out
        }
        None => trimmed.to_string(),
    }
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    /// The server answered with an error body: `{ "error": "..." }`.
    #[error("{message}")]
    Server { status: StatusCode, message: String },
}

/// What sign-in methods a Worker offers (`GET /`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub service: String,
    pub auth: AuthMethods,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthMethods {
    pub email: bool,
    pub apple: bool,
    pub api_key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub token: String,
    pub session: SessionInfo,
    pub user: UserInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub expires: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub kind: String,
    pub user: Option<MeUser>,
    #[serde(default)]
    pub sessions: Vec<MeSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeUser {
    pub id: String,
    pub email: Option<String>,
    pub created: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeSession {
    pub id: String,
    #[serde(rename = "deviceName")]
    pub device_name: String,
    pub created: u64,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailStartResult {
    pub sent: bool,
    /// Present only when the server runs with `AUTH_DEV_RETURN_CODE=1`.
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AuthClient {
    base: String,
    client: reqwest::Client,
}

impl AuthClient {
    pub fn new(worker_url: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base: normalize_worker_url(worker_url),
            client,
        }
    }

    pub fn worker_url(&self) -> &str {
        &self.base
    }

    fn url(&self, suffix: &str) -> String {
        format!("{}/{}", self.base, suffix.trim_start_matches('/'))
    }

    pub async fn capabilities(&self) -> Result<Capabilities, AuthError> {
        parse(self.client.get(self.url("")).send().await?).await
    }

    pub async fn email_start(&self, email: &str) -> Result<EmailStartResult, AuthError> {
        let body = serde_json::json!({ "email": email });
        parse(
            self.client
                .post(self.url("auth/email/start"))
                .json(&body)
                .send()
                .await?,
        )
        .await
    }

    pub async fn email_verify(
        &self,
        email: &str,
        code: &str,
        device_name: &str,
    ) -> Result<Session, AuthError> {
        let body = serde_json::json!({ "email": email, "code": code, "device_name": device_name });
        parse(
            self.client
                .post(self.url("auth/email/verify"))
                .json(&body)
                .send()
                .await?,
        )
        .await
    }

    /// Exchange an Apple identity token (JWT from `ASAuthorizationAppleIDCredential`)
    /// for a session. `email` is the credential's email, which Apple delivers
    /// only on the first authorization.
    pub async fn apple_sign_in(
        &self,
        identity_token: &str,
        device_name: &str,
        email: Option<&str>,
    ) -> Result<Session, AuthError> {
        let body = serde_json::json!({
            "identity_token": identity_token,
            "device_name": device_name,
            "email": email,
        });
        parse(
            self.client
                .post(self.url("auth/apple"))
                .json(&body)
                .send()
                .await?,
        )
        .await
    }

    pub async fn me(&self, token: &str) -> Result<Me, AuthError> {
        parse(
            self.client
                .get(self.url("auth/me"))
                .bearer_auth(token)
                .send()
                .await?,
        )
        .await
    }

    /// Revoke the current session (sign out this device).
    pub async fn logout(&self, token: &str) -> Result<(), AuthError> {
        expect_empty(
            self.client
                .delete(self.url("auth/session"))
                .bearer_auth(token)
                .send()
                .await?,
        )
        .await
    }

    /// Revoke another session of the same account.
    pub async fn revoke_session(&self, token: &str, session_id: &str) -> Result<(), AuthError> {
        expect_empty(
            self.client
                .delete(self.url(&format!("auth/sessions/{session_id}")))
                .bearer_auth(token)
                .send()
                .await?,
        )
        .await
    }

    /// Delete the account and every vault it owns. Irreversible.
    pub async fn delete_account(&self, token: &str) -> Result<(), AuthError> {
        expect_empty(
            self.client
                .delete(self.url("auth/account"))
                .bearer_auth(token)
                .send()
                .await?,
        )
        .await
    }
}

async fn parse<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T, AuthError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response.json().await?);
    }
    Err(server_error(status, response).await)
}

async fn expect_empty(response: reqwest::Response) -> Result<(), AuthError> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    Err(server_error(status, response).await)
}

async fn server_error(status: StatusCode, response: reqwest::Response) -> AuthError {
    #[derive(Deserialize)]
    struct Body {
        error: String,
    }
    let text = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<Body>(&text)
        .map(|body| body.error)
        .unwrap_or_else(|_| {
            if text.is_empty() {
                format!("server returned {status}")
            } else {
                text
            }
        });
    AuthError::Server { status, message }
}

#[cfg(test)]
mod tests {
    use httpmock::{Method::DELETE, Method::GET, Method::POST, MockServer};

    use super::{normalize_worker_url, AuthClient, AuthError};

    #[test]
    fn normalizes_urls() {
        assert_eq!(
            normalize_worker_url(" HTTPS://Example.Workers.dev/ "),
            "https://example.workers.dev"
        );
        assert_eq!(
            normalize_worker_url("https://x.dev/Path/"),
            "https://x.dev/Path"
        );
    }

    #[tokio::test]
    async fn email_flow_round_trip() {
        let server = MockServer::start_async().await;
        let start = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/auth/email/start")
                    .json_body(serde_json::json!({ "email": "a@b.co" }));
                then.status(200).json_body(serde_json::json!({ "sent": true }));
            })
            .await;
        let verify = server
            .mock_async(|when, then| {
                when.method(POST).path("/auth/email/verify");
                then.status(200).json_body(serde_json::json!({
                    "token": "os_abc",
                    "session": { "id": "ses_1", "expires": 99 },
                    "user": { "id": "usr_1", "email": "a@b.co" }
                }));
            })
            .await;
        let me = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/auth/me")
                    .header("authorization", "Bearer os_abc");
                then.status(200).json_body(serde_json::json!({
                    "kind": "user",
                    "user": { "id": "usr_1", "email": "a@b.co", "created": 1 },
                    "sessions": [{ "id": "ses_1", "deviceName": "cli", "created": 1, "current": true }]
                }));
            })
            .await;
        let logout = server
            .mock_async(|when, then| {
                when.method(DELETE).path("/auth/session");
                then.status(204);
            })
            .await;

        let client = AuthClient::new(&server.base_url());
        assert!(client.email_start("a@b.co").await.unwrap().sent);
        let session = client.email_verify("a@b.co", "123456", "cli").await.unwrap();
        assert_eq!(session.token, "os_abc");
        let me_response = client.me("os_abc").await.unwrap();
        assert_eq!(me_response.sessions[0].device_name, "cli");
        client.logout("os_abc").await.unwrap();

        start.assert_async().await;
        verify.assert_async().await;
        me.assert_async().await;
        logout.assert_async().await;
    }

    #[tokio::test]
    async fn surfaces_server_error_messages() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(POST).path("/auth/email/verify");
                then.status(401)
                    .json_body(serde_json::json!({ "error": "incorrect code" }));
            })
            .await;
        let client = AuthClient::new(&server.base_url());
        let error = client.email_verify("a@b.co", "000000", "cli").await.unwrap_err();
        match error {
            AuthError::Server { status, message } => {
                assert_eq!(status.as_u16(), 401);
                assert_eq!(message, "incorrect code");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
