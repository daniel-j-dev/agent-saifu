//! Bitcoin amounts and fee rates: exact text <-> integer conversions, with no floating point
//! anywhere an amount that is sent is concerned.

pub const SATS_PER_BTC: u64 = 100_000_000;
/// The most Bitcoin that can ever exist: 21 million coins.
pub const MAX_MONEY: u64 = 21_000_000 * SATS_PER_BTC;

/// Reads a decimal amount of bitcoin ("0.00012", "1.5", "2") into satoshis. At most eight
/// decimals, no signs, exponents or separators, greater than zero, and at most 21 million.
pub fn parse_btc(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let (whole, fraction) = match text.split_once('.') {
        Some((w, f)) => (w, f),
        None => (text, ""),
    };
    let digits_only = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (whole.is_empty() && fraction.is_empty())
        || !digits_only(whole)
        || !digits_only(fraction)
        || (text.contains('.') && fraction.is_empty())
    {
        return Err(format!(
            "'{}' is not an amount; write it in whole bitcoin, e.g. \"0.0015\"",
            crate::eth::explorer::sanitize_text_max(text, 40)
        ));
    }
    if fraction.len() > 8 {
        return Err(
            "a Bitcoin amount has at most 8 decimal places (1 satoshi = 0.00000001)".to_string(),
        );
    }
    let whole_sats = if whole.is_empty() {
        0
    } else {
        whole
            .parse::<u64>()
            .ok()
            .and_then(|w| w.checked_mul(SATS_PER_BTC))
            .ok_or("amount is too large")?
    };
    let padded = format!("{fraction:0<8}");
    let fraction_sats: u64 = padded.parse().map_err(|_| "amount is not a number")?;
    let total = whole_sats
        .checked_add(fraction_sats)
        .ok_or("amount is too large")?;
    if total == 0 {
        return Err("amount must be greater than zero".to_string());
    }
    if total > MAX_MONEY {
        return Err("amount is more than the 21 million bitcoin that can exist".to_string());
    }
    Ok(total)
}

/// Satoshis as bitcoin text without trailing zeros: `150000` -> `"0.0015"`, `0` -> `"0"`.
pub fn format_btc(sats: u64) -> String {
    let whole = sats / SATS_PER_BTC;
    let fraction = sats % SATS_PER_BTC;
    if fraction == 0 {
        return whole.to_string();
    }
    let digits = format!("{fraction:08}");
    format!("{whole}.{}", digits.trim_end_matches('0'))
}

/// Like `format_btc` for a value that can be negative, with an explicit `+` for gains.
pub fn format_signed_btc(sats: i128) -> String {
    let magnitude = u64::try_from(sats.unsigned_abs()).unwrap_or(u64::MAX);
    match sats {
        0 => "0".to_string(),
        s if s > 0 => format!("+{}", format_btc(magnitude)),
        _ => format!("-{}", format_btc(magnitude)),
    }
}

/// A fee rate in thousandths of a satoshi per virtual byte, so 0.1 sat/vB is 100 and no
/// floating-point rounding is involved in fee arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FeeRate(pub u64);

impl FeeRate {
    /// The lowest rate this wallet pays: 1 sat/vB, the long-standing default minimum relay fee
    /// of Bitcoin Core. (Newer nodes accept 0.1, but a transaction below 1 would not propagate
    /// through older ones.)
    pub const MIN: FeeRate = FeeRate(1_000);
    /// The highest rate accepted: 1,000 sat/vB, far above any rate the network has needed for
    /// a sustained period. Guards against typos and against a fee-estimation server that lies.
    pub const MAX: FeeRate = FeeRate(1_000_000);

    pub fn from_sat_per_vb(rate: f64) -> Option<FeeRate> {
        if !rate.is_finite() || rate < 0.0 || rate > 1e9 {
            return None;
        }
        Some(FeeRate((rate * 1000.0).round() as u64))
    }

    /// Reads "12", "12.5" or "0.125" (at most three decimals).
    pub fn parse(text: &str) -> Result<FeeRate, String> {
        let text = text.trim();
        let bad = || {
            format!(
                "'{}' is not a fee rate; give satoshis per virtual byte, e.g. \"12.5\"",
                crate::eth::explorer::sanitize_text_max(text, 30)
            )
        };
        let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
        let digits_only = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if whole.is_empty()
            || !digits_only(whole)
            || !digits_only(fraction)
            || fraction.len() > 3
            || (text.contains('.') && fraction.is_empty())
            || whole.len() > 9
        {
            return Err(bad());
        }
        let whole: u64 = whole.parse().map_err(|_| bad())?;
        let fraction: u64 = format!("{fraction:0<3}").parse().map_err(|_| bad())?;
        Ok(FeeRate(whole * 1000 + fraction))
    }

    /// The fee for `vsize` virtual bytes, rounded up to a whole satoshi.
    pub fn fee_for(&self, vsize: u64) -> u64 {
        (u128::from(self.0) * u128::from(vsize)).div_ceil(1000) as u64
    }

    /// Display text such as `"12.5"` (trailing zeros removed).
    pub fn display(&self) -> String {
        let whole = self.0 / 1000;
        let fraction = self.0 % 1000;
        if fraction == 0 {
            return whole.to_string();
        }
        let digits = format!("{fraction:03}");
        format!("{whole}.{}", digits.trim_end_matches('0'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_amounts_exactly() {
        for (text, sats) in [
            ("1", 100_000_000),
            ("0.00000001", 1),
            ("0.001", 100_000),
            (".5", 50_000_000),
            ("  2.50 ", 250_000_000),
            ("0.12345678", 12_345_678),
            ("21000000", MAX_MONEY),
            ("00.1", 10_000_000),
        ] {
            assert_eq!(parse_btc(text), Ok(sats), "{text}");
        }
    }

    #[test]
    fn refuses_malformed_amounts() {
        for text in [
            "",
            ".",
            "1.",
            "-1",
            "+1",
            "1e3",
            "1,5",
            "1_000",
            "abc",
            "0",
            "0.0",
            "0.000000001",
            "21000000.00000001",
            "99999999999999999999",
            "1 000",
            "0x10",
            "NaN",
            "inf",
        ] {
            assert!(parse_btc(text).is_err(), "{text:?} should be refused");
        }
    }

    #[test]
    fn formats_without_trailing_zeros() {
        assert_eq!(format_btc(0), "0");
        assert_eq!(format_btc(1), "0.00000001");
        assert_eq!(format_btc(150_000), "0.0015");
        assert_eq!(format_btc(100_000_000), "1");
        assert_eq!(format_btc(123_456_789), "1.23456789");
        assert_eq!(format_btc(MAX_MONEY), "21000000");
    }

    #[test]
    fn parse_and_format_round_trip() {
        for sats in [
            1u64,
            546,
            294,
            99_999_999,
            100_000_001,
            2_100_000_000_000_000,
        ] {
            assert_eq!(parse_btc(&format_btc(sats)), Ok(sats));
        }
    }

    #[test]
    fn signed_amounts_carry_their_sign() {
        assert_eq!(format_signed_btc(0), "0");
        assert_eq!(format_signed_btc(150_000), "+0.0015");
        assert_eq!(format_signed_btc(-150_000), "-0.0015");
        assert_eq!(format_signed_btc(-1), "-0.00000001");
    }

    #[test]
    fn fee_rates_parse_and_display() {
        assert_eq!(FeeRate::parse("12"), Ok(FeeRate(12_000)));
        assert_eq!(FeeRate::parse("12.5"), Ok(FeeRate(12_500)));
        assert_eq!(FeeRate::parse(" 0.125 "), Ok(FeeRate(125)));
        assert_eq!(FeeRate(12_500).display(), "12.5");
        assert_eq!(FeeRate(12_000).display(), "12");
        assert_eq!(FeeRate(125).display(), "0.125");
        for bad in ["", ".", "1.", "-1", "1e2", "abc", "1.2345", ".5", "1,5"] {
            assert!(FeeRate::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn fees_round_up_to_whole_satoshis() {
        assert_eq!(FeeRate(1_000).fee_for(141), 141);
        assert_eq!(FeeRate(1_500).fee_for(141), 212);
        assert_eq!(FeeRate(1_000).fee_for(0), 0);
        assert_eq!(FeeRate(2_000).fee_for(110), 220);
        assert_eq!(FeeRate(100).fee_for(141), 15);
    }

    #[test]
    fn rates_from_floating_point_are_rounded_and_checked() {
        assert_eq!(FeeRate::from_sat_per_vb(1.075), Some(FeeRate(1_075)));
        assert_eq!(
            FeeRate::from_sat_per_vb(0.6849999999999999),
            Some(FeeRate(685))
        );
        assert_eq!(FeeRate::from_sat_per_vb(f64::NAN), None);
        assert_eq!(FeeRate::from_sat_per_vb(-1.0), None);
        assert_eq!(FeeRate::from_sat_per_vb(f64::INFINITY), None);
        assert!(FeeRate::MIN < FeeRate::MAX);
    }
}
