//! Small text-formatting helpers for the screens.

use agent_saifu_core::blockchain::Blockchain;

/// `0x3c7c…cc21`; short values are returned unchanged.
pub fn short_address(address: &str) -> String {
    let chars: Vec<char> = address.chars().collect();
    if chars.len() <= 14 {
        return address.to_string();
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// `$8,241.80`.
pub fn usd(value: f64) -> String {
    let negative = value < 0.0;
    let cents = (value.abs() * 100.0).round() as u128;
    let whole = (cents / 100).to_string();
    let mut grouped = String::new();
    for (i, ch) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!(
        "{}${}.{:02}",
        if negative { "-" } else { "" },
        grouped,
        cents % 100
    )
}

/// Keeps at most `max_decimals` digits after the point and drops trailing zeros. Works on the
/// decimal text so no precision is lost to floating point.
pub fn trim_amount(amount: &str, max_decimals: usize) -> String {
    let Some((whole, frac)) = amount.split_once('.') else {
        return amount.to_string();
    };
    let frac: String = frac.chars().take(max_decimals).collect();
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{frac}")
    }
}

/// `max_fee_per_gas_gwei` -> `Max fee per gas gwei`.
pub fn humanize_key(key: &str) -> String {
    let spaced = key.replace(['_', '-'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `expires in 4 min`.
pub fn expires_in(seconds: u64) -> String {
    match seconds {
        0 => "expired".to_string(),
        1..=59 => format!("expires in {seconds}s"),
        60..=3599 => format!("expires in {} min", seconds.div_ceil(60)),
        _ => format!("expires in {} h", seconds / 3600),
    }
}

/// Friendly name for common EVM chain IDs.
pub fn chain_label(id: u64) -> String {
    match id {
        1 => "Ethereum".to_string(),
        10 => "Optimism".to_string(),
        56 => "BNB Chain".to_string(),
        137 => "Polygon".to_string(),
        8453 => "Base".to_string(),
        42161 => "Arbitrum".to_string(),
        43114 => "Avalanche".to_string(),
        other => format!("Chain {other}"),
    }
}

pub fn chain_symbol(chain: Blockchain) -> &'static str {
    match chain {
        Blockchain::Ethereum => "ETH",
        Blockchain::Sui => "SUI",
        Blockchain::Solana => "SOL",
        Blockchain::Bitcoin => "BTC",
    }
}

pub fn supports_swaps(chain: Blockchain) -> bool {
    chain != Blockchain::Bitcoin
}

pub fn supports_walletconnect(chain: Blockchain) -> bool {
    chain != Blockchain::Bitcoin
}

/// The networks a new wallet can be bound to (Ethereum wallets choose a chain per call instead).
pub fn network_choices(chain: Blockchain) -> &'static [(&'static str, &'static str)] {
    match chain {
        Blockchain::Ethereum => &[],
        Blockchain::Sui | Blockchain::Solana => &[
            ("mainnet", "Mainnet"),
            ("testnet", "Testnet"),
            ("devnet", "Devnet"),
        ],
        Blockchain::Bitcoin => &[
            ("mainnet", "Mainnet"),
            ("testnet", "Testnet4"),
            ("signet", "Signet"),
        ],
    }
}

/// Whole-number text only; anything else is `None`.
pub fn parse_chain_id(text: &str) -> Option<u64> {
    let t = text.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

/// A positive decimal such as `0.05`; used before anything is sent to the wallet so typos are
/// caught early. The wallet still parses the amount exactly.
pub fn looks_like_amount(text: &str) -> bool {
    let t = text.trim();
    let mut parts = t.splitn(2, '.');
    let whole = parts.next().unwrap_or("");
    let frac = parts.next();
    let digits_ok = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() && frac.is_none_or(str::is_empty) {
        return false;
    }
    if !digits_ok(whole) || !frac.is_none_or(digits_ok) {
        return false;
    }
    t.bytes().any(|b| (b'1'..=b'9').contains(&b))
}

/// Basic length rule shared with the wallet (it enforces the real one).
pub fn password_ok(password: &str) -> bool {
    password.chars().count() >= 8
}

/// How far below the quoted minimum the swap's `min_receive` floor is set. The wallet fetches a
/// fresh quote when it swaps, and prices move between the two, so a floor equal to the quote
/// would refuse swaps over a rounding-sized change. The slippage setting still bounds the price.
pub const MIN_RECEIVE_BUFFER_BPS: u64 = 50;

/// `amount` lowered by `bps` basis points (rounded down), as exact decimal text. `None` when it
/// is not a plain decimal number.
pub fn reduce_by_bps(amount: &str, bps: u64) -> Option<String> {
    let t = amount.trim();
    let (whole, frac) = t.split_once('.').unwrap_or((t, ""));
    let digits_ok = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (whole.is_empty() && frac.is_empty())
        || !digits_ok(whole)
        || !digits_ok(frac)
        || bps > 10_000
        || frac.len() > 30
    {
        return None;
    }
    let scale = 10u128.checked_pow(u32::try_from(frac.len()).ok()?)?;
    let whole: u128 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let frac_value: u128 = if frac.is_empty() {
        0
    } else {
        frac.parse().ok()?
    };
    let units = whole.checked_mul(scale)?.checked_add(frac_value)?;
    let reduced = units.checked_mul(u128::from(10_000 - bps))? / 10_000;
    let (w, f) = (reduced / scale, reduced % scale);
    if frac.is_empty() {
        return Some(w.to_string());
    }
    let f = format!("{f:0>width$}", width = frac.len());
    Some(trim_amount(&format!("{w}.{f}"), frac.len()))
}

/// `1.25` (percent) -> `125` basis points, within the 0.05%..=10% range the wallet allows.
/// Works on the text so `0.07` never turns into 6.99999 bps.
pub fn parse_slippage_bps(percent: &str) -> Option<u64> {
    let t = percent.trim();
    let (whole, frac) = t.split_once('.').unwrap_or((t, ""));
    let digits_ok = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() && frac.is_empty()
        || !digits_ok(whole)
        || !digits_ok(frac)
        || frac.len() > 2
    {
        return None;
    }
    let whole: u64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let frac: u64 = format!("{frac:0<2}").parse().ok()?;
    let bps = whole.checked_mul(100)?.checked_add(frac)?;
    (5..=1000).contains(&bps).then_some(bps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortens_long_addresses_only() {
        assert_eq!(
            short_address("0x3c7cb8F6c7e321CDe057F50Aa3D455D9D3C5cc21"),
            "0x3c7c…cc21"
        );
        assert_eq!(short_address("0x1234"), "0x1234");
        assert_eq!(short_address(""), "");
    }

    #[test]
    fn formats_usd_with_grouping_and_rounding() {
        assert_eq!(usd(0.0), "$0.00");
        assert_eq!(usd(8241.8), "$8,241.80");
        assert_eq!(usd(1234567.891), "$1,234,567.89");
        assert_eq!(usd(999.999), "$1,000.00");
        assert_eq!(usd(-12.5), "-$12.50");
    }

    #[test]
    fn trims_amounts_without_rounding_through_floats() {
        assert_eq!(trim_amount("1.500000", 8), "1.5");
        assert_eq!(trim_amount("2.000", 8), "2");
        assert_eq!(trim_amount("0.000001108999815", 8), "0.0000011");
        assert_eq!(trim_amount("42", 8), "42");
        assert_eq!(trim_amount("0.100000000000000001", 4), "0.1");
    }

    #[test]
    fn humanizes_keys() {
        assert_eq!(humanize_key("max_fee_per_gas_gwei"), "Max fee per gas gwei");
        assert_eq!(humanize_key("chain-id"), "Chain id");
        assert_eq!(humanize_key(""), "");
    }

    #[test]
    fn describes_expiry() {
        assert_eq!(expires_in(0), "expired");
        assert_eq!(expires_in(45), "expires in 45s");
        assert_eq!(expires_in(61), "expires in 2 min");
        assert_eq!(expires_in(300), "expires in 5 min");
        assert_eq!(expires_in(7300), "expires in 2 h");
    }

    #[test]
    fn parses_chain_ids_strictly() {
        assert_eq!(parse_chain_id(" 8453 "), Some(8453));
        assert_eq!(parse_chain_id(""), None);
        assert_eq!(parse_chain_id("-1"), None);
        assert_eq!(parse_chain_id("1.5"), None);
        assert_eq!(parse_chain_id("99999999999999999999999"), None);
    }

    #[test]
    fn validates_amount_shape() {
        for ok in ["1", "0.05", ".5", "10.", "0.000001"] {
            assert!(looks_like_amount(ok), "{ok}");
        }
        for bad in ["", ".", "0", "0.0", "-1", "1e3", "1,5", "abc", "1.2.3", " "] {
            assert!(!looks_like_amount(bad), "{bad}");
        }
    }

    #[test]
    fn lowers_amounts_exactly() {
        assert_eq!(
            reduce_by_bps("2.439293184", 50).as_deref(),
            Some("2.427096718")
        );
        // keeps the quote's own decimals (a token may have none), so this rounds down
        assert_eq!(reduce_by_bps("100", 50).as_deref(), Some("99"));
        assert_eq!(reduce_by_bps("100.0", 50).as_deref(), Some("99.5"));
        assert_eq!(reduce_by_bps("100", 0).as_deref(), Some("100"));
        assert_eq!(reduce_by_bps("1.5", 10_000).as_deref(), Some("0"));
        // rounds down, never up
        assert_eq!(reduce_by_bps("0.000001", 50).as_deref(), Some("0"));
        assert_eq!(reduce_by_bps("0.01", 50).as_deref(), Some("0"));
        assert_eq!(reduce_by_bps(".5", 100).as_deref(), Some("0.4"));
        for bad in ["", ".", "-1", "1e3", "1,5", "abc"] {
            assert_eq!(reduce_by_bps(bad, 50), None, "{bad}");
        }
        assert_eq!(reduce_by_bps("1", 10_001), None);
    }

    #[test]
    fn converts_slippage_percent_to_basis_points() {
        assert_eq!(parse_slippage_bps("1"), Some(100));
        assert_eq!(parse_slippage_bps("0.5"), Some(50));
        assert_eq!(parse_slippage_bps(" 0.07 "), Some(7));
        assert_eq!(parse_slippage_bps("10"), Some(1000));
        assert_eq!(parse_slippage_bps(".25"), Some(25));
        for bad in ["", ".", "0.04", "10.01", "11", "-1", "1.234", "abc", "1,5"] {
            assert_eq!(parse_slippage_bps(bad), None, "{bad}");
        }
    }

    #[test]
    fn labels_chains() {
        assert_eq!(chain_label(1), "Ethereum");
        assert_eq!(chain_label(8453), "Base");
        assert_eq!(chain_label(999999), "Chain 999999");
    }

    #[test]
    fn password_length_counts_characters() {
        assert!(password_ok("12345678"));
        assert!(!password_ok("1234567"));
        assert!(password_ok("pässwörd"));
    }

    #[test]
    fn network_choices_follow_the_chain() {
        assert!(network_choices(Blockchain::Ethereum).is_empty());
        assert_eq!(network_choices(Blockchain::Solana).len(), 3);
        assert_eq!(network_choices(Blockchain::Bitcoin)[1].0, "testnet");
    }
}
