use aes_gcm::{
    aead::{rand_core::RngCore, Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use thiserror::Error;

pub type KeyBytes = [u8; 32];

const NONCE_LEN: usize = 12;

/// Wire-format version for the encrypted manifest (HMAC hashes + encrypted paths).
pub const PROTOCOL_VERSION: u32 = 2;

type HmacSha256 = Hmac<Sha256>;

/// Purpose-separated sub-keys derived from the master key via HKDF-SHA256.
///
/// The master key (Argon2id output) is only ever used as HKDF input keying
/// material; every concrete operation uses a dedicated sub-key so a weakness in
/// one domain can't bleed into another.
#[derive(Debug, Clone)]
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

/// Derive the purpose-separated sub-keys from a master key.
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

/// Deterministic per-path token used as the manifest key, R2 object key, and
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
}

pub fn derive_key(passphrase: &str, salt: &[u8]) -> Result<KeyBytes, CryptoError> {
    // Argon2id at 64 MiB memory / 3 iterations / 1 lane. This comfortably
    // exceeds the OWASP 2024 floor (m=19 MiB, t=2, p=1) while staying fast
    // enough for an interactive unlock on a phone. Salt is the vault ID.
    let params = Params::new(64 * 1024, 3, 1, Some(32)).map_err(|_| CryptoError::KeyDerivation)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut key = [0_u8; 32];
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|_| CryptoError::KeyDerivation)?;

    Ok(key)
}

pub fn encrypt(key: &KeyBytes, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::Encrypt)?;
    let mut nonce_bytes = [0_u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);

    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|_| CryptoError::Encrypt)?;

    let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok(blob)
}

pub fn decrypt(key: &KeyBytes, blob: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if blob.len() <= NONCE_LEN {
        return Err(CryptoError::InvalidBlob);
    }

    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::Decrypt)?;
    let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);

    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|_| CryptoError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::{
        content_hmac, decrypt, decrypt_path, derive_key, derive_keys, encrypt, encrypt_path,
        path_token,
    };

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
}
