mod common;

use common::TestEnv;
use reqwest::Method;

async fn create_invite(env: &TestEnv, token: &str) -> String {
    let response = env
        .with_token(token, Method::POST, "/auth/invites")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["invite"]["status"], "active");
    body["invite"]["code"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn first_account_signs_up_without_invite_then_invites_are_required() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let caps: serde_json::Value = env
        .req(Method::GET, "/")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(caps["invite_required"], false);

    let first = env.email_token("first@example.com", "a", None).await;
    let caps: serde_json::Value = env
        .req(Method::GET, "/")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(caps["invite_required"], true);

    let refused = env.email_sign_in("second@example.com", "b", None).await;
    assert_eq!(refused.status(), 403);
    assert_eq!(
        refused.json::<serde_json::Value>().await.unwrap()["error"],
        "an invite code is required to create an account"
    );
    assert_eq!(env.table_count("users").await, 1);
    // The refused attempt did not burn the one-time code: same email may retry with a code.
    let code = create_invite(&env, &first).await;
    let accepted = env
        .req(Method::POST, "/auth/email/verify")
        .json(&serde_json::json!({
            "email": "second@example.com",
            "code": latest_code(&env, "second@example.com").await,
            "invite_code": code.to_lowercase(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), 200, "codes are case-insensitive");
    assert_eq!(env.table_count("users").await, 2);
    env.finish().await;
}

/// The dev server hands the code back on `/start`; fetch it again for a retry
/// without waiting out the cooldown.
async fn latest_code(env: &TestEnv, email: &str) -> String {
    env.clear_email_cooldown(email).await;
    let start: serde_json::Value = env
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": email }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    start["code"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn invite_is_single_use_and_expired_or_bogus_codes_are_rejected() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let first = env.email_token("first@example.com", "a", None).await;
    let code = create_invite(&env, &first).await;

    env.email_token("second@example.com", "b", Some(&code))
        .await;
    let reused = env
        .email_sign_in("third@example.com", "c", Some(&code))
        .await;
    assert_eq!(reused.status(), 403);
    assert_eq!(
        reused.json::<serde_json::Value>().await.unwrap()["error"],
        "invite code is invalid, used, or expired"
    );

    let expired = create_invite(&env, &first).await;
    sqlx::query("UPDATE invites SET expires = 1 WHERE code = $1")
        .bind(&expired)
        .execute(&env.state.pool)
        .await
        .unwrap();
    let late = env
        .email_sign_in("fourth@example.com", "d", Some(&expired))
        .await;
    assert_eq!(late.status(), 403);

    let bogus = env
        .email_sign_in("fifth@example.com", "e", Some("NOPE1234"))
        .await;
    assert_eq!(bogus.status(), 403);
    let malformed = env
        .email_sign_in("sixth@example.com", "f", Some("../../x"))
        .await;
    assert_eq!(malformed.status(), 403);
    assert_eq!(env.table_count("users").await, 2);

    // Existing accounts never need an invite.
    env.clear_email_cooldown("second@example.com").await;
    let second = env.email_token("second@example.com", "b2", None).await;

    // Deleting the redeemer's account nulls `used_by` (FK) but the code stays
    // spent: it must not become redeemable again.
    let deleted = env
        .with_token(&second, Method::DELETE, "/auth/account")
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 204);
    let again = env
        .email_sign_in("seventh@example.com", "g", Some(&code))
        .await;
    assert_eq!(again.status(), 403);

    let list: serde_json::Value = env
        .with_token(&first, Method::GET, "/auth/invites")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let invites = list["invites"].as_array().unwrap();
    assert_eq!(invites.len(), 2);
    let status_of =
        |code: &str| invites.iter().find(|i| i["code"] == code).unwrap()["status"].clone();
    assert_eq!(status_of(&code), "used");
    assert_eq!(status_of(&expired), "expired");
    assert!(invites.iter().find(|i| i["code"] == code).unwrap()["used_at"].is_u64());
    env.finish().await;
}

#[tokio::test]
async fn operator_can_create_and_list_invites() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    env.email_token("first@example.com", "a", None).await;
    let code = create_invite(&env, common::API_KEY).await;
    let list: serde_json::Value = env
        .operator(Method::GET, "/auth/invites")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["invites"][0]["code"], code);
    env.email_token("second@example.com", "b", Some(&code))
        .await;
    let list: serde_json::Value = env
        .operator(Method::GET, "/auth/invites")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["invites"][0]["status"], "used");
    // Users do not see operator-issued invites.
    env.clear_email_cooldown("first@example.com").await;
    let first_again = env.email_token("first@example.com", "a2", None).await;
    let mine: serde_json::Value = env
        .with_token(&first_again, Method::GET, "/auth/invites")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(mine["invites"].as_array().unwrap().is_empty());
    env.finish().await;
}

#[tokio::test]
async fn redemption_failures_are_rate_limited() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    env.email_token("first@example.com", "a", None).await;
    let mut last = 0;
    for i in 0..25 {
        let response = env
            .email_sign_in(&format!("guess{i}@example.com"), "g", Some("AAAAAAAA"))
            .await;
        last = response.status().as_u16();
        if last == 429 {
            break;
        }
        assert_eq!(last, 403);
    }
    assert_eq!(last, 429);
    env.finish().await;
}
