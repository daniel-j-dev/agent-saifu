//! Coin types, addresses and amounts on Sui.

use sui_sdk_types::{Address, StructTag};

use crate::eth::explorer::sanitize_text;
use crate::eth::u256::U256;

pub const NATIVE: &str = "native";
pub const SUI_DECIMALS: u32 = 9;
pub const MIST_PER_SUI: u64 = 1_000_000_000;
pub const SUI_COIN_TYPE: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000002::sui::SUI";

/// Sui addresses are 32 bytes. Short forms such as `0x2` are valid on-chain but almost always a
/// typo when sending funds, so recipients must be written out in full.
pub fn parse_address(input: &str) -> Result<Address, String> {
    let text = input.trim();
    let digits = text
        .strip_prefix("0x")
        .ok_or("a Sui address starts with 0x")?;
    if digits.len() != 64 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("a Sui address is 0x followed by 64 hex characters".to_string());
    }
    text.parse::<Address>()
        .map_err(|_| "not a valid Sui address".to_string())
}

pub fn format_address(address: &Address) -> String {
    address.to_string()
}

/// `native` or `sui` mean SUI; anything else must be a struct tag such as
/// `0xdba3...::usdc::USDC`. The result is the canonical long form Sui nodes report.
pub fn normalize_coin_type(input: &str) -> Result<String, String> {
    let text = input.trim();
    if text.eq_ignore_ascii_case(NATIVE) || text.eq_ignore_ascii_case("sui") {
        return Ok(SUI_COIN_TYPE.to_string());
    }
    if text.len() > 512 {
        return Err("coin type is too long".to_string());
    }
    let tag: StructTag = text.parse().map_err(|_| {
        "coin type must be 'native' or a type like 0x2::sui::SUI (package::module::NAME)"
            .to_string()
    })?;
    Ok(tag.to_string())
}

pub fn is_sui(coin_type: &str) -> bool {
    coin_type == SUI_COIN_TYPE
}

/// The coin's own name from its type, e.g. `USDC`; used when no metadata is available.
pub fn fallback_symbol(coin_type: &str) -> String {
    let name = coin_type
        .split('<')
        .next()
        .unwrap_or(coin_type)
        .rsplit("::")
        .next()
        .unwrap_or(coin_type);
    sanitize_text(name)
}

/// Parses a whole-unit amount such as `1.5` into base units. Zero is refused.
pub fn parse_amount(amount: &str, decimals: u32) -> Result<u64, String> {
    let units = U256::parse_units(amount.trim(), decimals)?;
    let raw = units
        .to_u64()
        .ok_or("amount is larger than any Sui coin balance can be")?;
    if raw == 0 {
        return Err("amount must be greater than zero".to_string());
    }
    Ok(raw)
}

pub fn format_amount(raw: u128, decimals: u32) -> String {
    U256::from_u128(raw).format_units(decimals)
}

#[cfg(test)]
mod tests {
    use super::*;

    const USDC: &str =
        "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC";

    #[test]
    fn native_and_sui_mean_the_sui_coin() {
        for s in ["native", " NATIVE ", "sui", "SUI"] {
            assert_eq!(normalize_coin_type(s).unwrap(), SUI_COIN_TYPE, "{s}");
        }
        assert!(is_sui(SUI_COIN_TYPE));
    }

    #[test]
    fn short_sui_type_is_expanded_to_the_canonical_form() {
        assert_eq!(normalize_coin_type("0x2::sui::SUI").unwrap(), SUI_COIN_TYPE);
    }

    #[test]
    fn other_coin_types_are_kept_in_canonical_form() {
        let normalized = normalize_coin_type(USDC).unwrap();
        assert_eq!(normalized, USDC);
        assert_eq!(normalize_coin_type(&normalized).unwrap(), normalized);
        assert_eq!(fallback_symbol(&normalized), "USDC");
    }

    #[test]
    fn bad_coin_types_are_rejected() {
        for s in [
            "",
            "usdc",
            "0x2::sui",
            "0x2::sui::SUI::X",
            "0xZZ::a::B",
            "::a::B",
        ] {
            assert!(normalize_coin_type(s).is_err(), "{s:?}");
        }
        assert!(normalize_coin_type(&format!("0x2::a::{}", "B".repeat(600))).is_err());
    }

    #[test]
    fn addresses_must_be_written_out_in_full() {
        let ok = format!("0x{}", "ab".repeat(32));
        assert_eq!(format_address(&parse_address(&ok).unwrap()), ok);
        assert!(parse_address(&format!("  {ok} ")).is_ok());
        for bad in [
            "0x2",
            "",
            &ok[2..],
            &format!("0x{}", "ab".repeat(31)),
            &format!("0x{}", "zz".repeat(32)),
            &format!("0x{}", "ab".repeat(33)),
        ] {
            assert!(parse_address(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn amounts_use_the_coins_decimals() {
        assert_eq!(parse_amount("1", 9).unwrap(), 1_000_000_000);
        assert_eq!(parse_amount("0.000000001", 9).unwrap(), 1);
        assert_eq!(parse_amount("2.5", 6).unwrap(), 2_500_000);
        assert!(parse_amount("0", 9).is_err());
        assert!(parse_amount("0.0000000001", 9).is_err());
        assert!(parse_amount("-1", 9).is_err());
        assert!(parse_amount("abc", 9).is_err());
        assert!(parse_amount("99999999999999999999", 9).is_err());
    }

    #[test]
    fn amounts_format_back() {
        assert_eq!(format_amount(1_500_000_000, 9), "1.5");
        assert_eq!(format_amount(0, 9), "0");
        assert_eq!(
            format_amount(u128::from(u64::MAX) * 2, 0),
            "36893488147419103230"
        );
    }

    #[test]
    fn fallback_symbol_is_clean() {
        assert_eq!(fallback_symbol("0x1::m::A\u{7}B"), "AB");
        assert_eq!(fallback_symbol("0x1::lp::LP<0x2::a::B, 0x3::c::D>"), "LP");
    }
}
