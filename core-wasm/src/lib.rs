//! The pure half of `obsink-core` for the browser: key derivation and the
//! AES-GCM / HMAC primitives behind an opaque [`VaultKeys`] handle (key
//! material never leaves wasm memory), the three-way manifest diff and
//! checkpoint, the ignore rules, the sync rules (batching, conflict choices,
//! copy names) and the driver pacing. The browser driver in `web/` runs the
//! sync cycle itself and calls in here for every decision the native engine
//! would make, so the two cannot drift on the rules that matter.
//!
//! Structured values cross the boundary as JSON strings with the same serde
//! shapes the native clients use (`Manifest`, `ManifestDiff`, `Conflict`,
//! `SyncAction`, `ConflictResolutionChoice`).

use std::{collections::BTreeSet, time::Duration};

use obsink_core::{
    backoff_wait, checkpoint_manifest, chunk_uploads, conflict_copy_path, conflict_to_upload,
    content_hmac, create_account_key, decode_base64, decrypt, decrypt_path, derive_key,
    derive_keys, diff_manifests, effective_choice, encode_base64, encrypt, encrypt_path,
    hash_cache_key_id, new_key, normalize_server_url, path_token, rewrap_account_key,
    unlock_account_key, unwrap_vault_key, wrap_vault_key, AccountKeyMaterial, Conflict,
    ConflictResolutionChoice, CryptoKeys, IgnoreRules, KeyBytes, Manifest, PollPacing,
    DEFAULT_IGNORE, PROTOCOL_VERSION,
};
use wasm_bindgen::prelude::*;
use zeroize::Zeroize;

fn key_bytes(bytes: &[u8]) -> Result<KeyBytes, JsError> {
    bytes
        .try_into()
        .map_err(|_| JsError::new("key must be 32 bytes"))
}

fn millis(ms: f64) -> Duration {
    Duration::from_millis(ms.max(0.0) as u64)
}

/// The wire-format version the native clients speak.
#[wasm_bindgen(js_name = protocolVersion)]
pub fn protocol_version() -> u32 {
    PROTOCOL_VERSION
}

/// One vault's derived sub-keys. Create it once per vault (Argon2id takes a
/// moment and 64 MiB) and keep it in a worker; `free()` wipes the keys.
#[wasm_bindgen]
pub struct VaultKeys {
    keys: CryptoKeys,
}

#[wasm_bindgen]
impl VaultKeys {
    /// Argon2id over the passphrase with the vault id as salt, then the HKDF
    /// sub-keys — the same derivation as every native client.
    pub fn derive(passphrase: &str, vault_id: &str) -> Result<VaultKeys, JsError> {
        let master = derive_key(passphrase, vault_id.as_bytes())?;
        Ok(VaultKeys {
            keys: derive_keys(&master),
        })
    }

    /// Sub-keys from a 32-byte vault key (v3: the key an [`AccountKey`]
    /// unwrapped from the vault's member blob).
    #[wasm_bindgen(js_name = fromVaultKey)]
    pub fn from_vault_key(vault_key: &[u8]) -> Result<VaultKeys, JsError> {
        Ok(VaultKeys {
            keys: derive_keys(&key_bytes(vault_key)?),
        })
    }

    /// The v2 name of [`VaultKeys::from_vault_key`] (tests, harnesses).
    #[wasm_bindgen(js_name = fromMaster)]
    pub fn from_master(master: &[u8]) -> Result<VaultKeys, JsError> {
        Self::from_vault_key(master)
    }

    /// AES-256-GCM: `nonce || ciphertext || tag`, the blob the server stores.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, JsError> {
        Ok(encrypt(&self.keys.content_enc, plaintext)?)
    }

    pub fn decrypt(&self, blob: &[u8]) -> Result<Vec<u8>, JsError> {
        Ok(decrypt(&self.keys.content_enc, blob)?)
    }

    /// The manifest `hash` of a file: HMAC-SHA256 over its plaintext, hex.
    #[wasm_bindgen(js_name = contentHmac)]
    pub fn content_hmac(&self, plaintext: &[u8]) -> String {
        content_hmac(&self.keys.content_mac, plaintext)
    }

    /// The deterministic server-side name of a vault-relative path.
    #[wasm_bindgen(js_name = pathToken)]
    pub fn path_token(&self, path: &str) -> String {
        path_token(&self.keys.path_token, path)
    }

    /// The `encPath` a manifest entry carries so a fresh device can recover
    /// the real path. Randomised: two calls give two ciphertexts.
    #[wasm_bindgen(js_name = encryptPath)]
    pub fn encrypt_path(&self, path: &str) -> Result<String, JsError> {
        Ok(encrypt_path(&self.keys.path_enc, path)?)
    }

    #[wasm_bindgen(js_name = decryptPath)]
    pub fn decrypt_path(&self, enc_path: &str) -> Result<String, JsError> {
        Ok(decrypt_path(&self.keys.path_enc, enc_path)?)
    }

    /// The fingerprint a hash cache written under these keys carries.
    #[wasm_bindgen(js_name = hashCacheKeyId)]
    pub fn hash_cache_key_id(&self) -> String {
        hash_cache_key_id(&self.keys.content_mac)
    }
}

/// A fresh 32-byte vault key for `Create vault`; the caller wraps it with
/// [`AccountKey::wrap_vault_key`] and feeds it to [`VaultKeys::from_vault_key`].
#[wasm_bindgen(js_name = newVaultKey)]
pub fn new_vault_key() -> Vec<u8> {
    new_key().to_vec()
}

/// What `PUT /auth/keys` and `/auth/keys/rewrap` send, base64 fields.
fn material_json(material: &AccountKeyMaterial) -> Result<String, JsError> {
    Ok(serde_json::to_string(&serde_json::json!({
        "wrapped": material.wrapped_b64(),
        "salt": material.salt_b64(),
        "verifier": material.verifier_b64(),
    }))?)
}

/// The account key of a signed-in user, unlocked from the passphrase (spec
/// §6.1). Lives in worker memory for the tab's lifetime; `free()` wipes it.
#[wasm_bindgen]
pub struct AccountKey {
    key: KeyBytes,
    user_id: String,
    material: Option<AccountKeyMaterial>,
}

impl Drop for AccountKey {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

#[wasm_bindgen]
impl AccountKey {
    /// Set the passphrase for the first time: a fresh account key. `material()`
    /// then holds what `PUT /auth/keys` needs.
    pub fn create(passphrase: &str, user_id: &str) -> Result<AccountKey, JsError> {
        let (key, material) = create_account_key(passphrase, user_id)?;
        Ok(AccountKey {
            key,
            user_id: user_id.to_string(),
            material: Some(material),
        })
    }

    /// Unlock from what `GET /auth/keys` returned. A wrong passphrase is an
    /// error (the GCM tag), before anything is downloaded.
    pub fn unlock(
        passphrase: &str,
        salt_b64: &str,
        wrapped_b64: &str,
        user_id: &str,
    ) -> Result<AccountKey, JsError> {
        let salt = decode_base64(salt_b64)?;
        let wrapped = decode_base64(wrapped_b64)?;
        let key = unlock_account_key(passphrase, &salt, &wrapped, user_id)?;
        Ok(AccountKey {
            key,
            user_id: user_id.to_string(),
            material: None,
        })
    }

    /// Rebuild the handle from raw bytes kept elsewhere in the worker.
    #[wasm_bindgen(js_name = fromBytes)]
    pub fn from_bytes(key: &[u8], user_id: &str) -> Result<AccountKey, JsError> {
        Ok(AccountKey {
            key: key_bytes(key)?,
            user_id: user_id.to_string(),
            material: None,
        })
    }

    /// JSON `{ wrapped, salt, verifier }` (base64) from the last `create` or
    /// `rewrap`; an error on a handle that came from `unlock`.
    pub fn material(&self) -> Result<String, JsError> {
        let material = self
            .material
            .as_ref()
            .ok_or_else(|| JsError::new("no material: this key was unlocked, not created"))?;
        material_json(material)
    }

    /// Change the passphrase: the same key under a new KEK. Returns the JSON
    /// for `PUT /auth/keys/rewrap` (the verifier is unchanged).
    pub fn rewrap(&mut self, passphrase: &str) -> Result<String, JsError> {
        let material = rewrap_account_key(&self.key, passphrase, &self.user_id)?;
        let json = material_json(&material)?;
        self.material = Some(material);
        Ok(json)
    }

    /// The base64 verifier for this account (what a rewrap must present).
    pub fn verifier(&self) -> String {
        encode_base64(&obsink_core::account_verifier(&self.key, &self.user_id))
    }

    /// Wrap a vault key for this account: the `wrapped_key` of `POST /vaults`.
    #[wasm_bindgen(js_name = wrapVaultKey)]
    pub fn wrap_vault_key(&self, vault_key: &[u8], vault_id: &str) -> Result<String, JsError> {
        let wrapped = wrap_vault_key(&self.key, &key_bytes(vault_key)?, vault_id)?;
        Ok(encode_base64(&wrapped))
    }

    /// Recover a vault key from the member blob `GET /vaults` returned.
    #[wasm_bindgen(js_name = unwrapVaultKey)]
    pub fn unwrap_vault_key(&self, wrapped_b64: &str, vault_id: &str) -> Result<Vec<u8>, JsError> {
        let wrapped = decode_base64(wrapped_b64)?;
        Ok(unwrap_vault_key(&self.key, &wrapped, vault_id)?.to_vec())
    }

    /// The raw key, for the worker to keep alongside the handle.
    pub fn bytes(&self) -> Vec<u8> {
        self.key.to_vec()
    }

    #[wasm_bindgen(js_name = userId)]
    pub fn user_id(&self) -> String {
        self.user_id.clone()
    }
}

/// `diff_manifests(base, local, remote)` over JSON manifests (keyed by real
/// path); returns a JSON `ManifestDiff { upload, download, conflicts }`.
#[wasm_bindgen(js_name = diffManifests)]
pub fn diff_manifests_json(base: &str, local: &str, remote: &str) -> Result<String, JsError> {
    let base: Manifest = serde_json::from_str(base)?;
    let local: Manifest = serde_json::from_str(local)?;
    let remote: Manifest = serde_json::from_str(remote)?;
    Ok(serde_json::to_string(&diff_manifests(
        &base, &local, &remote,
    ))?)
}

/// The next base manifest after a sync: the re-fetched server manifest with
/// every held-back path restored to its previous base entry.
#[wasm_bindgen(js_name = checkpointManifest)]
pub fn checkpoint_manifest_json(
    previous_base: &str,
    refetched: &str,
    hold_back: &str,
) -> Result<String, JsError> {
    let previous_base: Manifest = serde_json::from_str(previous_base)?;
    let refetched: Manifest = serde_json::from_str(refetched)?;
    let hold_back: BTreeSet<String> = serde_json::from_str(hold_back)?;
    Ok(serde_json::to_string(&checkpoint_manifest(
        &previous_base,
        &refetched,
        &hold_back,
    ))?)
}

/// The patterns every vault ignores, as a JSON array.
#[wasm_bindgen(js_name = defaultIgnore)]
pub fn default_ignore() -> String {
    serde_json::to_string(DEFAULT_IGNORE).expect("static string list serialises")
}

/// The defaults plus a vault's own patterns, compiled once per scan.
#[wasm_bindgen]
pub struct Ignore {
    rules: IgnoreRules,
}

#[wasm_bindgen]
impl Ignore {
    /// `extra` is a JSON array of the vault's own patterns (`[]` for none).
    #[wasm_bindgen(constructor)]
    pub fn new(extra: &str) -> Result<Ignore, JsError> {
        let extra: Vec<String> = serde_json::from_str(extra)?;
        Ok(Ignore {
            rules: IgnoreRules::defaults().with_extra(extra.iter().map(String::as_str)),
        })
    }

    /// `path` is vault-relative with `/` separators.
    #[wasm_bindgen(js_name = isIgnored)]
    pub fn is_ignored(&self, path: &str) -> bool {
        self.rules.is_ignored(path)
    }
}

/// Batch boundaries for uploads: JSON `[[start, end], ...]` index ranges over
/// the given JSON array of plaintext sizes.
#[wasm_bindgen(js_name = chunkUploads)]
pub fn chunk_uploads_json(sizes: &str) -> Result<String, JsError> {
    let sizes: Vec<u64> = serde_json::from_str(sizes)?;
    let ranges: Vec<[usize; 2]> = chunk_uploads(&sizes)
        .into_iter()
        .map(|range| [range.start, range.end])
        .collect();
    Ok(serde_json::to_string(&ranges)?)
}

/// The choice actually applied to a conflict (`KeepBoth` collapses when one
/// side is a deletion). Both arguments and the result are JSON.
#[wasm_bindgen(js_name = effectiveChoice)]
pub fn effective_choice_json(choice: &str, conflict: &str) -> Result<String, JsError> {
    let choice: ConflictResolutionChoice = serde_json::from_str(choice)?;
    let conflict: Conflict = serde_json::from_str(conflict)?;
    Ok(serde_json::to_string(&effective_choice(
        &choice, &conflict,
    ))?)
}

/// The upload (or remote delete) that keeps the local side of a JSON conflict.
#[wasm_bindgen(js_name = conflictToUpload)]
pub fn conflict_to_upload_json(conflict: &str) -> Result<String, JsError> {
    let conflict: Conflict = serde_json::from_str(conflict)?;
    Ok(serde_json::to_string(&conflict_to_upload(&conflict))?)
}

/// Where `KeepBoth` writes the remote version: `notes/today.conflict.md`.
#[wasm_bindgen(js_name = conflictCopyPath)]
pub fn conflict_copy_path_js(original: &str) -> String {
    conflict_copy_path(original)
}

/// The wait (ms) after `failures` consecutive fatal errors: `base * 2^n`, capped.
#[wasm_bindgen(js_name = backoffWaitMs)]
pub fn backoff_wait_ms(base_ms: f64, max_ms: f64, failures: u32) -> f64 {
    backoff_wait(millis(base_ms), millis(max_ms), failures).as_millis() as f64
}

/// The remote poll interval (ms): `active` within `active_window` of the last
/// activity, `idle` after.
#[wasm_bindgen(js_name = pollIntervalMs)]
pub fn poll_interval_ms(
    active_ms: f64,
    idle_ms: f64,
    active_window_ms: f64,
    since_activity_ms: f64,
) -> f64 {
    PollPacing {
        active: millis(active_ms),
        idle: millis(idle_ms),
        active_window: millis(active_window_ms),
    }
    .interval(millis(since_activity_ms))
    .as_millis() as f64
}

/// The canonical spelling of a server URL (trimmed, no trailing slash,
/// lowercase scheme and host).
#[wasm_bindgen(js_name = normalizeServerUrl)]
pub fn normalize_server_url_js(url: &str) -> String {
    normalize_server_url(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> VaultKeys {
        VaultKeys::from_master(&[7u8; 32]).unwrap()
    }

    #[test]
    fn keys_round_trip_content_and_paths() {
        let keys = keys();
        let blob = keys.encrypt(b"hello").unwrap();
        assert_eq!(keys.decrypt(&blob).unwrap(), b"hello");
        assert_eq!(keys.path_token("a/b.md"), keys.path_token("a\\b.md"));
        let enc = keys.encrypt_path("notes/x.md").unwrap();
        assert_eq!(keys.decrypt_path(&enc).unwrap(), "notes/x.md");
        assert_eq!(keys.content_hmac(b"x").len(), 64);
        assert_eq!(keys.hash_cache_key_id().len(), 64);
        // Error paths construct a `JsError`, which only exists on wasm: they
        // are covered by `tests/node.rs`.
    }

    #[test]
    fn derive_matches_the_native_derivation() {
        // Argon2 needs an 8-byte salt; real vault ids are `vault_<uuid>`.
        let native = derive_keys(&derive_key("hunter2", b"vault_test-id").unwrap());
        let bound = VaultKeys::derive("hunter2", "vault_test-id").unwrap();
        assert_eq!(
            bound.content_hmac(b"same"),
            content_hmac(&native.content_mac, b"same")
        );
    }

    #[test]
    fn account_key_wraps_vault_keys_and_speaks_json() {
        let created = AccountKey::create("correct horse battery", "usr_1").unwrap();
        let material: serde_json::Value =
            serde_json::from_str(&created.material().unwrap()).unwrap();
        let salt = material["salt"].as_str().unwrap();
        let wrapped = material["wrapped"].as_str().unwrap();
        assert_eq!(material["verifier"].as_str().unwrap(), created.verifier());

        let unlocked = AccountKey::unlock("correct horse battery", salt, wrapped, "usr_1").unwrap();
        assert_eq!(unlocked.bytes(), created.bytes());
        assert_eq!(unlocked.user_id(), "usr_1");
        // (A wrong passphrase is a JsError, covered in tests/node.rs.)

        let vault_key = new_vault_key();
        let blob = created.wrap_vault_key(&vault_key, "vault_a").unwrap();
        assert_eq!(
            unlocked.unwrap_vault_key(&blob, "vault_a").unwrap(),
            vault_key
        );
        let keys = VaultKeys::from_vault_key(&vault_key).unwrap();
        assert_eq!(
            keys.content_hmac(b"x"),
            content_hmac(
                &derive_keys(&vault_key.as_slice().try_into().unwrap()).content_mac,
                b"x"
            )
        );

        let mut rewrapping = AccountKey::from_bytes(&created.bytes(), "usr_1").unwrap();
        let rewrapped: serde_json::Value =
            serde_json::from_str(&rewrapping.rewrap("another passphrase").unwrap()).unwrap();
        assert_eq!(rewrapped["verifier"], material["verifier"]);
        assert_ne!(rewrapped["salt"], material["salt"]);
    }

    #[test]
    fn diff_and_checkpoint_speak_json() {
        let entry = |hash: &str| {
            format!(r#"{{"hash":"{hash}","modified":1,"size":1,"deleted":false,"encPath":""}}"#)
        };
        let local = format!(r#"{{"a.md":{}}}"#, entry("h1"));
        let diff: serde_json::Value =
            serde_json::from_str(&diff_manifests_json("{}", &local, "{}").unwrap()).unwrap();
        assert_eq!(diff["upload"][0]["path"], "a.md");
        assert_eq!(diff["upload"][0]["kind"], "Upload");
        assert!(diff["download"].as_array().unwrap().is_empty());

        let refetched = format!(r#"{{"a.md":{},"b.md":{}}}"#, entry("h1"), entry("h2"));
        let base: Manifest = serde_json::from_str(
            &checkpoint_manifest_json("{}", &refetched, r#"["b.md"]"#).unwrap(),
        )
        .unwrap();
        assert!(base.contains_key("a.md") && !base.contains_key("b.md"));
    }

    #[test]
    fn rules_match_the_native_ones() {
        let ignore = Ignore::new(r#"["drafts/"]"#).unwrap();
        assert!(ignore.is_ignored(".obsink/manifest.json"));
        assert!(ignore.is_ignored("drafts/a.md"));
        assert!(!ignore.is_ignored(".obsidian/app.json"));
        assert!(default_ignore().contains(".obsink/"));

        assert_eq!(chunk_uploads_json("[1,2,3]").unwrap(), "[[0,3]]");
        assert_eq!(
            conflict_copy_path_js("notes/today.md"),
            "notes/today.conflict.md"
        );

        let conflict = format!(
            r#"{{"path":"a.md","local":{},"remote":{}}}"#,
            r#"{"hash":"l","modified":1,"size":1,"deleted":false,"encPath":""}"#,
            r#"{"hash":"r","modified":1,"size":1,"deleted":true,"encPath":""}"#
        );
        assert_eq!(
            effective_choice_json(r#""KeepBoth""#, &conflict).unwrap(),
            r#""KeepLocal""#
        );
        let upload: serde_json::Value =
            serde_json::from_str(&conflict_to_upload_json(&conflict).unwrap()).unwrap();
        assert_eq!(upload["kind"], "Upload");

        assert_eq!(backoff_wait_ms(5000.0, 300_000.0, 7), 300_000.0);
        assert_eq!(
            poll_interval_ms(5000.0, 60_000.0, 60_000.0, 61_000.0),
            60_000.0
        );
        assert_eq!(normalize_server_url_js("HTTPS://X.dev/"), "https://x.dev");
    }
}
