//! Valuing Solana balances in USD for display. Failures degrade to "no value", never to an error.

use std::collections::HashMap;

use super::address::{self, Pubkey};
use super::tokens;
use crate::eth::prices::{self, native_price_url, parse_native_price};
use crate::http::HttpClient;
use crate::json;

/// Prices are looked up this many tokens at a time; the rest are shown without a value.
pub const MAX_PRICED_TOKENS: usize = 30;

pub fn sol_price_usd(http: &dyn HttpClient) -> Option<f64> {
    let body = http.get(&native_price_url("solana")).ok()?;
    parse_native_price(&body, "solana")
}

pub fn token_price_url(mints: &[Pubkey]) -> String {
    let list: Vec<String> = mints.iter().map(address::to_text).collect();
    format!(
        "{}/simple/token_price/solana?contract_addresses={}&vs_currencies=usd",
        prices::BASE_URL,
        list.join(",")
    )
}

/// Reads CoinGecko's answer; it may write the addresses in lower case, so they are matched
/// ignoring case.
pub fn parse_token_prices(body: &str, mints: &[Pubkey]) -> HashMap<Pubkey, f64> {
    let mut out = HashMap::new();
    let Ok(value) = json::parse(body) else {
        return out;
    };
    for mint in mints {
        let text = address::to_text(mint).to_lowercase();
        let entry = match &value {
            json::Value::Object(pairs) => pairs
                .iter()
                .find(|(k, _)| k.to_lowercase() == text)
                .map(|(_, v)| v),
            _ => None,
        };
        let price = entry
            .and_then(|e| e.get("usd"))
            .and_then(|p| match p {
                json::Value::Number(n) => n.parse::<f64>().ok(),
                _ => None,
            })
            .filter(|p| p.is_finite() && *p >= 0.0);
        if let Some(p) = price {
            out.insert(*mint, p);
        }
    }
    out
}

/// Prices of tokens in USD. USDC is taken as exactly 1.
pub fn token_prices_usd(http: &dyn HttpClient, mints: &[Pubkey]) -> HashMap<Pubkey, f64> {
    let mut prices = HashMap::new();
    let usdc = address::id(tokens::USDC_MINT);
    let rest: Vec<Pubkey> = mints
        .iter()
        .copied()
        .filter(|m| {
            if *m == usdc {
                prices.insert(usdc, 1.0);
                false
            } else {
                true
            }
        })
        .take(MAX_PRICED_TOKENS)
        .collect();
    if rest.is_empty() {
        return prices;
    }
    if let Ok(body) = http.get(&token_price_url(&rest)) {
        prices.extend(parse_token_prices(&body, &rest));
    }
    prices
}

/// `raw` base units at `price` USD per whole unit.
pub fn value_usd(raw: u128, decimals: u32, price: f64) -> f64 {
    raw as f64 / 10f64.powi(decimals as i32) * price
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solana::testing::SolMock;

    #[test]
    fn urls_list_the_mints() {
        let url = token_price_url(&[[1u8; 32], [2u8; 32]]);
        assert!(url.contains("/simple/token_price/solana?contract_addresses="));
        assert!(url.contains(&address::to_text(&[1u8; 32])));
        assert!(url.contains(','));
        assert!(url.ends_with("&vs_currencies=usd"));
    }

    #[test]
    fn parses_prices_ignoring_case_and_bad_entries() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let c = [3u8; 32];
        let body = format!(
            r#"{{"{}":{{"usd":1.5}},"{}":{{"usd":"x"}}}}"#,
            address::to_text(&a).to_lowercase(),
            address::to_text(&b)
        );
        let prices = parse_token_prices(&body, &[a, b, c]);
        assert_eq!(prices.get(&a), Some(&1.5));
        assert!(!prices.contains_key(&b) && !prices.contains_key(&c));
        assert!(parse_token_prices("not json", &[a]).is_empty());
        assert!(parse_token_prices("[]", &[a]).is_empty());
        let negative = format!(r#"{{"{}":{{"usd":-3}}}}"#, address::to_text(&a));
        assert!(parse_token_prices(&negative, &[a]).is_empty());
    }

    #[test]
    fn usdc_is_one_and_failures_give_no_price() {
        let usdc = address::id(tokens::USDC_MINT);
        let mock = SolMock::new().get("token_price", Err("HTTP status 429".to_string()));
        let prices = token_prices_usd(&mock, &[usdc, [9u8; 32]]);
        assert_eq!(prices.get(&usdc), Some(&1.0));
        assert_eq!(prices.len(), 1);
        let only_usdc = SolMock::new();
        assert_eq!(token_prices_usd(&only_usdc, &[usdc]).len(), 1);
        assert!(only_usdc.get_urls.borrow().is_empty());
    }

    #[test]
    fn sol_price_reads_coingecko() {
        let mock = SolMock::new().get("ids=solana", Ok(r#"{"solana":{"usd":142.5}}"#.to_string()));
        assert_eq!(sol_price_usd(&mock), Some(142.5));
        assert_eq!(sol_price_usd(&SolMock::new()), None);
    }

    #[test]
    fn values_scale_by_decimals() {
        assert_eq!(value_usd(2_500_000, 6, 2.0), 5.0);
        assert_eq!(value_usd(0, 9, 100.0), 0.0);
    }
}
