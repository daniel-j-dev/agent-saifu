//! X25519 key agreement (WalletConnect session keys), backed by `x25519-dalek`.

use x25519_dalek::{PublicKey, StaticSecret};

use super::random;
use super::secret::zeroize;

/// A private key that is wiped when dropped.
pub struct KeyPair {
    secret: [u8; 32],
    pub public: [u8; 32],
}

impl KeyPair {
    pub fn generate() -> Result<KeyPair, String> {
        Ok(KeyPair::from_secret(random::random_array::<32>()?))
    }

    pub fn from_secret(secret: [u8; 32]) -> KeyPair {
        let public = PublicKey::from(&StaticSecret::from(secret)).to_bytes();
        KeyPair { secret, public }
    }

    /// Raw Diffie-Hellman output. Fails for low-order peer keys (all-zero result).
    pub fn shared_secret(&self, peer_public: &[u8; 32]) -> Result<[u8; 32], String> {
        let shared = StaticSecret::from(self.secret).diffie_hellman(&PublicKey::from(*peer_public));
        if !shared.was_contributory() {
            return Err("invalid peer public key".to_string());
        }
        Ok(shared.to_bytes())
    }
}

impl Drop for KeyPair {
    fn drop(&mut self) {
        zeroize(&mut self.secret);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    #[test]
    fn rfc7748_diffie_hellman_vector() {
        let alice = KeyPair::from_secret(
            hex::decode_fixed::<32>(
                "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a",
            )
            .unwrap(),
        );
        let bob = KeyPair::from_secret(
            hex::decode_fixed::<32>(
                "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb",
            )
            .unwrap(),
        );
        assert_eq!(
            hex::encode(&alice.public),
            "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"
        );
        assert_eq!(
            hex::encode(&bob.public),
            "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f"
        );
        let expected = "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742";
        assert_eq!(
            hex::encode(&alice.shared_secret(&bob.public).unwrap()),
            expected
        );
        assert_eq!(
            hex::encode(&bob.shared_secret(&alice.public).unwrap()),
            expected
        );
    }

    #[test]
    fn low_order_peer_key_is_rejected() {
        let kp = KeyPair::generate().unwrap();
        assert!(kp.shared_secret(&[0u8; 32]).is_err());
    }
}
