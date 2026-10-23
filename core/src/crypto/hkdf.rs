//! HKDF-SHA256 (RFC 5869), built on the in-repo HMAC.

use super::hmac::hmac_sha256;

const HASH_LEN: usize = 32;
const MAX_OUTPUT: usize = 255 * HASH_LEN;

/// Extract step. An empty salt means a string of zeros, as the RFC specifies.
fn extract(salt: &[u8], ikm: &[u8]) -> [u8; HASH_LEN] {
    if salt.is_empty() {
        hmac_sha256(&[0u8; HASH_LEN], ikm)
    } else {
        hmac_sha256(salt, ikm)
    }
}

pub fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8], len: usize) -> Result<Vec<u8>, String> {
    if len == 0 || len > MAX_OUTPUT {
        return Err("invalid HKDF output length".to_string());
    }
    let prk = extract(salt, ikm);
    let mut out = Vec::with_capacity(len);
    let mut previous: Vec<u8> = vec![];
    let mut counter = 1u8;
    while out.len() < len {
        let mut block = previous;
        block.extend_from_slice(info);
        block.push(counter);
        previous = hmac_sha256(&prk, &block).to_vec();
        out.extend_from_slice(&previous);
        counter = counter.wrapping_add(1);
    }
    out.truncate(len);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    #[test]
    fn rfc5869_case_1() {
        let ikm = hex::decode("0x0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b").unwrap();
        let salt = hex::decode("0x000102030405060708090a0b0c").unwrap();
        let info = hex::decode("0xf0f1f2f3f4f5f6f7f8f9").unwrap();
        let okm = hkdf_sha256(&ikm, &salt, &info, 42).unwrap();
        assert_eq!(
            hex::encode(&okm),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
        );
    }

    #[test]
    fn rfc5869_case_3_empty_salt_and_info() {
        let ikm = [0x0bu8; 22];
        let okm = hkdf_sha256(&ikm, &[], &[], 42).unwrap();
        assert_eq!(
            hex::encode(&okm),
            "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8"
        );
    }

    #[test]
    fn rejects_bad_lengths() {
        assert!(hkdf_sha256(b"k", &[], &[], 0).is_err());
        assert!(hkdf_sha256(b"k", &[], &[], 255 * 32 + 1).is_err());
        assert_eq!(
            hkdf_sha256(b"k", &[], &[], 255 * 32).unwrap().len(),
            255 * 32
        );
    }
}
