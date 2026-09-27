//! The key hierarchy (spec §6.1, wire format v3):
//!
//! ```text
//! passphrase + salt ──Argon2id──▶ KEK ──unwrap──▶ account key (random, per account)
//!                                                     │
//!                                   HKDF "vault-wrap" ┴──unwrap──▶ vault key (random, per vault)
//!                                                                      │
//!                                              HKDF ─┬─▶ content_enc  content_mac  path_token  path_enc
//! ```
//!
//! The server holds the account key and every vault key only wrapped
//! (AES-256-GCM under a key it never has) plus a verifier that proves a
//! rewrap request comes from a client holding the unwrapped account key.
//!
//! Device approval (spec §12.1) is the second way onto a device: a device
//! without the account key makes an X25519 keypair, an unlocked device wraps
//! the account key to that public key after the user has typed the pending
//! device's fingerprint, and the server relays a blob it cannot open.

use aes_gcm::{
    aead::{rand_core::RngCore, Aead, KeyInit, OsRng, Payload},
    Aes256Gcm, Nonce,
};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use thiserror::Error;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

pub type KeyBytes = [u8; 32];

const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// Wire-format version: v3 is the account key hierarchy (one passphrase per
/// account, random wrapped vault keys); v2 was the HMAC-tokenized manifest
/// with a passphrase-derived key per vault.
pub const PROTOCOL_VERSION: u32 = 3;

/// The Argon2id salt is 16 random bytes generated when a passphrase is set.
pub const SALT_LEN: usize = 16;

/// A wrapped 32-byte key: `[12-byte nonce][32-byte ciphertext][16-byte tag]`.
pub const WRAPPED_KEY_LEN: usize = NONCE_LEN + 32 + TAG_LEN;

/// An X25519 public key (the pending device's, and the approver's ephemeral one).
pub const APPROVAL_PUBLIC_KEY_LEN: usize = 32;

/// What `POST /auth/devices/{id}/approval` relays: the approver's ephemeral
/// public key followed by the account key wrapped under the agreed key:
/// `[32-byte sender public key][12-byte nonce][32-byte ciphertext][16-byte tag]`.
pub const APPROVAL_BLOB_LEN: usize = APPROVAL_PUBLIC_KEY_LEN + WRAPPED_KEY_LEN;

/// The 32 symbols a fingerprint is spelled in (the invite alphabet: no `0`,
/// `O`, `1`, `I`), 5 bits each.
pub const FINGERPRINT_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Symbols in the fingerprint the user types on the approving device: 8 of
/// them are 40 bits. The single knob (decision 2026-09-27 on OBS-74).
pub const APPROVAL_FINGERPRINT_LEN: usize = 8;

const APPROVAL_FINGERPRINT_DOMAIN: &[u8] = b"obsink:v3:approval-fingerprint";
const APPROVAL_WRAP_INFO: &[u8] = b"obsink:v3:device-approval";

type HmacSha256 = Hmac<Sha256>;

/// Purpose-separated sub-keys derived from the vault key via HKDF-SHA256.
///
/// The vault key (32 random bytes) is only ever used as HKDF input keying
/// material; every concrete operation uses a dedicated sub-key so a weakness in
/// one domain can't bleed into another.
///
/// `Debug` is hand-written so a stray `tracing::debug!(?keys)` never writes
/// key material to a log, and the sub-keys are wiped on drop.
#[derive(Clone)]
pub struct CryptoKeys {
    /// Encrypts file *contents* (AES-256-GCM).
    pub content_enc: KeyBytes,
    /// Keys the HMAC over plaintext file contents (the manifest `hash`).
    pub content_mac: KeyBytes,
    /// Keys the deterministic HMAC that maps a real path to its server token.
    pub path_token: KeyBytes,
    /// Encrypts the real path so a fresh device can recover filenames.
    pub path_enc: KeyBytes,
}

impl std::fmt::Debug for CryptoKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CryptoKeys(..)")
    }
}

impl Drop for CryptoKeys {
    fn drop(&mut self) {
        self.content_enc.zeroize();
        self.content_mac.zeroize();
        self.path_token.zeroize();
        self.path_enc.zeroize();
    }
}

/// Derive the purpose-separated sub-keys from a vault key. (The HKDF infos keep
/// their `v2` labels: the fan-out is unchanged, only where the input key comes
/// from changed in v3.)
pub fn derive_keys(master: &KeyBytes) -> CryptoKeys {
    CryptoKeys {
        content_enc: hkdf_subkey(master, b"obsink:v2:content-enc"),
        content_mac: hkdf_subkey(master, b"obsink:v2:content-mac"),
        path_token: hkdf_subkey(master, b"obsink:v2:path-token"),
        path_enc: hkdf_subkey(master, b"obsink:v2:path-enc"),
    }
}

fn hkdf_subkey(master: &KeyBytes, info: &[u8]) -> KeyBytes {
    let hkdf = Hkdf::<Sha256>::new(None, master);
    let mut out = [0_u8; 32];
    hkdf.expand(info, &mut out)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    out
}

/// HMAC-SHA256 of file contents, hex-encoded. Replaces a bare content hash so
/// the server can't fingerprint known plaintext from the stored hash.
pub fn content_hmac(mac_key: &KeyBytes, bytes: &[u8]) -> String {
    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(mac_key).expect("HMAC accepts any key length");
    mac.update(bytes);
    hex::encode(mac.finalize().into_bytes())
}

/// Deterministic per-path token used as the manifest key, server blob key, and
/// URL segment. Deterministic so independent devices agree on the same token
/// for the same path (which is what makes manifest diffing work).
pub fn path_token(token_key: &KeyBytes, path: &str) -> String {
    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(token_key).expect("HMAC accepts any key length");
    mac.update(normalize_path(path).as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Encrypt a real path into a recoverable, base64-encoded blob (random nonce).
/// Stored in the manifest entry; not used for matching, so non-deterministic is fine.
pub fn encrypt_path(enc_key: &KeyBytes, path: &str) -> Result<String, CryptoError> {
    let blob = encrypt(enc_key, normalize_path(path).as_bytes())?;
    Ok(BASE64.encode(blob))
}

/// Recover a real path from its encrypted manifest entry.
pub fn decrypt_path(enc_key: &KeyBytes, encoded: &str) -> Result<String, CryptoError> {
    let blob = BASE64
        .decode(encoded)
        .map_err(|_| CryptoError::InvalidBlob)?;
    let bytes = decrypt(enc_key, &blob)?;
    String::from_utf8(bytes).map_err(|_| CryptoError::Decrypt)
}

/// Normalize separators so the same logical path yields a stable token/ciphertext
/// regardless of the platform that produced it.
fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("invalid encrypted blob")]
    InvalidBlob,
    #[error("key derivation failed")]
    KeyDerivation,
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed")]
    Decrypt,
    /// The fingerprint the user typed is not the one of the public key the
    /// server relayed (spec §12.3): nothing is wrapped, nothing is sent.
    #[error("fingerprint does not match")]
    FingerprintMismatch,
}

/// The KEK: Argon2id over the passphrase. In v3 the salt is the 16 random
/// bytes stored next to the wrapped account key ([`SALT_LEN`]); Argon2 accepts
/// any salt of 8 bytes or more, which the v2 callers (vault id as salt) rely on
/// until they move to the account key.
pub fn derive_key(passphrase: &str, salt: &[u8]) -> Result<KeyBytes, CryptoError> {
    // Argon2id at 64 MiB memory / 3 iterations / 1 lane. This comfortably
    // exceeds the OWASP 2024 floor (m=19 MiB, t=2, p=1) while staying fast
    // enough for an interactive unlock on a phone.
    let params = Params::new(64 * 1024, 3, 1, Some(32)).map_err(|_| CryptoError::KeyDerivation)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut key = [0_u8; 32];
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|_| CryptoError::KeyDerivation)?;

    Ok(key)
}

/// 16 random bytes for a new passphrase (the Argon2id salt).
pub fn new_salt() -> [u8; SALT_LEN] {
    let mut salt = [0_u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    salt
}

/// A fresh vault id in the server's shape (`vault_` + a UUID-like string),
/// minted by the client so the vault key can be wrapped under its real id
/// before `POST /vaults` (spec §4.3).
pub fn new_vault_id() -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let h = hex::encode(bytes);
    format!(
        "vault_{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// 32 random bytes: a fresh account key or vault key.
pub fn new_key() -> KeyBytes {
    let mut key = [0_u8; 32];
    OsRng.fill_bytes(&mut key);
    key
}

pub fn encrypt(key: &KeyBytes, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    encrypt_aad(key, &[], plaintext)
}

pub fn decrypt(key: &KeyBytes, blob: &[u8]) -> Result<Vec<u8>, CryptoError> {
    decrypt_aad(key, &[], blob)
}

/// AES-256-GCM with associated data: `nonce || ciphertext || tag`. Empty `aad`
/// is byte-for-byte the plain [`encrypt`], so file blobs are unaffected.
fn encrypt_aad(key: &KeyBytes, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::Encrypt)?;
    let mut nonce_bytes = [0_u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);

    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Encrypt)?;

    let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok(blob)
}

fn decrypt_aad(key: &KeyBytes, aad: &[u8], blob: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if blob.len() <= NONCE_LEN {
        return Err(CryptoError::InvalidBlob);
    }

    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::Decrypt)?;
    let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);

    cipher
        .decrypt(
            Nonce::from_slice(nonce_bytes),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Decrypt)
}

/// Wrap a 32-byte key under another key with a domain-binding AAD (the user
/// id for the account key, the vault id for a vault key). The blob is
/// [`WRAPPED_KEY_LEN`] bytes.
pub fn wrap_key(wrapping: &KeyBytes, key: &KeyBytes, aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    encrypt_aad(wrapping, aad, key)
}

/// The inverse of [`wrap_key`]. A wrong wrapping key, a wrong AAD, or a
/// tampered blob all fail the GCM tag and come back as [`CryptoError::Decrypt`];
/// a blob of the wrong length is [`CryptoError::InvalidBlob`].
pub fn unwrap_key(wrapping: &KeyBytes, blob: &[u8], aad: &[u8]) -> Result<KeyBytes, CryptoError> {
    if blob.len() != WRAPPED_KEY_LEN {
        return Err(CryptoError::InvalidBlob);
    }
    let mut plain = decrypt_aad(wrapping, aad, blob)?;
    let key: KeyBytes = plain
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::InvalidBlob)?;
    plain.zeroize();
    Ok(key)
}

/// The key that wraps vault keys for an account: HKDF of the account key.
pub fn vault_wrap_key(account: &KeyBytes) -> KeyBytes {
    hkdf_subkey(account, b"obsink:v3:vault-wrap")
}

/// The server-side proof that a client holds the account key:
/// `HMAC-SHA256(HKDF(account key, "obsink:v3:verify"), user id)`. Stored once,
/// never returned, compared in constant time on a rewrap.
pub fn account_verifier(account: &KeyBytes, user_id: &str) -> [u8; 32] {
    let mut verify_key = hkdf_subkey(account, b"obsink:v3:verify");
    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(&verify_key).expect("HMAC accepts any key length");
    mac.update(user_id.as_bytes());
    verify_key.zeroize();
    mac.finalize().into_bytes().into()
}

/// What the server stores for an account's passphrase: the wrapped account
/// key, the Argon2id salt, and the verifier. Nothing in here is secret on its
/// own; the passphrase is what unwraps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountKeyMaterial {
    pub wrapped: Vec<u8>,
    pub salt: [u8; SALT_LEN],
    pub verifier: [u8; 32],
}

impl AccountKeyMaterial {
    /// The wire encoding of each field (standard base64).
    pub fn wrapped_b64(&self) -> String {
        BASE64.encode(&self.wrapped)
    }

    pub fn salt_b64(&self) -> String {
        BASE64.encode(self.salt)
    }

    pub fn verifier_b64(&self) -> String {
        BASE64.encode(self.verifier)
    }
}

/// Decode a base64 field from the wire (`InvalidBlob` when it is not base64).
pub fn decode_base64(encoded: &str) -> Result<Vec<u8>, CryptoError> {
    BASE64
        .decode(encoded.trim())
        .map_err(|_| CryptoError::InvalidBlob)
}

pub fn encode_base64(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

fn wrap_under_passphrase(
    account: &KeyBytes,
    passphrase: &str,
    user_id: &str,
) -> Result<AccountKeyMaterial, CryptoError> {
    let salt = new_salt();
    let mut kek = derive_key(passphrase, &salt)?;
    let wrapped = wrap_key(&kek, account, user_id.as_bytes());
    kek.zeroize();
    Ok(AccountKeyMaterial {
        wrapped: wrapped?,
        salt,
        verifier: account_verifier(account, user_id),
    })
}

/// Set a passphrase for the first time: a fresh account key and the material
/// for `PUT /auth/keys`. The caller keeps the key (keychain) and sends the
/// material.
pub fn create_account_key(
    passphrase: &str,
    user_id: &str,
) -> Result<(KeyBytes, AccountKeyMaterial), CryptoError> {
    let account = new_key();
    let material = wrap_under_passphrase(&account, passphrase, user_id)?;
    Ok((account, material))
}

/// Unlock on a device: derive the KEK from the passphrase and the stored salt
/// and unwrap the stored account key. A wrong passphrase is `Decrypt`.
pub fn unlock_account_key(
    passphrase: &str,
    salt: &[u8],
    wrapped: &[u8],
    user_id: &str,
) -> Result<KeyBytes, CryptoError> {
    if salt.len() != SALT_LEN {
        return Err(CryptoError::InvalidBlob);
    }
    let mut kek = derive_key(passphrase, salt)?;
    let key = unwrap_key(&kek, wrapped, user_id.as_bytes());
    kek.zeroize();
    key
}

/// Change the passphrase: the same account key under a new KEK and salt. The
/// verifier is unchanged (it depends only on the account key), which is what
/// lets the server accept the request.
pub fn rewrap_account_key(
    account: &KeyBytes,
    passphrase: &str,
    user_id: &str,
) -> Result<AccountKeyMaterial, CryptoError> {
    wrap_under_passphrase(account, passphrase, user_id)
}

/// Wrap a vault key for this account (the `vault_members.wrapped_key` blob).
pub fn wrap_vault_key(
    account: &KeyBytes,
    vault_key: &KeyBytes,
    vault_id: &str,
) -> Result<Vec<u8>, CryptoError> {
    let mut wrapping = vault_wrap_key(account);
    let wrapped = wrap_key(&wrapping, vault_key, vault_id.as_bytes());
    wrapping.zeroize();
    wrapped
}

/// Recover a vault key from its member blob.
pub fn unwrap_vault_key(
    account: &KeyBytes,
    wrapped: &[u8],
    vault_id: &str,
) -> Result<KeyBytes, CryptoError> {
    let mut wrapping = vault_wrap_key(account);
    let key = unwrap_key(&wrapping, wrapped, vault_id.as_bytes());
    wrapping.zeroize();
    key
}

/// A pending device's approval keypair (spec §12.1). The secret lives in the
/// keychain (`approval:<user id>`) between polls; the public key goes to the
/// server; the fingerprint is what the user reads out.
///
/// `Debug` shows only the fingerprint; the secret is wiped on drop.
#[derive(Clone)]
pub struct ApprovalRequest {
    secret: KeyBytes,
    public: [u8; APPROVAL_PUBLIC_KEY_LEN],
}

impl std::fmt::Debug for ApprovalRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalRequest")
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

impl Drop for ApprovalRequest {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

impl ApprovalRequest {
    /// Rebuild the request from the stored secret (x25519-dalek 2 clamps at
    /// use, so the bytes round-trip through the keychain unchanged).
    pub fn from_secret(secret: &KeyBytes) -> Self {
        let scalar = StaticSecret::from(*secret);
        let public = PublicKey::from(&scalar).to_bytes();
        ApprovalRequest {
            secret: scalar.to_bytes(),
            public,
        }
    }

    /// The secret scalar, for the keychain.
    pub fn secret_bytes(&self) -> &KeyBytes {
        &self.secret
    }

    /// The public key `PUT /auth/approval` registers.
    pub fn public_key(&self) -> &[u8; APPROVAL_PUBLIC_KEY_LEN] {
        &self.public
    }

    /// What this device shows and the approver types.
    pub fn fingerprint(&self) -> String {
        approval_fingerprint(&self.public)
    }
}

/// A fresh approval keypair from 32 random bytes.
pub fn new_approval_request() -> ApprovalRequest {
    let mut seed = new_key();
    let request = ApprovalRequest::from_secret(&seed);
    seed.zeroize();
    request
}

/// The fingerprint of a pending device's public key: SHA-256 over
/// `"obsink:v3:approval-fingerprint" || public key`, read MSB-first in 5-bit
/// groups through [`FINGERPRINT_ALPHABET`], the first
/// [`APPROVAL_FINGERPRINT_LEN`] symbols. Computed on both ends, never by the
/// server.
pub fn approval_fingerprint(public: &[u8; APPROVAL_PUBLIC_KEY_LEN]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(APPROVAL_FINGERPRINT_DOMAIN);
    hasher.update(public);
    let digest = hasher.finalize();
    (0..APPROVAL_FINGERPRINT_LEN)
        .map(|symbol| {
            let first_bit = symbol * 5;
            let index = (0..5).fold(0_usize, |acc, offset| {
                let bit = first_bit + offset;
                let set = (digest[bit / 8] >> (7 - bit % 8)) & 1;
                (acc << 1) | usize::from(set)
            });
            char::from(FINGERPRINT_ALPHABET[index])
        })
        .collect()
}

/// What the user typed, in the shape [`approval_fingerprint`] produces:
/// whitespace and `-` dropped, uppercase, `0` read as `O` (the alphabet has
/// no zero).
pub fn normalize_fingerprint(typed: &str) -> String {
    typed
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            '0' => 'O',
            upper => upper,
        })
        .collect()
}

/// The key that wraps the account key between an approver and a pending
/// device: HKDF-SHA256 of the X25519 shared secret with both public keys in
/// the info, so a blob is bound to the pair that made it. An all-zero shared
/// secret (a low-order peer key) is refused as `KeyDerivation`.
fn approval_wrap_key(
    own: &StaticSecret,
    peer: &[u8; APPROVAL_PUBLIC_KEY_LEN],
    sender: &[u8; APPROVAL_PUBLIC_KEY_LEN],
    recipient: &[u8; APPROVAL_PUBLIC_KEY_LEN],
) -> Result<KeyBytes, CryptoError> {
    let shared = own.diffie_hellman(&PublicKey::from(*peer));
    if !shared.was_contributory() {
        return Err(CryptoError::KeyDerivation);
    }
    let mut info = Vec::with_capacity(APPROVAL_WRAP_INFO.len() + 2 * APPROVAL_PUBLIC_KEY_LEN);
    info.extend_from_slice(APPROVAL_WRAP_INFO);
    info.extend_from_slice(sender);
    info.extend_from_slice(recipient);
    // `SharedSecret` wipes itself on drop.
    Ok(hkdf_subkey(shared.as_bytes(), &info))
}

fn approval_aad(user_id: &str, device_id: &str) -> String {
    format!("{user_id}:{device_id}")
}

/// On an unlocked device: wrap the account key to a pending device's public
/// key, only when `typed` is that key's fingerprint. Returns the
/// [`APPROVAL_BLOB_LEN`]-byte blob `sender public key || wrapped account key`
/// for `POST /auth/devices/{device_id}/approval`. A key of the wrong length
/// is `InvalidBlob`; a wrong fingerprint is `FingerprintMismatch` and nothing
/// is wrapped; a low-order key is `KeyDerivation`.
pub fn approve_device(
    account: &KeyBytes,
    recipient_public: &[u8],
    typed: &str,
    user_id: &str,
    device_id: &str,
) -> Result<Vec<u8>, CryptoError> {
    let recipient: [u8; APPROVAL_PUBLIC_KEY_LEN] = recipient_public
        .try_into()
        .map_err(|_| CryptoError::InvalidBlob)?;
    if normalize_fingerprint(typed) != approval_fingerprint(&recipient) {
        return Err(CryptoError::FingerprintMismatch);
    }
    let mut seed = new_key();
    let sender = StaticSecret::from(seed);
    seed.zeroize();
    let sender_public = PublicKey::from(&sender).to_bytes();
    let mut wrapping = approval_wrap_key(&sender, &recipient, &sender_public, &recipient)?;
    let wrapped = wrap_key(
        &wrapping,
        account,
        approval_aad(user_id, device_id).as_bytes(),
    );
    wrapping.zeroize();
    let wrapped = wrapped?;
    let mut blob = Vec::with_capacity(APPROVAL_BLOB_LEN);
    blob.extend_from_slice(&sender_public);
    blob.extend_from_slice(&wrapped);
    Ok(blob)
}

/// On the pending device: open the relayed blob with the request's secret.
/// A blob of the wrong length is `InvalidBlob`; a blob made for another
/// request, user or device, or tampered with, is `Decrypt`.
pub fn accept_approval(
    request: &ApprovalRequest,
    blob: &[u8],
    user_id: &str,
    device_id: &str,
) -> Result<KeyBytes, CryptoError> {
    if blob.len() != APPROVAL_BLOB_LEN {
        return Err(CryptoError::InvalidBlob);
    }
    let (sender_public, wrapped) = blob.split_at(APPROVAL_PUBLIC_KEY_LEN);
    let sender_public: [u8; APPROVAL_PUBLIC_KEY_LEN] = sender_public
        .try_into()
        .map_err(|_| CryptoError::InvalidBlob)?;
    let own = StaticSecret::from(request.secret);
    let mut wrapping = approval_wrap_key(&own, &sender_public, &sender_public, &request.public)?;
    let key = unwrap_key(
        &wrapping,
        wrapped,
        approval_aad(user_id, device_id).as_bytes(),
    );
    wrapping.zeroize();
    key
}

#[cfg(test)]
mod tests {
    use super::{
        accept_approval, account_verifier, approval_fingerprint, approve_device, content_hmac,
        create_account_key, decode_base64, decrypt, decrypt_path, derive_key, derive_keys, encrypt,
        encrypt_path, new_approval_request, new_key, new_salt, new_vault_id, normalize_fingerprint,
        path_token, rewrap_account_key, unlock_account_key, unwrap_key, unwrap_vault_key,
        vault_wrap_key, wrap_key, wrap_vault_key, ApprovalRequest, CryptoError, APPROVAL_BLOB_LEN,
        APPROVAL_FINGERPRINT_LEN, APPROVAL_PUBLIC_KEY_LEN, FINGERPRINT_ALPHABET, PROTOCOL_VERSION,
        WRAPPED_KEY_LEN,
    };

    #[test]
    fn device_approval_round_trips_and_binds_its_context() {
        let account = new_key();
        let request = new_approval_request();
        let typed = request.fingerprint();
        assert_eq!(typed.len(), APPROVAL_FINGERPRINT_LEN);

        let blob =
            approve_device(&account, request.public_key(), &typed, "usr_1", "dev_2").unwrap();
        assert_eq!(blob.len(), APPROVAL_BLOB_LEN);
        assert_eq!(
            accept_approval(&request, &blob, "usr_1", "dev_2").unwrap(),
            account
        );
        // Two approvals of the same request use fresh ephemeral keys.
        let again =
            approve_device(&account, request.public_key(), &typed, "usr_1", "dev_2").unwrap();
        assert_ne!(
            again[..APPROVAL_PUBLIC_KEY_LEN],
            blob[..APPROVAL_PUBLIC_KEY_LEN]
        );

        // Another request, user, device, or a flipped byte: the GCM tag fails.
        assert!(matches!(
            accept_approval(&new_approval_request(), &blob, "usr_1", "dev_2"),
            Err(CryptoError::Decrypt)
        ));
        assert!(matches!(
            accept_approval(&request, &blob, "usr_2", "dev_2"),
            Err(CryptoError::Decrypt)
        ));
        assert!(matches!(
            accept_approval(&request, &blob, "usr_1", "dev_3"),
            Err(CryptoError::Decrypt)
        ));
        for index in [0, APPROVAL_PUBLIC_KEY_LEN + 5, APPROVAL_BLOB_LEN - 1] {
            let mut tampered = blob.clone();
            tampered[index] ^= 0x01;
            let error = accept_approval(&request, &tampered, "usr_1", "dev_2").unwrap_err();
            assert!(
                matches!(error, CryptoError::Decrypt | CryptoError::KeyDerivation),
                "byte {index}: {error:?}"
            );
        }
        // A short blob is rejected before any agreement runs.
        assert!(matches!(
            accept_approval(&request, &blob[..APPROVAL_BLOB_LEN - 1], "usr_1", "dev_2"),
            Err(CryptoError::InvalidBlob)
        ));
        assert!(matches!(
            approve_device(&account, &blob[..31], &typed, "usr_1", "dev_2"),
            Err(CryptoError::InvalidBlob)
        ));
    }

    #[test]
    fn a_wrong_fingerprint_wraps_nothing() {
        let account = new_key();
        let request = new_approval_request();
        let right = request.fingerprint();
        let mut wrong: Vec<char> = right.chars().collect();
        wrong[3] = if wrong[3] == 'A' { 'B' } else { 'A' };
        let wrong: String = wrong.into_iter().collect();
        assert!(matches!(
            approve_device(&account, request.public_key(), &wrong, "usr_1", "dev_2"),
            Err(CryptoError::FingerprintMismatch)
        ));
        assert!(matches!(
            approve_device(&account, request.public_key(), "", "usr_1", "dev_2"),
            Err(CryptoError::FingerprintMismatch)
        ));
        // Typed sloppily but right: lowercase, spaced, dashed, zero for O.
        let sloppy = format!(
            " {}-{} ",
            right[..4].to_ascii_lowercase(),
            right[4..].replace('O', "0")
        );
        assert!(approve_device(&account, request.public_key(), &sloppy, "usr_1", "dev_2").is_ok());
    }

    #[test]
    fn fingerprint_normalisation() {
        assert_eq!(normalize_fingerprint(" ab-cd 0f\tgh "), "ABCDOFGH");
        assert_eq!(normalize_fingerprint("ABCDEFGH"), "ABCDEFGH");
        assert_eq!(normalize_fingerprint(""), "");
        // Only `0` is folded; `1` stays what it is (and never matches).
        assert_eq!(normalize_fingerprint("1o"), "1O");
    }

    #[test]
    fn a_low_order_public_key_is_refused() {
        let account = new_key();
        let zero = [0u8; APPROVAL_PUBLIC_KEY_LEN];
        // The fingerprint of the zero key is valid; the agreement is not.
        assert!(matches!(
            approve_device(
                &account,
                &zero,
                &approval_fingerprint(&zero),
                "usr_1",
                "dev_2"
            ),
            Err(CryptoError::KeyDerivation)
        ));
        let request = new_approval_request();
        let mut blob = vec![0u8; APPROVAL_BLOB_LEN];
        blob[APPROVAL_PUBLIC_KEY_LEN..].copy_from_slice(&[1u8; WRAPPED_KEY_LEN]);
        assert!(matches!(
            accept_approval(&request, &blob, "usr_1", "dev_2"),
            Err(CryptoError::KeyDerivation)
        ));
    }

    #[test]
    fn fingerprints_are_deterministic_and_alphabet_bound() {
        let public = [0x5au8; APPROVAL_PUBLIC_KEY_LEN];
        let a = approval_fingerprint(&public);
        assert_eq!(a, approval_fingerprint(&public));
        assert_eq!(a.len(), APPROVAL_FINGERPRINT_LEN);
        assert!(a.bytes().all(|c| FINGERPRINT_ALPHABET.contains(&c)));
        assert_ne!(a, approval_fingerprint(&[0x5bu8; APPROVAL_PUBLIC_KEY_LEN]));
        // Pinned: the first 40 bits of SHA-256("obsink:v3:approval-fingerprint" || 0x5a*32),
        // so every client spells the same key the same way.
        let digest = {
            use sha2::Digest;
            let mut h = sha2::Sha256::new();
            h.update(b"obsink:v3:approval-fingerprint");
            h.update(public);
            h.finalize()
        };
        let mut bits = String::new();
        for byte in &digest[..5] {
            bits.push_str(&format!("{byte:08b}"));
        }
        let expected: String = (0..APPROVAL_FINGERPRINT_LEN)
            .map(|i| {
                let index = usize::from_str_radix(&bits[i * 5..i * 5 + 5], 2).unwrap();
                char::from(FINGERPRINT_ALPHABET[index])
            })
            .collect();
        assert_eq!(a, expected);
        assert_eq!(FINGERPRINT_ALPHABET.len(), 32);
        assert!(!FINGERPRINT_ALPHABET.contains(&b'0'));
        assert!(!FINGERPRINT_ALPHABET.contains(&b'O'));
        assert!(!FINGERPRINT_ALPHABET.contains(&b'1'));
        assert!(!FINGERPRINT_ALPHABET.contains(&b'I'));
    }

    #[test]
    fn an_approval_request_restores_from_its_secret_and_redacts_debug() {
        let request = new_approval_request();
        let restored = ApprovalRequest::from_secret(request.secret_bytes());
        assert_eq!(restored.secret_bytes(), request.secret_bytes());
        assert_eq!(restored.public_key(), request.public_key());
        assert_eq!(restored.fingerprint(), request.fingerprint());
        assert_ne!(request.public_key(), new_approval_request().public_key());
        // Any 32 bytes are a usable secret (clamped at use), and they come
        // back as stored, so the keychain entry is the bytes themselves.
        let seed = [0xffu8; 32];
        let from_seed = ApprovalRequest::from_secret(&seed);
        assert_eq!(from_seed.secret_bytes(), &seed);
        let account = new_key();
        let blob = approve_device(
            &account,
            from_seed.public_key(),
            &from_seed.fingerprint(),
            "usr_1",
            "dev_2",
        )
        .unwrap();
        assert_eq!(
            accept_approval(&from_seed, &blob, "usr_1", "dev_2").unwrap(),
            account
        );

        let printed = format!("{request:?}");
        assert!(printed.contains(&request.fingerprint()));
        assert!(!printed.contains(&hex::encode(request.secret_bytes())));
        assert!(!printed.contains(&format!(
            "{}, {}",
            request.secret_bytes()[0],
            request.secret_bytes()[1]
        )));
    }

    #[test]
    fn debug_output_carries_no_key_material() {
        let master = [0x41u8; 32];
        let keys = derive_keys(&master);
        let printed = format!("{keys:?}");
        assert_eq!(printed, "CryptoKeys(..)");
        for key in [
            &keys.content_enc,
            &keys.content_mac,
            &keys.path_token,
            &keys.path_enc,
        ] {
            assert!(!printed.contains(&hex::encode(key)));
            assert!(!printed.contains(&format!("{}, {}", key[0], key[1])));
        }
    }

    #[test]
    fn encrypt_round_trip() {
        let key = derive_key("hunter2", b"obsink-salt").unwrap();
        let plaintext = b"vault contents";

        let blob = encrypt(&key, plaintext).unwrap();
        let decrypted = decrypt(&key, &blob).unwrap();

        assert_eq!(decrypted, plaintext);
        assert_ne!(blob, plaintext);
    }

    #[test]
    fn reject_wrong_key() {
        let key = derive_key("hunter2", b"obsink-salt").unwrap();
        let wrong_key = derive_key("wrong-passphrase", b"obsink-salt").unwrap();

        let blob = encrypt(&key, b"secret").unwrap();

        assert!(decrypt(&wrong_key, &blob).is_err());
    }

    #[test]
    fn reject_tampered_ciphertext() {
        let key = derive_key("hunter2", b"obsink-salt").unwrap();
        let mut blob = encrypt(&key, b"secret").unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0x01;

        assert!(decrypt(&key, &blob).is_err());
    }

    #[test]
    fn subkeys_are_distinct_and_deterministic() {
        let master = derive_key("hunter2", b"obsink-salt").unwrap();
        let a = derive_keys(&master);
        let b = derive_keys(&master);

        // Deterministic for a given master key.
        assert_eq!(a.content_enc, b.content_enc);
        assert_eq!(a.path_token, b.path_token);

        // Domain separation: every sub-key differs from the others.
        assert_ne!(a.content_enc, a.content_mac);
        assert_ne!(a.content_enc, a.path_token);
        assert_ne!(a.content_enc, a.path_enc);
        assert_ne!(a.content_mac, a.path_token);
        assert_ne!(a.content_mac, a.path_enc);
        assert_ne!(a.path_token, a.path_enc);
    }

    #[test]
    fn content_hmac_hides_under_key() {
        let keys = derive_keys(&derive_key("pw", b"obsink-salt").unwrap());
        let other = derive_keys(&derive_key("other", b"obsink-salt").unwrap());

        // Deterministic and key-dependent.
        assert_eq!(
            content_hmac(&keys.content_mac, b"hello"),
            content_hmac(&keys.content_mac, b"hello")
        );
        assert_ne!(
            content_hmac(&keys.content_mac, b"hello"),
            content_hmac(&other.content_mac, b"hello")
        );
        assert_ne!(
            content_hmac(&keys.content_mac, b"hello"),
            content_hmac(&keys.content_mac, b"world")
        );
    }

    #[test]
    fn path_token_is_deterministic_and_normalized() {
        let keys = derive_keys(&derive_key("pw", b"obsink-salt").unwrap());

        assert_eq!(
            path_token(&keys.path_token, "notes/today.md"),
            path_token(&keys.path_token, "notes\\today.md")
        );
        assert_ne!(
            path_token(&keys.path_token, "a.md"),
            path_token(&keys.path_token, "b.md")
        );
    }

    #[test]
    fn path_encryption_round_trips() {
        let keys = derive_keys(&derive_key("pw", b"obsink-salt").unwrap());
        let enc = encrypt_path(&keys.path_enc, "notes/today.md").unwrap();

        assert_ne!(enc, "notes/today.md");
        assert_eq!(
            decrypt_path(&keys.path_enc, &enc).unwrap(),
            "notes/today.md"
        );
    }

    #[test]
    fn wrapped_keys_round_trip_and_bind_their_aad() {
        let wrapping = [0x11u8; 32];
        let key = [0x22u8; 32];

        let blob = wrap_key(&wrapping, &key, b"vault_a").unwrap();
        assert_eq!(blob.len(), WRAPPED_KEY_LEN);
        assert_eq!(unwrap_key(&wrapping, &blob, b"vault_a").unwrap(), key);

        // Another wrapping key, another AAD, or a flipped byte all fail the tag.
        assert!(matches!(
            unwrap_key(&[0x33u8; 32], &blob, b"vault_a"),
            Err(CryptoError::Decrypt)
        ));
        assert!(matches!(
            unwrap_key(&wrapping, &blob, b"vault_b"),
            Err(CryptoError::Decrypt)
        ));
        let mut tampered = blob.clone();
        tampered[20] ^= 0x01;
        assert!(matches!(
            unwrap_key(&wrapping, &tampered, b"vault_a"),
            Err(CryptoError::Decrypt)
        ));
        // A blob of the wrong length is rejected before any crypto runs.
        assert!(matches!(
            unwrap_key(&wrapping, &blob[..WRAPPED_KEY_LEN - 1], b"vault_a"),
            Err(CryptoError::InvalidBlob)
        ));
    }

    #[test]
    fn account_key_create_unlock_and_rewrap() {
        let user = "usr_1";
        let (account, material) = create_account_key("correct horse battery", user).unwrap();
        assert_eq!(material.wrapped.len(), WRAPPED_KEY_LEN);
        assert_eq!(material.verifier, account_verifier(&account, user));

        let unlocked = unlock_account_key(
            "correct horse battery",
            &material.salt,
            &material.wrapped,
            user,
        )
        .unwrap();
        assert_eq!(unlocked, account);

        assert!(matches!(
            unlock_account_key("wrong passphrase", &material.salt, &material.wrapped, user),
            Err(CryptoError::Decrypt)
        ));
        // The wrap is bound to the user id, so the same blob does not unlock
        // another account even with the right passphrase.
        assert!(matches!(
            unlock_account_key(
                "correct horse battery",
                &material.salt,
                &material.wrapped,
                "usr_2"
            ),
            Err(CryptoError::Decrypt)
        ));
        assert!(matches!(
            unlock_account_key("correct horse battery", b"short", &material.wrapped, user),
            Err(CryptoError::InvalidBlob)
        ));

        // A rewrap keeps the key and the verifier, changes the salt and blob.
        let rewrapped = rewrap_account_key(&account, "new passphrase here", user).unwrap();
        assert_eq!(rewrapped.verifier, material.verifier);
        assert_ne!(rewrapped.salt, material.salt);
        assert_ne!(rewrapped.wrapped, material.wrapped);
        assert_eq!(
            unlock_account_key(
                "new passphrase here",
                &rewrapped.salt,
                &rewrapped.wrapped,
                user
            )
            .unwrap(),
            account
        );

        // The wire encoding round-trips through the base64 helpers.
        assert_eq!(
            decode_base64(&material.wrapped_b64()).unwrap(),
            material.wrapped
        );
        assert_eq!(decode_base64(&material.salt_b64()).unwrap(), material.salt);
        assert_eq!(
            decode_base64(&material.verifier_b64()).unwrap(),
            material.verifier
        );
        assert!(matches!(
            decode_base64("not base64!"),
            Err(CryptoError::InvalidBlob)
        ));
    }

    #[test]
    fn verifier_is_stable_per_account_and_distinct_per_user_and_key() {
        let account = [0x44u8; 32];
        assert_eq!(
            account_verifier(&account, "usr_1"),
            account_verifier(&account, "usr_1")
        );
        assert_ne!(
            account_verifier(&account, "usr_1"),
            account_verifier(&account, "usr_2")
        );
        assert_ne!(
            account_verifier(&account, "usr_1"),
            account_verifier(&[0x45u8; 32], "usr_1")
        );
        // The verifier is not any of the keys it derives from.
        assert_ne!(account_verifier(&account, "usr_1"), account);
        assert_ne!(
            account_verifier(&account, "usr_1"),
            vault_wrap_key(&account)
        );
    }

    #[test]
    fn vault_keys_wrap_per_vault_and_feed_the_same_subkeys() {
        let account = new_key();
        let vault_key = new_key();

        let blob = wrap_vault_key(&account, &vault_key, "vault_a").unwrap();
        assert_eq!(
            unwrap_vault_key(&account, &blob, "vault_a").unwrap(),
            vault_key
        );
        // The blob for vault A is useless under vault B's id or another account.
        assert!(unwrap_vault_key(&account, &blob, "vault_b").is_err());
        assert!(unwrap_vault_key(&new_key(), &blob, "vault_a").is_err());

        // The sub-keys come from the vault key alone: two members that unwrap
        // the same vault key derive the same content keys.
        let a = derive_keys(&vault_key);
        let b = derive_keys(&unwrap_vault_key(&account, &blob, "vault_a").unwrap());
        assert_eq!(a.content_mac, b.content_mac);
        assert_eq!(
            content_hmac(&a.content_mac, b"note"),
            content_hmac(&b.content_mac, b"note")
        );
    }

    #[test]
    fn random_material_is_fresh() {
        assert_ne!(new_key(), new_key());
        assert_ne!(new_salt(), new_salt());
        assert_eq!(PROTOCOL_VERSION, 3);
        let id = new_vault_id();
        assert_ne!(id, new_vault_id());
        assert_eq!(id.len(), "vault_".len() + 36);
        assert!(id.starts_with("vault_"));
        assert!(id[6..].chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
    }
}
