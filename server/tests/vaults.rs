mod common;

use common::TestEnv;
use reqwest::Method;

#[tokio::test]
async fn rejects_unauthorized_requests() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let response = env.req(Method::GET, "/vaults").send().await.unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"],
        "unauthorized"
    );

    let wrong = env
        .with_token("os_nope", Method::GET, "/vaults")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);

    // Unknown routes with a valid bearer are 404, and 405s are folded into 404.
    let missing = env.operator(Method::GET, "/nope").send().await.unwrap();
    assert_eq!(missing.status(), 404);
    assert_eq!(
        missing.json::<serde_json::Value>().await.unwrap()["error"],
        "not_found"
    );
    let wrong_method = env.operator(Method::PUT, "/vaults").send().await.unwrap();
    assert_eq!(wrong_method.status(), 404);
    env.finish().await;
}

#[tokio::test]
async fn creates_vaults_and_lists_them() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let response = env
        .operator(Method::POST, "/vaults")
        .json(&serde_json::json!({ "name": "  Notes  ", "max_file_size": 1024 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let body: serde_json::Value = response.json().await.unwrap();
    let id = body["vault"]["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("vault_"));
    assert_eq!(body["vault"]["name"], "Notes");
    assert_eq!(body["vault"]["max_file_size"], 1024);

    let listed = env
        .api_client(common::API_KEY, "")
        .list_vaults()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    assert_eq!(listed[0].name, "Notes");

    // The per-vault cap cannot exceed the server-wide maximum.
    let big = env
        .operator(Method::POST, "/vaults")
        .json(&serde_json::json!({ "name": "big", "max_file_size": u64::MAX }))
        .send()
        .await
        .unwrap();
    assert_eq!(big.status(), 201);
    let big: serde_json::Value = big.json().await.unwrap();
    assert_eq!(
        big["vault"]["max_file_size"],
        obsink_server::config::DEFAULT_MAX_FILE_BYTES
    );
    env.finish().await;
}

#[tokio::test]
async fn malformed_json_returns_400() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let response = env
        .operator(Method::POST, "/vaults")
        .header("content-type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"],
        "body must be JSON"
    );

    let blank = env
        .operator(Method::POST, "/vaults")
        .json(&serde_json::json!({ "name": "   " }))
        .send()
        .await
        .unwrap();
    assert_eq!(blank.status(), 400);
    assert_eq!(
        blank.json::<serde_json::Value>().await.unwrap()["error"],
        "vault name is required"
    );
    env.finish().await;
}

#[tokio::test]
async fn deletes_a_vault_and_all_its_blobs() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let id = env.create_vault(common::API_KEY, "doomed").await;
    let client = env.api_client(common::API_KEY, &id);
    let put = env
        .operator(Method::PUT, &format!("/vaults/{id}/files/tok"))
        .header("X-Content-Hash", "h1")
        .body("bytes")
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 200);
    assert!(env.state.blobs.live_exists(&id, "tok"));

    client.delete_vault().await.unwrap();
    assert!(!env.state.blobs.live_exists(&id, "tok"));
    assert!(env
        .state
        .blobs
        .vault_dirs(obsink_server::blobs::Tier::Live)
        .unwrap()
        .is_empty());
    assert!(client.list_vaults().await.unwrap().is_empty());
    assert_eq!(env.table_count("files").await, 0);

    let again = env
        .operator(Method::DELETE, &format!("/vaults/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 404);
    let bogus = env
        .operator(Method::DELETE, "/vaults/../etc")
        .send()
        .await
        .unwrap();
    assert_eq!(bogus.status(), 404);
    env.finish().await;
}
