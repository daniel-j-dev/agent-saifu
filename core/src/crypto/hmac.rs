//! HMAC (RFC 2104), generic over the hash.

use super::sha2::{Digest, Sha256, Sha512};

/// HMAC with precomputed inner/outer states so it can be cloned cheaply
/// (this is what makes PBKDF2 with many iterations fast).
#[derive(Clone)]
pub struct Hmac<D: Digest> {
    inner: D,
    outer: D,
}

impl<D: Digest> Hmac<D> {
    pub fn new(key: &[u8]) -> Self {
        let mut block = vec![0u8; D::BLOCK_SIZE];
        if key.len() > D::BLOCK_SIZE {
            let mut h = D::new();
            h.update(key);
            let digest = h.finalize();
            block[..digest.len()].copy_from_slice(&digest);
        } else {
            block[..key.len()].copy_from_slice(key);
        }
        let ipad: Vec<u8> = block.iter().map(|b| b ^ 0x36).collect();
        let opad: Vec<u8> = block.iter().map(|b| b ^ 0x5c).collect();
        super::secret::zeroize(&mut block);
        let mut inner = D::new();
        inner.update(&ipad);
        let mut outer = D::new();
        outer.update(&opad);
        Hmac { inner, outer }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    pub fn finalize(self) -> Vec<u8> {
        let inner_digest = self.inner.finalize();
        let mut outer = self.outer;
        outer.update(&inner_digest);
        outer.finalize()
    }
}

fn mac<D: Digest>(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut h = Hmac::<D>::new(key);
    h.update(data);
    h.finalize()
}

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    mac::<Sha256>(key, data).try_into().expect("32-byte mac")
}

pub fn hmac_sha512(key: &[u8], data: &[u8]) -> [u8; 64] {
    mac::<Sha512>(key, data).try_into().expect("64-byte mac")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    #[test]
    fn rfc4231_case_1() {
        let key = [0x0bu8; 20];
        assert_eq!(
            hex::encode(&hmac_sha256(&key, b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex::encode(&hmac_sha512(&key, b"Hi There")),
            "87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cde\
             daa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854"
        );
    }

    #[test]
    fn rfc4231_case_2() {
        assert_eq!(
            hex::encode(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn rfc4231_long_key_is_hashed() {
        let key = [0xaau8; 131];
        assert_eq!(
            hex::encode(&hmac_sha256(
                &key,
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        assert_eq!(
            hex::encode(&hmac_sha512(
                &key,
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "80b24263c7c1a3ebb71493c1dd7be8b49b46d1f41b4aeec1121b013783f8f352\
             6b56d037e05f2598bd0fd2215d6a1e5295e64f73f63f0aec8b915a985d786598"
        );
    }

    #[test]
    fn cloned_state_is_independent() {
        let base = Hmac::<Sha256>::new(b"key");
        let mut a = base.clone();
        a.update(b"one");
        let mut b = base.clone();
        b.update(b"two");
        assert_ne!(a.finalize(), b.finalize());
        let mut c = base;
        c.update(b"one");
        assert_eq!(c.finalize(), hmac_sha256(b"key", b"one").to_vec());
    }

    #[test]
    fn empty_key_and_message() {
        assert_eq!(
            hex::encode(&hmac_sha256(b"", b"")),
            "b613679a0814d9ec772f95d778c35fc5ff1697c493715653c6c712144292c5ad"
        );
    }
}
