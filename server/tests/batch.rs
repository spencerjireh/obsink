mod common;

use common::TestEnv;
use reqwest::{
    multipart::{Form, Part},
    Method,
};

fn form(ops: serde_json::Value, contents: &[(usize, &[u8])]) -> Form {
    let mut form = Form::new().part(
        "operations",
        Part::text(serde_json::json!({ "operations": ops }).to_string())
            .mime_str("application/json")
            .unwrap(),
    );
    for (index, bytes) in contents {
        form = form.part(
            "content",
            Part::bytes(bytes.to_vec())
                .file_name(index.to_string())
                .mime_str("application/octet-stream")
                .unwrap(),
        );
    }
    form
}

async fn send(env: &TestEnv, vault: &str, form: Form) -> reqwest::Response {
    env.owner(Method::POST, &format!("/vaults/{vault}/batch"))
        .multipart(form)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn batch_handles_mixed_results() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let id = env.create_vault(env.owner_token(), "notes").await;
    let seed = env
        .owner(Method::PUT, &format!("/vaults/{id}/files/note.md"))
        .header("X-Content-Hash", "hash-1")
        .body("v1")
        .send()
        .await
        .unwrap();
    assert_eq!(seed.status(), 200);

    let ops = serde_json::json!([
        { "action": "put", "path": "note.md", "parentHash": "stale", "contentHash": "hash-2" },
        { "action": "put", "path": "fresh.md", "contentHash": "hash-3", "encPath": "enc-fresh" },
    ]);
    let response = send(&env, &id, form(ops, &[(0, b"second"), (1, b"fresh")])).await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    let statuses: Vec<u64> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_u64().unwrap())
        .collect();
    assert_eq!(statuses, vec![409, 200]);
    assert_eq!(body["results"][0]["conflict"]["current"]["hash"], "hash-1");
    assert!(body["results"][1]["conflict"].is_null());

    let manifest: serde_json::Value = env
        .owner(Method::GET, &format!("/vaults/{id}/manifest"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(manifest["note.md"]["hash"], "hash-1");
    assert_eq!(manifest["fresh.md"]["hash"], "hash-3");
    assert_eq!(manifest["fresh.md"]["encPath"], "enc-fresh");
    let file = env
        .owner(Method::GET, &format!("/vaults/{id}/files/fresh.md"))
        .send()
        .await
        .unwrap();
    assert_eq!(file.bytes().await.unwrap().as_ref(), b"fresh");
    env.finish().await;
}

#[tokio::test]
async fn batch_handles_delete_operations() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let id = env.create_vault(env.owner_token(), "notes").await;
    let ops = serde_json::json!([
        { "action": "put", "path": "a", "contentHash": "ha" },
        { "action": "delete", "path": "a", "parentHash": "ha" },
        { "action": "delete", "path": "never", "parentHash": "" },
        { "action": "put", "path": "b" },
    ]);
    let response = send(&env, &id, form(ops, &[(0, b"a-bytes"), (3, b"no-hash")])).await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    let statuses: Vec<u64> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_u64().unwrap())
        .collect();
    assert_eq!(statuses, vec![200, 200, 200, 400]);
    let manifest: serde_json::Value = env
        .owner(Method::GET, &format!("/vaults/{id}/manifest"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(manifest["a"]["deleted"], true);
    assert_eq!(manifest["never"]["deleted"], true);
    assert!(manifest.get("b").is_none());

    // A missing vault surfaces per operation, as the Worker did.
    let missing = send(
        &env,
        "vault_00000000-0000-4000-8000-000000000000",
        form(
            serde_json::json!([{ "action": "put", "path": "x", "contentHash": "h" }]),
            &[(0, b"x")],
        ),
    )
    .await;
    assert_eq!(missing.status(), 200);
    assert_eq!(
        missing.json::<serde_json::Value>().await.unwrap()["results"][0]["status"],
        404
    );
    env.finish().await;
}

#[tokio::test]
async fn batch_rejects_unknown_actions_without_touching_files() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let id = env.create_vault(env.owner_token(), "notes").await;
    let seed = env
        .owner(Method::PUT, &format!("/vaults/{id}/files/note.md"))
        .header("X-Content-Hash", "hash-1")
        .header("X-Enc-Path", "enc-note")
        .body("v1")
        .send()
        .await
        .unwrap();
    assert_eq!(seed.status(), 200);

    // A missing, uppercased, or misspelled action must not fall through to
    // the delete branch, even with the right parent hash.
    for op in [
        serde_json::json!({ "path": "note.md", "parentHash": "hash-1" }),
        serde_json::json!({ "action": "PUT", "path": "note.md", "parentHash": "hash-1" }),
        serde_json::json!({ "action": "remove", "path": "note.md", "parentHash": "hash-1" }),
    ] {
        let response = send(&env, &id, form(serde_json::json!([op]), &[])).await;
        assert_eq!(response.status(), 400);
    }
    let manifest: serde_json::Value = env
        .owner(Method::GET, &format!("/vaults/{id}/manifest"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(manifest["note.md"]["deleted"], false);
    assert_eq!(manifest["note.md"]["hash"], "hash-1");
    env.finish().await;
}

#[tokio::test]
async fn batch_rejects_non_multipart_with_415() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let id = env.create_vault(env.owner_token(), "notes").await;
    let response = env
        .owner(Method::POST, &format!("/vaults/{id}/batch"))
        .json(&serde_json::json!({ "operations": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 415);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"],
        "batch requires multipart/form-data"
    );
    env.finish().await;
}

#[tokio::test]
async fn batch_rejects_missing_content_part_with_400() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let id = env.create_vault(env.owner_token(), "notes").await;
    let ops = serde_json::json!([{ "action": "put", "path": "a", "contentHash": "h" }]);
    let missing = send(&env, &id, form(ops.clone(), &[])).await;
    assert_eq!(missing.status(), 400);
    assert!(missing.json::<serde_json::Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .contains("missing content part"));

    let stray = send(&env, &id, form(ops.clone(), &[(0, b"a"), (5, b"stray")])).await;
    assert_eq!(stray.status(), 400);

    let no_ops = send(
        &env,
        &id,
        Form::new().part("content", Part::bytes(b"x".to_vec()).file_name("0")),
    )
    .await;
    assert_eq!(no_ops.status(), 400);
    assert_eq!(
        no_ops.json::<serde_json::Value>().await.unwrap()["error"],
        "operations must be an array"
    );

    let not_array = send(
        &env,
        &id,
        Form::new().part("operations", Part::text(r#"{"operations": "nope"}"#)),
    )
    .await;
    assert_eq!(not_array.status(), 400);
    assert_eq!(
        not_array.json::<serde_json::Value>().await.unwrap()["error"],
        "operations must be an array"
    );
    env.finish().await;
}

#[tokio::test]
async fn batch_body_limit_returns_413() {
    let Some(env) = TestEnv::try_with_owner_and(|config| config.max_batch_bytes = 512).await else {
        return;
    };
    let id = env.create_vault(env.owner_token(), "notes").await;
    let ops = serde_json::json!([{ "action": "put", "path": "a", "contentHash": "h" }]);
    let big = vec![b'x'; 4096];
    let response = send(&env, &id, form(ops, &[(0, &big)])).await;
    assert_eq!(response.status(), 413);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"],
        "batch too large"
    );
    env.finish().await;
}
