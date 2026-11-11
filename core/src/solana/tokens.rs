//! Token amounts, the SPL token account layouts and a short list of well-known tokens.
//!
//! The well-known list only supplies display names and CoinGecko prices. Amounts that are sent
//! always use the decimals the token's mint reports on-chain.

use super::address::{self, Pubkey};
use crate::eth::u256::U256;

/// Size of an SPL token account without extensions; Token-2022 accounts add extension data.
pub const TOKEN_ACCOUNT_LEN: usize = 165;
/// Size of an SPL mint account without extensions.
pub const MINT_LEN: usize = 82;

pub struct KnownToken {
    pub mint: &'static str,
    pub symbol: &'static str,
    pub name: &'static str,
}

/// Display names for the most common Solana tokens. Nothing here is trusted for amounts.
pub const KNOWN_TOKENS: &[KnownToken] = &[
    KnownToken {
        mint: address::WRAPPED_SOL_MINT,
        symbol: "wSOL",
        name: "Wrapped SOL",
    },
    KnownToken {
        mint: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        symbol: "USDC",
        name: "USD Coin",
    },
    KnownToken {
        mint: "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
        symbol: "USDT",
        name: "Tether USD",
    },
    KnownToken {
        mint: "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo",
        symbol: "PYUSD",
        name: "PayPal USD",
    },
    KnownToken {
        mint: "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So",
        symbol: "mSOL",
        name: "Marinade staked SOL",
    },
    KnownToken {
        mint: "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn",
        symbol: "JitoSOL",
        name: "Jito Staked SOL",
    },
    KnownToken {
        mint: "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN",
        symbol: "JUP",
        name: "Jupiter",
    },
    KnownToken {
        mint: "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263",
        symbol: "BONK",
        name: "Bonk",
    },
    KnownToken {
        mint: "4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R",
        symbol: "RAY",
        name: "Raydium",
    },
    KnownToken {
        mint: "EKpQGSJtjMFqKZ9KQanSqYXRcF8fBopzLHYxdM65zcjm",
        symbol: "WIF",
        name: "dogwifhat",
    },
];

pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

pub fn known_token(mint: &Pubkey) -> Option<&'static KnownToken> {
    let text = address::to_text(mint);
    KNOWN_TOKENS.iter().find(|t| t.mint == text)
}

/// A readable name for a mint: its well-known symbol, otherwise a shortened address.
pub fn token_label(mint: &Pubkey) -> String {
    match known_token(mint) {
        Some(t) => t.symbol.to_string(),
        None => short_address(mint),
    }
}

/// `EPjF...t1v`
pub fn short_address(key: &Pubkey) -> String {
    let text = address::to_text(key);
    if text.len() <= 10 {
        return text;
    }
    format!("{}...{}", &text[..4], &text[text.len() - 4..])
}

pub fn format_amount(raw: u128, decimals: u32) -> String {
    U256::from_u128(raw).format_units(decimals)
}

pub fn format_sol(lamports: u64) -> String {
    format_amount(u128::from(lamports), super::tx::SOL_DECIMALS)
}

/// A priority fee given in whole SOL, as lamports. Zero is allowed (it means no priority fee);
/// anything above `tx::MAX_PRIORITY_FEE_LAMPORTS` is refused so a typo cannot burn funds.
pub fn parse_priority_fee(text: &str) -> Result<u64, String> {
    let units = U256::parse_units(text.trim(), super::tx::SOL_DECIMALS)
        .map_err(|e| format!("'priority_fee': {e}"))?;
    let lamports = units
        .to_u64()
        .filter(|l| *l <= super::tx::MAX_PRIORITY_FEE_LAMPORTS)
        .ok_or_else(|| {
            format!(
                "'priority_fee' is more than the {} SOL this wallet allows as a priority fee",
                format_sol(super::tx::MAX_PRIORITY_FEE_LAMPORTS)
            )
        })?;
    Ok(lamports)
}

/// Signed lamport or token-unit changes, e.g. `-0.25`.
pub fn format_signed(amount: i128, decimals: u32) -> String {
    let text = format_amount(amount.unsigned_abs(), decimals);
    if amount < 0 { format!("-{text}") } else { text }
}

/// Parses a whole-unit amount such as `1.5` into base units. Zero is refused.
pub fn parse_amount(amount: &str, decimals: u32) -> Result<u64, String> {
    let units = U256::parse_units(amount.trim(), decimals)?;
    let raw = units
        .to_u64()
        .ok_or("amount is larger than any Solana balance can be")?;
    if raw == 0 {
        return Err("amount must be greater than zero".to_string());
    }
    Ok(raw)
}

/// The fields of an SPL token account that matter for safety checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenAccountData {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
    pub delegate: Option<Pubkey>,
    pub close_authority: Option<Pubkey>,
}

fn optional_key(data: &[u8], at: usize) -> Option<Pubkey> {
    let tag = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?);
    if tag != 1 {
        return None;
    }
    data.get(at + 4..at + 36)?.try_into().ok()
}

/// Reads the shared SPL Token / Token-2022 account layout.
pub fn parse_token_account(data: &[u8]) -> Option<TokenAccountData> {
    if data.len() < TOKEN_ACCOUNT_LEN {
        return None;
    }
    Some(TokenAccountData {
        mint: data[0..32].try_into().ok()?,
        owner: data[32..64].try_into().ok()?,
        amount: u64::from_le_bytes(data[64..72].try_into().ok()?),
        delegate: optional_key(data, 72),
        close_authority: optional_key(data, 129),
    })
}

/// The decimals of an initialized mint account.
pub fn mint_decimals(data: &[u8]) -> Option<u8> {
    (data.len() >= MINT_LEN && data[45] == 1).then(|| data[44])
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// Token account data as the network stores it.
    pub fn token_account_bytes(
        mint: &Pubkey,
        owner: &Pubkey,
        amount: u64,
        delegate: Option<&Pubkey>,
    ) -> Vec<u8> {
        let mut data = vec![0u8; TOKEN_ACCOUNT_LEN];
        data[0..32].copy_from_slice(mint);
        data[32..64].copy_from_slice(owner);
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        if let Some(d) = delegate {
            data[72..76].copy_from_slice(&1u32.to_le_bytes());
            data[76..108].copy_from_slice(d);
        }
        data[108] = 1;
        data
    }

    pub fn mint_bytes(decimals: u8) -> Vec<u8> {
        let mut data = vec![0u8; MINT_LEN];
        data[44] = decimals;
        data[45] = 1;
        data
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn priority_fees_are_whole_sol_with_a_ceiling() {
        assert_eq!(parse_priority_fee("0").unwrap(), 0);
        assert_eq!(parse_priority_fee(" 0.0001 ").unwrap(), 100_000);
        assert_eq!(parse_priority_fee("0.000000001").unwrap(), 1);
        assert_eq!(parse_priority_fee("0.01").unwrap(), 10_000_000);
        for bad in ["0.010000001", "1", "-1", "abc", "", "0.0000000001", "1e-4"] {
            assert!(parse_priority_fee(bad).is_err(), "{bad}");
        }
        assert!(parse_priority_fee("1").unwrap_err().contains("0.01 SOL"));
    }

    #[test]
    fn known_tokens_are_valid_and_unique() {
        for (i, t) in KNOWN_TOKENS.iter().enumerate() {
            assert_eq!(address::to_text(&address::id(t.mint)), t.mint);
            for other in &KNOWN_TOKENS[i + 1..] {
                assert_ne!(t.mint, other.mint);
                assert_ne!(t.symbol, other.symbol);
            }
        }
        assert_eq!(USDC_MINT, KNOWN_TOKENS[1].mint);
    }

    #[test]
    fn labels_use_symbols_or_short_addresses() {
        assert_eq!(token_label(&address::id(USDC_MINT)), "USDC");
        assert_eq!(token_label(&address::id(address::WRAPPED_SOL_MINT)), "wSOL");
        assert_eq!(short_address(&[7u8; 32]).len(), 11);
        assert!(short_address(&[7u8; 32]).contains("..."));
        assert_eq!(short_address(&[0u8; 32]), "1111...1111");
    }

    #[test]
    fn amounts_format_and_parse() {
        assert_eq!(format_sol(1_500_000_000), "1.5");
        assert_eq!(format_sol(1), "0.000000001");
        assert_eq!(format_amount(1_000_000, 6), "1");
        assert_eq!(format_signed(-250_000, 6), "-0.25");
        assert_eq!(format_signed(5, 0), "5");
        assert_eq!(parse_amount("1.5", 9).unwrap(), 1_500_000_000);
        assert_eq!(parse_amount(" 0.000001 ", 6).unwrap(), 1);
        for bad in [
            "0",
            "0.0",
            "-1",
            "abc",
            "",
            "1.0000001",
            "99999999999999999999",
        ] {
            assert!(parse_amount(bad, 6).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_token_accounts() {
        let (mint, owner, delegate) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let plain = token_account_bytes(&mint, &owner, 42, None);
        let parsed = parse_token_account(&plain).unwrap();
        assert_eq!(parsed.mint, mint);
        assert_eq!(parsed.owner, owner);
        assert_eq!(parsed.amount, 42);
        assert_eq!(parsed.delegate, None);
        assert_eq!(parsed.close_authority, None);
        let delegated = token_account_bytes(&mint, &owner, 42, Some(&delegate));
        assert_eq!(
            parse_token_account(&delegated).unwrap().delegate,
            Some(delegate)
        );
        let mut closing = plain.clone();
        closing[129..133].copy_from_slice(&1u32.to_le_bytes());
        closing[133..165].copy_from_slice(&[9u8; 32]);
        assert_eq!(
            parse_token_account(&closing).unwrap().close_authority,
            Some([9u8; 32])
        );
        assert!(parse_token_account(&plain[..164]).is_none());
        assert!(parse_token_account(&[]).is_none());
    }

    #[test]
    fn reads_mint_decimals_only_for_initialized_mints() {
        assert_eq!(mint_decimals(&mint_bytes(6)), Some(6));
        let mut uninitialized = mint_bytes(6);
        uninitialized[45] = 0;
        assert_eq!(mint_decimals(&uninitialized), None);
        assert_eq!(mint_decimals(&[0u8; 10]), None);
    }
}
