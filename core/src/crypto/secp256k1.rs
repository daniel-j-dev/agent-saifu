//! secp256k1 operations, backed by the `k256` crate.

use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};

pub const SECP256K1_ORDER: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,
    0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
];

fn signing_key(private_key: &[u8; 32]) -> Result<SigningKey, String> {
    SigningKey::from_slice(private_key).map_err(|_| "invalid secp256k1 private key".to_string())
}

/// True if the bytes are a valid private key (non-zero and below the curve order).
pub fn is_valid_private_key(private_key: &[u8; 32]) -> bool {
    signing_key(private_key).is_ok()
}

/// 65-byte uncompressed public key (`0x04 || X || Y`).
pub fn public_key_uncompressed(private_key: &[u8; 32]) -> Result<[u8; 65], String> {
    let key = signing_key(private_key)?;
    let point = key.verifying_key().to_sec1_point(false);
    <[u8; 65]>::try_from(point.as_bytes()).map_err(|_| "unexpected public key length".to_string())
}

/// 33-byte compressed public key.
pub fn public_key_compressed(private_key: &[u8; 32]) -> Result<[u8; 33], String> {
    let key = signing_key(private_key)?;
    let point = key.verifying_key().to_sec1_point(true);
    <[u8; 33]>::try_from(point.as_bytes()).map_err(|_| "unexpected public key length".to_string())
}

/// Deterministic (RFC 6979), low-S signature over a 32-byte digest.
/// Returns `(r || s, recovery_id)` where the recovery id is 0 or 1.
pub fn sign_recoverable(
    private_key: &[u8; 32],
    digest: &[u8; 32],
) -> Result<([u8; 64], u8), String> {
    let key = signing_key(private_key)?;
    let (sig, recid): (Signature, RecoveryId) = key.sign_prehash_recoverable(digest);
    let bytes: [u8; 64] = sig.to_bytes().into();
    Ok((bytes, recid.to_byte()))
}

/// Recovers the uncompressed public key from a signature (used for self-checks).
pub fn recover_public_key(
    digest: &[u8; 32],
    signature: &[u8; 64],
    recovery_id: u8,
) -> Result<[u8; 65], String> {
    let sig = Signature::from_slice(signature).map_err(|_| "invalid signature".to_string())?;
    let recid = RecoveryId::from_byte(recovery_id).ok_or("invalid recovery id")?;
    let key = VerifyingKey::recover_from_prehash(digest, &sig, recid)
        .map_err(|_| "public key recovery failed".to_string())?;
    let point = key.to_sec1_point(false);
    <[u8; 65]>::try_from(point.as_bytes()).map_err(|_| "unexpected public key length".to_string())
}

/// Computes `(parent + tweak) mod n` as used by BIP32 child key derivation.
/// Fails if `tweak >= n` or the result is zero (BIP32 says to skip such indices).
pub fn add_private_keys(parent: &[u8; 32], tweak: &[u8; 32]) -> Result<[u8; 32], String> {
    if !less_than_order(tweak) {
        return Err("tweak is not below the curve order".to_string());
    }
    let mut sum = [0u8; 33];
    let mut carry = 0u16;
    for i in (0..32).rev() {
        let v = u16::from(parent[i]) + u16::from(tweak[i]) + carry;
        sum[i + 1] = v as u8;
        carry = v >> 8;
    }
    sum[0] = carry as u8;
    let mut result = sum;
    if sum[0] != 0 || ge_order(&sum[1..]) {
        let mut borrow = 0i16;
        for i in (0..32).rev() {
            let mut v = i16::from(sum[i + 1]) - i16::from(SECP256K1_ORDER[i]) - borrow;
            borrow = if v < 0 { 1 } else { 0 };
            if v < 0 {
                v += 256;
            }
            result[i + 1] = v as u8;
        }
    }
    let out: [u8; 32] = result[1..].try_into().expect("32 bytes");
    if out.iter().all(|b| *b == 0) {
        return Err("derived key is zero".to_string());
    }
    Ok(out)
}

fn ge_order(value: &[u8]) -> bool {
    value >= &SECP256K1_ORDER[..]
}

fn less_than_order(value: &[u8; 32]) -> bool {
    !ge_order(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{hex, keccak::keccak256};

    fn key(n: u8) -> [u8; 32] {
        let mut k = [0u8; 32];
        k[31] = n;
        k
    }

    #[test]
    fn public_key_of_one_is_generator() {
        let pubkey = public_key_uncompressed(&key(1)).unwrap();
        assert_eq!(
            hex::encode(&pubkey[1..33]),
            "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        );
        let compressed = public_key_compressed(&key(1)).unwrap();
        assert_eq!(
            hex::encode(&compressed),
            "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        );
    }

    #[test]
    fn address_of_private_key_one() {
        let pubkey = public_key_uncompressed(&key(1)).unwrap();
        let hash = keccak256(&pubkey[1..]);
        assert_eq!(
            hex::encode(&hash[12..]),
            "7e5f4552091a69125d5dfcb7b8c2659029395bdf"
        );
    }

    #[test]
    fn invalid_private_keys_rejected() {
        assert!(!is_valid_private_key(&[0u8; 32]));
        assert!(!is_valid_private_key(&SECP256K1_ORDER));
        assert!(!is_valid_private_key(&[0xffu8; 32]));
        assert!(is_valid_private_key(&key(1)));
        assert!(public_key_uncompressed(&[0u8; 32]).is_err());
    }

    #[test]
    fn signature_is_deterministic_and_recoverable() {
        let digest = keccak256(b"message");
        let (sig, recid) = sign_recoverable(&key(1), &digest).unwrap();
        assert_eq!((sig, recid), sign_recoverable(&key(1), &digest).unwrap());
        assert!(recid <= 1);
        let recovered = recover_public_key(&digest, &sig, recid).unwrap();
        assert_eq!(recovered, public_key_uncompressed(&key(1)).unwrap());
    }

    #[test]
    fn signature_is_low_s() {
        let half_order = {
            // n / 2, floor
            let mut out = [0u8; 32];
            let mut carry = 0u16;
            for i in 0..32 {
                let v = (carry << 8) | u16::from(SECP256K1_ORDER[i]);
                out[i] = (v / 2) as u8;
                carry = v % 2;
            }
            out
        };
        for i in 1..20u8 {
            let digest = keccak256(&[i]);
            let (sig, _) = sign_recoverable(&key(i), &digest).unwrap();
            assert!(sig[32..] <= half_order[..], "high-S signature for {i}");
        }
    }

    #[test]
    fn recovery_rejects_bad_id() {
        let digest = [1u8; 32];
        assert!(recover_public_key(&digest, &[1u8; 64], 7).is_err());
    }

    #[test]
    fn add_private_keys_basic_and_wraparound() {
        assert_eq!(add_private_keys(&key(2), &key(3)).unwrap(), key(5));
        let mut n_minus_1 = SECP256K1_ORDER;
        n_minus_1[31] -= 1;
        assert_eq!(add_private_keys(&n_minus_1, &key(2)).unwrap(), key(1));
        assert!(add_private_keys(&n_minus_1, &key(1)).is_err());
    }

    #[test]
    fn add_private_keys_carries_across_bytes() {
        let mut a = [0u8; 32];
        a[31] = 0xff;
        let mut expected = [0u8; 32];
        expected[30] = 1;
        expected[31] = 0x01;
        assert_eq!(add_private_keys(&a, &key(2)).unwrap(), expected);
    }

    #[test]
    fn add_private_keys_rejects_oversized_tweak() {
        assert!(add_private_keys(&key(1), &SECP256K1_ORDER).is_err());
        assert!(add_private_keys(&key(1), &[0xffu8; 32]).is_err());
    }
}
