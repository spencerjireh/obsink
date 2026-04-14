use aes_gcm::{
    aead::{rand_core::RngCore, Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use argon2::{Algorithm, Argon2, Params, Version};
use thiserror::Error;

pub type KeyBytes = [u8; 32];

const NONCE_LEN: usize = 12;

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
    use super::{decrypt, derive_key, encrypt};

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
}
