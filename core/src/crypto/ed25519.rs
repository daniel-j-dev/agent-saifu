//! Ed25519 signatures (relay client authentication), backed by `ed25519-dalek`.

use ed25519_dalek::{Signer, SigningKey};

pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(seed).verifying_key().to_bytes()
}

pub fn sign(seed: &[u8; 32], message: &[u8]) -> [u8; 64] {
    SigningKey::from_bytes(seed).sign(message).to_bytes()
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
}
