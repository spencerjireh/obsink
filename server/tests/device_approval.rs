//! Device key approval (spec §4.1, §12.1, §12.3): a second device of an
//! account gets the account key from an unlocked one through a blob the
//! server relays but cannot open.

mod common;

use common::{Account, TestEnv};
use obsink_core::{
    accept_approval, account_verifier, approve_device, decode_base64, encode_base64,
    new_approval_request, AuthError, KeyBytes, APPROVAL_BLOB_LEN,
};
use reqwest::Method;

const EMAIL: &str = "pair@example.com";
const PASSPHRASE: &str = "correct horse battery staple";

/// Device A (unlocked, holds the account key) and device B (signed in, no
/// key) of one account, plus the key and its id.
struct Pair {
    a: Account,
    b: Account,
    key: KeyBytes,
    key_id: String,
}

async fn pair(env: &TestEnv) -> Pair {
    let a = env.sign_in(EMAIL, "dev-a", None).await;
    let (key, _) = env.set_passphrase(&a, PASSPHRASE).await;
    let keys: serde_json::Value = env
        .with_token(&a.token, Method::GET, "/auth/keys")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let key_id = keys["account_key"]["key_id"].as_str().unwrap().to_string();
    env.clear_email_cooldown(EMAIL).await;
    let b = env.sign_in(EMAIL, "dev-b", None).await;
    assert_eq!(a.user_id, b.user_id);
    Pair { a, b, key, key_id }
}

async fn register(env: &TestEnv, token: &str, public_key: &[u8]) -> reqwest::Response {
    env.with_token(token, Method::PUT, "/auth/approval")
        .json(&serde_json::json!({ "public_key": encode_base64(public_key) }))
        .send()
        .await
        .unwrap()
}

async fn poll(env: &TestEnv, token: &str) -> serde_json::Value {
    let response = env
        .with_token(token, Method::GET, "/auth/approval")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    response.json().await.unwrap()
}

async fn approve(
    env: &TestEnv,
    token: &str,
    device_id: &str,
    wrapped: &[u8],
    verifier: &[u8],
) -> reqwest::Response {
    env.with_token(
        token,
        Method::POST,
        &format!("/auth/devices/{device_id}/approval"),
    )
    .json(&serde_json::json!({
        "wrapped": encode_base64(wrapped),
        "verifier": encode_base64(verifier),
    }))
    .send()
    .await
    .unwrap()
}

async fn me_devices(env: &TestEnv, token: &str) -> serde_json::Value {
    let me: serde_json::Value = env
        .with_token(token, Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    me["devices"].clone()
}

fn device<'a>(devices: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    devices
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["id"] == id)
        .unwrap_or_else(|| panic!("device {id} listed"))
}

async fn error_of(response: reqwest::Response, status: u16) -> String {
    assert_eq!(response.status(), status);
    response.json::<serde_json::Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn approves_a_device_over_http_with_the_core_crypto() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let Pair { a, b, key, key_id } = pair(&env).await;
    let request = new_approval_request();

    let registered = register(&env, &b.token, request.public_key()).await;
    assert_eq!(registered.status(), 201);
    let registered: serde_json::Value = registered.json().await.unwrap();
    let requested = registered["requested"].as_u64().unwrap();
    let expires = registered["expires"].as_u64().unwrap();
    assert_eq!(expires - requested, 600);

    let pending = poll(&env, &b.token).await;
    assert_eq!(
        pending,
        serde_json::json!({ "approval": {
            "public_key": encode_base64(request.public_key()),
            "requested": requested,
            "expires": expires,
            "wrapped": null,
        } })
    );

    // The approver sees the pending row; its own row carries no request.
    let devices = me_devices(&env, &a.token).await;
    let row = device(&devices, "dev-b");
    assert_eq!(
        row["approval"],
        serde_json::json!({
            "public_key": encode_base64(request.public_key()),
            "requested": requested,
            "expires": expires,
            "approved": false,
        })
    );
    assert!(device(&devices, "dev-a").get("approval").is_none());

    let wrapped = approve_device(
        &key,
        request.public_key(),
        &request.fingerprint(),
        &a.user_id,
        "dev-b",
    )
    .unwrap();
    let verifier = account_verifier(&key, &a.user_id);
    let approved = approve(&env, &a.token, "dev-b", &wrapped, &verifier).await;
    assert_eq!(approved.status(), 204, "{}", approved.text().await.unwrap());

    let done = poll(&env, &b.token).await;
    let approval = &done["approval"];
    assert_eq!(approval["public_key"], encode_base64(request.public_key()));
    assert_eq!(approval["wrapped"], encode_base64(&wrapped));
    assert_eq!(approval["key_id"], key_id);
    assert_eq!(approval["approved_by"], "dev-a");
    let approved_at = approval["approved"].as_u64().unwrap();
    assert!(approved_at >= requested);
    assert_eq!(
        approval["expires"].as_u64().unwrap(),
        approved_at + 600,
        "approval gives the poller a fresh 10 minutes"
    );
    let blob = decode_base64(approval["wrapped"].as_str().unwrap()).unwrap();
    assert_eq!(
        accept_approval(&request, &blob, &b.user_id, "dev-b").unwrap(),
        key
    );

    let devices = me_devices(&env, &a.token).await;
    assert_eq!(device(&devices, "dev-b")["approval"]["approved"], true);

    let cleared = env
        .with_token(&b.token, Method::DELETE, "/auth/approval")
        .send()
        .await
        .unwrap();
    assert_eq!(cleared.status(), 204);
    assert_eq!(
        poll(&env, &b.token).await,
        serde_json::json!({ "approval": null })
    );
    let again = env
        .with_token(&b.token, Method::DELETE, "/auth/approval")
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 204, "idempotent");
    assert!(device(&me_devices(&env, &a.token).await, "dev-b")
        .get("approval")
        .is_none());
    env.finish().await;
}

#[tokio::test]
async fn approves_a_device_through_the_core_auth_client() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let Pair { a, b, key, key_id } = pair(&env).await;
    let client = env.auth_client();
    let request = new_approval_request();

    let registered = client
        .register_approval(&b.token, request.public_key())
        .await
        .unwrap();
    assert_eq!(registered.expires - registered.requested, 600);
    assert!(client
        .take_approved_key(&b.token, &request, &b.user_id, "dev-b")
        .await
        .unwrap()
        .is_none());

    let me = client.me(&a.token).await.unwrap();
    let pending = me
        .devices
        .iter()
        .find(|device| device.id == "dev-b")
        .unwrap()
        .approval
        .clone()
        .unwrap();
    assert_eq!(pending.public_key, encode_base64(request.public_key()));
    assert!(!pending.approved);

    // A wrong fingerprint sends nothing.
    let wrong = client
        .approve_with_key(
            &a.token,
            &key,
            &a.user_id,
            "dev-b",
            &pending.public_key,
            "AAAAAAAA",
        )
        .await;
    assert!(matches!(wrong, Err(AuthError::Fingerprint)), "{wrong:?}");
    assert!(client
        .approval_status(&b.token)
        .await
        .unwrap()
        .unwrap()
        .wrapped
        .is_none());

    client
        .approve_with_key(
            &a.token,
            &key,
            &a.user_id,
            "dev-b",
            &pending.public_key,
            &request.fingerprint().to_lowercase(),
        )
        .await
        .unwrap();
    let (taken, taken_id) = client
        .take_approved_key(&b.token, &request, &b.user_id, "dev-b")
        .await
        .unwrap()
        .expect("approved");
    assert_eq!(taken, key);
    assert_eq!(taken_id, key_id);
    assert!(
        client.approval_status(&b.token).await.unwrap().is_none(),
        "taking the key clears the request"
    );
    env.finish().await;
}

#[tokio::test]
async fn relays_an_opaque_blob_that_the_client_rejects() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let Pair { a, b, key, .. } = pair(&env).await;
    let request = new_approval_request();
    assert_eq!(
        register(&env, &b.token, request.public_key())
            .await
            .status(),
        201
    );
    let mut junk = [0u8; APPROVAL_BLOB_LEN];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut junk);
    let verifier = account_verifier(&key, &a.user_id);
    assert_eq!(
        approve(&env, &a.token, "dev-b", &junk, &verifier)
            .await
            .status(),
        204,
        "the server stores what it is sent"
    );
    let status = poll(&env, &b.token).await;
    assert_eq!(status["approval"]["wrapped"], encode_base64(&junk));
    assert!(accept_approval(&request, &junk, &b.user_id, "dev-b").is_err());
    let taken = env
        .auth_client()
        .take_approved_key(&b.token, &request, &b.user_id, "dev-b")
        .await;
    assert!(matches!(taken, Err(AuthError::Crypto(_))), "{taken:?}");
    assert!(
        status["approval"]["wrapped"].is_string()
            && poll(&env, &b.token).await["approval"]["wrapped"].is_string(),
        "a rejected blob is not cleared"
    );
    env.finish().await;
}

#[tokio::test]
async fn an_expired_request_is_invisible_and_cannot_be_approved() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let Pair { a, b, key, .. } = pair(&env).await;
    let request = new_approval_request();
    assert_eq!(
        register(&env, &b.token, request.public_key())
            .await
            .status(),
        201
    );
    sqlx::query("UPDATE devices SET approval_expires = 1")
        .execute(&env.state.pool)
        .await
        .unwrap();
    assert_eq!(
        poll(&env, &b.token).await,
        serde_json::json!({ "approval": null })
    );
    assert!(device(&me_devices(&env, &a.token).await, "dev-b")
        .get("approval")
        .is_none());
    let wrapped = approve_device(
        &key,
        request.public_key(),
        &request.fingerprint(),
        &a.user_id,
        "dev-b",
    )
    .unwrap();
    let verifier = account_verifier(&key, &a.user_id);
    assert_eq!(
        error_of(
            approve(&env, &a.token, "dev-b", &wrapped, &verifier).await,
            404
        )
        .await,
        "no pending request for this device"
    );
    env.finish().await;
}

#[tokio::test]
async fn registering_again_replaces_the_request() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let Pair { a, b, key, .. } = pair(&env).await;
    let first = new_approval_request();
    assert_eq!(
        register(&env, &b.token, first.public_key()).await.status(),
        201
    );
    let wrapped = approve_device(
        &key,
        first.public_key(),
        &first.fingerprint(),
        &a.user_id,
        "dev-b",
    )
    .unwrap();
    let verifier = account_verifier(&key, &a.user_id);
    assert_eq!(
        approve(&env, &a.token, "dev-b", &wrapped, &verifier)
            .await
            .status(),
        204
    );

    let second = new_approval_request();
    assert_eq!(
        register(&env, &b.token, second.public_key()).await.status(),
        201
    );
    let status = poll(&env, &b.token).await;
    assert_eq!(
        status["approval"]["public_key"],
        encode_base64(second.public_key())
    );
    assert!(status["approval"]["wrapped"].is_null());
    assert!(status["approval"].get("key_id").is_none());
    assert!(status["approval"].get("approved_by").is_none());
    let row = device(&me_devices(&env, &a.token).await, "dev-b").clone();
    assert_eq!(row["approval"]["approved"], false);
    env.finish().await;
}

#[tokio::test]
async fn a_re_sign_in_or_a_revoke_clears_the_request() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let Pair { a, b, .. } = pair(&env).await;
    let request = new_approval_request();
    assert_eq!(
        register(&env, &b.token, request.public_key())
            .await
            .status(),
        201
    );

    // Signing the same device in again restarts the flow.
    env.clear_email_cooldown(EMAIL).await;
    let b_again = env.sign_in(EMAIL, "dev-b", None).await;
    assert_eq!(
        poll(&env, &b_again.token).await,
        serde_json::json!({ "approval": null })
    );
    assert!(device(&me_devices(&env, &a.token).await, "dev-b")
        .get("approval")
        .is_none());

    // Revoking the device takes its request with it.
    assert_eq!(
        register(&env, &b_again.token, request.public_key())
            .await
            .status(),
        201
    );
    let revoked = env
        .with_token(&a.token, Method::DELETE, "/auth/devices/dev-b")
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), 204);
    let gone = env
        .with_token(&b_again.token, Method::GET, "/auth/approval")
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), 401);
    assert!(!me_devices(&env, &a.token)
        .await
        .as_array()
        .unwrap()
        .iter()
        .any(|device| device["id"] == "dev-b"));
    env.finish().await;
}

#[tokio::test]
async fn refuses_approvals_that_do_not_fit() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let Pair { a, b, key, .. } = pair(&env).await;
    let verifier = account_verifier(&key, &a.user_id);
    let junk = [9u8; APPROVAL_BLOB_LEN];

    // No request yet.
    assert_eq!(
        error_of(
            approve(&env, &a.token, "dev-b", &junk, &verifier).await,
            404
        )
        .await,
        "no pending request for this device"
    );

    let request = new_approval_request();
    assert_eq!(
        register(&env, &b.token, request.public_key())
            .await
            .status(),
        201
    );
    let wrapped = approve_device(
        &key,
        request.public_key(),
        &request.fingerprint(),
        &a.user_id,
        "dev-b",
    )
    .unwrap();

    // Own id, wrong verifier, wrong lengths.
    assert_eq!(
        error_of(
            approve(&env, &a.token, "dev-a", &wrapped, &verifier).await,
            400
        )
        .await,
        "a device cannot approve itself"
    );
    let mut wrong_verifier = verifier;
    wrong_verifier[0] ^= 1;
    assert_eq!(
        error_of(
            approve(&env, &a.token, "dev-b", &wrapped, &wrong_verifier).await,
            403
        )
        .await,
        "passphrase does not match this account"
    );
    assert_eq!(
        error_of(
            approve(&env, &a.token, "dev-b", &wrapped[1..], &verifier).await,
            400
        )
        .await,
        "wrapped must decode to 92 bytes"
    );
    assert_eq!(
        error_of(
            approve(&env, &a.token, "dev-b", &wrapped, &verifier[1..]).await,
            400
        )
        .await,
        "verifier must decode to 32 bytes"
    );
    let not_base64 = env
        .with_token(&a.token, Method::POST, "/auth/devices/dev-b/approval")
        .json(&serde_json::json!({ "wrapped": "%%%", "verifier": encode_base64(&verifier) }))
        .send()
        .await
        .unwrap();
    assert_eq!(error_of(not_base64, 400).await, "wrapped must be base64");
    assert!(
        poll(&env, &b.token).await["approval"]["wrapped"].is_null(),
        "nothing above stored a blob"
    );

    // Another account's device is not found, even with a live request.
    let invite = env.mint_invite(&a.token).await;
    let c = env
        .sign_in("other@example.com", "dev-c", Some(&invite))
        .await;
    env.set_passphrase(&c, PASSPHRASE).await;
    env.clear_email_cooldown("other@example.com").await;
    let c2 = env.sign_in("other@example.com", "dev-c2", None).await;
    let c_request = new_approval_request();
    assert_eq!(
        register(&env, &c2.token, c_request.public_key())
            .await
            .status(),
        201
    );
    assert_eq!(
        error_of(
            approve(&env, &a.token, "dev-c2", &junk, &verifier).await,
            404
        )
        .await,
        "no pending request for this device"
    );

    // A second approval of the same request.
    assert_eq!(
        approve(&env, &a.token, "dev-b", &wrapped, &verifier)
            .await
            .status(),
        204
    );
    assert_eq!(
        error_of(
            approve(&env, &a.token, "dev-b", &wrapped, &verifier).await,
            409
        )
        .await,
        "already approved"
    );

    // Registration body checks.
    assert_eq!(
        error_of(register(&env, &b.token, &[1u8; 31]).await, 400).await,
        "public_key must decode to 32 bytes"
    );
    let missing = env
        .with_token(&b.token, Method::PUT, "/auth/approval")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(error_of(missing, 400).await, "public_key is required");
    env.finish().await;
}

#[tokio::test]
async fn needs_a_bearer_and_an_account_key() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    for (method, path) in [
        (Method::PUT, "/auth/approval"),
        (Method::GET, "/auth/approval"),
        (Method::DELETE, "/auth/approval"),
        (Method::POST, "/auth/devices/dev-b/approval"),
    ] {
        let response = env
            .req(method.clone(), path)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401, "{method} {path}");
    }

    // An account without a passphrase has no key to hand over.
    let a = env.sign_in(EMAIL, "dev-a", None).await;
    env.clear_email_cooldown(EMAIL).await;
    let b = env.sign_in(EMAIL, "dev-b", None).await;
    let request = new_approval_request();
    assert_eq!(
        error_of(register(&env, &b.token, request.public_key()).await, 400).await,
        "set a passphrase first"
    );
    assert_eq!(
        error_of(
            approve(
                &env,
                &a.token,
                "dev-b",
                &[1u8; APPROVAL_BLOB_LEN],
                &[2u8; 32]
            )
            .await,
            400
        )
        .await,
        "set a passphrase first"
    );
    assert_eq!(
        poll(&env, &b.token).await,
        serde_json::json!({ "approval": null })
    );
    env.finish().await;
}
