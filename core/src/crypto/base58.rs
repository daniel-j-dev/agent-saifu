//! Base58 (Bitcoin alphabet): `did:key` identifiers, Solana addresses, keys and signatures.

const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

pub fn encode(data: &[u8]) -> String {
    let zeros = data.iter().take_while(|b| **b == 0).count();
    let mut digits: Vec<u8> = vec![];
    for &byte in &data[zeros..] {
        let mut carry = u32::from(byte);
        for d in digits.iter_mut() {
            carry += u32::from(*d) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut out = String::with_capacity(zeros + digits.len());
    out.extend(std::iter::repeat_n('1', zeros));
    out.extend(digits.iter().rev().map(|d| ALPHABET[*d as usize] as char));
    out
}

/// Longest input `decode` accepts; Solana's largest base58 values (64-byte signatures and
/// keypairs) are 88 characters.
pub const MAX_DECODE_CHARS: usize = 2048;

/// Inverse of `encode`: leading `1`s become zero bytes. Rejects any character outside the alphabet.
pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    if text.len() > MAX_DECODE_CHARS {
        return Err("base58 text is too long".to_string());
    }
    let zeros = text.bytes().take_while(|b| *b == b'1').count();
    let mut bytes: Vec<u8> = vec![];
    for c in text.bytes().skip(zeros) {
        let value = ALPHABET
            .iter()
            .position(|a| *a == c)
            .ok_or("invalid base58 character")?;
        let mut carry = value as u32;
        for b in bytes.iter_mut() {
            carry += u32::from(*b) * 58;
            *b = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let mut out = vec![0u8; zeros];
    out.extend(bytes.iter().rev());
    Ok(out)
}

/// Decodes to exactly `N` bytes.
pub fn decode_fixed<const N: usize>(text: &str) -> Result<[u8; N], String> {
    let bytes = decode(text)?;
    <[u8; N]>::try_from(bytes.as_slice()).map_err(|_| format!("expected {N} bytes of base58 data"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"hello world"), "StV1DL6CwTryKyV");
        assert_eq!(encode(&[0, 0, 0x28, 0x7f, 0xb4, 0xcd]), "11233QC4");
        assert_eq!(encode(&[0]), "1");
        assert_eq!(encode(&[0x61]), "2g");
    }

    #[test]
    fn decode_inverts_encode() {
        for data in [
            &b""[..],
            b"hello world",
            &[0, 0, 0x28, 0x7f, 0xb4, 0xcd],
            &[0],
            &[0, 0],
            &[0x61],
            &[0xff; 32],
            &[0, 0xff, 0, 1],
        ] {
            assert_eq!(decode(&encode(data)).unwrap(), data);
        }
        assert_eq!(decode("StV1DL6CwTryKyV").unwrap(), b"hello world");
        assert_eq!(decode("11233QC4").unwrap(), [0, 0, 0x28, 0x7f, 0xb4, 0xcd]);
    }

    #[test]
    fn decode_rejects_bad_text() {
        for bad in ["0", "O", "I", "l", "abc+", "a b", "é"] {
            assert!(decode(bad).is_err(), "{bad}");
        }
        assert!(decode(&"2".repeat(MAX_DECODE_CHARS + 1)).is_err());
    }

    #[test]
    fn decode_fixed_checks_the_length() {
        let key = [7u8; 32];
        assert_eq!(decode_fixed::<32>(&encode(&key)).unwrap(), key);
        assert!(decode_fixed::<32>(&encode(&[7u8; 31])).is_err());
        assert!(decode_fixed::<32>(&encode(&[7u8; 33])).is_err());
    }
}
