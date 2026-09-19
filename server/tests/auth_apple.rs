mod common;

use axum::{routing::get, Json, Router};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use common::{TestEnv, APPLE_AUDIENCE};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::Method;
use rsa::{pkcs1::EncodeRsaPrivateKey, traits::PublicKeyParts, RsaPrivateKey};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct AppleFixture {
    jwks_url: String,
    signing: EncodingKey,
    kid: String,
    /// Number of JWKS fetches the server has made.
    jwks_hits: Arc<AtomicUsize>,
    _server: tokio::task::JoinHandle<()>,
}

impl AppleFixture {
    async fn new() -> Self {
        let key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let der = key.to_pkcs1_der().unwrap();
        let kid = "test-kid".to_string();
        let jwks = serde_json::json!({
            "keys": [{
                "kty": "RSA", "kid": kid, "use": "sig", "alg": "RS256",
                "n": URL_SAFE_NO_PAD.encode(key.n().to_bytes_be()),
                "e": URL_SAFE_NO_PAD.encode(key.e().to_bytes_be()),
            }]
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let jwks_hits = Arc::new(AtomicUsize::new(0));
        let hits = jwks_hits.clone();
        let app = Router::new().route(
            "/keys",
            get(move || {
                let jwks = jwks.clone();
                hits.fetch_add(1, Ordering::SeqCst);
                async move { Json(jwks) }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            jwks_url: format!("http://{addr}/keys"),
            signing: EncodingKey::from_rsa_der(der.as_bytes()),
            kid,
            jwks_hits,
            _server: server,
        }
    }

    fn token(&self, claims: serde_json::Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.kid.clone());
        encode(&header, &claims, &self.signing).unwrap()
    }

    fn valid_claims(sub: &str, email: Option<&str>) -> serde_json::Value {
        let exp = obsink_server::db::now() + 600;
        let mut claims = serde_json::json!({
            "iss": "https://appleid.apple.com", "aud": APPLE_AUDIENCE, "exp": exp, "sub": sub,
        });
        if let Some(email) = email {
            claims["email"] = serde_json::Value::String(email.to_string());
        }
        claims
    }
}

async fn apple(env: &TestEnv, token: &str, email: Option<&str>) -> reqwest::Response {
    env.req(Method::POST, "/auth/apple")
        .json(&serde_json::json!({ "identity_token": token, "device_name": "iPhone", "email": email }))
        .send()
        .await
        .unwrap()
}

async fn mint_invite(env: &TestEnv, token: &str) -> String {
    let invite: serde_json::Value = env
        .with_token(token, Method::POST, "/auth/invites")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    invite["invite"]["code"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn accepts_a_valid_identity_token_and_links_by_email() {
    if !common::db_available() {
        let _ = TestEnv::try_new().await;
        return;
    }
    let fixture = AppleFixture::new().await;
    let url = fixture.jwks_url.clone();
    let Some(env) = TestEnv::try_with(move |config| config.apple_jwks_url = url).await else {
        return;
    };

    // An email account already exists; Apple sign-in with the same email links to it.
    let email_token = env.email_token("link@example.com", "mac", None).await;
    let me: serde_json::Value = env
        .with_token(&email_token, Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let user_id = me["user"]["id"].as_str().unwrap().to_string();

    let token = fixture.token(AppleFixture::valid_claims(
        "apple-sub-1",
        Some("Link@Example.com"),
    ));
    let response = apple(&env, &token, None).await;
    assert_eq!(response.status(), 200);
    let session: serde_json::Value = response.json().await.unwrap();
    assert_eq!(session["user"]["id"], user_id);
    assert_eq!(session["user"]["email"], "link@example.com");

    // Later tokens carry no email; the subject resolves the same account.
    let token = fixture.token(AppleFixture::valid_claims("apple-sub-1", None));
    let session: serde_json::Value = apple(&env, &token, None).await.json().await.unwrap();
    assert_eq!(session["user"]["id"], user_id);
    assert_eq!(env.table_count("users").await, 1);

    // The client-supplied email hint is unverified: without a one-time code it
    // neither links to an existing account nor creates one under that address.
    let victim_invite = mint_invite(&env, &email_token).await;
    env.email_token("victim@example.com", "mac", Some(&victim_invite))
        .await;
    let token = fixture.token(AppleFixture::valid_claims("apple-sub-2", None));
    let response = apple(&env, &token, Some("victim@example.com")).await;
    assert_eq!(response.status(), 403);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("email verification required"));
    assert_eq!(env.table_count("users").await, 2);
    let (linked,): (bool,) =
        sqlx::query_as("SELECT apple_sub_hmac IS NOT NULL FROM users WHERE email_hmac = $1")
            .bind(env.state.keys.index("email", "victim@example.com"))
            .fetch_one(&env.state.pool)
            .await
            .unwrap();
    assert!(!linked, "an unverified hint must not link an Apple subject");

    // A wrong code is a 401 and burns an attempt; the right code links the
    // Apple subject to the account that owns the address.
    env.clear_email_cooldown("victim@example.com").await;
    let start: serde_json::Value = env
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "victim@example.com" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let code = start["code"].as_str().unwrap().to_string();
    let wrong = if code == "000000" { "111111" } else { "000000" };
    let response = env
        .req(Method::POST, "/auth/apple")
        .json(&serde_json::json!({ "identity_token": token, "email": "victim@example.com", "code": wrong }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let (attempts,): (i32,) =
        sqlx::query_as("SELECT attempts FROM email_codes WHERE email_hmac = $1")
            .bind(env.state.keys.index("email", "victim@example.com"))
            .fetch_one(&env.state.pool)
            .await
            .unwrap();
    assert_eq!(attempts, 1);

    let response = env
        .req(Method::POST, "/auth/apple")
        .json(&serde_json::json!({ "identity_token": token, "email": "Victim@Example.com", "code": code }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let session: serde_json::Value = response.json().await.unwrap();
    assert_eq!(session["user"]["email"], "victim@example.com");
    assert_eq!(env.table_count("users").await, 2);
    // The code was consumed: a replay with the same code is refused.
    let response = env
        .req(Method::POST, "/auth/apple")
        .json(&serde_json::json!({ "identity_token": token, "email": "victim@example.com", "code": code }))
        .send()
        .await
        .unwrap();
    // The subject is linked now, so the hint is not needed; the token alone signs in.
    assert_eq!(response.status(), 401);
    let session: serde_json::Value = apple(&env, &token, None).await.json().await.unwrap();
    assert_eq!(session["user"]["email"], "victim@example.com");

    // A verified hint for an address with no account creates one (invite required).
    let invite_code = mint_invite(&env, &email_token).await;
    let start: serde_json::Value = env
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "new@example.com" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let code = start["code"].as_str().unwrap().to_string();
    let token = fixture.token(AppleFixture::valid_claims("apple-sub-3", None));
    let response = env
        .req(Method::POST, "/auth/apple")
        .json(&serde_json::json!({
            "identity_token": token, "email": "new@example.com", "code": code, "invite_code": invite_code
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let session: serde_json::Value = response.json().await.unwrap();
    assert_eq!(session["user"]["email"], "new@example.com");
    assert_eq!(env.table_count("users").await, 3);
    env.finish().await;
}

#[tokio::test]
async fn rejects_bad_signature_wrong_audience_wrong_issuer_and_expiry() {
    if !common::db_available() {
        let _ = TestEnv::try_new().await;
        return;
    }
    let fixture = AppleFixture::new().await;
    let other = AppleFixture::new().await;
    let url = fixture.jwks_url.clone();
    let Some(env) = TestEnv::try_with(move |config| config.apple_jwks_url = url).await else {
        return;
    };

    let cases: Vec<(String, &str)> = vec![
        (
            other.token(AppleFixture::valid_claims("s", None)),
            "identity token signature is invalid",
        ),
        (
            {
                let mut claims = AppleFixture::valid_claims("s", None);
                claims["aud"] = serde_json::json!("com.other.app");
                fixture.token(claims)
            },
            "identity token audience mismatch",
        ),
        (
            {
                let mut claims = AppleFixture::valid_claims("s", None);
                claims["iss"] = serde_json::json!("https://evil.example");
                fixture.token(claims)
            },
            "identity token issuer mismatch",
        ),
        (
            {
                let mut claims = AppleFixture::valid_claims("s", None);
                claims["exp"] = serde_json::json!(1);
                fixture.token(claims)
            },
            "identity token expired",
        ),
        (
            {
                let mut claims = AppleFixture::valid_claims("s", None);
                claims["sub"] = serde_json::json!("");
                fixture.token(claims)
            },
            "identity token has no subject",
        ),
        ("not.a.jwt".to_string(), "malformed identity token"),
        ("nope".to_string(), "malformed identity token"),
    ];
    for (token, message) in cases {
        let response = apple(&env, &token, None).await;
        assert_eq!(response.status(), 401, "{message}");
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["error"],
            message
        );
    }
    let unknown_kid = {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("other-kid".to_string());
        encode(
            &header,
            &AppleFixture::valid_claims("s", None),
            &fixture.signing,
        )
        .unwrap()
    };
    let hits_before = fixture.jwks_hits.load(Ordering::SeqCst);
    let response = apple(&env, &unknown_kid, None).await;
    assert_eq!(response.status(), 401);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"],
        "unknown Apple signing key"
    );
    // One forced refresh for the unknown kid; a second unknown-kid request
    // inside the refresh window must not cost another fetch.
    assert_eq!(fixture.jwks_hits.load(Ordering::SeqCst), hits_before + 1);
    let response = apple(&env, &unknown_kid, None).await;
    assert_eq!(response.status(), 401);
    assert_eq!(fixture.jwks_hits.load(Ordering::SeqCst), hits_before + 1);

    let empty = env
        .req(Method::POST, "/auth/apple")
        .json(&serde_json::json!({ "identity_token": "" }))
        .send()
        .await
        .unwrap();
    assert_eq!(empty.status(), 400);
    assert_eq!(env.table_count("users").await, 0);
    env.finish().await;

    let Some(off) = TestEnv::try_with(|config| config.apple_client_ids = Vec::new()).await else {
        return;
    };
    let response = apple(
        &off,
        &fixture.token(AppleFixture::valid_claims("s", None)),
        None,
    )
    .await;
    assert_eq!(response.status(), 503);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"],
        "Sign in with Apple is not configured on this server"
    );
    let caps: serde_json::Value = off
        .req(Method::GET, "/")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(caps["auth"]["apple"], false);
    off.finish().await;
}
