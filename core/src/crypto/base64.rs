//! Standard (RFC 4648) base64 with padding, as used by MetaMask vaults.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn sextet(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some(u32::from(c - b'A')),
        b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Strict decoder: requires correct padding and rejects non-canonical trailing bits.
pub fn decode(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err("base64 length must be a multiple of 4".to_string());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let last_group = bytes.len().saturating_sub(4);
    for (i, group) in bytes.chunks(4).enumerate() {
        let pad = group.iter().rev().take_while(|c| **c == b'=').count();
        if pad > 2 || (pad > 0 && i * 4 != last_group) {
            return Err("invalid base64 padding".to_string());
        }
        let mut n = 0u32;
        for c in &group[..4 - pad] {
            n = (n << 6) | sextet(*c).ok_or("invalid base64 character")?;
        }
        n <<= 6 * pad as u32;
        let triple = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        if pad > 0 && triple[3 - pad..].iter().any(|b| *b != 0) {
            return Err("non-canonical base64".to_string());
        }
        out.extend_from_slice(&triple[..3 - pad]);
    }
    Ok(out)
}

/// URL-safe alphabet without padding (JWT segments).
pub fn encode_url(data: &[u8]) -> String {
    encode(data)
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encoding_has_no_padding_or_unsafe_characters() {
        assert_eq!(encode_url(&[0xfb, 0xff, 0xbf]), "-_-_");
        assert_eq!(encode_url(b"f"), "Zg");
        assert_eq!(encode_url(b"fo"), "Zm8");
        assert_eq!(encode_url(b""), "");
    }

    #[test]
    fn rfc4648_vectors() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, enc) in cases {
            assert_eq!(encode(plain.as_bytes()), enc);
            assert_eq!(decode(enc), Ok(plain.as_bytes().to_vec()));
        }
    }

    #[test]
    fn uses_plus_and_slash() {
        assert_eq!(encode(&[0xfb, 0xff, 0xbf]), "+/+/");
        assert_eq!(decode("+/+/"), Ok(vec![0xfb, 0xff, 0xbf]));
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "Zg", "Zg=", "Z===", "=Zg=", "Zg==Zg==", "Zm9*", "Zh==", "Zm9=", "Zg=a",
        ] {
            assert!(decode(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn round_trip_all_lengths() {
        for len in 0..40usize {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(decode(&encode(&data)), Ok(data));
        }
    }
}
