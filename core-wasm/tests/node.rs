//! The bindings inside a real wasm runtime: `wasm-pack test --node --release
//! core-wasm`. Argon2id at 64 MiB runs here too, so the browser's memory
//! budget is exercised, not just the native one.
#![cfg(target_arch = "wasm32")]

use obsink_core_wasm::{diff_manifests_json as diff_manifests, Ignore, VaultKeys};
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
fn diffs_and_ignores() {
    let local = r#"{"a.md":{"hash":"h","modified":1,"size":1,"deleted":false,"encPath":""}}"#;
    let diff = diff_manifests("{}", local, "{}").unwrap();
    assert!(diff.contains(r#""kind":"Upload""#));
    assert!(Ignore::new("[]").unwrap().is_ignored(".DS_Store"));
    // Error paths surface as `JsError`s (only constructible on wasm).
    assert!(VaultKeys::from_master(&[0u8; 31]).is_err());
    assert!(Ignore::new("not json").is_err());
}
