//! Ethereum addresses and EIP-55 checksums.

use crate::crypto::{hex, keccak::keccak256, secp256k1};

pub type Address = [u8; 20];

pub fn address_from_private_key(private_key: &[u8; 32]) -> Result<Address, String> {
    let public = secp256k1::public_key_uncompressed(private_key)?;
    let hash = keccak256(&public[1..]);
    let mut out = [0u8; 20];
    out.copy_from_slice(&hash[12..]);
    Ok(out)
}

/// EIP-55 mixed-case encoding, with `0x` prefix.
pub fn to_checksum(address: &Address) -> String {
    let lower = hex::encode(address);
    let hash = keccak256(lower.as_bytes());
    let mut out = String::with_capacity(42);
    out.push_str("0x");
    for (i, c) in lower.chars().enumerate() {
        let nibble = if i % 2 == 0 {
            hash[i / 2] >> 4
        } else {
            hash[i / 2] & 0x0f
        };
        if c.is_ascii_alphabetic() && nibble >= 8 {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Parses `0x` + 40 hex digits. Mixed-case input must carry a valid EIP-55 checksum.
pub fn parse_address(s: &str) -> Result<Address, String> {
    let body = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .ok_or("address must start with 0x")?;
    if body.len() != 40 {
        return Err("address must be 40 hex characters".to_string());
    }
    let address =
        hex::decode_fixed::<20>(body).map_err(|_| "address contains invalid hex".to_string())?;
    let has_upper = body.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = body.chars().any(|c| c.is_ascii_lowercase());
    if has_upper && has_lower && to_checksum(&address)[2..] != *body {
        return Err("address has an invalid EIP-55 checksum".to_string());
    }
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EIP55: [&str; 4] = [
        "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
        "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359",
        "0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB",
        "0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb",
    ];

    #[test]
    fn eip55_vectors_round_trip() {
        for s in EIP55 {
            let addr = parse_address(s).unwrap();
            assert_eq!(to_checksum(&addr), s);
            assert_eq!(parse_address(&s.to_lowercase()).unwrap(), addr);
        }
    }

    #[test]
    fn bad_checksum_is_rejected() {
        let bad = "0x5AAeb6053F3E94C9b9A09f33669435E7Ef1BeAed";
        assert!(parse_address(bad).unwrap_err().contains("checksum"));
    }

    #[test]
    fn all_upper_or_lower_skips_checksum() {
        let lower = "0xde709f2102306220921060314715629080e2fb77";
        assert!(parse_address(lower).is_ok());
        assert!(parse_address(&format!("0x{}", lower[2..].to_uppercase())).is_ok());
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",
            "0x",
            "0x1234",
            "5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            &format!("0x{}", "g".repeat(40)),
            &format!("0x{}", "a".repeat(41)),
        ] {
            assert!(parse_address(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn derives_known_addresses() {
        let mut key = [0u8; 32];
        key[31] = 1;
        assert_eq!(
            to_checksum(&address_from_private_key(&key).unwrap()),
            "0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf"
        );
        assert!(address_from_private_key(&[0u8; 32]).is_err());
    }
}
