//! BIP32 hierarchical key derivation for secp256k1.

use super::hmac::hmac_sha512;
use super::secp256k1;
use super::secret::zeroize;

const HARDENED: u32 = 0x8000_0000;

/// Ethereum default account path (first account).
pub const ETH_DEFAULT_PATH: &str = "m/44'/60'/0'/0/0";

pub struct ExtendedKey {
    key: [u8; 32],
    chain_code: [u8; 32],
}

impl Drop for ExtendedKey {
    fn drop(&mut self) {
        zeroize(&mut self.key);
        zeroize(&mut self.chain_code);
    }
}

impl ExtendedKey {
    pub fn private_key(&self) -> [u8; 32] {
        self.key
    }

    pub fn chain_code(&self) -> [u8; 32] {
        self.chain_code
    }

    pub fn from_seed(seed: &[u8]) -> Result<ExtendedKey, String> {
        if seed.len() < 16 || seed.len() > 64 {
            return Err("seed must be 16 to 64 bytes".to_string());
        }
        let mut i = hmac_sha512(b"Bitcoin seed", seed);
        let mut key = [0u8; 32];
        let mut chain_code = [0u8; 32];
        key.copy_from_slice(&i[..32]);
        chain_code.copy_from_slice(&i[32..]);
        zeroize(&mut i);
        if !secp256k1::is_valid_private_key(&key) {
            zeroize(&mut key);
            return Err("seed produced an invalid master key".to_string());
        }
        Ok(ExtendedKey { key, chain_code })
    }

    pub fn derive_child(&self, index: u32) -> Result<ExtendedKey, String> {
        let mut data = Vec::with_capacity(37);
        if index >= HARDENED {
            data.push(0);
            data.extend_from_slice(&self.key);
        } else {
            data.extend_from_slice(&secp256k1::public_key_compressed(&self.key)?);
        }
        data.extend_from_slice(&index.to_be_bytes());
        let mut i = hmac_sha512(&self.chain_code, &data);
        zeroize(&mut data);
        let mut tweak = [0u8; 32];
        tweak.copy_from_slice(&i[..32]);
        let mut chain_code = [0u8; 32];
        chain_code.copy_from_slice(&i[32..]);
        zeroize(&mut i);
        let child = secp256k1::add_private_keys(&self.key, &tweak);
        zeroize(&mut tweak);
        Ok(ExtendedKey {
            key: child?,
            chain_code,
        })
    }

    pub fn derive_path(&self, path: &str) -> Result<ExtendedKey, String> {
        let indices = parse_path(path)?;
        let mut current = ExtendedKey {
            key: self.key,
            chain_code: self.chain_code,
        };
        for index in indices {
            current = current.derive_child(index)?;
        }
        Ok(current)
    }
}

/// Parses `m/44'/60'/0'/0/0` into child indices. `'`, `h` and `H` mark hardened levels.
pub fn parse_path(path: &str) -> Result<Vec<u32>, String> {
    let mut parts = path.split('/');
    if parts.next() != Some("m") {
        return Err("derivation path must start with 'm'".to_string());
    }
    parts.map(parse_path_component).collect()
}

fn parse_path_component(part: &str) -> Result<u32, String> {
    let (digits, hardened) = match part.strip_suffix(['\'', 'h', 'H']) {
        Some(d) => (d, true),
        None => (part, false),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("invalid path component '{part}'"));
    }
    let value: u32 = digits
        .parse()
        .map_err(|_| format!("path component '{part}' out of range"))?;
    if value >= HARDENED {
        return Err(format!("path component '{part}' out of range"));
    }
    Ok(if hardened { value | HARDENED } else { value })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    fn seed() -> Vec<u8> {
        hex::decode("000102030405060708090a0b0c0d0e0f").unwrap()
    }

    #[test]
    fn bip32_test_vector_1() {
        let m = ExtendedKey::from_seed(&seed()).unwrap();
        assert_eq!(
            hex::encode(&m.private_key()),
            "e8f32e723decf4051aefac8e2c93c9c5b214313817cdb01a1494b917c8436b35"
        );
        assert_eq!(
            hex::encode(&m.chain_code()),
            "873dff81c02f525623fd1fe5167eac3a55a049de3d314bb42ee227ffed37d508"
        );
        let m0h = m.derive_path("m/0'").unwrap();
        assert_eq!(
            hex::encode(&m0h.private_key()),
            "edb2e14f9ee77d26dd93b4ecede8d16ed408ce149b6cd80b0715a2d911a0afea"
        );
        assert_eq!(
            hex::encode(&m0h.chain_code()),
            "47fdacbd0f1097043b78c63c20c34ef4ed9a111d980047ad16282c7ae6236141"
        );
        let m0h1 = m.derive_path("m/0'/1").unwrap();
        assert_eq!(
            hex::encode(&m0h1.private_key()),
            "3c6cb8d0f6a264c91ea8b5030fadaa8e538b020f0a387421a12de9319dc93368"
        );
    }

    #[test]
    fn derive_path_equals_step_by_step() {
        let m = ExtendedKey::from_seed(&seed()).unwrap();
        let stepwise = m
            .derive_child(44 | HARDENED)
            .unwrap()
            .derive_child(60 | HARDENED)
            .unwrap()
            .derive_child(HARDENED)
            .unwrap()
            .derive_child(0)
            .unwrap()
            .derive_child(0)
            .unwrap();
        assert_eq!(
            stepwise.private_key(),
            m.derive_path(ETH_DEFAULT_PATH).unwrap().private_key()
        );
    }

    #[test]
    fn path_parsing() {
        assert_eq!(parse_path("m"), Ok(vec![]));
        assert_eq!(
            parse_path("m/44'/60h/0H/0/1"),
            Ok(vec![44 | HARDENED, 60 | HARDENED, HARDENED, 0, 1])
        );
        for bad in [
            "",
            "44'/60'",
            "m/",
            "m//1",
            "m/a",
            "m/-1",
            "m/2147483648",
            "m/'",
            "m/1''",
            "m/99999999999",
        ] {
            assert!(parse_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn seed_length_limits() {
        assert!(ExtendedKey::from_seed(&[0u8; 15]).is_err());
        assert!(ExtendedKey::from_seed(&[1u8; 65]).is_err());
        assert!(ExtendedKey::from_seed(&[1u8; 16]).is_ok());
    }
}
