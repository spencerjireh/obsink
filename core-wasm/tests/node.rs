//! The bindings inside a real wasm runtime: `wasm-pack test --node --release
//! core-wasm`. Argon2id at 64 MiB runs here too, so the browser's memory
//! budget is exercised, not just the native one.
#![cfg(target_arch = "wasm32")]

use obsink_core_wasm::{
    backoff_wait_ms, checkpoint_manifest_json, chunk_uploads_json, conflict_copy_path_js,
    conflict_to_upload_json, default_ignore, diff_manifests_json as diff_manifests,
    effective_choice_json, new_vault_key, normalize_server_url_js, poll_interval_ms,
    protocol_version, AccountKey, Ignore, VaultKeys,
};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn derives_keys_and_round_trips_a_blob() {
    let keys = VaultKeys::derive("hunter2", "vault_test").unwrap();
    let blob = keys.encrypt(b"note").unwrap();
    assert_eq!(keys.decrypt(&blob).unwrap(), b"note");
    let token = keys.path_token("a.md");
    assert_eq!(token.len(), 64);
    assert_eq!(
        keys.decrypt_path(&keys.encrypt_path("a.md").unwrap())
            .unwrap(),
        "a.md"
    );
}

#[wasm_bindgen_test]
fn account_keys_unlock_and_wrap_vault_keys() {
    let created = AccountKey::create("correct horse battery", "usr_1").unwrap();
    let material: serde_json::Value = serde_json::from_str(&created.material().unwrap()).unwrap();
    let salt = material["salt"].as_str().unwrap();
    let wrapped = material["wrapped"].as_str().unwrap();

    let unlocked = AccountKey::unlock("correct horse battery", salt, wrapped, "usr_1").unwrap();
    assert_eq!(unlocked.bytes(), created.bytes());
    // A wrong passphrase, a wrong user, and bad base64 all surface as errors.
    assert!(AccountKey::unlock("wrong", salt, wrapped, "usr_1").is_err());
    assert!(AccountKey::unlock("correct horse battery", salt, wrapped, "usr_2").is_err());
    assert!(AccountKey::unlock("correct horse battery", "!!", wrapped, "usr_1").is_err());
    assert!(AccountKey::unlock("correct horse battery", salt, "!!", "usr_1").is_err());

    let vault_key = new_vault_key();
    let blob = created.wrap_vault_key(&vault_key, "vault_a").unwrap();
    assert_eq!(
        unlocked.unwrap_vault_key(&blob, "vault_a").unwrap(),
        vault_key
    );
    assert!(unlocked.unwrap_vault_key(&blob, "vault_b").is_err());
    let keys = VaultKeys::from_vault_key(&vault_key).unwrap();
    let note = keys.encrypt(b"note").unwrap();
    assert_eq!(keys.decrypt(&note).unwrap(), b"note");
    assert!(unlocked.material().is_err());
}

#[wasm_bindgen_test]
fn diffs_and_ignores() {
    let local = r#"{"a.md":{"hash":"h","modified":1,"size":1,"deleted":false,"encPath":""}}"#;
    let diff = diff_manifests("{}", local, "{}").unwrap();
    assert!(diff.contains(r#""kind":"Upload""#));
    assert!(Ignore::new("[]").unwrap().is_ignored(".DS_Store"));
    // Error paths surface as `JsError`s (only constructible on wasm).
    assert!(VaultKeys::from_master(&[0u8; 31]).is_err());
    assert!(Ignore::new("not json").is_err());
}

// Every remaining export crosses the boundary with a well-formed value; the
// rules themselves are tested natively in core.
#[wasm_bindgen_test]
fn the_rest_of_the_surface_crosses_the_boundary() {
    assert_eq!(protocol_version(), 3);

    let keys = VaultKeys::from_master(&[7u8; 32]).unwrap();
    assert_eq!(keys.content_hmac(b"x").len(), 64);
    assert!(!keys.hash_cache_key_id().is_empty());

    let defaults: Vec<String> = serde_json::from_str(&default_ignore()).unwrap();
    assert!(defaults.iter().any(|pattern| pattern.contains(".obsink")));

    let base = r#"{"a.md":{"hash":"h0","modified":1,"size":1,"deleted":false,"encPath":""}}"#;
    let local = r#"{"a.md":{"hash":"h1","modified":2,"size":1,"deleted":false,"encPath":""}}"#;
    let remote = r#"{"a.md":{"hash":"h2","modified":3,"size":1,"deleted":false,"encPath":""}}"#;
    let diff: serde_json::Value =
        serde_json::from_str(&diff_manifests(base, local, remote).unwrap()).unwrap();
    let conflict = serde_json::to_string(&diff["conflicts"][0]).unwrap();
    assert!(conflict.contains(r#""path":"a.md""#));
    let choice = effective_choice_json(r#""KeepBoth""#, &conflict).unwrap();
    assert!(
        choice.starts_with('"'),
        "a JSON string choice, got {choice}"
    );
    let upload = conflict_to_upload_json(&conflict).unwrap();
    assert!(upload.contains(r#""kind":"Upload""#), "{upload}");

    let held = checkpoint_manifest_json(base, remote, r#"["a.md"]"#).unwrap();
    assert!(
        held.contains(r#""hash":"h0""#),
        "held-back path keeps its base entry: {held}"
    );

    assert_eq!(chunk_uploads_json("[1,2,3]").unwrap(), "[[0,3]]");
    assert_eq!(
        conflict_copy_path_js("notes/today.md"),
        "notes/today.conflict.md"
    );

    assert_eq!(backoff_wait_ms(5000.0, 300_000.0, 0), 5000.0);
    assert_eq!(backoff_wait_ms(5000.0, 300_000.0, 20), 300_000.0);
    assert_eq!(poll_interval_ms(5000.0, 60_000.0, 60_000.0, 0.0), 5000.0);
    assert_eq!(
        poll_interval_ms(5000.0, 60_000.0, 60_000.0, 120_000.0),
        60_000.0
    );

    assert_eq!(
        normalize_server_url_js("HTTPS://Example.com/"),
        "https://example.com"
    );
}
