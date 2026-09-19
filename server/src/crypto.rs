//! Server-side envelope encryption.
//!
//! The master key (`OBSINK_SERVER_KEY`) is only HKDF input, mirroring the
//! client's key hygiene (spec §6.1). Three purpose-separated sub-keys come out:
//! `blob_enc` wraps stored blobs (which are already client ciphertext),
//! `field_enc` seals sensitive Postgres columns, and `index_mac` keys the
//! lookup HMACs (email, Apple subject, one-time codes) so the database holds
//! neither plaintext identifiers nor unkeyed hashes of them.
//!
//! Sealed format: `"OBSK" || 0x01 || nonce[12] || ciphertext || tag[16]`.
//! Every seal binds an AAD string naming the row/column (or blob) so
//! ciphertexts cannot be swapped between rows by someone with DB access.

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL, Engine};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 4] = b"OBSK";
const VERSION: u8 = 1;
const NONCE_LEN: usize = 12;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("sealed data is malformed")]
    Malformed,
    #[error("sealed data failed authentication")]
    Tamper,
}

#[derive(Clone)]
pub struct ServerKeys {
    blob_enc: [u8; 32],
    field_enc: [u8; 32],
    index_mac: [u8; 32],
}

impl std::fmt::Debug for ServerKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerKeys(..)")
    }
}

impl ServerKeys {
    pub fn from_master(master: &[u8; 32]) -> Self {
        let hk = Hkdf::<Sha256>::new(None, master);
        let derive = |info: &[u8]| {
            let mut out = [0u8; 32];
            hk.expand(info, &mut out)
                .expect("32 bytes is a valid HKDF length");
            out
        };
        Self {
            blob_enc: derive(b"obsink-server:v1:blob-enc"),
            field_enc: derive(b"obsink-server:v1:field-enc"),
            index_mac: derive(b"obsink-server:v1:index-mac"),
        }
    }

    // --- blobs ---------------------------------------------------------------

    pub fn seal_blob(&self, vault_id: &str, path: &str, plaintext: &[u8]) -> Vec<u8> {
        seal(&self.blob_enc, &blob_aad(vault_id, path), plaintext)
    }

    pub fn open_blob(
        &self,
        vault_id: &str,
        path: &str,
        sealed: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        open(&self.blob_enc, &blob_aad(vault_id, path), sealed)
    }

    // --- fields --------------------------------------------------------------

    /// Seal one column value; `aad` is `<table>.<column>:<row id>`.
    pub fn seal_field(&self, table: &str, column: &str, row_id: &str, value: &str) -> Vec<u8> {
        seal(
            &self.field_enc,
            &field_aad(table, column, row_id),
            value.as_bytes(),
        )
    }

    pub fn open_field(
        &self,
        table: &str,
        column: &str,
        row_id: &str,
        sealed: &[u8],
    ) -> Result<String, CryptoError> {
        let bytes = open(&self.field_enc, &field_aad(table, column, row_id), sealed)?;
        String::from_utf8(bytes).map_err(|_| CryptoError::Malformed)
    }

    // --- lookup indexes ------------------------------------------------------

    /// Deterministic keyed index for equality lookups (`kind` separates
    /// namespaces: `email`, `apple_sub`, `otp`).
    pub fn index(&self, kind: &str, value: &str) -> Vec<u8> {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.index_mac)
            .expect("any key length is valid");
        mac.update(kind.as_bytes());
        mac.update(b":");
        mac.update(value.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
}

fn blob_aad(vault_id: &str, path: &str) -> Vec<u8> {
    format!("blob:{vault_id}:{path}").into_bytes()
}

fn field_aad(table: &str, column: &str, row_id: &str) -> Vec<u8> {
    format!("{table}.{column}:{row_id}").into_bytes()
}

pub fn seal(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("AES-GCM encryption is infallible for in-memory inputs");
    let mut out = Vec::with_capacity(MAGIC.len() + 1 + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    out
}

pub fn open(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let header = MAGIC.len() + 1;
    if sealed.len() < header + NONCE_LEN + 16
        || &sealed[..MAGIC.len()] != MAGIC
        || sealed[MAGIC.len()] != VERSION
    {
        return Err(CryptoError::Malformed);
    }
    let (nonce, ciphertext) = sealed[header..].split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new(key.into());
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Tamper)
}

// --- Random identifiers ------------------------------------------------------

pub fn sha256(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

/// `<prefix>_<uuid v4>`, the shape clients already parse.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4())
}

/// `os_` + 256 random bits, base64url. Only its SHA-256 is stored.
pub fn session_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("os_{}", BASE64URL.encode(bytes))
}

/// Six decimal digits, rejection-sampled so every digit is uniform.
pub fn random_digits(count: usize) -> String {
    let mut out = String::with_capacity(count);
    let mut byte = [0u8; 1];
    while out.len() < count {
        rand::rngs::OsRng.fill_bytes(&mut byte);
        if byte[0] < 250 {
            out.push(char::from(b'0' + byte[0] % 10));
        }
    }
    out
}

/// Invite codes avoid look-alike characters (0/O, 1/I/L).
pub const INVITE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
pub const INVITE_CODE_LEN: usize = 8;

pub fn random_invite_code() -> String {
    let mut out = String::with_capacity(INVITE_CODE_LEN);
    let mut byte = [0u8; 1];
    while out.len() < INVITE_CODE_LEN {
        rand::rngs::OsRng.fill_bytes(&mut byte);
        // 32 symbols: mask to 5 bits, no rejection needed.
        out.push(char::from(INVITE_ALPHABET[(byte[0] & 0x1f) as usize]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> ServerKeys {
        ServerKeys::from_master(&[7u8; 32])
    }

    #[test]
    fn envelope_round_trips() {
        let k = keys();
        let sealed = k.seal_blob("vault_a", "tok", b"ciphertext-from-client");
        assert_eq!(
            k.open_blob("vault_a", "tok", &sealed).unwrap(),
            b"ciphertext-from-client"
        );
        let field = k.seal_field("users", "email", "usr_1", "a@b.c");
        assert_eq!(
            k.open_field("users", "email", "usr_1", &field).unwrap(),
            "a@b.c"
        );
        assert_ne!(
            sealed,
            k.seal_blob("vault_a", "tok", b"ciphertext-from-client"),
            "nonces differ"
        );
    }

    #[test]
    fn envelope_rejects_tampered_ciphertext() {
        let k = keys();
        let mut sealed = k.seal_blob("vault_a", "tok", b"data");
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(matches!(
            k.open_blob("vault_a", "tok", &sealed),
            Err(CryptoError::Tamper)
        ));
        assert!(matches!(
            k.open_blob("vault_a", "tok", b"short"),
            Err(CryptoError::Malformed)
        ));
    }

    #[test]
    fn envelope_rejects_wrong_aad() {
        let k = keys();
        let sealed = k.seal_field("users", "email", "usr_1", "a@b.c");
        assert!(k.open_field("users", "email", "usr_2", &sealed).is_err());
        let other = ServerKeys::from_master(&[8u8; 32]);
        assert!(other
            .open_field("users", "email", "usr_1", &sealed)
            .is_err());
    }

    #[test]
    fn hmac_index_is_deterministic_and_namespaced() {
        let k = keys();
        assert_eq!(k.index("email", "a@b.c"), k.index("email", "a@b.c"));
        assert_ne!(k.index("email", "a@b.c"), k.index("apple_sub", "a@b.c"));
        assert_ne!(
            k.index("email", "a@b.c"),
            keys_other().index("email", "a@b.c")
        );
    }

    fn keys_other() -> ServerKeys {
        ServerKeys::from_master(&[9u8; 32])
    }

    #[test]
    fn random_helpers_have_the_right_shape() {
        assert_eq!(random_digits(6).len(), 6);
        assert!(random_digits(6).chars().all(|c| c.is_ascii_digit()));
        let code = random_invite_code();
        assert_eq!(code.len(), 8);
        assert!(code.bytes().all(|b| INVITE_ALPHABET.contains(&b)));
        assert!(session_token().starts_with("os_"));
        assert!(new_id("vault").starts_with("vault_"));
    }
}
