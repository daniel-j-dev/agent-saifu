//! Hex encoding and decoding.

const DIGITS: &[u8; 16] = b"0123456789abcdef";

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

/// Encodes with a leading `0x`.
pub fn encode_prefixed(bytes: &[u8]) -> String {
    format!("0x{}", encode(bytes))
}

/// Removes one optional `0x` / `0X` prefix.
pub fn strip_prefix(s: &str) -> &str {
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s)
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Decodes a hex string (optional `0x` prefix). Errors on odd length or bad digits.
pub fn decode(s: &str) -> Result<Vec<u8>, String> {
    let s = strip_prefix(s);
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err("hex string has odd length".to_string());
    }
    bytes
        .chunks(2)
        .map(|pair| match (nibble(pair[0]), nibble(pair[1])) {
            (Some(h), Some(l)) => Ok((h << 4) | l),
            _ => Err("invalid hex digit".to_string()),
        })
        .collect()
}

/// Decodes hex that must be exactly `N` bytes long.
pub fn decode_fixed<const N: usize>(s: &str) -> Result<[u8; N], String> {
    let bytes = decode(s)?;
    <[u8; N]>::try_from(bytes.as_slice()).map_err(|_| format!("expected {N} bytes of hex"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes() {
        assert_eq!(encode(&[]), "");
        assert_eq!(encode(&[0x00, 0xff, 0x1a]), "00ff1a");
        assert_eq!(encode_prefixed(&[0xab]), "0xab");
    }

    #[test]
    fn decodes_with_and_without_prefix_and_case() {
        assert_eq!(decode("00ff1A"), Ok(vec![0, 255, 26]));
        assert_eq!(decode("0xAbCd"), Ok(vec![0xab, 0xcd]));
        assert_eq!(decode("0XAB"), Ok(vec![0xab]));
        assert_eq!(decode(""), Ok(vec![]));
        assert_eq!(decode("0x"), Ok(vec![]));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(decode("abc").is_err());
        assert!(decode("zz").is_err());
        assert!(decode("0x0x00").is_err());
        assert!(decode("é1").is_err());
    }

    #[test]
    fn fixed_length() {
        assert_eq!(decode_fixed::<2>("0102"), Ok([1, 2]));
        assert!(decode_fixed::<2>("010203").is_err());
        assert!(decode_fixed::<2>("01").is_err());
    }

    #[test]
    fn round_trip_all_bytes() {
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&all)), Ok(all));
    }
}
