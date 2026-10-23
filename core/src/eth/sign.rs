//! EIP-191 `personal_sign`: sign a message with the Ethereum signed-message prefix.

use crate::crypto::keccak::keccak256;
use crate::crypto::secp256k1;

pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// `keccak256("\x19Ethereum Signed Message:\n" + len(message) + message)`
pub fn personal_message_hash(message: &[u8]) -> [u8; 32] {
    let mut data = format!("\x19Ethereum Signed Message:\n{}", message.len()).into_bytes();
    data.extend_from_slice(message);
    keccak256(&data)
}

/// 65-byte `r || s || v` signature with `v` of 27 or 28, as wallets return it.
pub fn sign_digest(private_key: &[u8; 32], digest: &[u8; 32]) -> Result<[u8; 65], String> {
    let (rs, recovery) = secp256k1::sign_recoverable(private_key, digest)?;
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&rs);
    out[64] = 27 + recovery;
    Ok(out)
}

pub fn sign_personal_message(private_key: &[u8; 32], message: &[u8]) -> Result<[u8; 65], String> {
    if message.len() > MAX_MESSAGE_BYTES {
        return Err("message is too large to sign".to_string());
    }
    sign_digest(private_key, &personal_message_hash(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;
    use crate::eth::address::address_from_private_key;

    const KEY: &str = "4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";

    #[test]
    fn hash_matches_web3_example() {
        assert_eq!(
            hex::encode_prefixed(&personal_message_hash(b"Some data")),
            "0x1da44b586eb0729ff70a73c326926f6ed5a25f5b056e7f47fbc6e58d86871655"
        );
    }

    #[test]
    fn signature_matches_web3_example() {
        let key = hex::decode_fixed::<32>(KEY).unwrap();
        let sig = sign_personal_message(&key, b"Some data").unwrap();
        assert_eq!(
            hex::encode_prefixed(&sig),
            "0xb91467e570a6466aa9e9876cbcd013baba02900b8979d43fe208a4a4f339f5fd6007e74cd82e037b800186422fc2da167c747ef045e5d18a5f5d4300f8e1a0291c"
        );
    }

    #[test]
    fn signature_recovers_to_the_signer() {
        let key = hex::decode_fixed::<32>(KEY).unwrap();
        let hash = personal_message_hash(b"hello");
        let sig = sign_digest(&key, &hash).unwrap();
        assert!(sig[64] == 27 || sig[64] == 28);
        let rs: [u8; 64] = sig[..64].try_into().unwrap();
        let public = secp256k1::recover_public_key(&hash, &rs, sig[64] - 27).unwrap();
        let hashed = keccak256(&public[1..]);
        assert_eq!(hashed[12..], address_from_private_key(&key).unwrap());
    }

    #[test]
    fn oversized_message_is_refused() {
        let key = [1u8; 32];
        assert!(sign_personal_message(&key, &vec![0u8; MAX_MESSAGE_BYTES + 1]).is_err());
    }
}
