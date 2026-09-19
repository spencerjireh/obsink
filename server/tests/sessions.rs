mod common;

use common::{TestEnv, API_KEY};
use reqwest::Method;

#[tokio::test]
async fn revokes_sessions_and_deletes_the_account_with_its_vaults() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let phone = env.email_token("acct@example.com", "phone", None).await;
    env.clear_email_cooldown("acct@example.com").await;
    let laptop = env.email_token("acct@example.com", "laptop", None).await;
    let vault = env.create_vault(&phone, "mine").await;
    let put = env
        .with_token(&phone, Method::PUT, &format!("/vaults/{vault}/files/tok"))
        .header("X-Content-Hash", "h")
        .body("data")
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 200);

    let me: serde_json::Value = env
        .with_token(&phone, Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = me["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    let laptop_id = sessions
        .iter()
        .find(|s| s["deviceName"] == "laptop")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Revoke the laptop from the phone; the laptop is now signed out.
    let revoke = env
        .with_token(
            &phone,
            Method::DELETE,
            &format!("/auth/sessions/{laptop_id}"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(revoke.status(), 204);
    assert_eq!(
        env.with_token(&laptop, Method::GET, "/auth/me")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let again = env
        .with_token(
            &phone,
            Method::DELETE,
            &format!("/auth/sessions/{laptop_id}"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 404);

    // Another account cannot revoke it either.
    let invite: serde_json::Value = env
        .with_token(&phone, Method::POST, "/auth/invites")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let other = env
        .email_token("other@example.com", "x", invite["invite"]["code"].as_str())
        .await;
    let me: serde_json::Value = env
        .with_token(&phone, Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let phone_id = me["sessions"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        env.with_token(
            &other,
            Method::DELETE,
            &format!("/auth/sessions/{phone_id}")
        )
        .send()
        .await
        .unwrap()
        .status(),
        404
    );

    // Delete the account: sessions, vaults, files, blobs all go.
    assert!(env.state.blobs.live_exists(&vault, "tok"));
    let deleted = env
        .with_token(&phone, Method::DELETE, "/auth/account")
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 204);
    assert_eq!(
        env.with_token(&phone, Method::GET, "/auth/me")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert!(!env.state.blobs.live_exists(&vault, "tok"));
    assert_eq!(env.table_count("users").await, 1);
    assert_eq!(env.table_count("vaults").await, 0);
    assert_eq!(env.table_count("files").await, 0);
    assert_eq!(env.table_count("sessions").await, 1);

    // The core AuthClient drives sign-out.
    env.auth_client().logout(&other).await.unwrap();
    assert_eq!(
        env.with_token(&other, Method::GET, "/auth/me")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    env.finish().await;
}

#[tokio::test]
async fn the_operator_bearer_has_no_session_to_revoke() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let response = env
        .operator(Method::DELETE, "/auth/session")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"],
        "operator bearer has no session"
    );
    let me: serde_json::Value = env
        .operator(Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["kind"], "operator");
    assert!(me["user"].is_null());
    env.finish().await;
}

#[tokio::test]
async fn operator_cannot_delete_account_and_nothing_is_deleted() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let vault = env.create_vault(API_KEY, "ops").await;
    let response = env
        .operator(Method::DELETE, "/auth/account")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"],
        "operator bearer has no account"
    );
    let listed = env.api_client(API_KEY, "").list_vaults().await.unwrap();
    assert_eq!(listed[0].id, vault);
    env.finish().await;
}

#[tokio::test]
async fn expired_sessions_are_hidden_and_rejected() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let token = env.email_token("exp@example.com", "old", None).await;
    env.clear_email_cooldown("exp@example.com").await;
    let fresh = env.email_token("exp@example.com", "new", None).await;
    sqlx::query("UPDATE sessions SET expires = 1 WHERE token_hash = $1")
        .bind(obsink_server::crypto::sha256(token.as_bytes()))
        .execute(&env.state.pool)
        .await
        .unwrap();
    assert_eq!(
        env.with_token(&token, Method::GET, "/auth/me")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let me: serde_json::Value = env
        .with_token(&fresh, Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(me["sessions"][0]["deviceName"], "new");
    env.finish().await;
}
