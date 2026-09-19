mod common;

use common::{TestEnv, API_KEY};
use reqwest::Method;

#[tokio::test]
async fn manifest_returns_etag_and_304_on_if_none_match() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let id = env.create_vault(API_KEY, "notes").await;
    let first = env
        .operator(Method::GET, &format!("/vaults/{id}/manifest"))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);
    let etag = first.headers()["etag"].to_str().unwrap().to_string();
    assert_eq!(etag, "\"0\"");
    assert_eq!(first.headers()["cache-control"], "private, no-cache");

    let cached = env
        .operator(Method::GET, &format!("/vaults/{id}/manifest"))
        .header("If-None-Match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(cached.status(), 304);
    assert_eq!(cached.headers()["etag"].to_str().unwrap(), etag);
    assert!(cached.bytes().await.unwrap().is_empty());

    for variant in [
        format!("W/{etag}"),
        format!("\"x\", {etag}"),
        "*".to_string(),
    ] {
        let response = env
            .operator(Method::GET, &format!("/vaults/{id}/manifest"))
            .header("If-None-Match", variant.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 304, "{variant}");
    }
    let other = env
        .operator(Method::GET, &format!("/vaults/{id}/manifest"))
        .header("If-None-Match", "\"99\"")
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), 200);
    env.finish().await;
}

#[tokio::test]
async fn etag_changes_after_write_and_delete() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let id = env.create_vault(API_KEY, "notes").await;
    let etag_of = |env: &TestEnv| {
        let url = env.url(&format!("/vaults/{id}/manifest"));
        let client = env.http.clone();
        async move {
            let response = client.get(url).bearer_auth(API_KEY).send().await.unwrap();
            response.headers()["etag"].to_str().unwrap().to_string()
        }
    };
    let e0 = etag_of(&env).await;
    let put = env
        .operator(Method::PUT, &format!("/vaults/{id}/files/tok"))
        .header("X-Content-Hash", "h1")
        .body("a")
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 200);
    let e1 = etag_of(&env).await;
    assert_ne!(e0, e1);
    // A rejected write leaves the ETag alone.
    let conflict = env
        .operator(Method::PUT, &format!("/vaults/{id}/files/tok"))
        .header("X-Parent-Hash", "stale")
        .header("X-Content-Hash", "h2")
        .body("b")
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), 409);
    assert_eq!(etag_of(&env).await, e1);
    let del = env
        .operator(Method::DELETE, &format!("/vaults/{id}/files/tok"))
        .header("X-Parent-Hash", "h1")
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 200);
    assert_ne!(etag_of(&env).await, e1);
    env.finish().await;
}
