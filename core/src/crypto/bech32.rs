//! Bech32 and Bech32m (BIP 173, BIP 350): the checksummed encoding of SegWit addresses.

const CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const BECH32_CONSTANT: u32 = 1;
const BECH32M_CONSTANT: u32 = 0x2bc8_30a3;
const CHECKSUM_LEN: usize = 6;
/// BIP 173's limit for the whole string.
pub const MAX_LEN: usize = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Bech32,
    Bech32m,
}

impl Variant {
    fn constant(self) -> u32 {
        match self {
            Variant::Bech32 => BECH32_CONSTANT,
            Variant::Bech32m => BECH32M_CONSTANT,
        }
    }
}

fn polymod(values: &[u8]) -> u32 {
    const GENERATOR: [u32; 5] = [
        0x3b6a_57b2,
        0x2650_8e6d,
        0x1ea1_19fa,
        0x3d42_33dd,
        0x2a14_62b3,
    ];
    let mut chk: u32 = 1;
    for v in values {
        let top = chk >> 25;
        chk = ((chk & 0x01ff_ffff) << 5) ^ u32::from(*v);
        for (i, g) in GENERATOR.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                chk ^= g;
            }
        }
    }
    chk
}

fn expand_hrp(hrp: &str) -> Vec<u8> {
    let mut out: Vec<u8> = hrp.bytes().map(|b| b >> 5).collect();
    out.push(0);
    out.extend(hrp.bytes().map(|b| b & 0x1f));
    out
}

fn checksum(hrp: &str, data: &[u8], variant: Variant) -> [u8; CHECKSUM_LEN] {
    let mut values = expand_hrp(hrp);
    values.extend_from_slice(data);
    values.extend_from_slice(&[0u8; CHECKSUM_LEN]);
    let modulus = polymod(&values) ^ variant.constant();
    let mut out = [0u8; CHECKSUM_LEN];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = ((modulus >> (5 * (CHECKSUM_LEN - 1 - i))) & 0x1f) as u8;
    }
    out
}

/// Encodes 5-bit `data` values under a lower-case human-readable part.
pub fn encode(hrp: &str, data: &[u8], variant: Variant) -> Result<String, String> {
    if hrp.is_empty()
        || !hrp
            .bytes()
            .all(|b| (33..=126).contains(&b) && !b.is_ascii_uppercase())
    {
        return Err("invalid bech32 prefix".to_string());
    }
    if data.iter().any(|v| *v > 31) {
        return Err("bech32 data values must be 5 bits".to_string());
    }
    if hrp.len() + 1 + data.len() + CHECKSUM_LEN > MAX_LEN {
        return Err("bech32 text is too long".to_string());
    }
    let mut out = String::from(hrp);
    out.push('1');
    for v in data.iter().copied().chain(checksum(hrp, data, variant)) {
        out.push(CHARSET[v as usize] as char);
    }
    Ok(out)
}

/// Decodes and verifies the checksum. Mixed-case text is refused; the result is lower case.
/// Returns the human-readable part, the 5-bit data (checksum removed) and which checksum matched.
pub fn decode(text: &str) -> Result<(String, Vec<u8>, Variant), String> {
    if text.len() > MAX_LEN {
        return Err("bech32 text is too long".to_string());
    }
    if text.bytes().any(|b| !(33..=126).contains(&b)) {
        return Err("bech32 text contains an invalid character".to_string());
    }
    let has_lower = text.bytes().any(|b| b.is_ascii_lowercase());
    let has_upper = text.bytes().any(|b| b.is_ascii_uppercase());
    if has_lower && has_upper {
        return Err("bech32 text mixes upper and lower case".to_string());
    }
    let text = text.to_ascii_lowercase();
    let separator = text.rfind('1').ok_or("bech32 text has no separator")?;
    if separator == 0 || text.len() - separator - 1 < CHECKSUM_LEN {
        return Err("bech32 text is too short".to_string());
    }
    let (hrp, rest) = (&text[..separator], &text[separator + 1..]);
    let data = rest
        .bytes()
        .map(|c| {
            CHARSET
                .iter()
                .position(|x| *x == c)
                .map(|p| p as u8)
                .ok_or_else(|| "bech32 text contains an invalid character".to_string())
        })
        .collect::<Result<Vec<u8>, String>>()?;
    let mut values = expand_hrp(hrp);
    values.extend_from_slice(&data);
    let variant = match polymod(&values) {
        BECH32_CONSTANT => Variant::Bech32,
        BECH32M_CONSTANT => Variant::Bech32m,
        _ => return Err("bech32 checksum is wrong (a character was mistyped)".to_string()),
    };
    let payload = data[..data.len() - CHECKSUM_LEN].to_vec();
    Ok((hrp.to_string(), payload, variant))
}

/// Regroups bits, e.g. 8-bit bytes into 5-bit values (`pad = true`) and back (`pad = false`,
/// which refuses non-zero or oversized padding).
pub fn convert_bits(data: &[u8], from: u32, to: u32, pad: bool) -> Result<Vec<u8>, String> {
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let max_value: u32 = (1 << to) - 1;
    let mut out = vec![];
    for value in data {
        if u32::from(*value) >> from != 0 {
            return Err("value does not fit the source bit width".to_string());
        }
        acc = (acc << from) | u32::from(*value);
        bits += from;
        while bits >= to {
            bits -= to;
            out.push(((acc >> bits) & max_value) as u8);
        }
        acc &= (1 << bits) - 1;
    }
    if pad {
        if bits > 0 {
            out.push(((acc << (to - bits)) & max_value) as u8);
        }
    } else if bits >= from || (acc << (to - bits)) & max_value != 0 {
        return Err("invalid padding".to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    #[test]
    fn valid_bech32_strings_from_bip173() {
        for text in [
            "A12UEL5L",
            "a12uel5l",
            "an83characterlonghumanreadablepartthatcontainsthenumber1andtheexcludedcharactersbio1tt5tgs",
            "abcdef1qpzry9x8gf2tvdw0s3jn54khce6mua7lmqqqxw",
            "split1checkupstagehandshakeupstreamerranterredcaperred2y9e3w",
        ] {
            let (_, _, variant) = decode(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(variant, Variant::Bech32, "{text}");
        }
        // The longest allowed string: prefix "1" and 82 data characters.
        let longest = format!("11{}c8247j", "q".repeat(82));
        assert_eq!(longest.len(), MAX_LEN);
        assert!(decode(&longest).is_ok());
    }

    #[test]
    fn valid_bech32m_strings_from_bip350() {
        for text in [
            "A1LQFN3A",
            "a1lqfn3a",
            "abcdef1l7aum6echk45nj3s0wdvt2fg8x9yrzpqzd3ryx",
            "split1checkupstagehandshakeupstreamerranterredcaperredlc445v",
            "?1v759aa",
        ] {
            let (_, _, variant) = decode(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(variant, Variant::Bech32m, "{text}");
        }
    }

    #[test]
    fn invalid_strings_are_refused() {
        for text in [
            "\u{20}1nwldj5",
            "\u{7f}1axkwrx",
            "an84characterslonghumanreadablepartthatcontainsthenumber1andtheexcludedcharactersbio1569pvx",
            "pzry9x0s0muk",
            "1pzry9x0s0muk",
            "x1b4n0q5v",
            "li1dgmt3",
            "de1lg7wt\u{ff}",
            "A1G7SGD8",
            "10a06t8",
            "1qzzfhee",
            "A12uEL5L",
        ] {
            assert!(decode(text).is_err(), "{text:?} should be refused");
        }
    }

    #[test]
    fn encode_round_trips_and_checks_input() {
        let data = [
            0u8, 14, 20, 15, 7, 13, 26, 0, 25, 18, 6, 11, 13, 8, 21, 4, 20, 3, 17, 2,
        ];
        for variant in [Variant::Bech32, Variant::Bech32m] {
            let text = encode("bc", &data, variant).unwrap();
            assert_eq!(
                decode(&text).unwrap(),
                ("bc".to_string(), data.to_vec(), variant)
            );
        }
        assert!(encode("", &data, Variant::Bech32).is_err());
        assert!(encode("BC", &data, Variant::Bech32).is_err());
        assert!(encode("bc", &[32], Variant::Bech32).is_err());
        assert!(encode("bc", &[0u8; 90], Variant::Bech32).is_err());
    }

    #[test]
    fn upper_case_text_decodes_to_lower_case() {
        let (hrp, _, _) = decode("A12UEL5L").unwrap();
        assert_eq!(hrp, "a");
    }

    #[test]
    fn convert_bits_round_trips() {
        let bytes = hex::decode("751e76e8199196d454941c45d1b3a323f1433bd6").unwrap();
        let five = convert_bits(&bytes, 8, 5, true).unwrap();
        assert_eq!(five.len(), 32);
        assert_eq!(convert_bits(&five, 5, 8, false).unwrap(), bytes);
    }

    #[test]
    fn convert_bits_refuses_bad_padding_and_values() {
        assert!(convert_bits(&[32], 5, 8, false).is_err());
        assert_eq!(convert_bits(&[0, 0], 5, 8, false).unwrap(), vec![0]);
        // Two groups leave two padding bits, which must be zero.
        assert!(convert_bits(&[0, 1], 5, 8, false).is_err());
        // Three groups leave seven padding bits: a whole group or more is refused.
        assert!(convert_bits(&[0, 0, 0], 5, 8, false).is_err());
    }
}
