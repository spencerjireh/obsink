mod common;

use std::sync::atomic::Ordering;

use common::TestEnv;
use obsink_server::config::{SmtpConfig, SmtpTls};
use reqwest::Method;

fn smtp() -> SmtpConfig {
    SmtpConfig {
        host: "mail.test".to_string(),
        port: 25,
        username: None,
        password: None,
        from: "ObSink <login@test>".to_string(),
        tls: SmtpTls::None,
    }
}

#[tokio::test]
async fn advertises_configured_sign_in_methods_without_auth() {
    let Some(env) = TestEnv::try_with(|config| config.dev_return_code = false).await else {
        return;
    };
    let body: serde_json::Value = env
        .req(Method::GET, "/")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body,
        serde_json::json!({
            "service": "obsink",
            "protocol": obsink_core::PROTOCOL_VERSION,
            "auth": { "email": false, "apple": true },
            "invite_required": false
        })
    );
    let caps = env.auth_client().capabilities().await.unwrap();
    assert!(!caps.auth.email && caps.auth.apple && !caps.auth.api_key);
    env.finish().await;
}

#[tokio::test]
async fn issues_a_session_for_a_valid_code_and_rejects_a_wrong_one() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let start = env
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "Person@Example.com " }))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 200);
    let start: serde_json::Value = start.json().await.unwrap();
    assert_eq!(start["sent"], false, "no SMTP configured");
    let code = start["code"].as_str().unwrap().to_string();
    assert_eq!(code.len(), 6);

    let wrong = env
        .req(Method::POST, "/auth/email/verify")
        .json(&serde_json::json!({ "email": "person@example.com", "code": "000000" }))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
    assert_eq!(
        wrong.json::<serde_json::Value>().await.unwrap()["error"],
        "incorrect code"
    );

    let ok = env
        .req(Method::POST, "/auth/email/verify")
        .json(&serde_json::json!({ "email": "person@example.com", "code": format!(" {code} "), "device": { "id": "phone-1", "name": "iPhone", "platform": "ios" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let session: serde_json::Value = ok.json().await.unwrap();
    let token = session["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("os_"));
    assert_eq!(session["user"]["email"], "person@example.com");
    assert!(session["session"]["id"]
        .as_str()
        .unwrap()
        .starts_with("ses_"));
    assert_eq!(session["session"]["device_id"], "phone-1");

    // Single use.
    let reuse = env
        .req(Method::POST, "/auth/email/verify")
        .json(&serde_json::json!({ "email": "person@example.com", "code": code }))
        .send()
        .await
        .unwrap();
    assert_eq!(reuse.status(), 401);
    assert_eq!(
        reuse.json::<serde_json::Value>().await.unwrap()["error"],
        "code expired; request a new one"
    );

    let me = env
        .with_token(&token, Method::GET, "/auth/me")
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), 200);
    let me: serde_json::Value = me.json().await.unwrap();
    assert!(me.get("kind").is_none(), "one principal, no kind");
    assert_eq!(me["user"]["email"], "person@example.com");
    assert_eq!(me["devices"].as_array().unwrap().len(), 1);
    assert_eq!(me["devices"][0]["id"], "phone-1");
    assert_eq!(me["devices"][0]["name"], "iPhone");
    assert_eq!(me["devices"][0]["platform"], "ios");
    assert_eq!(me["devices"][0]["current"], true);
    assert!(me["devices"][0]["vault_ids"].as_array().unwrap().is_empty());
    assert_eq!(me["usage"]["total_bytes"], 0);
    assert_eq!(me["usage"]["max_vaults"], 10);

    // The core AuthClient (still the v2 shape) parses the response: `kind`
    // and `sessions` default until OBS-136 moves it to devices.
    let core_me = env.auth_client().me(&token).await.unwrap();
    assert_eq!(core_me.kind, "");
    assert!(core_me.sessions.is_empty());
    assert_eq!(
        core_me.user.unwrap().email.as_deref(),
        Some("person@example.com")
    );
    env.finish().await;
}

#[tokio::test]
async fn rate_limits_code_requests_and_refuses_when_email_is_not_configured() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let first = env
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "a@b.co" }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);
    let second = env
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "a@b.co" }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 429);
    let bad = env
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "not-an-email" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    assert_eq!(
        bad.json::<serde_json::Value>().await.unwrap()["error"],
        "a valid email address is required"
    );
    let short_code = env
        .req(Method::POST, "/auth/email/verify")
        .json(&serde_json::json!({ "email": "a@b.co", "code": "12" }))
        .send()
        .await
        .unwrap();
    assert_eq!(short_code.status(), 400);
    env.finish().await;

    let Some(off) = TestEnv::try_with(|config| config.dev_return_code = false).await else {
        return;
    };
    let refused = off
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "a@b.co" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 503);
    assert_eq!(
        refused.json::<serde_json::Value>().await.unwrap()["error"],
        "email sign-in is not configured on this server"
    );
    off.finish().await;
}

#[tokio::test]
async fn locks_the_code_after_five_wrong_attempts() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let start: serde_json::Value = env
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "lock@example.com" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let code = start["code"].as_str().unwrap().to_string();
    for _ in 0..5 {
        let wrong = env
            .req(Method::POST, "/auth/email/verify")
            .json(&serde_json::json!({ "email": "lock@example.com", "code": "000000" }))
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), 401);
    }
    let locked = env
        .req(Method::POST, "/auth/email/verify")
        .json(&serde_json::json!({ "email": "lock@example.com", "code": code }))
        .send()
        .await
        .unwrap();
    assert_eq!(locked.status(), 401);
    assert_eq!(
        locked.json::<serde_json::Value>().await.unwrap()["error"],
        "too many attempts; request a new code"
    );
    env.finish().await;
}

#[tokio::test]
async fn returns_the_same_account_on_repeat_sign_in() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let first = env.email_token("same@example.com", "one", None).await;
    env.clear_email_cooldown("same@example.com").await;
    let second = env.email_token("same@example.com", "two", None).await;
    assert_ne!(first, second);
    let me: serde_json::Value = env
        .with_token(&second, Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["devices"].as_array().unwrap().len(), 2);
    let me_first: serde_json::Value = env
        .with_token(&first, Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me_first["user"]["id"], me["user"]["id"]);
    assert_eq!(env.table_count("users").await, 1);
    env.finish().await;
}

#[tokio::test]
async fn sends_mail_through_the_mailer_and_hides_the_code_without_the_dev_flag() {
    let Some(env) = TestEnv::try_with(|config| {
        config.smtp = Some(smtp());
        config.dev_return_code = false;
    })
    .await
    else {
        return;
    };
    let start = env
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "mail@example.com" }))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 200);
    let body: serde_json::Value = start.json().await.unwrap();
    assert_eq!(body, serde_json::json!({ "sent": true }));
    let sent = env.mailer.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "mail@example.com");
    let code: String = sent[0].1.chars().take(6).collect();
    assert!(code.chars().all(|c| c.is_ascii_digit()));
    assert!(sent[0].2.contains(&code));

    let ok = env
        .req(Method::POST, "/auth/email/verify")
        .json(&serde_json::json!({ "email": "mail@example.com", "code": code }))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    env.finish().await;
}

#[tokio::test]
async fn failed_send_does_not_consume_the_resend_cooldown() {
    let Some(env) = TestEnv::try_with(|config| config.smtp = Some(smtp())).await else {
        return;
    };
    env.mailer.fail.store(true, Ordering::SeqCst);
    let failed = env
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "retry@example.com" }))
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), 502);
    assert_eq!(
        failed.json::<serde_json::Value>().await.unwrap()["error"],
        "could not send sign-in email"
    );
    assert_eq!(env.table_count("email_codes").await, 0);

    env.mailer.fail.store(false, Ordering::SeqCst);
    let retry = env
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "retry@example.com" }))
        .send()
        .await
        .unwrap();
    assert_eq!(retry.status(), 200, "no cooldown after a failed send");
    env.finish().await;
}
