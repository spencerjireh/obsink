mod common;

use std::sync::atomic::Ordering;

use common::TestEnv;
use obsink_server::{
    auth::email,
    config::{SmtpConfig, SmtpTls},
};
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
            // Email code sign-in is always offered (spec §4.1): without a
            // mailer the operator mints the code with `obsink-server code`.
            "auth": { "email": true, "apple": false },
            "invite_required": false
        })
    );
    let caps = env.auth_client().capabilities().await.unwrap();
    assert!(caps.auth.email && !caps.auth.apple);
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
        .json(&serde_json::json!({ "email": "person@example.com", "code": "000000", "device": TestEnv::device("phone-1") }))
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
        .json(&serde_json::json!({ "email": "person@example.com", "code": code, "device": TestEnv::device("phone-1") }))
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

    // The core AuthClient understands the same response.
    let core_me = env.auth_client().me(&token).await.unwrap();
    assert_eq!(core_me.devices[0].name, "iPhone");
    assert_eq!(core_me.devices[0].platform, "ios");
    assert!(core_me.devices[0].current);
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
        "this server does not send email; ask the operator for a sign-in code"
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
            .json(&serde_json::json!({ "email": "lock@example.com", "code": "000000", "device": TestEnv::device("lock-1") }))
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), 401);
    }
    let locked = env
        .req(Method::POST, "/auth/email/verify")
        .json(&serde_json::json!({ "email": "lock@example.com", "code": code, "device": TestEnv::device("lock-1") }))
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
        .json(&serde_json::json!({ "email": "mail@example.com", "code": code, "device": TestEnv::device("mail") }))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    env.finish().await;
}

/// `obsink-server code <email>` (spec §4.1): a code minted from the shell
/// verifies like a mailed one; mints have no cooldown between them, only
/// the latest counts. `/auth/email/start` right after answers 503 on a
/// server that cannot send mail (before the cooldown, so the client opens
/// the code field) and 429 on one that can.
#[tokio::test]
async fn a_code_minted_from_the_shell_signs_in_without_mail() {
    let Some(env) = TestEnv::try_with(|config| config.dev_return_code = false).await else {
        return;
    };
    let now = obsink_server::db::now();
    let first = email::mint_code(&env.state, " Shell@Example.com", now)
        .await
        .unwrap();
    assert_eq!(first.len(), 6);
    assert!(first.chars().all(|c| c.is_ascii_digit()));
    assert!(
        env.mailer.sent.lock().unwrap().is_empty(),
        "no mail goes out"
    );
    let second = email::mint_code(&env.state, "shell@example.com", now)
        .await
        .unwrap();
    assert_eq!(env.table_count("email_codes").await, 1);

    let stale = env
        .req(Method::POST, "/auth/email/verify")
        .json(&serde_json::json!({ "email": "shell@example.com", "code": first, "device": TestEnv::device("shell") }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), 401, "only the latest mint verifies");
    let ok = env
        .req(Method::POST, "/auth/email/verify")
        .json(&serde_json::json!({ "email": "shell@example.com", "code": second, "device": TestEnv::device("shell") }))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200, "{}", ok.text().await.unwrap());

    let third = email::mint_code(&env.state, "shell@example.com", obsink_server::db::now())
        .await
        .unwrap();
    assert_eq!(third.len(), 6);
    let started = env
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "shell@example.com" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        started.status(),
        503,
        "no mailer: 503 before the cooldown, the client opens the code field"
    );
    assert!(email::mint_code(&env.state, "not-an-email", now)
        .await
        .is_err());
    env.finish().await;

    let Some(mail) = TestEnv::try_with(|config| {
        config.smtp = Some(smtp());
        config.dev_return_code = false;
    })
    .await
    else {
        return;
    };
    email::mint_code(&mail.state, "shell@example.com", obsink_server::db::now())
        .await
        .unwrap();
    let limited = mail
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "shell@example.com" }))
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), 429, "a mint sets last_sent");
    assert!(mail.mailer.sent.lock().unwrap().is_empty());
    mail.finish().await;
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
