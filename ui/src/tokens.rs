//! Token search for the swap screen: find a token by name or symbol instead of pasting its
//! contract address.
//!
//! Sources (all keyless, all over the core's `HttpClient`):
//! - Ethereum and other EVM networks: the token list of the OpenOcean swap aggregator, the same
//!   host the wallet swaps through. It covers every network OpenOcean serves.
//! - Solana: Jupiter's token search (the aggregator the wallet swaps through), which flags
//!   verified tokens.
//! - Sui: OpenOcean's Sui list plus DexScreener pairs on Sui (full coin types, ranked by
//!   liquidity), because no single Sui list covers the coins Cetus can swap.
//!
//! Lists are third-party data. Every address is checked for the shape of its network, text is
//! stripped of control characters and shortened, and the swap itself is still verified by the
//! wallet, which refuses anything that does not match what was requested.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use agent_saifu_core::eth::explorer::sanitize_text_max;
use agent_saifu_core::http::HttpClient;
use agent_saifu_core::json::{self, Value};
use agent_saifu_core::swap_config::OPENOCEAN_API_BASE_URL;

const JUPITER_SEARCH_URLS: [&str; 2] = [
    "https://api.jup.ag/tokens/v2/search",
    "https://lite-api.jup.ag/tokens/v2/search",
];
const DEXSCREENER_SEARCH_URL: &str = "https://api.dexscreener.com/latest/dex/search";
const MAX_RESULTS: usize = 40;
const MAX_QUERY_CHARS: usize = 64;
/// Pairs with less liquidity than this (USD) are mostly dead or scam tokens.
const MIN_LIQUIDITY_USD: f64 = 1_000.0;
const EVM_NATIVE: &str = "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const SOL_MINT: &str = "So11111111111111111111111111111111111111112";
const SUI_TYPE: &str = "0x2::sui::SUI";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Net {
    Evm(u64),
    Sui,
    Solana,
}

impl Net {
    fn key(self) -> String {
        match self {
            Net::Evm(id) => format!("evm-{id}"),
            Net::Sui => "sui".to_string(),
            Net::Solana => "solana".to_string(),
        }
    }

    /// `net` is `ethereum` (with a chain ID), `sui` or `solana`.
    pub fn parse(net: &str, chain_id: Option<u64>) -> Option<Net> {
        match net {
            "ethereum" => Some(Net::Evm(chain_id.unwrap_or(1))),
            "sui" => Some(Net::Sui),
            "solana" => Some(Net::Solana),
            _ => None,
        }
    }
}

/// One search result.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub symbol: String,
    pub name: String,
    /// What the wallet's swap takes: `native`, a contract, a mint or a Sui coin type.
    pub address: String,
    /// Marked by a curated source (Jupiter's verified flag, a listed OpenOcean token).
    pub verified: bool,
    pub note: String,
}

impl Hit {
    pub fn to_value(&self) -> Value {
        Value::object(vec![
            ("symbol", Value::str(&self.symbol)),
            ("name", Value::str(&self.name)),
            ("address", Value::str(&self.address)),
            ("verified", Value::Bool(self.verified)),
            ("note", Value::str(&self.note)),
        ])
    }
}

/// Reads the `tokens` list a search answers with.
pub fn hits_from_value(value: &Value) -> Vec<Hit> {
    value
        .get("tokens")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|t| Hit {
                    symbol: str_of(t, "symbol").unwrap_or_default().to_string(),
                    name: str_of(t, "name").unwrap_or_default().to_string(),
                    address: str_of(t, "address").unwrap_or_default().to_string(),
                    verified: t.get("verified").and_then(Value::as_bool) == Some(true),
                    note: str_of(t, "note").unwrap_or_default().to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn hits_value(hits: &[Hit]) -> Value {
    Value::object(vec![(
        "tokens",
        Value::Array(hits.iter().map(Hit::to_value).collect()),
    )])
}

// ---- pure helpers -----------------------------------------------------------------------------

/// Percent-encodes everything but unreserved characters.
pub fn encode_query(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !s.as_bytes()[0].is_ascii_digit()
}

fn is_base58(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() && !matches!(b, b'0' | b'O' | b'I' | b'l'))
}

/// Whether `address` has the shape of a token on this network.
pub fn valid_address(net: Net, address: &str) -> bool {
    match net {
        Net::Evm(_) => {
            address == "native"
                || (address.len() == 42 && address.starts_with("0x") && is_hex(&address[2..]))
        }
        Net::Solana => {
            address == "native" || ((32..=44).contains(&address.len()) && is_base58(address))
        }
        Net::Sui => {
            if address == "native" {
                return true;
            }
            let mut parts = address.split("::");
            let (Some(package), Some(module), Some(name), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return false;
            };
            package.starts_with("0x")
                && package.len() <= 66
                && is_hex(&package[2..])
                && is_ident(module)
                && is_ident(name)
        }
    }
}

/// Whether what the user typed is itself a token address, so it can be used as it is.
pub fn looks_like_address(net: Net, text: &str) -> bool {
    let t = text.trim();
    t != "native" && valid_address(net, t)
}

/// Maps a network's native-coin address to the `native` the wallet's swap expects.
fn normalise(net: Net, address: &str) -> String {
    let native = match net {
        Net::Evm(_) => address.eq_ignore_ascii_case(EVM_NATIVE),
        Net::Solana => address == SOL_MINT,
        Net::Sui => {
            let a = address.trim();
            let short = a == SUI_TYPE;
            let long = a
                .strip_prefix("0x")
                .and_then(|r| r.strip_suffix("::sui::SUI"))
                .is_some_and(|zeros| !zeros.is_empty() && zeros.trim_start_matches('0') == "2");
            short || long
        }
    };
    if native {
        "native".to_string()
    } else {
        address.to_string()
    }
}

fn clean(text: Option<&str>, max: usize) -> String {
    sanitize_text_max(text.unwrap_or_default(), max)
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn float_of(v: &Value, key: &str) -> f64 {
    match v.get(key) {
        Some(Value::Number(n)) => n.parse().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn data_array(body: &str) -> Result<Vec<Value>, String> {
    let parsed = json::parse(body).map_err(|e| format!("unreadable token list: {e}"))?;
    let list = match &parsed {
        Value::Array(items) => Some(items),
        other => other.get("data").and_then(Value::as_array),
    };
    list.cloned()
        .ok_or_else(|| "unexpected token list".to_string())
}

/// OpenOcean's `tokenList` answer.
pub fn parse_openocean(body: &str, net: Net) -> Result<Vec<Hit>, String> {
    let mut hits = vec![];
    for t in data_array(body)? {
        // Sui lists the full coin type in `customAddress`; `address` is only its package.
        let raw = match net {
            Net::Sui => str_of(&t, "customAddress").or_else(|| str_of(&t, "address")),
            _ => str_of(&t, "address"),
        };
        let Some(raw) = raw else { continue };
        let address = normalise(net, raw);
        if !valid_address(net, &address) {
            continue;
        }
        let symbol = clean(str_of(&t, "symbol"), 16);
        if symbol.is_empty() {
            continue;
        }
        hits.push(Hit {
            name: clean(str_of(&t, "name"), 40),
            symbol,
            address,
            verified: str_of(&t, "code").is_some_and(|c| !c.is_empty()),
            note: String::new(),
        });
    }
    Ok(hits)
}

/// Jupiter's `tokens/v2/search` answer.
pub fn parse_jupiter(body: &str) -> Result<Vec<Hit>, String> {
    let mut hits: Vec<(Hit, f64)> = vec![];
    for t in data_array(body)? {
        let Some(id) = str_of(&t, "id") else { continue };
        let address = normalise(Net::Solana, id);
        if !valid_address(Net::Solana, &address) {
            continue;
        }
        let symbol = clean(str_of(&t, "symbol"), 16);
        if symbol.is_empty() {
            continue;
        }
        let liquidity = float_of(&t, "liquidity");
        let verified = t.get("isVerified").and_then(Value::as_bool) == Some(true);
        hits.push((
            Hit {
                name: clean(str_of(&t, "name"), 40),
                symbol,
                address,
                verified,
                note: liquidity_note(liquidity),
            },
            liquidity,
        ));
    }
    // Verified tokens first, then the deepest markets.
    hits.sort_by(|a, b| {
        b.0.verified
            .cmp(&a.0.verified)
            .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
    });
    Ok(hits.into_iter().map(|(h, _)| h).collect())
}

/// DexScreener's `latest/dex/search` answer, reduced to tokens on one chain, deepest first.
pub fn parse_dexscreener(body: &str, slug: &str, net: Net) -> Result<Vec<Hit>, String> {
    let parsed = json::parse(body).map_err(|e| format!("unreadable search result: {e}"))?;
    let pairs = parsed
        .get("pairs")
        .and_then(Value::as_array)
        .ok_or("unexpected search result")?;
    let mut found: Vec<(Hit, f64)> = vec![];
    for pair in pairs {
        if str_of(pair, "chainId") != Some(slug) {
            continue;
        }
        let liquidity = pair
            .get("liquidity")
            .map(|l| float_of(l, "usd"))
            .unwrap_or(0.0);
        if liquidity < MIN_LIQUIDITY_USD {
            continue;
        }
        let Some(base) = pair.get("baseToken") else {
            continue;
        };
        let Some(raw) = str_of(base, "address") else {
            continue;
        };
        let address = normalise(net, raw);
        if !valid_address(net, &address) {
            continue;
        }
        let symbol = clean(str_of(base, "symbol"), 16);
        if symbol.is_empty() {
            continue;
        }
        match found
            .iter_mut()
            .find(|(h, _)| h.address.eq_ignore_ascii_case(&address))
        {
            Some((_, best)) => *best = best.max(liquidity),
            None => found.push((
                Hit {
                    name: clean(str_of(base, "name"), 40),
                    symbol,
                    address,
                    verified: false,
                    note: String::new(),
                },
                liquidity,
            )),
        }
    }
    found.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(found
        .into_iter()
        .map(|(mut h, liq)| {
            h.note = liquidity_note(liq);
            h
        })
        .collect())
}

/// `$1.2M liquidity`.
pub fn liquidity_note(usd: f64) -> String {
    if usd < MIN_LIQUIDITY_USD {
        return String::new();
    }
    let text = if usd >= 1e9 {
        format!("${:.1}B", usd / 1e9)
    } else if usd >= 1e6 {
        format!("${:.1}M", usd / 1e6)
    } else {
        format!("${:.0}K", usd / 1e3)
    };
    format!("{text} liquidity")
}

/// Filters a list by a name, symbol or address fragment: exact symbol first, then symbol
/// prefix, name prefix, then anything containing the text. The list's own order breaks ties.
pub fn rank(list: &[Hit], query: &str) -> Vec<Hit> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return vec![];
    }
    let mut scored: Vec<(u8, usize, &Hit)> = list
        .iter()
        .enumerate()
        .filter_map(|(i, h)| {
            let (symbol, name, address) = (
                h.symbol.to_lowercase(),
                h.name.to_lowercase(),
                h.address.to_lowercase(),
            );
            let score = if symbol == q || address == q {
                0
            } else if symbol.starts_with(&q) {
                1
            } else if name.starts_with(&q) || name.split_whitespace().any(|w| w.starts_with(&q)) {
                2
            } else if symbol.contains(&q) || name.contains(&q) {
                3
            } else {
                return None;
            };
            Some((score, i, h))
        })
        .collect();
    scored.sort_by_key(|(score, index, _)| (*score, *index));
    scored
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, _, h)| h.clone())
        .collect()
}

fn dexscreener_slug(chain_id: u64) -> Option<&'static str> {
    Some(match chain_id {
        1 => "ethereum",
        10 => "optimism",
        56 => "bsc",
        100 => "gnosischain",
        137 => "polygon",
        250 => "fantom",
        324 => "zksync",
        5000 => "mantle",
        8453 => "base",
        42161 => "arbitrum",
        43114 => "avalanche",
        59144 => "linea",
        81457 => "blast",
        534352 => "scroll",
        _ => return None,
    })
}

fn merge(mut first: Vec<Hit>, more: Vec<Hit>) -> Vec<Hit> {
    for hit in more {
        if !first
            .iter()
            .any(|h| h.address.eq_ignore_ascii_case(&hit.address))
        {
            first.push(hit);
        }
    }
    first.truncate(MAX_RESULTS);
    first
}

// ---- searching --------------------------------------------------------------------------------

pub struct TokenSearch {
    http: Rc<dyn HttpClient>,
    /// Whole token lists, fetched once per network and kept until the app closes.
    lists: RefCell<HashMap<String, Vec<Hit>>>,
}

impl TokenSearch {
    pub fn new(http: Rc<dyn HttpClient>) -> TokenSearch {
        TokenSearch {
            http,
            lists: RefCell::new(HashMap::new()),
        }
    }

    pub fn search(&self, net: Net, query: &str) -> Result<Vec<Hit>, String> {
        let query: String = query
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_QUERY_CHARS)
            .collect();
        let query = query.trim();
        if query.is_empty() {
            return Ok(vec![]);
        }
        match net {
            Net::Evm(chain_id) => self.search_evm(chain_id, query),
            Net::Solana => self.search_solana(query),
            Net::Sui => self.search_sui(query),
        }
    }

    fn list(&self, net: Net) -> Result<Vec<Hit>, String> {
        if let Some(cached) = self.lists.borrow().get(&net.key()) {
            return Ok(cached.clone());
        }
        let path = match net {
            Net::Evm(id) => id.to_string(),
            Net::Sui => "sui".to_string(),
            Net::Solana => "solana".to_string(),
        };
        let body = self
            .http
            .get(&format!("{OPENOCEAN_API_BASE_URL}/{path}/tokenList"))?;
        let hits = parse_openocean(&body, net)?;
        if hits.is_empty() {
            return Err("this network has no token list".to_string());
        }
        self.lists.borrow_mut().insert(net.key(), hits.clone());
        Ok(hits)
    }

    fn dexscreener(&self, slug: &str, net: Net, query: &str) -> Result<Vec<Hit>, String> {
        let url = format!("{DEXSCREENER_SEARCH_URL}?q={}", encode_query(query));
        let found = parse_dexscreener(&self.http.get(&url)?, slug, net)?;
        // The search also returns pairs where the query is only the other side of the pair.
        Ok(rank(&found, query))
    }

    fn search_evm(&self, chain_id: u64, query: &str) -> Result<Vec<Hit>, String> {
        let net = Net::Evm(chain_id);
        match self.list(net) {
            Ok(list) => {
                let listed = rank(&list, query);
                if !listed.is_empty() || query.len() < 3 {
                    return Ok(listed);
                }
                // Not in the curated list: look for real markets before giving up.
                Ok(dexscreener_slug(chain_id)
                    .and_then(|slug| self.dexscreener(slug, net, query).ok())
                    .unwrap_or_default())
            }
            Err(list_error) => match dexscreener_slug(chain_id) {
                Some(slug) => self.dexscreener(slug, net, query),
                None => Err(format!(
                    "No token list for this network ({list_error}). Paste the contract address instead."
                )),
            },
        }
    }

    fn search_solana(&self, query: &str) -> Result<Vec<Hit>, String> {
        let mut last = String::new();
        for base in JUPITER_SEARCH_URLS {
            match self
                .http
                .get(&format!("{base}?query={}", encode_query(query)))
                .and_then(|body| parse_jupiter(&body))
            {
                Ok(mut hits) => {
                    hits.truncate(MAX_RESULTS);
                    return Ok(hits);
                }
                Err(e) => last = e,
            }
        }
        Err(format!("Token search is unavailable right now ({last})."))
    }

    fn search_sui(&self, query: &str) -> Result<Vec<Hit>, String> {
        let listed = self.list(Net::Sui).map(|l| rank(&l, query));
        let live = if query.len() >= 2 {
            self.dexscreener("sui", Net::Sui, query)
        } else {
            Ok(vec![])
        };
        match (listed, live) {
            (Ok(a), Ok(b)) => Ok(merge(a, b)),
            (Ok(a), Err(_)) => Ok(a),
            (Err(_), Ok(b)) => Ok(b),
            (Err(e), Err(_)) => Err(format!("Token search is unavailable right now ({e}).")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Canned {
        replies: Vec<(String, Result<String, String>)>,
        requested: RefCell<Vec<String>>,
    }

    impl Canned {
        fn new(replies: Vec<(&str, Result<&str, &str>)>) -> Rc<Canned> {
            Rc::new(Canned {
                replies: replies
                    .into_iter()
                    .map(|(u, r)| (u.to_string(), r.map(str::to_string).map_err(str::to_string)))
                    .collect(),
                requested: RefCell::new(vec![]),
            })
        }
    }

    impl HttpClient for Canned {
        fn get(&self, url: &str) -> Result<String, String> {
            self.requested.borrow_mut().push(url.to_string());
            self.replies
                .iter()
                .find(|(u, _)| url.starts_with(u.as_str()))
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| Err("no such url".to_string()))
        }

        fn post_json(&self, _: &str, _: &str) -> Result<String, String> {
            Err("unused".to_string())
        }
    }

    const USDC_ETH: &str = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48";
    const OPENOCEAN_ETH: &str = r#"{"code":200,"data":[
        {"address":"0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE","symbol":"ETH","name":"Ethereum","code":"ethereum"},
        {"address":"0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48","symbol":"USDC","name":"USD Coin","code":"usd-coin"},
        {"address":"0x1111111111111111111111111111111111111111","symbol":"USDCX","name":"USD Coin Fork","code":null},
        {"address":"0xdac17f958d2ee523a2206206994597c13d831ec7","symbol":"USDT","name":"Tether USD","code":"tether"},
        {"address":"not-an-address","symbol":"BAD","name":"Bad","code":null},
        {"address":"0x2222222222222222222222222222222222222222","symbol":"EVIL\u0007","name":"Name\nwith control","code":null}]}"#;

    #[test]
    fn encodes_queries_for_urls() {
        assert_eq!(encode_query("usd coin"), "usd%20coin");
        assert_eq!(encode_query("a&b=c/d"), "a%26b%3Dc%2Fd");
        assert_eq!(encode_query("ok-_.~9"), "ok-_.~9");
        assert_eq!(encode_query("é"), "%C3%A9");
    }

    #[test]
    fn validates_addresses_per_network() {
        assert!(valid_address(Net::Evm(1), USDC_ETH));
        assert!(valid_address(Net::Evm(8453), "native"));
        assert!(!valid_address(Net::Evm(1), "0x123"));
        assert!(!valid_address(
            Net::Evm(1),
            "0xzz86991c6218b36c1d19d4a2e9eb0ce3606eb48"
        ));
        assert!(valid_address(
            Net::Solana,
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
        ));
        assert!(!valid_address(Net::Solana, "0OIl"));
        assert!(!valid_address(Net::Solana, "short"));
        assert!(valid_address(
            Net::Sui,
            "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC"
        ));
        assert!(valid_address(Net::Sui, "0x2::sui::SUI"));
        assert!(!valid_address(
            Net::Sui,
            "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7"
        ));
        assert!(!valid_address(Net::Sui, "0xabc::usdc"));
        assert!(!valid_address(Net::Sui, "0xabc::us dc::USDC"));
        assert!(!valid_address(Net::Sui, "0xabc::usdc::USDC::extra"));
    }

    #[test]
    fn recognises_pasted_addresses() {
        assert!(looks_like_address(Net::Evm(1), &format!(" {USDC_ETH} ")));
        assert!(!looks_like_address(Net::Evm(1), "usdc"));
        assert!(!looks_like_address(Net::Evm(1), "native"));
    }

    #[test]
    fn native_coins_become_native() {
        assert_eq!(
            normalise(Net::Evm(1), "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE"),
            "native"
        );
        assert_eq!(normalise(Net::Solana, SOL_MINT), "native");
        assert_eq!(normalise(Net::Sui, "0x2::sui::SUI"), "native");
        assert_eq!(
            normalise(
                Net::Sui,
                "0x0000000000000000000000000000000000000000000000000000000000000002::sui::SUI"
            ),
            "native"
        );
        assert_eq!(normalise(Net::Evm(1), USDC_ETH), USDC_ETH);
    }

    #[test]
    fn parses_openocean_lists_and_drops_bad_rows() {
        let hits = parse_openocean(OPENOCEAN_ETH, Net::Evm(1)).unwrap();
        let symbols: Vec<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        assert_eq!(symbols, vec!["ETH", "USDC", "USDCX", "USDT", "EVIL"]);
        assert_eq!(hits[0].address, "native");
        assert!(hits[1].verified && !hits[2].verified);
        assert_eq!(hits[4].name, "Namewith control");
        assert!(parse_openocean("nope", Net::Evm(1)).is_err());
        assert!(parse_openocean(r#"{"code":500}"#, Net::Evm(1)).is_err());
    }

    #[test]
    fn sui_lists_use_the_full_coin_type() {
        let body = r#"{"data":[{"address":"0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7","customAddress":"0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC","symbol":"USDC","name":"USDC","code":"usd-coin"},
            {"address":"0xabc","symbol":"NOTYPE","name":"x","code":null}]}"#;
        let hits = parse_openocean(body, Net::Sui).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].address.ends_with("::usdc::USDC"));
    }

    #[test]
    fn ranks_exact_symbols_before_looser_matches() {
        let list = parse_openocean(OPENOCEAN_ETH, Net::Evm(1)).unwrap();
        let by = |q: &str| {
            rank(&list, q)
                .into_iter()
                .map(|h| h.symbol)
                .collect::<Vec<_>>()
        };
        assert_eq!(by("usdc"), vec!["USDC", "USDCX"]);
        assert_eq!(by("tether"), vec!["USDT"]);
        assert_eq!(by("coin"), vec!["USDC", "USDCX"]);
        assert_eq!(
            by("0xdac17f958d2ee523a2206206994597c13d831ec7"),
            vec!["USDT"]
        );
        assert!(by("zzz").is_empty());
        assert!(by("   ").is_empty());
    }

    #[test]
    fn jupiter_results_put_verified_and_deep_markets_first() {
        let body = r#"[
            {"id":"Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":"Bonkey","symbol":"Bonkey","liquidity":3044,"isVerified":null},
            {"id":"DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263","name":"Bonk","symbol":"Bonk","liquidity":5889077.4,"isVerified":true},
            {"id":"So11111111111111111111111111111111111111112","name":"Wrapped SOL","symbol":"SOL","liquidity":9000000,"isVerified":true},
            {"id":"bad id","name":"Bad","symbol":"BAD","liquidity":1,"isVerified":true}]"#;
        let hits = parse_jupiter(body).unwrap();
        let symbols: Vec<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        assert_eq!(symbols, vec!["SOL", "Bonk", "Bonkey"]);
        assert_eq!(hits[0].address, "native");
        assert_eq!(hits[1].note, "$5.9M liquidity");
        assert_eq!(hits[2].note, "$3K liquidity");
    }

    #[test]
    fn dexscreener_keeps_one_chain_and_real_markets() {
        let body = r#"{"pairs":[
            {"chainId":"sui","baseToken":{"address":"0xa99b8952d4f7d947ea77fe0ecdcc9e5fc0bcab2841d6e2a5aa00c3044e5544b5::navx::NAVX","name":"NAVX Token","symbol":"NAVX"},"liquidity":{"usd":167339.58}},
            {"chainId":"sui","baseToken":{"address":"0xa99b8952d4f7d947ea77fe0ecdcc9e5fc0bcab2841d6e2a5aa00c3044e5544b5::navx::NAVX","name":"NAVX Token","symbol":"NAVX"},"liquidity":{"usd":900000}},
            {"chainId":"sui","baseToken":{"address":"0x9c86d1926a0a39e906f20674d6a35f337be8625ebcb6b799ee8ff011f328bee2::liq::LIQ","name":"Liquor","symbol":"LIQ"},"liquidity":{"usd":0.16}},
            {"chainId":"base","baseToken":{"address":"0x1111111111111111111111111111111111111111","name":"Other","symbol":"OTH"},"liquidity":{"usd":5000000}},
            {"chainId":"sui","baseToken":{"address":"0xnotatype","name":"x","symbol":"X"},"liquidity":{"usd":5000000}}]}"#;
        let hits = parse_dexscreener(body, "sui", Net::Sui).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].symbol, "NAVX");
        assert_eq!(hits[0].note, "$900K liquidity");
        assert!(!hits[0].verified);
        assert!(parse_dexscreener("{}", "sui", Net::Sui).is_err());
    }

    #[test]
    fn hits_survive_the_trip_through_json() {
        let hits = parse_openocean(OPENOCEAN_ETH, Net::Evm(1)).unwrap();
        let back = hits_from_value(&json::parse(&hits_value(&hits).to_json()).unwrap());
        assert_eq!(back, hits);
        assert!(hits_from_value(&Value::Null).is_empty());
    }

    #[test]
    fn formats_liquidity() {
        assert_eq!(liquidity_note(999.0), "");
        assert_eq!(liquidity_note(1500.0), "$2K liquidity");
        assert_eq!(liquidity_note(2_500_000.0), "$2.5M liquidity");
        assert_eq!(liquidity_note(3.2e9), "$3.2B liquidity");
    }

    #[test]
    fn searches_an_evm_network_from_one_cached_list() {
        let http = Canned::new(vec![(
            "https://open-api.de1.exchange/v4/8453/tokenList",
            Ok(OPENOCEAN_ETH),
        )]);
        let search = TokenSearch::new(http.clone());
        let first = search.search(Net::Evm(8453), "usdt").unwrap();
        assert_eq!(first[0].symbol, "USDT");
        let second = search.search(Net::Evm(8453), "tether").unwrap();
        assert_eq!(second[0].symbol, "USDT");
        assert_eq!(http.requested.borrow().len(), 1, "the list is fetched once");
        assert!(search.search(Net::Evm(8453), "  ").unwrap().is_empty());
    }

    #[test]
    fn an_unlisted_evm_network_without_a_fallback_says_to_paste_the_address() {
        let http = Canned::new(vec![(
            "https://open-api.de1.exchange/v4/999999/tokenList",
            Err("HTTP status 404"),
        )]);
        let err = TokenSearch::new(http)
            .search(Net::Evm(999999), "usdc")
            .unwrap_err();
        assert!(err.contains("Paste the contract address"), "{err}");
    }

    #[test]
    fn an_evm_network_falls_back_to_markets_when_its_list_is_missing() {
        let http = Canned::new(vec![
            (
                "https://open-api.de1.exchange/v4/59144/tokenList",
                Err("HTTP status 500"),
            ),
            (
                "https://api.dexscreener.com/latest/dex/search?q=foo",
                Ok(
                    r#"{"pairs":[{"chainId":"linea","baseToken":{"address":"0x3333333333333333333333333333333333333333","name":"Foo","symbol":"FOO"},"liquidity":{"usd":50000}}]}"#,
                ),
            ),
        ]);
        let hits = TokenSearch::new(http)
            .search(Net::Evm(59144), "foo")
            .unwrap();
        assert_eq!(hits[0].symbol, "FOO");
    }

    #[test]
    fn solana_search_falls_back_to_the_second_host() {
        let http = Canned::new(vec![
            (
                "https://api.jup.ag/tokens/v2/search?query=bonk",
                Err("HTTP status 429"),
            ),
            (
                "https://lite-api.jup.ag/tokens/v2/search?query=bonk",
                Ok(
                    r#"[{"id":"DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263","name":"Bonk","symbol":"Bonk","liquidity":10000,"isVerified":true}]"#,
                ),
            ),
        ]);
        let hits = TokenSearch::new(http).search(Net::Solana, "bonk").unwrap();
        assert_eq!(hits[0].symbol, "Bonk");
    }

    #[test]
    fn solana_search_reports_when_every_host_fails() {
        let http = Canned::new(vec![]);
        let err = TokenSearch::new(http)
            .search(Net::Solana, "bonk")
            .unwrap_err();
        assert!(err.contains("unavailable"), "{err}");
    }

    #[test]
    fn sui_search_merges_the_curated_list_with_live_markets() {
        let listed = r#"{"data":[{"address":"0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7","customAddress":"0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC","symbol":"USDC","name":"USDC","code":"usd-coin"}]}"#;
        let live = r#"{"pairs":[
            {"chainId":"sui","baseToken":{"address":"0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC","name":"USDC","symbol":"USDC"},"liquidity":{"usd":9000000}},
            {"chainId":"sui","baseToken":{"address":"0xfa7ac3951fdca92c5200d468d31a365eb03b2be9936fde615e69f0c1274ad3a0::usdcfoo::USDCFOO","name":"USDC Foo","symbol":"USDCFOO"},"liquidity":{"usd":112627}},
            {"chainId":"sui","baseToken":{"address":"0x549e8b69270defbfafd4f94e17ec44cdbdd99820b33bda2278dea3b9a32d3f55::cert::CERT","name":"Volo Staked SUI","symbol":"vSUI"},"liquidity":{"usd":2303}}]}"#;
        let http = Canned::new(vec![
            ("https://open-api.de1.exchange/v4/sui/tokenList", Ok(listed)),
            (
                "https://api.dexscreener.com/latest/dex/search?q=usdc",
                Ok(live),
            ),
        ]);
        let hits = TokenSearch::new(http).search(Net::Sui, "usdc").unwrap();
        let symbols: Vec<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        assert_eq!(
            symbols,
            vec!["USDC", "USDCFOO"],
            "no duplicate USDC, and no pair that only mentions the query"
        );
        assert!(hits[0].verified);
    }

    #[test]
    fn sui_search_still_answers_when_one_source_is_down() {
        let live = r#"{"pairs":[{"chainId":"sui","baseToken":{"address":"0xfa7ac3951fdca92c5200d468d31a365eb03b2be9936fde615e69f0c1274ad3a0::blub::BLUB","name":"BLUB","symbol":"BLUB"},"liquidity":{"usd":112627}}]}"#;
        let http = Canned::new(vec![
            (
                "https://open-api.de1.exchange/v4/sui/tokenList",
                Err("HTTP status 502"),
            ),
            (
                "https://api.dexscreener.com/latest/dex/search?q=blub",
                Ok(live),
            ),
        ]);
        let hits = TokenSearch::new(http).search(Net::Sui, "blub").unwrap();
        assert_eq!(hits[0].symbol, "BLUB");
    }

    /// Hits the real services: `cargo test live -- --ignored --nocapture`.
    fn live() -> TokenSearch {
        TokenSearch::new(Rc::new(agent_saifu_core::http::UreqClient::new()))
    }

    fn show(hits: &[Hit]) {
        for h in hits.iter().take(5) {
            println!(
                "   {} | {} | {} | verified={} | {}",
                h.symbol, h.name, h.address, h.verified, h.note
            );
        }
    }

    #[test]
    #[ignore = "needs the network"]
    fn live_ethereum_and_layer_twos() {
        for (chain, query, expect) in [
            (1, "usd coin", "USDC"),
            (8453, "usdc", "USDC"),
            (42161, "arb", "ARB"),
            (10, "op", "OP"),
            (137, "matic", ""),
        ] {
            let hits = live().search(Net::Evm(chain), query).unwrap();
            println!("chain {chain} '{query}': {} hits", hits.len());
            show(&hits);
            if !expect.is_empty() {
                assert!(
                    hits.iter().any(|h| h.symbol.eq_ignore_ascii_case(expect)),
                    "{chain} {query}"
                );
            }
        }
    }

    #[test]
    #[ignore = "needs the network"]
    fn live_solana() {
        let hits = live().search(Net::Solana, "bonk").unwrap();
        show(&hits);
        assert!(hits.iter().any(|h| h.symbol == "Bonk" && h.verified));
        let usdc = live().search(Net::Solana, "usdc").unwrap();
        assert!(
            usdc.iter()
                .any(|h| h.address == "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v")
        );
    }

    #[test]
    #[ignore = "needs the network"]
    fn live_sui() {
        for (query, expect) in [
            ("usdc", "::usdc::USDC"),
            ("navx", "::navx::NAVX"),
            ("cetus", "::cetus::CETUS"),
            ("deep", "::deep::DEEP"),
            ("sui", "native"),
        ] {
            let hits = live().search(Net::Sui, query).unwrap();
            println!("sui '{query}': {} hits", hits.len());
            show(&hits);
            assert!(
                hits.iter()
                    .any(|h| h.address.ends_with(expect) || h.address == expect),
                "{query}"
            );
        }
    }

    #[test]
    fn parses_network_names() {
        assert_eq!(Net::parse("ethereum", Some(8453)), Some(Net::Evm(8453)));
        assert_eq!(Net::parse("ethereum", None), Some(Net::Evm(1)));
        assert_eq!(Net::parse("sui", None), Some(Net::Sui));
        assert_eq!(Net::parse("bitcoin", None), None);
    }
}
