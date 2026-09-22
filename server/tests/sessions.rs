mod common;

use common::TestEnv;
use reqwest::Method;

#[tokio::test]
async fn revokes_devices_and_deletes_the_account_with_its_vaults() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let phone = env.sign_in("acct@example.com", "phone", None).await;
    env.clear_email_cooldown("acct@example.com").await;
    let laptop = env.sign_in("acct@example.com", "laptop", None).await;
    let vault = env.create_vault(&phone.token, "mine").await;
    let put = env
        .with_token(
            &phone.token,
            Method::PUT,
            &format!("/vaults/{vault}/files/tok"),
        )
        .header("X-Content-Hash", "h")
        .body("data")
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 200);

    let me: serde_json::Value = env
        .with_token(&phone.token, Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let devices = me["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 2);
    let by_id = |id: &str| devices.iter().find(|d| d["id"] == id).unwrap();
    assert_eq!(by_id("phone")["current"], true);
    assert_eq!(by_id("laptop")["current"], false);

    // Revoke the laptop from the phone; the laptop is now signed out.
    let revoke = env
        .with_token(&phone.token, Method::DELETE, "/auth/devices/laptop")
        .send()
        .await
        .unwrap();
    assert_eq!(revoke.status(), 204);
    assert_eq!(
        env.with_token(&laptop.token, Method::GET, "/auth/me")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let again = env
        .with_token(&phone.token, Method::DELETE, "/auth/devices/laptop")
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 404);
    assert_eq!(env.table_count("devices").await, 1);

    // Another account cannot revoke it either, by device or by session id.
    let code = env.mint_invite(&phone.token).await;
    let other = env.sign_in("other@example.com", "x", Some(&code)).await;
    assert_eq!(
        env.with_token(&other.token, Method::DELETE, "/auth/devices/phone")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let phone_session = sqlx::query_scalar::<_, String>(
        "SELECT id FROM sessions WHERE user_id = $1 AND device_id = 'phone'",
    )
    .bind(&phone.user_id)
    .fetch_one(&env.state.pool)
    .await
    .unwrap();
    assert_eq!(
        env.with_token(
            &other.token,
            Method::DELETE,
            &format!("/auth/sessions/{phone_session}")
        )
        .send()
        .await
        .unwrap()
        .status(),
        404
    );

    // Delete the account: devices, sessions, vaults, files, blobs all go.
    assert!(env.state.blobs.live_exists(&vault, "tok"));
    let deleted = env
        .with_token(&phone.token, Method::DELETE, "/auth/account")
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 204);
    assert_eq!(
        env.with_token(&phone.token, Method::GET, "/auth/me")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert!(!env.state.blobs.live_exists(&vault, "tok"));
    assert_eq!(env.table_count("users").await, 1);
    assert_eq!(env.table_count("vaults").await, 0);
    assert_eq!(env.table_count("vault_members").await, 0);
    assert_eq!(env.table_count("files").await, 0);
    assert_eq!(env.table_count("devices").await, 1);
    assert_eq!(env.table_count("sessions").await, 1);

    // The core AuthClient drives sign-out, which removes this device.
    env.auth_client().logout(&other.token).await.unwrap();
    assert_eq!(
        env.with_token(&other.token, Method::GET, "/auth/me")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(env.table_count("devices").await, 0);
    env.finish().await;
}

#[tokio::test]
async fn a_device_has_one_session_and_a_v2_sign_in_gets_a_legacy_device() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let first = env.sign_in("one@example.com", "mac", None).await;
    env.clear_email_cooldown("one@example.com").await;
    let second = env.sign_in("one@example.com", "mac", None).await;
    assert_ne!(first.token, second.token);
    // The earlier session of the same device is gone; the row was replaced.
    assert_eq!(
        env.with_token(&first.token, Method::GET, "/auth/me")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(env.table_count("devices").await, 1);
    assert_eq!(env.table_count("sessions").await, 1);

    // Two accounts may share a device id: the row is keyed per user.
    let code = env.mint_invite(&second.token).await;
    let other = env.sign_in("two@example.com", "mac", Some(&code)).await;
    assert_eq!(other.device_id, "mac");
    assert_eq!(env.table_count("devices").await, 2);
    assert_eq!(
        env.with_token(&second.token, Method::GET, "/auth/me")
            .send()
            .await
            .unwrap()
            .status(),
        200,
        "the other account's sign-in did not touch this one"
    );

    // The v2 spelling (`device_name`, no `device`) still signs in, with a
    // synthesized device per sign-in. Removed in OBS-143.
    env.clear_email_cooldown("one@example.com").await;
    let start: serde_json::Value = env
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "one@example.com" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let legacy = env
        .req(Method::POST, "/auth/email/verify")
        .json(&serde_json::json!({ "email": "one@example.com", "code": start["code"], "device_name": "old phone" }))
        .send()
        .await
        .unwrap();
    assert_eq!(legacy.status(), 200);
    let legacy: serde_json::Value = legacy.json().await.unwrap();
    let legacy_device = legacy["session"]["device_id"].as_str().unwrap();
    assert!(legacy_device.starts_with("legacy_"));
    let me: serde_json::Value = env
        .with_token(legacy["token"].as_str().unwrap(), Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let devices = me["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 2);
    let old = devices.iter().find(|d| d["id"] == legacy_device).unwrap();
    assert_eq!(old["name"], "old phone");
    assert_eq!(old["platform"], "unknown");

    // A malformed device object is refused before anything is written.
    env.clear_email_cooldown("one@example.com").await;
    let start: serde_json::Value = env
        .req(Method::POST, "/auth/email/start")
        .json(&serde_json::json!({ "email": "one@example.com" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bad = env
        .req(Method::POST, "/auth/email/verify")
        .json(&serde_json::json!({ "email": "one@example.com", "code": start["code"], "device": { "id": "has space", "platform": "cli" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    env.finish().await;
}

#[tokio::test]
async fn devices_can_be_renamed_and_carry_their_vaults() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let mac = env.sign_in("dev@example.com", "mac", None).await;
    let renamed = env
        .with_token(&mac.token, Method::PATCH, "/auth/devices/mac")
        .json(&serde_json::json!({ "name": "  Spencer's MacBook  " }))
        .send()
        .await
        .unwrap();
    assert_eq!(renamed.status(), 204);
    let missing = env
        .with_token(&mac.token, Method::PATCH, "/auth/devices/nope")
        .json(&serde_json::json!({ "name": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);

    let vault = env.create_vault(&mac.token, "notes").await;
    sqlx::query(
        "INSERT INTO device_vaults (user_id, device_id, vault_id, attached) VALUES ($1, 'mac', $2, 1)",
    )
    .bind(&mac.user_id)
    .bind(&vault)
    .execute(&env.state.pool)
    .await
    .unwrap();
    let me: serde_json::Value = env
        .with_token(&mac.token, Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["devices"][0]["name"], "Spencer's MacBook");
    assert_eq!(me["devices"][0]["vault_ids"], serde_json::json!([vault]));
    assert!(me["devices"][0]["last_seen"].is_u64());

    // Signing the device out removes its attachments with it.
    env.with_token(&mac.token, Method::DELETE, "/auth/session")
        .send()
        .await
        .unwrap();
    assert_eq!(env.table_count("device_vaults").await, 0);
    assert_eq!(env.table_count("vaults").await, 1);
    env.finish().await;
}

#[tokio::test]
async fn expired_sessions_are_rejected() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let old = env.sign_in("exp@example.com", "old", None).await;
    env.clear_email_cooldown("exp@example.com").await;
    let fresh = env.sign_in("exp@example.com", "new", None).await;
    sqlx::query("UPDATE sessions SET expires = 1 WHERE token_hash = $1")
        .bind(obsink_server::crypto::sha256(old.token.as_bytes()))
        .execute(&env.state.pool)
        .await
        .unwrap();
    assert_eq!(
        env.with_token(&old.token, Method::GET, "/auth/me")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let me: serde_json::Value = env
        .with_token(&fresh.token, Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // The device stays listed; only its session lapsed.
    let devices = me["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 2);
    let current = devices.iter().find(|d| d["current"] == true).unwrap();
    assert_eq!(current["id"], "new");
    env.finish().await;
}
