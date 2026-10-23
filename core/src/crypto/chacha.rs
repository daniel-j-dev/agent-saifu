//! ChaCha20-Poly1305 (RFC 8439) with a 12-byte nonce, backed by `chacha20poly1305`.

use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, Key, KeyInit, Nonce};

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;

fn build(key: &[u8], nonce: &[u8]) -> Result<(ChaCha20Poly1305, Nonce<ChaCha20Poly1305>), String> {
    let key = Key::<ChaCha20Poly1305>::try_from(key)
        .map_err(|_| "ChaCha20 key must be 32 bytes".to_string())?;
    let nonce = Nonce::<ChaCha20Poly1305>::try_from(nonce)
        .map_err(|_| "ChaCha20 nonce must be 12 bytes".to_string())?;
    Ok((ChaCha20Poly1305::new(&key), nonce))
}

/// Returns ciphertext followed by the 16-byte tag.
pub fn seal(key: &[u8], nonce: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let (cipher, nonce) = build(key, nonce)?;
    cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| "encryption failed".to_string())
}

/// Fails if the data was modified or the key is wrong.
pub fn open(key: &[u8], nonce: &[u8], sealed: &[u8]) -> Result<Vec<u8>, String> {
    let (cipher, nonce) = build(key, nonce)?;
    cipher
        .decrypt(&nonce, sealed)
        .map_err(|_| "decryption failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    // RFC 8439 section 2.8.2 has AAD; without AAD we check round trips and tamper detection.
    const KEY: [u8; 32] = [3u8; 32];
    const NONCE: [u8; 12] = [5u8; 12];

    #[test]
    fn round_trip() {
        let sealed = seal(&KEY, &NONCE, b"hello").unwrap();
        assert_eq!(sealed.len(), 5 + TAG_LEN);
        assert_eq!(open(&KEY, &NONCE, &sealed).unwrap(), b"hello");
    }

    #[test]
    fn known_answer_zero_key_and_nonce() {
        // ChaCha20-Poly1305, all-zero key and nonce, empty plaintext: the tag of nothing.
        let sealed = seal(&[0u8; 32], &[0u8; 12], b"").unwrap();
        assert_eq!(hex::encode(&sealed), "4eb972c9a8fb3a1b382bb4d36f5ffad1");
    }

    #[test]
    fn tampering_and_wrong_key_fail() {
        let mut sealed = seal(&KEY, &NONCE, b"hello").unwrap();
        assert!(open(&[4u8; 32], &NONCE, &sealed).is_err());
        sealed[0] ^= 1;
        assert!(open(&KEY, &NONCE, &sealed).is_err());
    }

    #[test]
    fn bad_lengths_are_errors() {
        assert!(seal(&[0u8; 31], &NONCE, b"x").is_err());
        assert!(seal(&KEY, &[0u8; 11], b"x").is_err());
        assert!(open(&KEY, &NONCE, &[0u8; 3]).is_err());
    }
}
