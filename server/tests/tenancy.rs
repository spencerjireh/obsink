mod common;

use common::{TestEnv, API_KEY};
use reqwest::Method;

#[tokio::test]
async fn isolates_vault_lists_between_accounts_and_the_operator() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let alice = env.email_token("alice@example.com", "a", None).await;
    let invite: serde_json::Value = env
        .with_token(&alice, Method::POST, "/auth/invites")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bob = env
        .email_token("bob@example.com", "b", invite["invite"]["code"].as_str())
        .await;

    let alice_vault = env.create_vault(&alice, "alice").await;
    let bob_vault = env.create_vault(&bob, "bob").await;
    let operator_vault = env.create_vault(API_KEY, "ops").await;

    let ids =
        |list: Vec<obsink_core::VaultSummary>| list.into_iter().map(|v| v.id).collect::<Vec<_>>();
    assert_eq!(
        ids(env.api_client(&alice, "").list_vaults().await.unwrap()),
        vec![alice_vault.clone()]
    );
    assert_eq!(
        ids(env.api_client(&bob, "").list_vaults().await.unwrap()),
        vec![bob_vault.clone()]
    );
    assert_eq!(
        ids(env.api_client(API_KEY, "").list_vaults().await.unwrap()),
        vec![operator_vault.clone()]
    );

    // Cross-tenant access is indistinguishable from a missing vault.
    for (token, vault) in [
        (bob.as_str(), &alice_vault),
        (API_KEY, &alice_vault),
        (alice.as_str(), &operator_vault),
    ] {
        let manifest = env
            .with_token(token, Method::GET, &format!("/vaults/{vault}/manifest"))
            .send()
            .await
            .unwrap();
        assert_eq!(manifest.status(), 404);
        let put = env
            .with_token(token, Method::PUT, &format!("/vaults/{vault}/files/tok"))
            .header("X-Content-Hash", "h")
            .body("x")
            .send()
            .await
            .unwrap();
        assert_eq!(put.status(), 404);
        let del = env
            .with_token(token, Method::DELETE, &format!("/vaults/{vault}"))
            .send()
            .await
            .unwrap();
        assert_eq!(del.status(), 404);
    }
    assert_eq!(env.table_count("vaults").await, 3);
    env.finish().await;
}

#[tokio::test]
async fn enforces_the_per_account_vault_limit_and_per_vault_byte_budget() {
    let Some(env) = TestEnv::try_with(|config| {
        config.max_vaults_per_user = 2;
        config.max_vault_bytes = 10;
    })
    .await
    else {
        return;
    };
    let user = env.email_token("quota@example.com", "q", None).await;
    let first = env.create_vault(&user, "one").await;
    env.create_vault(&user, "two").await;
    let third = env
        .with_token(&user, Method::POST, "/vaults")
        .json(&serde_json::json!({ "name": "three" }))
        .send()
        .await
        .unwrap();
    assert_eq!(third.status(), 403);
    assert_eq!(
        third.json::<serde_json::Value>().await.unwrap()["error"],
        "vault limit reached (2 per account)"
    );

    let put = |path: &'static str, body: &'static str, parent: Option<&'static str>| {
        let mut request = env
            .with_token(&user, Method::PUT, &format!("/vaults/{first}/files/{path}"))
            .header("X-Content-Hash", format!("h-{body}"))
            .body(body);
        if let Some(parent) = parent {
            request = request.header("X-Parent-Hash", parent);
        }
        request.send()
    };
    assert_eq!(put("a", "123456", None).await.unwrap().status(), 200);
    let over = put("b", "12345", None).await.unwrap();
    assert_eq!(over.status(), 507);
    assert_eq!(
        over.json::<serde_json::Value>().await.unwrap()["error"],
        "vault storage limit reached"
    );
    assert_eq!(put("b", "1234", None).await.unwrap().status(), 200);
    // Replacing a file counts only the delta.
    assert_eq!(
        put("a", "12345", Some("h-123456")).await.unwrap().status(),
        200
    );

    let me: serde_json::Value = env
        .with_token(&user, Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["usage"]["total_bytes"], 9);
    assert_eq!(me["usage"]["max_vault_bytes"], 10);
    assert_eq!(me["usage"]["max_vaults"], 2);
    assert_eq!(me["usage"]["vaults"].as_array().unwrap().len(), 2);

    // The operator has no budget.
    let ops = env.create_vault(API_KEY, "ops").await;
    let big = env
        .operator(Method::PUT, &format!("/vaults/{ops}/files/tok"))
        .header("X-Content-Hash", "h")
        .body("more than ten bytes here")
        .send()
        .await
        .unwrap();
    assert_eq!(big.status(), 200);
    let me: serde_json::Value = env
        .operator(Method::GET, "/auth/me")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["kind"], "operator");
    assert!(me["usage"]["max_vault_bytes"].is_null());
    assert_eq!(me["usage"]["vaults"][0]["id"], ops);
    env.finish().await;
}

/// A test that panics never reaches `finish()`; `Drop` must still remove
/// the database it created.
#[tokio::test]
async fn a_dropped_env_removes_its_database() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let admin_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let db_name = env.db_name().to_string();
    assert!(TestEnv::database_exists(&admin_url, &db_name).await);

    // Dropping from an async context is what a panic unwinding through a
    // `#[tokio::test]` does; the cleanup runs on its own thread.
    tokio::task::spawn_blocking(move || drop(env))
        .await
        .expect("drop on a blocking thread");

    assert!(
        !TestEnv::database_exists(&admin_url, &db_name).await,
        "{db_name} was left behind"
    );
}
