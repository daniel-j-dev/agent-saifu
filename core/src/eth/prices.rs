//! USD prices from CoinGecko's public API. Failures degrade to "no price", never to an error.

use std::collections::HashMap;

use super::address::Address;
use super::tokens::coingecko_ids;
use super::u256::U256;
use crate::crypto::hex;
use crate::http::HttpClient;
use crate::json::{self, Value};

const BASE_URL: &str = "https://api.coingecko.com/api/v3";

pub struct PriceClient<'a> {
    http: &'a dyn HttpClient,
}

fn price_from(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.parse::<f64>().ok().filter(|p| p.is_finite() && *p >= 0.0),
        _ => None,
    }
}

pub fn native_price_url(coin_id: &str) -> String {
    format!("{BASE_URL}/simple/price?ids={coin_id}&vs_currencies=usd")
}

pub fn token_price_url(platform: &str, addresses: &[Address]) -> String {
    let list: Vec<String> = addresses.iter().map(|a| hex::encode_prefixed(a)).collect();
    format!(
        "{BASE_URL}/simple/token_price/{platform}?contract_addresses={}&vs_currencies=usd",
        list.join(",")
    )
}

pub fn parse_native_price(body: &str, coin_id: &str) -> Option<f64> {
    let v = json::parse(body).ok()?;
    price_from(v.get(coin_id)?.get("usd"))
}

pub fn parse_token_prices(body: &str, addresses: &[Address]) -> HashMap<Address, f64> {
    let mut out = HashMap::new();
    let Ok(v) = json::parse(body) else { return out };
    for addr in addresses {
        let key = hex::encode_prefixed(addr);
        if let Some(price) = v.get(&key).and_then(|e| price_from(e.get("usd"))) {
            out.insert(*addr, price);
        }
    }
    out
}

impl<'a> PriceClient<'a> {
    pub fn new(http: &'a dyn HttpClient) -> PriceClient<'a> {
        PriceClient { http }
    }

    pub fn native_usd(&self, chain_id: u64) -> Option<f64> {
        let (_, coin) = coingecko_ids(chain_id)?;
        let body = self.http.get(&native_price_url(coin)).ok()?;
        parse_native_price(&body, coin)
    }

    pub fn tokens_usd(&self, chain_id: u64, addresses: &[Address]) -> HashMap<Address, f64> {
        let Some((platform, _)) = coingecko_ids(chain_id) else {
            return HashMap::new();
        };
        if addresses.is_empty() {
            return HashMap::new();
        }
        match self.http.get(&token_price_url(platform, addresses)) {
            Ok(body) => parse_token_prices(&body, addresses),
            Err(_) => HashMap::new(),
        }
    }
}

/// Value of `amount` base units at `price` USD per whole unit.
pub fn usd_value(amount: &U256, decimals: u32, price: f64) -> f64 {
    amount.to_f64() / 10f64.powi(decimals as i32) * price
}

/// Two-decimal dollar string, e.g. `1234.57`.
pub fn format_usd(value: f64) -> String {
    if !value.is_finite() || value < 0.0 {
        return "0.00".to_string();
    }
    format!("{value:.2}")
}

/// Convenience for building a `Value` with an optional price.
pub fn usd_json(value: Option<f64>) -> Value {
    match value {
        Some(v) => Value::String(format_usd(v)),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::mock::MockHttp;

    #[test]
    fn urls_are_built_correctly() {
        assert_eq!(
            native_price_url("ethereum"),
            "https://api.coingecko.com/api/v3/simple/price?ids=ethereum&vs_currencies=usd"
        );
        let url = token_price_url("base", &[[0xab; 20], [0xcd; 20]]);
        assert!(url.contains("/simple/token_price/base?contract_addresses=0xabab"));
        assert!(url.contains(",0xcdcd"));
        assert!(url.ends_with("&vs_currencies=usd"));
    }

    #[test]
    fn parses_native_price() {
        assert_eq!(
            parse_native_price(r#"{"ethereum":{"usd":2394.27}}"#, "ethereum"),
            Some(2394.27)
        );
        assert_eq!(
            parse_native_price(r#"{"ethereum":{"usd":2394.27}}"#, "bitcoin"),
            None
        );
        assert_eq!(parse_native_price("garbage", "ethereum"), None);
        assert_eq!(
            parse_native_price(r#"{"ethereum":{"usd":"x"}}"#, "ethereum"),
            None
        );
        assert_eq!(
            parse_native_price(r#"{"ethereum":{"usd":-5}}"#, "ethereum"),
            None
        );
    }

    #[test]
    fn parses_token_prices_by_lowercase_address() {
        let a = [0xab; 20];
        let b = [0xcd; 20];
        let body = format!(r#"{{"{}":{{"usd":0.99947}}}}"#, hex::encode_prefixed(&a));
        let prices = parse_token_prices(&body, &[a, b]);
        assert_eq!(prices.get(&a), Some(&0.99947));
        assert_eq!(prices.get(&b), None);
        assert!(parse_token_prices("[", &[a]).is_empty());
    }

    #[test]
    fn client_degrades_gracefully() {
        let failing = MockHttp::new(vec![Err("boom".into()), Err("boom".into())]);
        let client = PriceClient::new(&failing);
        assert_eq!(client.native_usd(1), None);
        assert!(client.tokens_usd(1, &[[1u8; 20]]).is_empty());
    }

    #[test]
    fn unknown_chain_makes_no_requests() {
        let http = MockHttp::new(vec![]);
        let client = PriceClient::new(&http);
        assert_eq!(client.native_usd(999_999), None);
        assert!(client.tokens_usd(999_999, &[[1u8; 20]]).is_empty());
        assert!(client.tokens_usd(1, &[]).is_empty());
        assert!(http.requests.borrow().is_empty());
    }

    #[test]
    fn client_reads_native_price() {
        let http = MockHttp::new(vec![Ok(r#"{"ethereum":{"usd":2000}}"#.to_string())]);
        assert_eq!(PriceClient::new(&http).native_usd(8453), Some(2000.0));
    }

    #[test]
    fn value_math_and_formatting() {
        let one_eth = U256::parse_units("1", 18).unwrap();
        assert!((usd_value(&one_eth, 18, 2000.0) - 2000.0).abs() < 1e-6);
        let usdc = U256::from_u64(2_500_000);
        assert!((usd_value(&usdc, 6, 0.9995) - 2.49875).abs() < 1e-9);
        assert_eq!(format_usd(1234.567), "1234.57");
        assert_eq!(format_usd(0.0), "0.00");
        assert_eq!(format_usd(f64::NAN), "0.00");
        assert_eq!(format_usd(-1.0), "0.00");
        assert_eq!(usd_json(None), Value::Null);
        assert_eq!(usd_json(Some(1.0)), Value::str("1.00"));
    }
}
