//! Version and trash reads (spec §4.3, §8.2, §9.3).
mod common;

use common::TestEnv;
use reqwest::Method;

async fn put(env: &TestEnv, vault: &str, path: &str, body: &str, parent: Option<&str>) {
    let mut request = env
        .owner(Method::PUT, &format!("/vaults/{vault}/files/{path}"))
        .header("X-Content-Hash", format!("h-{body}"))
        .header("X-Enc-Path", format!("enc-{path}"))
        .body(body.to_string());
    if let Some(parent) = parent {
        request = request.header("X-Parent-Hash", parent);
    }
    let response = request.send().await.unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
}

#[tokio::test]
async fn lists_and_serves_archived_versions() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let vault = env.create_vault(env.owner_token(), "notes").await;
    put(&env, &vault, "note", "one", None).await;
    put(&env, &vault, "note", "two", Some("h-one")).await;
    put(&env, &vault, "note", "three", Some("h-two")).await;

    let listed: serde_json::Value = env
        .owner(Method::GET, &format!("/vaults/{vault}/history/note"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let versions = listed["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2, "two overwrites archive two versions");
    for version in versions {
        assert!(version["ts"].is_u64());
        assert!(version["size"].as_u64().unwrap() > 3, "sealed size");
        assert!(version["name"]
            .as_str()
            .unwrap()
            .starts_with(&version["ts"].to_string()));
    }
    // Newest first: the first entry is "two", then "one".
    let mut bodies = Vec::new();
    for version in versions {
        let name = version["name"].as_str().unwrap();
        let response = env
            .owner(
                Method::GET,
                &format!("/vaults/{vault}/versions/{name}/note"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-store");
        bodies.push(response.text().await.unwrap());
    }
    assert_eq!(bodies, vec!["two", "one"]);

    // A file without history lists nothing; a bogus entry name is 404, as
    // is anything that is not a plain `<unix>[-n]` name.
    let empty: serde_json::Value = env
        .owner(Method::GET, &format!("/vaults/{vault}/history/other"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(empty["versions"].as_array().unwrap().is_empty());
    for name in ["999", "..", "-1"] {
        let response = env
            .owner(
                Method::GET,
                &format!("/vaults/{vault}/versions/{name}/note"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404, "{name}");
    }

    // Another account is not a member: 404 everywhere.
    let other = env.invited("other@example.com", "o").await;
    for path in [
        format!("/vaults/{vault}/history/note"),
        format!("/vaults/{vault}/trash"),
        format!("/vaults/{vault}/trash/note"),
    ] {
        let response = env
            .with_token(&other.token, Method::GET, &path)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404, "{path}");
    }
    env.finish().await;
}

#[tokio::test]
async fn lists_and_serves_the_trash() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let vault = env.create_vault(env.owner_token(), "notes").await;
    put(&env, &vault, "gone", "bye", None).await;
    put(&env, &vault, "kept", "hi", None).await;
    let deleted = env
        .owner(Method::DELETE, &format!("/vaults/{vault}/files/gone"))
        .header("X-Parent-Hash", "h-bye")
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 200);

    let listed: serde_json::Value = env
        .owner(Method::GET, &format!("/vaults/{vault}/trash"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entries = listed["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["path"], "gone");
    assert_eq!(entries[0]["encPath"], "enc-gone");
    assert_eq!(entries[0]["hash"], "h-bye");
    assert_eq!(entries[0]["size"], 3);
    assert!(entries[0]["deleted_at"].is_u64());

    let blob = env
        .owner(Method::GET, &format!("/vaults/{vault}/trash/gone"))
        .send()
        .await
        .unwrap();
    assert_eq!(blob.status(), 200);
    assert_eq!(blob.text().await.unwrap(), "bye");
    let live = env
        .owner(Method::GET, &format!("/vaults/{vault}/trash/kept"))
        .send()
        .await
        .unwrap();
    assert_eq!(live.status(), 404, "a live file has no trash entry");

    // A restore is an ordinary upload with the tombstone's hash as parent;
    // afterwards the path leaves the trash listing.
    put(&env, &vault, "gone", "back", Some("h-bye")).await;
    let listed: serde_json::Value = env
        .owner(Method::GET, &format!("/vaults/{vault}/trash"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed["entries"].as_array().unwrap().is_empty());
    env.finish().await;
}
