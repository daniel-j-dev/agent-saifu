//! SLIP-0010 hierarchical key derivation for Ed25519 (used by Sui).
//!
//! Ed25519 only supports hardened children, so every level of the path must be hardened.

use super::bip32::parse_path;
use super::hmac::hmac_sha512;
use super::secret::zeroize;

const HARDENED: u32 = 0x8000_0000;
const MASTER_KEY: &[u8] = b"ed25519 seed";

/// Sui's default account path (first account): `m/44'/784'/0'/0'/0'`.
pub const SUI_DEFAULT_PATH: &str = "m/44'/784'/0'/0'/0'";

pub struct Ed25519Node {
    key: [u8; 32],
    chain_code: [u8; 32],
}

impl Drop for Ed25519Node {
    fn drop(&mut self) {
        zeroize(&mut self.key);
        zeroize(&mut self.chain_code);
    }
}

impl Ed25519Node {
    fn from_digest(mut digest: [u8; 64]) -> Ed25519Node {
        let mut key = [0u8; 32];
        let mut chain_code = [0u8; 32];
        key.copy_from_slice(&digest[..32]);
        chain_code.copy_from_slice(&digest[32..]);
        zeroize(&mut digest);
        Ed25519Node { key, chain_code }
    }

    pub fn from_seed(seed: &[u8]) -> Result<Ed25519Node, String> {
        if seed.len() < 16 || seed.len() > 64 {
            return Err("seed must be 16 to 64 bytes".to_string());
        }
        Ok(Self::from_digest(hmac_sha512(MASTER_KEY, seed)))
    }

    pub fn private_key(&self) -> [u8; 32] {
        self.key
    }

    pub fn chain_code(&self) -> [u8; 32] {
        self.chain_code
    }

    pub fn derive_child(&self, index: u32) -> Result<Ed25519Node, String> {
        if index < HARDENED {
            return Err("Ed25519 derivation supports hardened indices only".to_string());
        }
        let mut data = Vec::with_capacity(37);
        data.push(0);
        data.extend_from_slice(&self.key);
        data.extend_from_slice(&index.to_be_bytes());
        let digest = hmac_sha512(&self.chain_code, &data);
        zeroize(&mut data);
        Ok(Self::from_digest(digest))
    }

    pub fn derive_path(&self, path: &str) -> Result<Ed25519Node, String> {
        let indices = parse_path(path)?;
        let mut current = Ed25519Node {
            key: self.key,
            chain_code: self.chain_code,
        };
        for index in indices {
            current = current.derive_child(index)?;
        }
        Ok(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    fn seed() -> Vec<u8> {
        hex::decode("000102030405060708090a0b0c0d0e0f").unwrap()
    }

    // Test vector 1 from the SLIP-0010 specification.
    #[test]
    fn slip10_test_vector_1() {
        let m = Ed25519Node::from_seed(&seed()).unwrap();
        assert_eq!(
            hex::encode(&m.private_key()),
            "2b4be7f19ee27bbf30c667b642d5f4aa69fd169872f8fc3059c08ebae2eb19e7"
        );
        assert_eq!(
            hex::encode(&m.chain_code()),
            "90046a93de5380a72b5e45010748567d5ea02bbf6522f979e05c0d8d8ca9fffb"
        );
        let c = m.derive_path("m/0'").unwrap();
        assert_eq!(
            hex::encode(&c.private_key()),
            "68e0fe46dfb67e368c75379acec591dad19df3cde26e63b93a8e704f1dade7a3"
        );
        assert_eq!(
            hex::encode(&c.chain_code()),
            "8b59aa11380b624e81507a27fedda59fea6d0b779a778918a2fd3590e16e9c69"
        );
        let deep = m.derive_path("m/0'/1'/2'/2'/1000000000'").unwrap();
        assert_eq!(
            hex::encode(&deep.private_key()),
            "8f94d394a8e8fd6b1bc2f3f49f5c47e385281d5c17e65324b0f62483e37e8793"
        );
    }

    #[test]
    fn non_hardened_levels_are_refused() {
        let m = Ed25519Node::from_seed(&seed()).unwrap();
        assert!(m.derive_path("m/0'/1").is_err());
        assert!(m.derive_child(5).is_err());
    }

    #[test]
    fn rejects_bad_paths_and_seeds() {
        let m = Ed25519Node::from_seed(&seed()).unwrap();
        assert!(m.derive_path("0'/1'").is_err());
        assert!(m.derive_path("m/x'").is_err());
        assert!(Ed25519Node::from_seed(&[1u8; 8]).is_err());
        assert!(Ed25519Node::from_seed(&[1u8; 65]).is_err());
    }

    #[test]
    fn sui_default_path_is_all_hardened() {
        let indices = parse_path(SUI_DEFAULT_PATH).unwrap();
        assert_eq!(indices.len(), 5);
        assert!(indices.iter().all(|i| *i >= HARDENED));
        assert_eq!(indices[1], 784 | HARDENED);
    }
}
