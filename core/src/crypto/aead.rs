//! AES-256-GCM with a 16-byte IV, matching WebCrypto as used by MetaMask.

use aes_gcm::AesGcm;
use aes_gcm::aead::{Aead, Key, KeyInit, Nonce};
use aes_gcm::aes::Aes256;
use aes_gcm::aes::cipher::consts::U16;

type Cipher = AesGcm<Aes256, U16>;

pub const KEY_LEN: usize = 32;
pub const IV_LEN: usize = 16;
pub const TAG_LEN: usize = 16;

fn build(key: &[u8], iv: &[u8]) -> Result<(Cipher, Nonce<Cipher>), String> {
    let key = Key::<Cipher>::try_from(key).map_err(|_| "AES key must be 32 bytes".to_string())?;
    let nonce =
        Nonce::<Cipher>::try_from(iv).map_err(|_| "AES-GCM IV must be 16 bytes".to_string())?;
    Ok((Cipher::new(&key), nonce))
}

/// Returns ciphertext followed by the 16-byte tag (the WebCrypto layout).
pub fn encrypt(key: &[u8], iv: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let (cipher, nonce) = build(key, iv)?;
    cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| "encryption failed".to_string())
}

/// Fails if the data was modified or the key is wrong.
pub fn decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    let (cipher, nonce) = build(key, iv)?;
    cipher
        .decrypt(&nonce, data)
        .map_err(|_| "decryption failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7u8; 32];
    const IV: [u8; 16] = [9u8; 16];

    #[test]
    fn round_trip() {
        let ct = encrypt(&KEY, &IV, b"hello vault").unwrap();
        assert_eq!(ct.len(), "hello vault".len() + TAG_LEN);
        assert_eq!(decrypt(&KEY, &IV, &ct).unwrap(), b"hello vault");
    }

    #[test]
    fn empty_plaintext() {
        let ct = encrypt(&KEY, &IV, b"").unwrap();
        assert_eq!(ct.len(), TAG_LEN);
        assert_eq!(decrypt(&KEY, &IV, &ct).unwrap(), b"");
    }

    #[test]
    fn wrong_key_or_iv_fails() {
        let ct = encrypt(&KEY, &IV, b"secret").unwrap();
        assert!(decrypt(&[8u8; 32], &IV, &ct).is_err());
        assert!(decrypt(&KEY, &[1u8; 16], &ct).is_err());
    }

    #[test]
    fn tampering_is_detected() {
        let mut ct = encrypt(&KEY, &IV, b"secret").unwrap();
        ct[0] ^= 1;
        assert!(decrypt(&KEY, &IV, &ct).is_err());
        let mut ct = encrypt(&KEY, &IV, b"secret").unwrap();
        let last = ct.len() - 1;
        ct[last] ^= 1;
        assert!(decrypt(&KEY, &IV, &ct).is_err());
    }

    #[test]
    fn truncated_data_fails() {
        assert!(decrypt(&KEY, &IV, &[0u8; 5]).is_err());
        assert!(decrypt(&KEY, &IV, &[]).is_err());
    }

    #[test]
    fn bad_lengths_rejected() {
        assert!(encrypt(&[0u8; 16], &IV, b"x").is_err());
        assert!(encrypt(&KEY, &[0u8; 12], b"x").is_err());
    }
}
