//! Account sign-in, the account key, devices and device approval against an
//! ObSink server (spec §4.1, §6.1, §12.1).
//!
//! An emailed one-time code is the one way to obtain a session bearer (Sign
//! in with Apple went away in 0.5). Every sign-in names the device (spec
//! §4.1); the resulting token is stored by the client in the OS keychain and
//! used as [`VaultConfig::bearer`](crate::VaultConfig). A signed-in device
//! without the account key either enters the passphrase or waits for another
//! device to approve it ([`AuthClient::register_approval`] and friends).

use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    crypto::{
        accept_approval, account_verifier, approve_device, decode_base64, encode_base64,
        AccountKeyMaterial, ApprovalRequest, CryptoError, KeyBytes, APPROVAL_PUBLIC_KEY_LEN,
        PROTOCOL_VERSION,
    },
    server_url::normalize_server_url,
};

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    /// The server answered with an error body: `{ "error": "..." }`.
    #[error("{message}")]
    Server { status: StatusCode, message: String },
    /// The server speaks another wire format: the client shows `Update ObSink`.
    #[error("this ObSink is too old for the server (protocol {server}, client {client})")]
    ProtocolMismatch { server: u32, client: u32 },
    /// The typed fingerprint is not the pending device's: nothing was sent.
    #[error("fingerprint does not match")]
    Fingerprint,
    /// Key material from the wire could not be decoded or opened.
    #[error("{0}")]
    Crypto(CryptoError),
}

impl From<CryptoError> for AuthError {
    fn from(error: CryptoError) -> Self {
        match error {
            CryptoError::FingerprintMismatch => AuthError::Fingerprint,
            other => AuthError::Crypto(other),
        }
    }
}

/// What sign-in methods a server offers (`GET /`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub service: String,
    /// The wire format the server speaks; `0` from a server too old to say.
    #[serde(default)]
    pub protocol: u32,
    pub auth: AuthMethods,
    /// True once the server has an account: new sign-ups need an invite code.
    #[serde(default)]
    pub invite_required: bool,
}

impl Capabilities {
    /// The protocol gate (spec §15.5): a client speaks exactly one version.
    pub fn check_protocol(&self) -> Result<(), AuthError> {
        if self.protocol == PROTOCOL_VERSION {
            Ok(())
        } else {
            Err(AuthError::ProtocolMismatch {
                server: self.protocol,
                client: PROTOCOL_VERSION,
            })
        }
    }
}

/// The platform a device reports at sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DevicePlatform {
    Macos,
    Ios,
    Browser,
    Cli,
}

/// The physical machine signing in: a client-generated id the client keeps
/// for good (spec §4.1), a display name, and the platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub platform: DevicePlatform,
}

/// The wrapped account key as the server holds it (`GET /auth/keys`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountKeyBlob {
    pub key_id: String,
    /// Base64 of the wrapped key.
    pub wrapped: String,
    /// Base64 of the Argon2id salt.
    pub salt: String,
}

impl AccountKeyBlob {
    /// Unlock the account key with the passphrase (spec §12.1).
    pub fn unlock(
        &self,
        passphrase: &str,
        user_id: &str,
    ) -> Result<crate::KeyBytes, crate::CryptoError> {
        let salt = decode_base64(&self.salt)?;
        let wrapped = decode_base64(&self.wrapped)?;
        crate::unlock_account_key(passphrase, &salt, &wrapped, user_id)
    }
}

/// What `PUT /auth/keys` answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetKeysOutcome {
    /// This device set the passphrase; the generated key is the account key.
    Created { key_id: String },
    /// Another device set it first (spec §12.1): discard the generated key and
    /// unlock this blob instead.
    Exists(AccountKeyBlob),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthMethods {
    pub email: bool,
    /// Always `false` since 0.5 (Sign in with Apple was removed); kept so
    /// the wire shape an older server or client speaks still parses.
    #[serde(default)]
    pub apple: bool,
}

/// `Debug` redacts `token` (the bearer).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub token: String,
    pub session: SessionInfo,
    pub user: UserInfo,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("token", &"..")
            .field("session", &self.session)
            .field("user", &self.user)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub expires: u64,
    /// The device this session belongs to (absent from a v2 server).
    #[serde(default)]
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub user: Option<MeUser>,
    /// Every device of the account, this one tagged `current`.
    #[serde(default)]
    pub devices: Vec<MeDevice>,
    /// Storage accounting for the account's vaults.
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub vaults: Vec<VaultUsage>,
    pub total_bytes: u64,
    /// `None` for the operator bearer, which has no limits.
    pub max_vault_bytes: Option<u64>,
    pub max_vaults: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultUsage {
    pub id: String,
    pub bytes: u64,
}

/// An invite code that lets one new account sign up (spec §4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    pub code: String,
    #[serde(default)]
    pub created: u64,
    pub expires: u64,
    /// `active`, `used`, or `expired`.
    #[serde(default = "default_invite_status")]
    pub status: String,
    #[serde(default)]
    pub used_at: Option<u64>,
}

fn default_invite_status() -> String {
    "active".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeUser {
    pub id: String,
    pub email: Option<String>,
    pub created: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeDevice {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub created: u64,
    #[serde(default)]
    pub last_seen: u64,
    pub current: bool,
    /// The vaults this device holds.
    #[serde(default)]
    pub vault_ids: Vec<String>,
    /// A live approval request from this device (spec §12.3); absent when
    /// none is pending or the server predates approval.
    #[serde(default)]
    pub approval: Option<MeApproval>,
}

/// A device's pending approval request as `GET /auth/me` lists it: the
/// public key the approver wraps to, when it was made and runs out, and
/// whether an approver already answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeApproval {
    /// Base64 of the device's X25519 public key.
    pub public_key: String,
    pub requested: u64,
    pub expires: u64,
    #[serde(default)]
    pub approved: bool,
}

/// What `PUT /auth/approval` answered: when the request runs out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRegistered {
    pub requested: u64,
    pub expires: u64,
}

/// This device's own request as `GET /auth/approval` reports it; `wrapped`
/// and the fields after it arrive once another device approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalStatus {
    /// Base64 of the registered public key.
    pub public_key: String,
    pub requested: u64,
    pub expires: u64,
    /// Base64 of the [`APPROVAL_BLOB_LEN`](crate::APPROVAL_BLOB_LEN)-byte blob.
    #[serde(default)]
    pub wrapped: Option<String>,
    /// The account key's id, to store next to the key.
    #[serde(default)]
    pub key_id: Option<String>,
    #[serde(default)]
    pub approved_by: Option<String>,
    #[serde(default)]
    pub approved: Option<u64>,
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
    pub fn new(server_url: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base: normalize_server_url(server_url),
            client,
        }
    }

    pub fn server_url(&self) -> &str {
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

    /// Trade a one-time code for a session. `invite_code` is needed only when
    /// this email has no account yet and the server already has users.
    pub async fn email_verify(
        &self,
        email: &str,
        code: &str,
        device: &Device,
        invite_code: Option<&str>,
    ) -> Result<Session, AuthError> {
        let body = serde_json::json!({
            "email": email,
            "code": code,
            "device": device,
            "invite_code": invite_code,
        });
        parse(
            self.client
                .post(self.url("auth/email/verify"))
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

    /// Sign another device of the account out for good (spec §4.1): its
    /// session and vault attachments go with it; its folders stay.
    pub async fn revoke_device(&self, token: &str, device_id: &str) -> Result<(), AuthError> {
        expect_empty(
            self.client
                .delete(self.url(&format!("auth/devices/{device_id}")))
                .bearer_auth(token)
                .send()
                .await?,
        )
        .await
    }

    /// Rename a device of the account, from any device.
    pub async fn rename_device(
        &self,
        token: &str,
        device_id: &str,
        name: &str,
    ) -> Result<(), AuthError> {
        expect_empty(
            self.client
                .patch(self.url(&format!("auth/devices/{device_id}")))
                .bearer_auth(token)
                .json(&serde_json::json!({ "name": name }))
                .send()
                .await?,
        )
        .await
    }

    /// The wrapped account key, or `None` before the first `set_keys`.
    pub async fn get_keys(&self, token: &str) -> Result<Option<AccountKeyBlob>, AuthError> {
        #[derive(Deserialize)]
        struct Body {
            account_key: Option<AccountKeyBlob>,
        }
        let body: Body = parse(
            self.client
                .get(self.url("auth/keys"))
                .bearer_auth(token)
                .send()
                .await?,
        )
        .await?;
        Ok(body.account_key)
    }

    /// Set the passphrase for the first time. Create-only: when another
    /// device won the race the answer is `Exists` with its blob.
    pub async fn set_keys(
        &self,
        token: &str,
        material: &AccountKeyMaterial,
    ) -> Result<SetKeysOutcome, AuthError> {
        let response = self
            .client
            .put(self.url("auth/keys"))
            .bearer_auth(token)
            .json(&material_json(material))
            .send()
            .await?;
        match response.status() {
            StatusCode::CREATED => {
                #[derive(Deserialize)]
                struct Body {
                    key_id: String,
                }
                let body: Body = response.json().await?;
                Ok(SetKeysOutcome::Created {
                    key_id: body.key_id,
                })
            }
            StatusCode::CONFLICT => {
                #[derive(Deserialize)]
                struct Body {
                    account_key: Option<AccountKeyBlob>,
                }
                let body: Body = response.json().await?;
                match body.account_key {
                    Some(blob) => Ok(SetKeysOutcome::Exists(blob)),
                    None => Err(AuthError::Server {
                        status: StatusCode::CONFLICT,
                        message: "the passphrase was set elsewhere; sign in again".to_string(),
                    }),
                }
            }
            status => Err(server_error(status, response).await),
        }
    }

    /// A passphrase change: the same account key under a new KEK. The
    /// material's verifier must match the one the server holds.
    pub async fn rewrap_keys(
        &self,
        token: &str,
        material: &AccountKeyMaterial,
    ) -> Result<(), AuthError> {
        expect_empty(
            self.client
                .put(self.url("auth/keys/rewrap"))
                .bearer_auth(token)
                .json(&material_json(material))
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

    /// Mint an invite code for someone else to create an account.
    pub async fn create_invite(&self, token: &str) -> Result<Invite, AuthError> {
        #[derive(Deserialize)]
        struct Body {
            invite: Invite,
        }
        let body: Body = parse(
            self.client
                .post(self.url("auth/invites"))
                .bearer_auth(token)
                .send()
                .await?,
        )
        .await?;
        Ok(body.invite)
    }

    /// Invites this principal has minted, newest first.
    pub async fn list_invites(&self, token: &str) -> Result<Vec<Invite>, AuthError> {
        #[derive(Deserialize)]
        struct Body {
            invites: Vec<Invite>,
        }
        let body: Body = parse(
            self.client
                .get(self.url("auth/invites"))
                .bearer_auth(token)
                .send()
                .await?,
        )
        .await?;
        Ok(body.invites)
    }

    /// Register this device's approval public key (`PUT /auth/approval`,
    /// spec §12.1). Re-registering replaces a live request. The server
    /// answers 400 while the account has no passphrase yet.
    pub async fn register_approval(
        &self,
        token: &str,
        public_key: &[u8; APPROVAL_PUBLIC_KEY_LEN],
    ) -> Result<ApprovalRegistered, AuthError> {
        parse(
            self.client
                .put(self.url("auth/approval"))
                .bearer_auth(token)
                .json(&serde_json::json!({ "public_key": encode_base64(public_key) }))
                .send()
                .await?,
        )
        .await
    }

    /// This device's live request, or `None` when there is none (never
    /// registered, expired, or cleared).
    pub async fn approval_status(&self, token: &str) -> Result<Option<ApprovalStatus>, AuthError> {
        #[derive(Deserialize)]
        struct Body {
            approval: Option<ApprovalStatus>,
        }
        let body: Body = parse(
            self.client
                .get(self.url("auth/approval"))
                .bearer_auth(token)
                .send()
                .await?,
        )
        .await?;
        Ok(body.approval)
    }

    /// Drop this device's request (after the key was taken, or on `Use
    /// passphrase instead`). Idempotent.
    pub async fn clear_approval(&self, token: &str) -> Result<(), AuthError> {
        expect_empty(
            self.client
                .delete(self.url("auth/approval"))
                .bearer_auth(token)
                .send()
                .await?,
        )
        .await
    }

    /// Relay an approval blob to a pending device
    /// (`POST /auth/devices/{device_id}/approval`). `verifier` is the account
    /// verifier, which only a holder of the account key can compute.
    pub async fn approve_device(
        &self,
        token: &str,
        device_id: &str,
        wrapped: &[u8],
        verifier: &[u8; 32],
    ) -> Result<(), AuthError> {
        expect_empty(
            self.client
                .post(self.url(&format!("auth/devices/{device_id}/approval")))
                .bearer_auth(token)
                .json(&serde_json::json!({
                    "wrapped": encode_base64(wrapped),
                    "verifier": encode_base64(verifier),
                }))
                .send()
                .await?,
        )
        .await
    }

    /// The approver's whole step (spec §12.3): check the typed fingerprint
    /// against the relayed public key, wrap the account key to it, and post
    /// the blob with the verifier. A wrong fingerprint is
    /// [`AuthError::Fingerprint`] and nothing is sent.
    pub async fn approve_with_key(
        &self,
        token: &str,
        account: &KeyBytes,
        user_id: &str,
        device_id: &str,
        public_key_b64: &str,
        typed: &str,
    ) -> Result<(), AuthError> {
        let public_key = decode_base64(public_key_b64)?;
        let wrapped = approve_device(account, &public_key, typed, user_id, device_id)?;
        let verifier = account_verifier(account, user_id);
        self.approve_device(token, device_id, &wrapped, &verifier)
            .await
    }

    /// One poll of the pending device (spec §12.1 step 3-4): `None` while no
    /// request is live or nobody has approved yet; otherwise the account key
    /// and its `key_id`, with the request cleared on the server. A blob the
    /// request cannot open is an error, not a retry.
    pub async fn take_approved_key(
        &self,
        token: &str,
        request: &ApprovalRequest,
        user_id: &str,
        device_id: &str,
    ) -> Result<Option<(KeyBytes, String)>, AuthError> {
        let Some(status) = self.approval_status(token).await? else {
            return Ok(None);
        };
        let Some(wrapped) = status.wrapped else {
            return Ok(None);
        };
        let key_id = status.key_id.ok_or_else(|| AuthError::Server {
            status: StatusCode::OK,
            message: "the approval carries no key id".to_string(),
        })?;
        let blob = decode_base64(&wrapped)?;
        let key = accept_approval(request, &blob, user_id, device_id)?;
        self.clear_approval(token).await?;
        Ok(Some((key, key_id)))
    }
}

fn material_json(material: &AccountKeyMaterial) -> serde_json::Value {
    serde_json::json!({
        "wrapped": material.wrapped_b64(),
        "salt": material.salt_b64(),
        "verifier": material.verifier_b64(),
    })
}

async fn parse<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, AuthError> {
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
    use httpmock::{
        Method::DELETE, Method::GET, Method::PATCH, Method::POST, Method::PUT, MockServer,
    };

    use super::{AuthClient, AuthError, Capabilities, Device, DevicePlatform, SetKeysOutcome};

    fn cli_device() -> Device {
        Device {
            id: "dev-1".into(),
            name: "cli".into(),
            platform: DevicePlatform::Cli,
        }
    }

    #[test]
    fn session_debug_redacts_the_token() {
        let session = super::Session {
            token: "bearer-secret".into(),
            session: super::SessionInfo {
                id: "sess_1".into(),
                expires: 0,
                device_id: None,
            },
            user: super::UserInfo {
                id: "user_1".into(),
                email: Some("a@b.test".into()),
            },
        };
        let printed = format!("{session:?}");
        assert!(printed.contains("sess_1"));
        assert!(!printed.contains("bearer-secret"));
    }

    #[tokio::test]
    async fn email_flow_round_trip() {
        let server = MockServer::start_async().await;
        let start = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/auth/email/start")
                    .json_body(serde_json::json!({ "email": "a@b.co" }));
                then.status(200)
                    .json_body(serde_json::json!({ "sent": true }));
            })
            .await;
        let verify = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/auth/email/verify")
                    .json_body(serde_json::json!({
                        "email": "a@b.co", "code": "123456", "invite_code": "ABCD2345",
                        "device": { "id": "dev-1", "name": "cli", "platform": "cli" }
                    }));
                then.status(200).json_body(serde_json::json!({
                    "token": "os_abc",
                    "session": { "id": "ses_1", "expires": 99, "device_id": "dev-1" },
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
                    "user": { "id": "usr_1", "email": "a@b.co", "created": 1 },
                    "devices": [{ "id": "dev-1", "name": "cli", "platform": "cli", "created": 1,
                                  "last_seen": 2, "current": true, "vault_ids": ["vault_1"] }],
                    "usage": { "vaults": [{ "id": "vault_1", "bytes": 12 }], "total_bytes": 12,
                               "max_vault_bytes": 1024, "max_vaults": 10 }
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
        let device = cli_device();
        let session = client
            .email_verify("a@b.co", "123456", &device, Some("ABCD2345"))
            .await
            .unwrap();
        assert_eq!(session.token, "os_abc");
        assert_eq!(session.session.device_id.as_deref(), Some("dev-1"));
        let me_response = client.me("os_abc").await.unwrap();
        assert_eq!(me_response.devices[0].name, "cli");
        assert!(me_response.devices[0].current);
        assert_eq!(me_response.devices[0].vault_ids, vec!["vault_1"]);
        let usage = me_response.usage.unwrap();
        assert_eq!(usage.total_bytes, 12);
        assert_eq!(usage.vaults[0].id, "vault_1");
        assert_eq!(usage.max_vaults, Some(10));
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
        let error = client
            .email_verify(
                "a@b.co",
                "000000",
                &Device {
                    id: "dev_1".into(),
                    name: "cli".into(),
                    platform: DevicePlatform::Cli,
                },
                None,
            )
            .await
            .unwrap_err();
        match error {
            AuthError::Server { status, message } => {
                assert_eq!(status.as_u16(), 401);
                assert_eq!(message, "incorrect code");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn invites_round_trip() {
        let server = MockServer::start_async().await;
        let create = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/auth/invites")
                    .header("authorization", "Bearer os_abc");
                then.status(201).json_body(serde_json::json!({
                    "invite": { "code": "ABCD2345", "created": 1, "expires": 2, "status": "active", "used_at": null }
                }));
            })
            .await;
        let list = server
            .mock_async(|when, then| {
                when.method(GET).path("/auth/invites");
                then.status(200).json_body(serde_json::json!({
                    "invites": [{ "code": "ABCD2345", "created": 1, "expires": 2, "status": "used", "used_at": 3 }]
                }));
            })
            .await;
        let client = AuthClient::new(&server.base_url());
        let invite = client.create_invite("os_abc").await.unwrap();
        assert_eq!(invite.code, "ABCD2345");
        assert_eq!(invite.status, "active");
        let invites = client.list_invites("os_abc").await.unwrap();
        assert_eq!(invites[0].used_at, Some(3));
        create.assert_async().await;
        list.assert_async().await;
    }

    #[test]
    fn the_protocol_gate_accepts_only_the_client_version() {
        let mut caps: Capabilities = serde_json::from_value(serde_json::json!({
            "service": "obsink", "protocol": super::PROTOCOL_VERSION,
            "auth": { "email": true, "apple": false }
        }))
        .unwrap();
        assert!(caps.check_protocol().is_ok());
        caps.protocol = 2;
        assert!(matches!(
            caps.check_protocol(),
            Err(AuthError::ProtocolMismatch { server: 2, .. })
        ));
    }

    #[tokio::test]
    async fn account_keys_set_unlock_race_and_rewrap() {
        let server = MockServer::start_async().await;
        let (key, material) = crate::create_account_key("correct horse battery", "usr_1").unwrap();
        let empty = server
            .mock_async(|when, then| {
                when.method(GET).path("/auth/keys");
                then.status(200)
                    .json_body(serde_json::json!({ "account_key": null }));
            })
            .await;
        let client = AuthClient::new(&server.base_url());
        assert!(client.get_keys("os_abc").await.unwrap().is_none());
        empty.delete_async().await;

        let created = server
            .mock_async(|when, then| {
                when.method(PUT).path("/auth/keys").json_body_partial(
                    serde_json::json!({ "wrapped": material.wrapped_b64() }).to_string(),
                );
                then.status(201)
                    .json_body(serde_json::json!({ "key_id": "key_1" }));
            })
            .await;
        assert_eq!(
            client.set_keys("os_abc", &material).await.unwrap(),
            SetKeysOutcome::Created {
                key_id: "key_1".into()
            }
        );
        created.delete_async().await;

        // A second device lost the race: it gets the winner's blob and unlocks it.
        let winner = serde_json::json!({ "account_key": {
            "key_id": "key_1", "wrapped": material.wrapped_b64(), "salt": material.salt_b64()
        }});
        server
            .mock_async(|when, then| {
                when.method(PUT).path("/auth/keys");
                then.status(409).json_body(winner.clone());
            })
            .await;
        let (_, losing) = crate::create_account_key("other", "usr_1").unwrap();
        let SetKeysOutcome::Exists(blob) = client.set_keys("os_abc", &losing).await.unwrap() else {
            panic!("expected the winner's blob");
        };
        assert_eq!(blob.key_id, "key_1");
        assert_eq!(blob.unlock("correct horse battery", "usr_1").unwrap(), key);
        assert!(blob.unlock("wrong", "usr_1").is_err());

        let rewrapped = crate::rewrap_account_key(&key, "new passphrase here", "usr_1").unwrap();
        let rewrap = server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path("/auth/keys/rewrap")
                    .json_body_partial(
                        serde_json::json!({ "verifier": rewrapped.verifier_b64() }).to_string(),
                    );
                then.status(204);
            })
            .await;
        client.rewrap_keys("os_abc", &rewrapped).await.unwrap();
        rewrap.assert_async().await;
    }

    #[tokio::test]
    async fn devices_rename_and_revoke() {
        let server = MockServer::start_async().await;
        let rename = server
            .mock_async(|when, then| {
                when.method(PATCH)
                    .path("/auth/devices/dev-2")
                    .json_body(serde_json::json!({ "name": "Kitchen iPad" }));
                then.status(204);
            })
            .await;
        let revoke = server
            .mock_async(|when, then| {
                when.method(DELETE).path("/auth/devices/dev-2");
                then.status(204);
            })
            .await;
        let client = AuthClient::new(&server.base_url());
        client
            .rename_device("os_abc", "dev-2", "Kitchen iPad")
            .await
            .unwrap();
        client.revoke_device("os_abc", "dev-2").await.unwrap();
        rename.assert_async().await;
        revoke.assert_async().await;
    }

    #[tokio::test]
    async fn approval_register_status_clear_and_approve() {
        let server = MockServer::start_async().await;
        let request = crate::new_approval_request();
        let public_b64 = crate::encode_base64(request.public_key());
        let register = server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path("/auth/approval")
                    .header("authorization", "Bearer os_abc")
                    .json_body(serde_json::json!({ "public_key": public_b64 }));
                then.status(201)
                    .json_body(serde_json::json!({ "requested": 10, "expires": 610 }));
            })
            .await;
        let status = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/auth/approval")
                    .header("authorization", "Bearer os_abc");
                then.status(200).json_body(serde_json::json!({ "approval": {
                    "public_key": public_b64, "requested": 10, "expires": 610, "wrapped": null
                }}));
            })
            .await;
        let clear = server
            .mock_async(|when, then| {
                when.method(DELETE)
                    .path("/auth/approval")
                    .header("authorization", "Bearer os_abc");
                then.status(204);
            })
            .await;
        let approve = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/auth/devices/dev-2/approval")
                    .header("authorization", "Bearer os_abc")
                    .json_body(serde_json::json!({
                        "wrapped": crate::encode_base64(&[7u8; 92]),
                        "verifier": crate::encode_base64(&[8u8; 32]),
                    }));
                then.status(204);
            })
            .await;
        let me = server
            .mock_async(|when, then| {
                when.method(GET).path("/auth/me");
                then.status(200).json_body(serde_json::json!({
                    "user": { "id": "usr_1", "email": "a@b.co", "created": 1 },
                    "devices": [
                        { "id": "dev-1", "name": "mac", "platform": "macos", "created": 1,
                          "last_seen": 2, "current": true },
                        { "id": "dev-2", "name": "phone", "platform": "ios", "created": 3,
                          "last_seen": 3, "current": false,
                          "approval": { "public_key": public_b64, "requested": 10, "expires": 610 } }
                    ]
                }));
            })
            .await;

        let client = AuthClient::new(&server.base_url());
        assert_eq!(
            client
                .register_approval("os_abc", request.public_key())
                .await
                .unwrap(),
            super::ApprovalRegistered {
                requested: 10,
                expires: 610
            }
        );
        let pending = client.approval_status("os_abc").await.unwrap().unwrap();
        assert_eq!(pending.public_key, public_b64);
        assert_eq!(pending.expires, 610);
        assert!(pending.wrapped.is_none() && pending.key_id.is_none());
        client.clear_approval("os_abc").await.unwrap();
        client
            .approve_device("os_abc", "dev-2", &[7u8; 92], &[8u8; 32])
            .await
            .unwrap();
        let devices = client.me("os_abc").await.unwrap().devices;
        assert!(devices[0].approval.is_none());
        let approval = devices[1].approval.as_ref().unwrap();
        assert_eq!(approval.public_key, public_b64);
        assert!(!approval.approved);

        register.assert_async().await;
        status.assert_async().await;
        clear.assert_async().await;
        approve.assert_async().await;
        me.assert_async().await;
    }

    #[tokio::test]
    async fn approve_with_key_refuses_a_wrong_fingerprint_before_any_request() {
        let server = MockServer::start_async().await;
        let post = server
            .mock_async(|when, then| {
                when.method(POST).path("/auth/devices/dev-2/approval");
                then.status(204);
            })
            .await;
        let account = crate::new_key();
        let request = crate::new_approval_request();
        let public_b64 = crate::encode_base64(request.public_key());
        let client = AuthClient::new(&server.base_url());
        let error = client
            .approve_with_key(
                "os_abc",
                &account,
                "usr_1",
                "dev-2",
                &public_b64,
                "AAAAAAAA",
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::Fingerprint), "{error:?}");
        assert_eq!(error.to_string(), "fingerprint does not match");
        assert!(matches!(
            client
                .approve_with_key("os_abc", &account, "usr_1", "dev-2", "!!", "AAAAAAAA")
                .await
                .unwrap_err(),
            AuthError::Crypto(crate::CryptoError::InvalidBlob)
        ));
        assert_eq!(post.hits_async().await, 0);

        // The right one (typed loosely) posts a blob the request can open.
        let typed = request.fingerprint().to_ascii_lowercase();
        client
            .approve_with_key("os_abc", &account, "usr_1", "dev-2", &public_b64, &typed)
            .await
            .unwrap();
        assert_eq!(post.hits_async().await, 1);
    }

    #[tokio::test]
    async fn take_approved_key_waits_then_unwraps_and_clears() {
        let server = MockServer::start_async().await;
        let account = crate::new_key();
        let request = crate::new_approval_request();
        let public_b64 = crate::encode_base64(request.public_key());
        let client = AuthClient::new(&server.base_url());

        let none = server
            .mock_async(|when, then| {
                when.method(GET).path("/auth/approval");
                then.status(200)
                    .json_body(serde_json::json!({ "approval": null }));
            })
            .await;
        assert!(client
            .take_approved_key("os_abc", &request, "usr_1", "dev-2")
            .await
            .unwrap()
            .is_none());
        none.delete_async().await;

        let pending = server
            .mock_async(|when, then| {
                when.method(GET).path("/auth/approval");
                then.status(200).json_body(serde_json::json!({ "approval": {
                    "public_key": public_b64, "requested": 10, "expires": 610
                }}));
            })
            .await;
        assert!(client
            .take_approved_key("os_abc", &request, "usr_1", "dev-2")
            .await
            .unwrap()
            .is_none());
        pending.delete_async().await;

        let blob = crate::approve_device(
            &account,
            request.public_key(),
            &request.fingerprint(),
            "usr_1",
            "dev-2",
        )
        .unwrap();
        let approved = server
            .mock_async(|when, then| {
                when.method(GET).path("/auth/approval");
                then.status(200).json_body(serde_json::json!({ "approval": {
                    "public_key": public_b64, "requested": 10, "expires": 900,
                    "wrapped": crate::encode_base64(&blob), "key_id": "key_1",
                    "approved_by": "dev-1", "approved": 300
                }}));
            })
            .await;
        let clear = server
            .mock_async(|when, then| {
                when.method(DELETE).path("/auth/approval");
                then.status(204);
            })
            .await;
        let (key, key_id) = client
            .take_approved_key("os_abc", &request, "usr_1", "dev-2")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(key, account);
        assert_eq!(key_id, "key_1");
        clear.assert_async().await;
        approved.delete_async().await;

        // A blob for another device (or a tampered one) is an error, and the
        // request is left in place.
        let foreign = server
            .mock_async(|when, then| {
                when.method(GET).path("/auth/approval");
                then.status(200).json_body(serde_json::json!({ "approval": {
                    "public_key": public_b64, "requested": 10, "expires": 900,
                    "wrapped": crate::encode_base64(&blob), "key_id": "key_1"
                }}));
            })
            .await;
        assert!(matches!(
            client
                .take_approved_key("os_abc", &request, "usr_1", "dev-3")
                .await
                .unwrap_err(),
            AuthError::Crypto(crate::CryptoError::Decrypt)
        ));
        assert_eq!(clear.hits_async().await, 1);
        foreign.assert_async().await;
    }
}
