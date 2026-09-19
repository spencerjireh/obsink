mod common;

use axum::{routing::get, Json, Router};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use common::{TestEnv, APPLE_AUDIENCE};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::Method;
use rsa::{pkcs1::EncodeRsaPrivateKey, traits::PublicKeyParts, RsaPrivateKey};

struct AppleFixture {
    jwks_url: String,
    signing: EncodingKey,
    kid: String,
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
        let app = Router::new().route(
            "/keys",
            get(move || {
                let jwks = jwks.clone();
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

    // The client-supplied email hint links when the token has none (needs an invite: not first user).
    let invite: serde_json::Value = env
        .with_token(&email_token, Method::POST, "/auth/invites")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let code = invite["invite"]["code"].as_str().unwrap().to_string();
    let token = fixture.token(AppleFixture::valid_claims("apple-sub-2", None));
    let response = env
        .req(Method::POST, "/auth/apple")
        .json(&serde_json::json!({ "identity_token": token, "email": "hint@example.com", "invite_code": code }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let session: serde_json::Value = response.json().await.unwrap();
    assert_eq!(session["user"]["email"], "hint@example.com");
    assert_eq!(env.table_count("users").await, 2);
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
    let response = apple(&env, &unknown_kid, None).await;
    assert_eq!(response.status(), 401);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"],
        "unknown Apple signing key"
    );

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
