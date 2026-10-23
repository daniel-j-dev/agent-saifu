//! RLP encoding (encode-only; transactions are built, never parsed).

use super::u256::U256;

fn length_prefix(len: usize, short_base: u8, long_base: u8) -> Vec<u8> {
    if len < 56 {
        return vec![short_base + len as u8];
    }
    let len_bytes = U256::from_u64(len as u64).to_be_bytes_minimal();
    let mut out = vec![long_base + len_bytes.len() as u8];
    out.extend(len_bytes);
    out
}

pub fn encode_bytes(data: &[u8]) -> Vec<u8> {
    if data.len() == 1 && data[0] < 0x80 {
        return vec![data[0]];
    }
    let mut out = length_prefix(data.len(), 0x80, 0xb7);
    out.extend_from_slice(data);
    out
}

/// `items` must already be RLP-encoded.
pub fn encode_list(items: &[Vec<u8>]) -> Vec<u8> {
    let payload: Vec<u8> = items.iter().flatten().copied().collect();
    let mut out = length_prefix(payload.len(), 0xc0, 0xf7);
    out.extend(payload);
    out
}

pub fn encode_u64(v: u64) -> Vec<u8> {
    encode_u256(&U256::from_u64(v))
}

pub fn encode_u256(v: &U256) -> Vec<u8> {
    encode_bytes(&v.to_be_bytes_minimal())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    #[test]
    fn official_string_vectors() {
        assert_eq!(hex::encode(&encode_bytes(b"dog")), "83646f67");
        assert_eq!(hex::encode(&encode_bytes(b"")), "80");
        assert_eq!(hex::encode(&encode_bytes(&[0x00])), "00");
        assert_eq!(hex::encode(&encode_bytes(&[0x0f])), "0f");
        assert_eq!(hex::encode(&encode_bytes(&[0x80])), "8180");
    }

    #[test]
    fn official_list_vectors() {
        assert_eq!(hex::encode(&encode_list(&[])), "c0");
        assert_eq!(
            hex::encode(&encode_list(&[encode_bytes(b"cat"), encode_bytes(b"dog")])),
            "c88363617483646f67"
        );
        // [ [], [[]], [ [], [[]] ] ]
        let a = encode_list(&[]);
        let b = encode_list(std::slice::from_ref(&a));
        let c = encode_list(&[a.clone(), b.clone()]);
        assert_eq!(hex::encode(&encode_list(&[a, b, c])), "c7c0c1c0c3c0c1c0");
    }

    #[test]
    fn official_integer_vectors() {
        assert_eq!(hex::encode(&encode_u64(0)), "80");
        assert_eq!(hex::encode(&encode_u64(15)), "0f");
        assert_eq!(hex::encode(&encode_u64(1024)), "820400");
        assert_eq!(hex::encode(&encode_u256(&U256::MAX)[..3]), "a0ffff");
    }

    #[test]
    fn long_string_uses_long_form() {
        let text = b"Lorem ipsum dolor sit amet, consectetur adipisicing elit";
        let enc = encode_bytes(text);
        assert_eq!(&enc[..2], &[0xb8, 0x38]);
        assert_eq!(&enc[2..], text);
    }

    #[test]
    fn long_list_and_two_byte_length() {
        let item = encode_bytes(&[7u8; 200]);
        let list = encode_list(&[item.clone(), item]);
        assert_eq!(list[0], 0xf9);
        assert_eq!(
            usize::from(list[1]) * 256 + usize::from(list[2]),
            list.len() - 3
        );
        let big = encode_bytes(&vec![1u8; 1024]);
        assert_eq!(&big[..3], &[0xb9, 0x04, 0x00]);
    }

    #[test]
    fn boundary_55_and_56_bytes() {
        assert_eq!(encode_bytes(&[1u8; 55])[0], 0xb7);
        assert_eq!(&encode_bytes(&[1u8; 56])[..2], &[0xb8, 56]);
    }
}
