//! Minimal unsigned 256-bit integer for balances, fees and token amounts.

use std::cmp::Ordering;
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct U256([u64; 4]); // little-endian limbs

impl fmt::Debug for U256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "U256({})", self.to_dec_string())
    }
}

impl fmt::Display for U256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_dec_string())
    }
}

impl PartialOrd for U256 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for U256 {
    fn cmp(&self, other: &Self) -> Ordering {
        for i in (0..4).rev() {
            match self.0[i].cmp(&other.0[i]) {
                Ordering::Equal => continue,
                ord => return ord,
            }
        }
        Ordering::Equal
    }
}

impl U256 {
    pub const ZERO: U256 = U256([0; 4]);
    pub const ONE: U256 = U256([1, 0, 0, 0]);
    pub const MAX: U256 = U256([u64::MAX; 4]);

    pub fn from_u64(v: u64) -> U256 {
        U256([v, 0, 0, 0])
    }

    pub fn from_u128(v: u128) -> U256 {
        U256([v as u64, (v >> 64) as u64, 0, 0])
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0; 4]
    }

    pub fn to_u64(&self) -> Option<u64> {
        if self.0[1..].iter().all(|l| *l == 0) {
            Some(self.0[0])
        } else {
            None
        }
    }

    pub fn from_be_bytes(bytes: &[u8]) -> Result<U256, String> {
        if bytes.len() > 32 {
            return Err("value exceeds 256 bits".to_string());
        }
        let mut padded = [0u8; 32];
        padded[32 - bytes.len()..].copy_from_slice(bytes);
        let mut limbs = [0u64; 4];
        for (i, limb) in limbs.iter_mut().enumerate() {
            let start = 32 - 8 * (i + 1);
            *limb = u64::from_be_bytes(padded[start..start + 8].try_into().expect("8 bytes"));
        }
        Ok(U256(limbs))
    }

    pub fn to_be_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for i in 0..4 {
            let start = 32 - 8 * (i + 1);
            out[start..start + 8].copy_from_slice(&self.0[i].to_be_bytes());
        }
        out
    }

    /// Big-endian bytes without leading zeros (empty for zero), as RLP requires.
    pub fn to_be_bytes_minimal(&self) -> Vec<u8> {
        let full = self.to_be_bytes();
        let first = full.iter().position(|b| *b != 0).unwrap_or(32);
        full[first..].to_vec()
    }

    /// Parses a JSON-RPC quantity such as `0x1a`. Also accepts an empty `0x` as zero.
    pub fn from_hex_quantity(s: &str) -> Result<U256, String> {
        let digits = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .ok_or("hex quantity must start with 0x")?;
        if digits.len() > 64 {
            return Err("hex quantity exceeds 256 bits".to_string());
        }
        let padded = if digits.len() % 2 == 1 {
            format!("0{digits}")
        } else {
            digits.to_string()
        };
        let bytes = crate::crypto::hex::decode(&padded)?;
        U256::from_be_bytes(&bytes)
    }

    pub fn to_hex_quantity(&self) -> String {
        let bytes = self.to_be_bytes_minimal();
        if bytes.is_empty() {
            return "0x0".to_string();
        }
        let hex = crate::crypto::hex::encode(&bytes);
        format!("0x{}", hex.trim_start_matches('0'))
    }

    pub fn checked_add(&self, other: &U256) -> Option<U256> {
        let mut out = [0u64; 4];
        let mut carry = false;
        for i in 0..4 {
            let (s1, c1) = self.0[i].overflowing_add(other.0[i]);
            let (s2, c2) = s1.overflowing_add(u64::from(carry));
            out[i] = s2;
            carry = c1 || c2;
        }
        if carry { None } else { Some(U256(out)) }
    }

    pub fn checked_sub(&self, other: &U256) -> Option<U256> {
        if self < other {
            return None;
        }
        let mut out = [0u64; 4];
        let mut borrow = false;
        for i in 0..4 {
            let (d1, b1) = self.0[i].overflowing_sub(other.0[i]);
            let (d2, b2) = d1.overflowing_sub(u64::from(borrow));
            out[i] = d2;
            borrow = b1 || b2;
        }
        Some(U256(out))
    }

    pub fn checked_mul(&self, other: &U256) -> Option<U256> {
        let mut product = [0u64; 8];
        for i in 0..4 {
            let mut carry = 0u128;
            for j in 0..4 {
                let cur = u128::from(product[i + j])
                    + u128::from(self.0[i]) * u128::from(other.0[j])
                    + carry;
                product[i + j] = cur as u64;
                carry = cur >> 64;
            }
            product[i + 4] = carry as u64;
        }
        if product[4..].iter().any(|l| *l != 0) {
            None
        } else {
            Some(U256([product[0], product[1], product[2], product[3]]))
        }
    }

    pub fn checked_mul_u64(&self, v: u64) -> Option<U256> {
        self.checked_mul(&U256::from_u64(v))
    }

    pub fn div_rem_u64(&self, divisor: u64) -> Option<(U256, u64)> {
        if divisor == 0 {
            return None;
        }
        let mut out = [0u64; 4];
        let mut rem = 0u128;
        for i in (0..4).rev() {
            let cur = (rem << 64) | u128::from(self.0[i]);
            out[i] = (cur / u128::from(divisor)) as u64;
            rem = cur % u128::from(divisor);
        }
        Some((U256(out), rem as u64))
    }

    pub fn from_dec_str(s: &str) -> Result<U256, String> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err("not a non-negative decimal integer".to_string());
        }
        let mut value = U256::ZERO;
        for b in s.bytes() {
            value = value
                .checked_mul_u64(10)
                .and_then(|v| v.checked_add(&U256::from_u64(u64::from(b - b'0'))))
                .ok_or("value exceeds 256 bits")?;
        }
        Ok(value)
    }

    pub fn to_dec_string(&self) -> String {
        if self.is_zero() {
            return "0".to_string();
        }
        let mut digits = Vec::new();
        let mut cur = *self;
        while !cur.is_zero() {
            let (q, r) = cur.div_rem_u64(10).expect("non-zero divisor");
            digits.push(b'0' + r as u8);
            cur = q;
        }
        digits.reverse();
        String::from_utf8(digits).expect("ascii digits")
    }

    /// Parses a human amount like `1.5` into base units (`decimals` fractional digits).
    pub fn parse_units(amount: &str, decimals: u32) -> Result<U256, String> {
        let (whole, frac) = match amount.split_once('.') {
            Some((w, f)) => (w, f),
            None => (amount, ""),
        };
        if whole.is_empty() && frac.is_empty() {
            return Err("amount is empty".to_string());
        }
        if !whole.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return Err("amount must be a plain decimal number".to_string());
        }
        if amount.contains('.') && frac.is_empty() {
            return Err("amount has a trailing decimal point".to_string());
        }
        if frac.len() > decimals as usize {
            return Err(format!("amount has more than {decimals} decimal places"));
        }
        let whole = if whole.is_empty() { "0" } else { whole };
        let combined = format!(
            "{whole}{frac}{}",
            "0".repeat(decimals as usize - frac.len())
        );
        U256::from_dec_str(&combined)
    }

    /// Formats base units as a human amount, trimming trailing zeros.
    pub fn format_units(&self, decimals: u32) -> String {
        let raw = self.to_dec_string();
        let decimals = decimals as usize;
        if decimals == 0 {
            return raw;
        }
        let padded = if raw.len() <= decimals {
            format!("{}{raw}", "0".repeat(decimals - raw.len() + 1))
        } else {
            raw
        };
        let split = padded.len() - decimals;
        let (whole, frac) = padded.split_at(split);
        let frac = frac.trim_end_matches('0');
        if frac.is_empty() {
            whole.to_string()
        } else {
            format!("{whole}.{frac}")
        }
    }

    /// Lossy conversion used only for price display.
    pub fn to_f64(&self) -> f64 {
        self.0
            .iter()
            .rev()
            .fold(0.0, |acc, limb| acc * 18446744073709551616.0 + *limb as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_round_trip() {
        for s in [
            "0",
            "1",
            "10",
            "18446744073709551616",
            "340282366920938463463374607431768211456",
        ] {
            assert_eq!(U256::from_dec_str(s).unwrap().to_dec_string(), s);
        }
        let max = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
        assert_eq!(U256::from_dec_str(max).unwrap(), U256::MAX);
        assert_eq!(U256::MAX.to_dec_string(), max);
    }

    #[test]
    fn decimal_rejects_bad_input() {
        assert!(U256::from_dec_str("").is_err());
        assert!(U256::from_dec_str("-1").is_err());
        assert!(U256::from_dec_str("1.5").is_err());
        assert!(U256::from_dec_str(" 1").is_err());
        let too_big =
            "115792089237316195423570985008687907853269984665640564039457584007913129639936";
        assert!(U256::from_dec_str(too_big).is_err());
    }

    #[test]
    fn hex_quantities() {
        assert_eq!(U256::from_hex_quantity("0x0").unwrap(), U256::ZERO);
        assert_eq!(U256::from_hex_quantity("0x").unwrap(), U256::ZERO);
        assert_eq!(U256::from_hex_quantity("0x1a").unwrap(), U256::from_u64(26));
        assert_eq!(
            U256::from_hex_quantity("0xABC").unwrap(),
            U256::from_u64(0xabc)
        );
        assert!(U256::from_hex_quantity("1a").is_err());
        assert!(U256::from_hex_quantity("0xzz").is_err());
        assert!(U256::from_hex_quantity(&format!("0x{}", "f".repeat(65))).is_err());
        assert_eq!(U256::from_u64(255).to_hex_quantity(), "0xff");
        assert_eq!(U256::from_u64(256).to_hex_quantity(), "0x100");
        assert_eq!(U256::ZERO.to_hex_quantity(), "0x0");
        assert_eq!(U256::from_u64(0x10).to_hex_quantity(), "0x10");
    }

    #[test]
    fn byte_conversions() {
        let v = U256::from_u128(0x0102030405060708090a0b0c0d0e0f10);
        assert_eq!(U256::from_be_bytes(&v.to_be_bytes()).unwrap(), v);
        assert_eq!(v.to_be_bytes_minimal().len(), 16);
        assert_eq!(U256::ZERO.to_be_bytes_minimal(), Vec::<u8>::new());
        assert_eq!(U256::from_u64(0x100).to_be_bytes_minimal(), vec![1, 0]);
        assert!(U256::from_be_bytes(&[0u8; 33]).is_err());
        assert_eq!(U256::from_be_bytes(&[]).unwrap(), U256::ZERO);
    }

    #[test]
    fn arithmetic() {
        let a = U256::from_u64(u64::MAX);
        let two = U256::from_u64(2);
        assert_eq!(
            a.checked_add(&U256::ONE).unwrap().to_dec_string(),
            "18446744073709551616"
        );
        assert_eq!(U256::MAX.checked_add(&U256::ONE), None);
        assert_eq!(
            a.checked_mul(&two).unwrap().to_dec_string(),
            "36893488147419103230"
        );
        assert_eq!(U256::MAX.checked_mul(&two), None);
        assert_eq!(U256::MAX.checked_mul(&U256::ONE), Some(U256::MAX));
        assert_eq!(U256::ONE.checked_sub(&two), None);
        assert_eq!(
            U256::from_dec_str("18446744073709551616")
                .unwrap()
                .checked_sub(&U256::ONE),
            Some(a)
        );
        let big = U256::from_u128(1u128 << 100);
        assert_eq!(
            big.checked_mul(&big).unwrap().to_dec_string(),
            "1606938044258990275541962092341162602522202993782792835301376"
        );
    }

    #[test]
    fn division() {
        let v = U256::from_dec_str("1000000000000000000000").unwrap();
        let (q, r) = v.div_rem_u64(1_000_000_000_000_000_000).unwrap();
        assert_eq!((q, r), (U256::from_u64(1000), 0));
        assert_eq!(
            U256::from_u64(7).div_rem_u64(2),
            Some((U256::from_u64(3), 1))
        );
        assert_eq!(U256::ONE.div_rem_u64(0), None);
    }

    #[test]
    fn ordering() {
        assert!(U256::from_u128(1 << 70) > U256::from_u64(u64::MAX));
        assert!(U256::ZERO < U256::ONE);
        assert_eq!(U256::from_u64(5).cmp(&U256::from_u64(5)), Ordering::Equal);
        assert!(U256::MAX > U256::from_u128(u128::MAX));
    }

    #[test]
    fn parse_units() {
        assert_eq!(
            U256::parse_units("1", 18).unwrap().to_dec_string(),
            "1000000000000000000"
        );
        assert_eq!(
            U256::parse_units("1.5", 6).unwrap(),
            U256::from_u64(1_500_000)
        );
        assert_eq!(U256::parse_units(".5", 6).unwrap(), U256::from_u64(500_000));
        assert_eq!(U256::parse_units("0.000001", 6).unwrap(), U256::ONE);
        assert_eq!(U256::parse_units("12", 0).unwrap(), U256::from_u64(12));
        assert_eq!(U256::parse_units("0", 18).unwrap(), U256::ZERO);
    }

    #[test]
    fn parse_units_rejects_bad_input() {
        for bad in ["", ".", "1.", "-1", "1e3", "1,5", "0x10", "1.2.3", " 1"] {
            assert!(U256::parse_units(bad, 18).is_err(), "{bad}");
        }
        assert!(U256::parse_units("1.0000001", 6).is_err());
        assert!(U256::parse_units("1.5", 0).is_err());
        let huge = "9".repeat(70);
        assert!(U256::parse_units(&huge, 18).is_err());
    }

    #[test]
    fn format_units() {
        assert_eq!(U256::from_u64(1_500_000).format_units(6), "1.5");
        assert_eq!(U256::from_u64(1_000_000).format_units(6), "1");
        assert_eq!(U256::ONE.format_units(6), "0.000001");
        assert_eq!(U256::ZERO.format_units(18), "0");
        assert_eq!(U256::from_u64(42).format_units(0), "42");
        assert_eq!(
            U256::from_dec_str("123456789012345678901")
                .unwrap()
                .format_units(18),
            "123.456789012345678901"
        );
    }

    #[test]
    fn units_round_trip() {
        for (amount, d) in [
            ("0.1", 18),
            ("123456.789", 6),
            ("5", 0),
            ("0.000000000000000001", 18),
        ] {
            assert_eq!(
                U256::parse_units(amount, d).unwrap().format_units(d),
                amount
            );
        }
    }

    #[test]
    fn to_u64_and_f64() {
        assert_eq!(U256::from_u64(9).to_u64(), Some(9));
        assert_eq!(U256::from_u128(1 << 64).to_u64(), None);
        assert_eq!(U256::from_u64(1000).to_f64(), 1000.0);
        assert!((U256::from_u128(1 << 64).to_f64() - 18446744073709551616.0).abs() < 1.0);
    }
}
