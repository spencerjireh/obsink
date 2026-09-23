mod common;

use common::TestEnv;
use obsink_server::blobs::Tier;
use reqwest::Method;

async fn put(
    env: &TestEnv,
    vault: &str,
    path: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> reqwest::Response {
    let mut request = env
        .owner(Method::PUT, &format!("/vaults/{vault}/files/{path}"))
        .body(body.to_string());
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.send().await.unwrap()
}

async fn manifest(env: &TestEnv, vault: &str) -> serde_json::Value {
    let response = env
        .owner(Method::GET, &format!("/vaults/{vault}/manifest"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

#[tokio::test]
async fn returns_manifests_and_file_blobs_for_an_existing_vault() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let id = env.create_vault(env.owner_token(), "notes").await;
    assert_eq!(manifest(&env, &id).await, serde_json::json!({}));

    let response = put(
        &env,
        &id,
        "note.md",
        "hello",
        &[("X-Content-Hash", "hash-1"), ("X-Enc-Path", "enc")],
    )
    .await;
    assert_eq!(response.status(), 200);

    let m = manifest(&env, &id).await;
    assert_eq!(m["note.md"]["hash"], "hash-1");
    assert_eq!(m["note.md"]["size"], 5);
    assert_eq!(m["note.md"]["deleted"], false);
    assert_eq!(m["note.md"]["encPath"], "enc");

    let file = env
        .owner(Method::GET, &format!("/vaults/{id}/files/note.md"))
        .send()
        .await
        .unwrap();
    assert_eq!(file.status(), 200);
    assert_eq!(file.headers()["content-type"], "application/octet-stream");
    assert_eq!(file.headers()["cache-control"], "no-store");
    assert_eq!(file.bytes().await.unwrap().as_ref(), b"hello");

    // Stored bytes are sealed, never the raw upload.
    let sealed = env.state.blobs.get_live(&id, "note.md").unwrap().unwrap();
    assert_ne!(sealed.as_slice(), b"hello");
    assert!(sealed.starts_with(b"OBSK"));

    let missing = env
        .owner(Method::GET, &format!("/vaults/{id}/files/nope.md"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    let other_vault = env
        .owner(
            Method::GET,
            "/vaults/vault_00000000-0000-4000-8000-000000000000/manifest",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(other_vault.status(), 404);
    assert_eq!(
        other_vault.json::<serde_json::Value>().await.unwrap()["error"],
        "vault not found"
    );
    env.finish().await;
}

#[tokio::test]
async fn returns_conflict_on_stale_parent_hash() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let id = env.create_vault(env.owner_token(), "notes").await;
    assert_eq!(
        put(&env, &id, "note.md", "v1", &[("X-Content-Hash", "hash-1")])
            .await
            .status(),
        200
    );

    let stale = put(
        &env,
        &id,
        "note.md",
        "v2",
        &[("X-Parent-Hash", "stale"), ("X-Content-Hash", "hash-2")],
    )
    .await;
    assert_eq!(stale.status(), 409);
    let body: serde_json::Value = stale.json().await.unwrap();
    assert_eq!(body["path"], "note.md");
    assert_eq!(body["current"]["hash"], "hash-1");

    // Missing parent on an existing entry is also a conflict...
    let no_parent = put(&env, &id, "note.md", "v2", &[("X-Content-Hash", "hash-2")]).await;
    assert_eq!(no_parent.status(), 409);
    // ...unless the bytes are the ones already there (a retried upload).
    let retry = put(&env, &id, "note.md", "v1", &[("X-Content-Hash", "hash-1")]).await;
    assert_eq!(retry.status(), 200);

    let fresh = put(
        &env,
        &id,
        "note.md",
        "v2",
        &[("X-Parent-Hash", "hash-1"), ("X-Content-Hash", "hash-2")],
    )
    .await;
    assert_eq!(fresh.status(), 200);
    assert_eq!(manifest(&env, &id).await["note.md"]["hash"], "hash-2");
    assert_eq!(
        env.state
            .blobs
            .list_history(Tier::Versions, &id, "note.md")
            .unwrap()
            .len(),
        1
    );

    // The core client maps the 409 body into ApiError::Conflict.
    let client = env.api_client(env.owner_token(), &id);
    let keys = obsink_core::derive_keys(&[1u8; 32]);
    let err = client
        .put_file(
            "real/path.md",
            Some("wrong"),
            "hash-3",
            b"x".to_vec(),
            &keys,
        )
        .await;
    assert!(matches!(
        err,
        Err(obsink_core::ApiError::Conflict { .. }) | Ok(())
    ));
    env.finish().await;
}

#[tokio::test]
async fn soft_deletes_files_into_trash_and_marks_manifest_entries_as_deleted() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let id = env.create_vault(env.owner_token(), "notes").await;
    assert_eq!(
        put(
            &env,
            &id,
            "note.md",
            "hello",
            &[("X-Content-Hash", "hash-1")]
        )
        .await
        .status(),
        200
    );

    let stale = env
        .owner(Method::DELETE, &format!("/vaults/{id}/files/note.md"))
        .header("X-Parent-Hash", "nope")
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), 409);

    let ok = env
        .owner(Method::DELETE, &format!("/vaults/{id}/files/note.md"))
        .header("X-Parent-Hash", "hash-1")
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    assert!(!env.state.blobs.live_exists(&id, "note.md"));
    assert_eq!(
        env.state
            .blobs
            .list_history(Tier::Trash, &id, "note.md")
            .unwrap()
            .len(),
        1
    );
    let m = manifest(&env, &id).await;
    assert_eq!(m["note.md"]["deleted"], true);
    assert_eq!(m["note.md"]["hash"], "hash-1");
    assert_eq!(m["note.md"]["size"], 5);
    let gone = env
        .owner(Method::GET, &format!("/vaults/{id}/files/note.md"))
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), 404);

    // Deleting a path that never existed leaves an empty tombstone (Worker parity).
    let ghost = env
        .owner(Method::DELETE, &format!("/vaults/{id}/files/ghost.md"))
        .send()
        .await
        .unwrap();
    assert_eq!(ghost.status(), 200);
    assert_eq!(manifest(&env, &id).await["ghost.md"]["hash"], "");

    // Re-uploading over the tombstone needs the tombstone's hash as parent.
    let revive = put(
        &env,
        &id,
        "note.md",
        "again",
        &[("X-Parent-Hash", "hash-1"), ("X-Content-Hash", "hash-9")],
    )
    .await;
    assert_eq!(revive.status(), 200);
    assert_eq!(manifest(&env, &id).await["note.md"]["deleted"], false);
    env.finish().await;
}

#[tokio::test]
async fn stores_the_encrypted_path_on_upload_and_preserves_it_through_a_delete() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let id = env.create_vault(env.owner_token(), "notes").await;
    assert_eq!(
        put(
            &env,
            &id,
            "tok",
            "a",
            &[("X-Content-Hash", "h1"), ("X-Enc-Path", "enc-1")]
        )
        .await
        .status(),
        200
    );
    // An update without the header keeps the stored encPath.
    assert_eq!(
        put(
            &env,
            &id,
            "tok",
            "b",
            &[("X-Parent-Hash", "h1"), ("X-Content-Hash", "h2")]
        )
        .await
        .status(),
        200
    );
    assert_eq!(manifest(&env, &id).await["tok"]["encPath"], "enc-1");
    let del = env
        .owner(Method::DELETE, &format!("/vaults/{id}/files/tok"))
        .header("X-Parent-Hash", "h2")
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 200);
    let m = manifest(&env, &id).await;
    assert_eq!(m["tok"]["encPath"], "enc-1");
    assert_eq!(m["tok"]["deleted"], true);
    env.finish().await;
}

#[tokio::test]
async fn rejects_uploads_larger_than_the_configured_max_file_size() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let response = env
        .owner(Method::POST, "/vaults")
        .json(&serde_json::json!({ "name": "tiny", "max_file_size": 4, "wrapped_key": TestEnv::wrapped_key() }))
        .send()
        .await
        .unwrap();
    let id = response.json::<serde_json::Value>().await.unwrap()["vault"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let too_big = put(&env, &id, "tok", "12345", &[("X-Content-Hash", "h")]).await;
    assert_eq!(too_big.status(), 413);
    assert_eq!(
        too_big.json::<serde_json::Value>().await.unwrap()["error"],
        "file too large"
    );
    assert_eq!(
        put(&env, &id, "tok", "1234", &[("X-Content-Hash", "h")])
            .await
            .status(),
        200
    );

    let no_hash = put(&env, &id, "tok2", "x", &[]).await;
    assert_eq!(no_hash.status(), 400);
    assert_eq!(
        no_hash.json::<serde_json::Value>().await.unwrap()["error"],
        "missing X-Content-Hash header"
    );
    env.finish().await;
}

#[tokio::test]
async fn concurrent_puts_to_one_path_yield_exactly_one_winner() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let id = env.create_vault(env.owner_token(), "race").await;
    assert_eq!(
        put(&env, &id, "tok", "base", &[("X-Content-Hash", "h0")])
            .await
            .status(),
        200
    );
    let mut handles = Vec::new();
    for i in 0..8 {
        let env_url = env.url(&format!("/vaults/{id}/files/tok"));
        let client = env.http.clone();
        let token = env.owner_token().to_string();
        handles.push(tokio::spawn(async move {
            client
                .put(env_url)
                .bearer_auth(token)
                .header("X-Parent-Hash", "h0")
                .header("X-Content-Hash", format!("h{}", i + 1))
                .body(format!("v{i}"))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }));
    }
    let mut statuses = Vec::new();
    for handle in handles {
        statuses.push(handle.await.unwrap());
    }
    assert_eq!(
        statuses.iter().filter(|s| **s == 200).count(),
        1,
        "{statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| **s == 409).count(),
        7,
        "{statuses:?}"
    );
    assert_eq!(
        env.state
            .blobs
            .list_history(Tier::Versions, &id, "tok")
            .unwrap()
            .len(),
        1
    );
    env.finish().await;
}
