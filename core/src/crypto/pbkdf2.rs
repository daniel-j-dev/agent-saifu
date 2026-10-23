//! PBKDF2 (RFC 8018), generic over the HMAC hash.

use super::hmac::Hmac;
use super::sha2::{Digest, Sha256, Sha512};

/// Derives `out_len` bytes. `iterations` must be at least 1.
pub fn pbkdf2<D: Digest>(
    password: &[u8],
    salt: &[u8],
    iterations: u32,
    out_len: usize,
) -> Result<Vec<u8>, String> {
    if iterations == 0 {
        return Err("PBKDF2 iterations must be at least 1".to_string());
    }
    let prf = Hmac::<D>::new(password);
    let mut out = Vec::with_capacity(out_len);
    let mut block_index: u32 = 1;
    while out.len() < out_len {
        let mut first = prf.clone();
        first.update(salt);
        first.update(&block_index.to_be_bytes());
        let mut u = first.finalize();
        let mut t = u.clone();
        for _ in 1..iterations {
            let mut next = prf.clone();
            next.update(&u);
            u = next.finalize();
            for (acc, byte) in t.iter_mut().zip(&u) {
                *acc ^= byte;
            }
        }
        let take = (out_len - out.len()).min(t.len());
        out.extend_from_slice(&t[..take]);
        block_index = block_index
            .checked_add(1)
            .ok_or_else(|| "PBKDF2 output too long".to_string())?;
    }
    Ok(out)
}

pub fn pbkdf2_sha256(
    password: &[u8],
    salt: &[u8],
    iterations: u32,
    out_len: usize,
) -> Result<Vec<u8>, String> {
    pbkdf2::<Sha256>(password, salt, iterations, out_len)
}

pub fn pbkdf2_sha512(
    password: &[u8],
    salt: &[u8],
    iterations: u32,
    out_len: usize,
) -> Result<Vec<u8>, String> {
    pbkdf2::<Sha512>(password, salt, iterations, out_len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    #[test]
    fn rfc7914_sha256_vectors() {
        assert_eq!(
            hex::encode(&pbkdf2_sha256(b"password", b"salt", 1, 32).unwrap()),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        assert_eq!(
            hex::encode(&pbkdf2_sha256(b"password", b"salt", 2, 32).unwrap()),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
        );
        assert_eq!(
            hex::encode(&pbkdf2_sha256(b"password", b"salt", 4096, 32).unwrap()),
            "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"
        );
    }

    #[test]
    fn multi_block_output() {
        let out = pbkdf2_sha256(
            b"passwordPASSWORDpassword",
            b"saltSALTsaltSALTsaltSALTsaltSALTsalt",
            4096,
            40,
        )
        .unwrap();
        assert_eq!(
            hex::encode(&out),
            "348c89dbcbd32b2f32d814b8116e84cf2b17347ebc1800181c4e2a1fb8dd53e1c635518c7dac47e9"
        );
    }

    #[test]
    fn sha512_vector() {
        assert_eq!(
            hex::encode(&pbkdf2_sha512(b"password", b"salt", 1, 64).unwrap()),
            "867f70cf1ade02cff3752599a3a53dc4af34c7a669815ae5d513554e1c8cf252\
             c02d470a285a0501bad999bfe943c08f050235d7d68b1da55e63f73b60a57fce"
        );
    }

    #[test]
    fn output_is_prefix_consistent() {
        let long = pbkdf2_sha256(b"pw", b"salt", 3, 70).unwrap();
        let short = pbkdf2_sha256(b"pw", b"salt", 3, 10).unwrap();
        assert_eq!(&long[..10], &short[..]);
        assert_eq!(long.len(), 70);
    }

    #[test]
    fn zero_iterations_rejected() {
        assert!(pbkdf2_sha256(b"pw", b"salt", 0, 32).is_err());
    }

    #[test]
    fn zero_length_output() {
        assert_eq!(
            pbkdf2_sha256(b"pw", b"salt", 1, 0).unwrap(),
            Vec::<u8>::new()
        );
    }
}
