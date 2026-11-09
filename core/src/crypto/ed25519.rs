//! Ed25519 signatures (relay authentication, Solana), backed by `ed25519-dalek`.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(seed).verifying_key().to_bytes()
}

pub fn sign(seed: &[u8; 32], message: &[u8]) -> [u8; 64] {
    SigningKey::from_bytes(seed).sign(message).to_bytes()
}

/// Whether 32 bytes decode to a point on the Ed25519 curve. Solana program-derived addresses
/// must be *off* the curve, so nobody can hold a private key for them.
pub fn is_on_curve(bytes: &[u8; 32]) -> bool {
    VerifyingKey::from_bytes(bytes).is_ok()
}

pub fn verify(public: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    VerifyingKey::from_bytes(public)
        .map(|key| {
            key.verify(message, &Signature::from_bytes(signature))
                .is_ok()
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    // RFC 8032 section 7.1, test 1 (empty message).
    #[test]
    fn rfc8032_vector() {
        let seed = hex::decode_fixed::<32>(
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        )
        .unwrap();
        assert_eq!(
            hex::encode(&public_key(&seed)),
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
        );
        assert_eq!(
            hex::encode(&sign(&seed, b"")),
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
        );
    }

    #[test]
    fn verify_accepts_only_the_right_signature() {
        let seed = [9u8; 32];
        let public = public_key(&seed);
        let signature = sign(&seed, b"hello");
        assert!(verify(&public, b"hello", &signature));
        assert!(!verify(&public, b"hellp", &signature));
        let mut bad = signature;
        bad[0] ^= 1;
        assert!(!verify(&public, b"hello", &bad));
    }

    #[test]
    fn curve_membership() {
        assert!(is_on_curve(&public_key(&[1u8; 32])));
        // y = 2 has no x on the curve.
        let mut off = [0u8; 32];
        off[0] = 2;
        assert!(!is_on_curve(&off));
    }
}
