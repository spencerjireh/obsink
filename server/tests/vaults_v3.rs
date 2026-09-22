//! The v3 vault surface (spec §4.3): the list shape, wrapped keys, rename,
//! device attachment, and membership scoping.
mod common;

use common::TestEnv;
use reqwest::Method;

async fn list(env: &TestEnv, token: &str) -> serde_json::Value {
    env.with_token(token, Method::GET, "/vaults")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn lists_vaults_with_wrapped_keys_devices_and_revisions() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let owner = env.owner.clone().unwrap();
    let (account_key, _) = env.set_passphrase(&owner, "correct horse battery").await;
    let vault_key = obsink_core::new_key();

    // A wrapped key needs an account key: a vault created before the
    // passphrase (the v2 path) is allowed but records none.
    let bare = env.create_vault(env.owner_token(), "bare").await;
    let created = env
        .owner(Method::POST, "/vaults")
        .json(&serde_json::json!({ "name": "notes", "wrapped_key": "AAAA" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 400);
    assert_eq!(
        created.json::<serde_json::Value>().await.unwrap()["error"],
        "wrapped_key must decode to 60 bytes"
    );
    let wrapped = obsink_core::wrap_vault_key(&account_key, &vault_key, "pending").unwrap();
    let created = env
        .owner(Method::POST, "/vaults")
        .json(&serde_json::json!({ "name": "notes", "wrapped_key": obsink_core::encode_base64(&wrapped) }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let created: serde_json::Value = created.json().await.unwrap();
    let notes = created["vault"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["vault"]["revision"], 0);
    assert_eq!(created["vault"]["bytes"], 0);

    // One write bumps the revision and last_write; the list shows it.
    let put = env
        .owner(Method::PUT, &format!("/vaults/{notes}/files/tok"))
        .header("X-Content-Hash", "h1")
        .body("hello")
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 200);
    let listed = list(&env, env.owner_token()).await;
    let vaults = listed["vaults"].as_array().unwrap();
    assert_eq!(vaults.len(), 2);
    let entry = vaults.iter().find(|v| v["id"] == notes).unwrap();
    assert_eq!(entry["name"], "notes");
    assert_eq!(entry["revision"], 1);
    assert_eq!(entry["bytes"], 5);
    assert!(entry["last_write"].as_u64().unwrap() >= entry["created"].as_u64().unwrap());
    assert_eq!(
        entry["wrapped_key"].as_str().unwrap(),
        obsink_core::encode_base64(&wrapped)
    );
    assert!(entry["devices"].as_array().unwrap().is_empty());
    let bare_entry = vaults.iter().find(|v| v["id"] == bare).unwrap();
    assert!(bare_entry["wrapped_key"].is_null());

    // The core client parses the new shape and recovers the key.
    let core_list = env
        .api_client(env.owner_token(), "")
        .list_vaults()
        .await
        .unwrap();
    let core_entry = core_list.iter().find(|v| v.id == notes).unwrap();
    let blob = obsink_core::decode_base64(core_entry.wrapped_key.as_deref().unwrap()).unwrap();
    assert_eq!(
        obsink_core::unwrap_vault_key(&account_key, &blob, "pending").unwrap(),
        vault_key
    );

    // Attach this device, report a checkpoint, and see it on the vault and on /auth/me.
    let attach = env
        .owner(Method::PUT, &format!("/vaults/{notes}/devices/self"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(attach.status(), 204);
    let report = env
        .owner(Method::PUT, &format!("/vaults/{notes}/devices/self"))
        .json(&serde_json::json!({ "revision": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(report.status(), 204);
    let listed = list(&env, env.owner_token()).await;
    let entry = listed["vaults"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == notes)
        .unwrap()
        .clone();
    let devices = entry["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["id"], common::OWNER_DEVICE);
    assert_eq!(devices[0]["platform"], "cli");
    assert_eq!(devices[0]["last_revision"], 1);
    assert!(devices[0]["last_synced"].is_u64());
    let me: serde_json::Value = env
        .owner(Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["devices"][0]["vault_ids"], serde_json::json!([notes]));

    // Detach: the row goes, the vault stays.
    let detach = env
        .owner(Method::DELETE, &format!("/vaults/{notes}/devices/self"))
        .send()
        .await
        .unwrap();
    assert_eq!(detach.status(), 204);
    let listed = list(&env, env.owner_token()).await;
    assert!(listed["vaults"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == notes)
        .unwrap()["devices"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(env.table_count("vaults").await, 2);
    env.finish().await;
}

#[tokio::test]
async fn renames_and_scopes_by_membership() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let vault = env.create_vault(env.owner_token(), "old name").await;
    let renamed = env
        .owner(Method::PATCH, &format!("/vaults/{vault}"))
        .json(&serde_json::json!({ "name": "  new name  " }))
        .send()
        .await
        .unwrap();
    assert_eq!(renamed.status(), 204);
    let listed = list(&env, env.owner_token()).await;
    assert_eq!(listed["vaults"][0]["name"], "new name");
    let blank = env
        .owner(Method::PATCH, &format!("/vaults/{vault}"))
        .json(&serde_json::json!({ "name": " " }))
        .send()
        .await
        .unwrap();
    assert_eq!(blank.status(), 400);

    // A non-member sees 404 on every route, attachment included.
    let other = env.invited("other@example.com", "o").await;
    for (method, path) in [
        (Method::PATCH, format!("/vaults/{vault}")),
        (Method::DELETE, format!("/vaults/{vault}")),
        (Method::PUT, format!("/vaults/{vault}/devices/self")),
        (Method::DELETE, format!("/vaults/{vault}/devices/self")),
        (Method::GET, format!("/vaults/{vault}/manifest")),
        (Method::GET, format!("/vaults/{vault}/files/tok")),
    ] {
        let response = env
            .with_token(&other.token, method.clone(), &path)
            .json(&serde_json::json!({ "name": "x" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404, "{method} {path}");
    }
    assert!(list(&env, &other.token).await["vaults"]
        .as_array()
        .unwrap()
        .is_empty());

    // A member row without the owner role can read and write but not rename or delete.
    sqlx::query(
        "INSERT INTO vault_members (vault_id, user_id, role, wrapped_key, created) VALUES ($1, $2, 'member', NULL, 1)",
    )
    .bind(&vault)
    .bind(&other.user_id)
    .execute(&env.state.pool)
    .await
    .unwrap();
    assert_eq!(list(&env, &other.token).await["vaults"][0]["id"], vault);
    let put = env
        .with_token(
            &other.token,
            Method::PUT,
            &format!("/vaults/{vault}/files/tok"),
        )
        .header("X-Content-Hash", "h")
        .body("shared")
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 200);
    let forbidden = env
        .with_token(&other.token, Method::DELETE, &format!("/vaults/{vault}"))
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), 403);
    let forbidden = env
        .with_token(&other.token, Method::PATCH, &format!("/vaults/{vault}"))
        .json(&serde_json::json!({ "name": "theirs" }))
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), 403);

    // The owner's quota counts the vaults it owns, not the ones it is a member of.
    let me: serde_json::Value = env
        .with_token(&other.token, Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(me["usage"]["vaults"].as_array().unwrap().is_empty());
    env.finish().await;
}
