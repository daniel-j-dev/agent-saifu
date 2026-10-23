//! WalletConnect typed envelopes: type 0 (shared symmetric key) and type 1 (carries the
//! sender's public key), sealed with ChaCha20-Poly1305 and transported as base64.

use crate::crypto::base64;
use crate::crypto::chacha::{self, NONCE_LEN};
use crate::crypto::hkdf::hkdf_sha256;
use crate::crypto::random;
use crate::crypto::sha2::sha256;
use crate::crypto::{hex, x25519::KeyPair};

pub const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub plaintext: Vec<u8>,
    /// Present on type 1 envelopes.
    pub sender_public_key: Option<[u8; 32]>,
}

pub fn topic_of(sym_key: &[u8; 32]) -> String {
    hex::encode(&sha256(sym_key))
}

/// Session key: HKDF-SHA256 (no salt, no info) of the X25519 shared secret.
pub fn derive_session_key(own: &KeyPair, peer_public: &[u8; 32]) -> Result<[u8; 32], String> {
    let mut shared = own.shared_secret(peer_public)?;
    let key = hkdf_sha256(&shared, &[], &[], 32);
    crate::crypto::secret::zeroize(&mut shared);
    key?.try_into()
        .map_err(|_| "unexpected key length".to_string())
}

pub fn encode_type0(sym_key: &[u8; 32], plaintext: &[u8]) -> Result<String, String> {
    encode_with_nonce(
        sym_key,
        plaintext,
        None,
        random::random_array::<NONCE_LEN>()?,
    )
}

pub fn encode_type1(
    sym_key: &[u8; 32],
    plaintext: &[u8],
    sender_public_key: &[u8; 32],
) -> Result<String, String> {
    encode_with_nonce(
        sym_key,
        plaintext,
        Some(sender_public_key),
        random::random_array::<NONCE_LEN>()?,
    )
}

fn encode_with_nonce(
    sym_key: &[u8; 32],
    plaintext: &[u8],
    sender_public_key: Option<&[u8; 32]>,
    nonce: [u8; NONCE_LEN],
) -> Result<String, String> {
    let sealed = chacha::seal(sym_key, &nonce, plaintext)?;
    let mut out = Vec::with_capacity(1 + 32 + NONCE_LEN + sealed.len());
    match sender_public_key {
        Some(public) => {
            out.push(1);
            out.extend_from_slice(public);
        }
        None => out.push(0),
    }
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(base64::encode(&out))
}

pub fn decode(sym_key: &[u8; 32], message: &str) -> Result<Decoded, String> {
    if message.len() > MAX_ENVELOPE_BYTES * 4 / 3 + 8 {
        return Err("message is too large".to_string());
    }
    let bytes = base64::decode(message)?;
    let (&kind, rest) = bytes.split_first().ok_or("empty message")?;
    let (sender_public_key, rest) = match kind {
        0 => (None, rest),
        1 => {
            if rest.len() < 32 {
                return Err("truncated message".to_string());
            }
            let (public, rest) = rest.split_at(32);
            (Some(<[u8; 32]>::try_from(public).expect("32 bytes")), rest)
        }
        other => return Err(format!("unsupported envelope type {other}")),
    };
    if rest.len() < NONCE_LEN + chacha::TAG_LEN {
        return Err("truncated message".to_string());
    }
    let (nonce, sealed) = rest.split_at(NONCE_LEN);
    Ok(Decoded {
        plaintext: chacha::open(sym_key, nonce, sealed)?,
        sender_public_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [9u8; 32];

    #[test]
    fn type0_round_trip() {
        let msg = encode_type0(&KEY, b"{\"hello\":1}").unwrap();
        let decoded = decode(&KEY, &msg).unwrap();
        assert_eq!(decoded.plaintext, b"{\"hello\":1}");
        assert_eq!(decoded.sender_public_key, None);
    }

    #[test]
    fn type1_round_trip_carries_the_public_key() {
        let public = [7u8; 32];
        let msg = encode_type1(&KEY, b"x", &public).unwrap();
        let decoded = decode(&KEY, &msg).unwrap();
        assert_eq!(decoded.plaintext, b"x");
        assert_eq!(decoded.sender_public_key, Some(public));
    }

    #[test]
    fn layout_is_type_nonce_sealbox() {
        let nonce = [1u8; NONCE_LEN];
        let msg = encode_with_nonce(&KEY, b"abc", None, nonce).unwrap();
        let raw = base64::decode(&msg).unwrap();
        assert_eq!(raw[0], 0);
        assert_eq!(raw[1..13], nonce);
        assert_eq!(raw.len(), 1 + 12 + 3 + 16);
        let typed = encode_with_nonce(&KEY, b"abc", Some(&[5u8; 32]), nonce).unwrap();
        let raw = base64::decode(&typed).unwrap();
        assert_eq!(raw[0], 1);
        assert_eq!(raw[1..33], [5u8; 32]);
        assert_eq!(raw[33..45], nonce);
    }

    #[test]
    fn fresh_nonce_for_every_message() {
        assert_ne!(
            encode_type0(&KEY, b"same").unwrap(),
            encode_type0(&KEY, b"same").unwrap()
        );
    }

    #[test]
    fn wrong_key_tampering_and_garbage_are_rejected() {
        let msg = encode_type0(&KEY, b"secret").unwrap();
        assert!(decode(&[1u8; 32], &msg).is_err());
        let mut raw = base64::decode(&msg).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        assert!(decode(&KEY, &base64::encode(&raw)).is_err());
        assert!(decode(&KEY, "").is_err());
        assert!(decode(&KEY, "not base64!").is_err());
        assert!(
            decode(&KEY, &base64::encode(&[2u8; 40]))
                .unwrap_err()
                .contains("type 2")
        );
        assert!(decode(&KEY, &base64::encode(&[0u8; 20])).is_err());
        assert!(decode(&KEY, &base64::encode(&[1u8; 20])).is_err());
        assert!(decode(&KEY, &"A".repeat(MAX_ENVELOPE_BYTES * 2)).is_err());
    }

    #[test]
    fn topic_is_sha256_of_the_key() {
        assert_eq!(
            topic_of(&[0u8; 32]),
            "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925"
        );
    }

    #[test]
    fn both_sides_derive_the_same_session_key_and_topic() {
        let a = KeyPair::generate().unwrap();
        let b = KeyPair::generate().unwrap();
        let ka = derive_session_key(&a, &b.public).unwrap();
        let kb = derive_session_key(&b, &a.public).unwrap();
        assert_eq!(ka, kb);
        assert_eq!(topic_of(&ka), topic_of(&kb));
        assert!(derive_session_key(&a, &[0u8; 32]).is_err());
    }
}
