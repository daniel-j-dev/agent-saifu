//! Swaps on Sui through the Cetus aggregator (https://cetus-1.gitbook.io/cetus-developer-docs).
//!
//! The aggregator's public API (`router_v3/find_routes`) only *finds* the best route; the wallet
//! builds the transaction itself and the aggregator's Move contract enforces the result on-chain:
//! `router::new_swap_context_v3` records the quoted output, the allowed slippage and the fee
//! (the "overlay fee", paid to `sui_swap_config::SUI_SWAP_FEE_RECIPIENT`), each hop calls a pool,
//! and `router::confirm_swap_v3` aborts the whole transaction if the output is too low.
//!
//! Only routes made purely of Cetus's own concentrated-liquidity pools (provider `CETUS`) are
//! used: that is the one provider whose transaction calls are built and verified here. Every
//! package and shared object a route touches is checked against Cetus's published on-chain
//! packages before anything is signed, so a dishonest API cannot point the wallet at other code.

use std::collections::HashMap;

use sui_sdk_types::{Address, Identifier, StructTag, TypeTag};
use sui_transaction_builder::{Argument, Function, ObjectInput, TransactionBuilder};

use super::coins::{self, normalize_coin_type};
use super::gql::Gql;
use crate::eth::explorer::sanitize_text_max;
use crate::http::HttpClient;
use crate::json::{self, Value};
use crate::sui_swap_config;

/// Original (first-published) package IDs from Cetus's documentation. Upgrades keep the original
/// ID, so the newest version is resolved on-chain and routes must use exactly that.
pub const AGGREGATOR_V3_PACKAGE: &str =
    "0x33ec64e9bb369bf045ddc198c81adbf2acab424da37465d95296ee02045d2b17";
pub const CETUS_ROUTER_PACKAGE: &str =
    "0xb1e11ceaf3e7cd3031ef5e24804478ec3441c5aecdace910bdaca317a0c1c535";
/// Cetus's concentrated-liquidity (CLMM) package: every pool, the global config and the partner
/// object must be of types it defines.
pub const CLMM_PACKAGE: &str = "0x1eabed72c53feb3805120a081dc15963c204dc8d091542592abaf7a35689b2fb";
pub const GLOBAL_CONFIG: &str =
    "0xdaa46292632c3c4d8f31f23ea0f9b36a28ff3677e9684980e4438403a67a3d8f";
pub const PARTNER: &str = "0x639b5e433da31739e800cd085f356e64cae222966d0f1b11bd9dc76b322ff58b";
pub const CLOCK: &str = "0x6";

pub const PROVIDER: &str = "CETUS";
const API_VERSION: &str = "1010705";
pub const FEE_DENOMINATOR: u64 = 1_000_000;
pub const MAX_FEE_BPS: u32 = 1000;
pub const MIN_SLIPPAGE_BPS: u64 = 5;
pub const MAX_SLIPPAGE_BPS: u64 = 1000;
pub const DEFAULT_SLIPPAGE_BPS: u64 = 100;
/// Swaps whose quoted price is this far from the market price are refused.
pub const MAX_PRICE_DEVIATION: f64 = 0.05;
pub const WARN_PRICE_DEVIATION: f64 = 0.01;
const MAX_PATHS: usize = 16;

fn address_const(text: &str) -> Address {
    text.parse().expect("constant addresses are valid")
}

// ---- fee and settings ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuiSwapFee {
    pub recipient: Address,
    pub bps: u32,
}

impl SuiSwapFee {
    pub fn new(recipient: &str, bps: u32) -> Result<SuiSwapFee, String> {
        let recipient = coins::parse_address(recipient)
            .map_err(|e| format!("SUI_SWAP_FEE_RECIPIENT in sui_swap_config.rs: {e}"))?;
        if recipient == Address::ZERO {
            return Err(
                "SUI_SWAP_FEE_RECIPIENT in sui_swap_config.rs must not be the zero address"
                    .to_string(),
            );
        }
        if !(1..=MAX_FEE_BPS).contains(&bps) {
            return Err(format!(
                "SUI_SWAP_FEE_BPS in sui_swap_config.rs must be between 1 and {MAX_FEE_BPS}"
            ));
        }
        Ok(SuiSwapFee { recipient, bps })
    }

    /// `Ok(None)` while no recipient is configured.
    pub fn from_config() -> Result<Option<SuiSwapFee>, String> {
        let recipient = sui_swap_config::SUI_SWAP_FEE_RECIPIENT.trim();
        if recipient.is_empty() {
            return Ok(None);
        }
        SuiSwapFee::new(recipient, sui_swap_config::SUI_SWAP_FEE_BPS).map(Some)
    }

    /// The contract's fee rate: millionths of the output.
    pub fn rate(&self) -> u32 {
        self.bps * 100
    }

    pub fn amount_for(&self, gross_out: u64) -> u64 {
        fee_amount(gross_out, self.rate())
    }
}

pub fn fee_amount(gross_out: u64, rate: u32) -> u64 {
    (u128::from(gross_out) * u128::from(rate) / u128::from(FEE_DENOMINATOR)) as u64
}

#[derive(Debug, Clone)]
pub struct CetusSettings {
    pub fee: Option<SuiSwapFee>,
    pub base_url: String,
}

impl Default for CetusSettings {
    fn default() -> Self {
        CetusSettings {
            fee: None,
            base_url: sui_swap_config::CETUS_AGGREGATOR_URL.to_string(),
        }
    }
}

impl CetusSettings {
    pub fn new(fee: Option<SuiSwapFee>) -> CetusSettings {
        CetusSettings {
            fee,
            ..CetusSettings::default()
        }
    }

    pub fn from_config() -> Result<CetusSettings, String> {
        let settings = CetusSettings::new(SuiSwapFee::from_config()?);
        if !settings.base_url.starts_with("https://") {
            return Err(
                "CETUS_AGGREGATOR_URL in sui_swap_config.rs must start with https://".to_string(),
            );
        }
        Ok(settings)
    }
}

// ---- the aggregator's answer ----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub pool: Address,
    pub provider: String,
    pub from: String,
    pub target: String,
    /// `true` swaps coin A for coin B of the pool.
    pub direction: bool,
    pub amount_in: u64,
    pub amount_out: u64,
    pub published_at: Address,
    pub fee_rate: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub quote_id: String,
    pub amount_in: u64,
    pub amount_out: u64,
    pub deviation: f64,
    pub paths: Vec<Path>,
    pub aggregator_package: Address,
}

fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.:~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn route_url(base: &str, from: &str, target: &str, amount: u64) -> String {
    format!(
        "{}/find_routes?from={}&target={}&amount={amount}&by_amount_in=true&providers={PROVIDER}&v={API_VERSION}",
        base.trim_end_matches('/'),
        percent_encode(from),
        percent_encode(target)
    )
}

fn field<'a>(v: &'a Value, key: &str) -> Result<&'a Value, String> {
    v.get(key)
        .ok_or_else(|| format!("the Cetus route has no '{key}'"))
}

fn text_field(v: &Value, key: &str) -> Result<String, String> {
    field(v, key)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("the Cetus route's '{key}' is not text"))
}

fn amount_field(v: &Value, key: &str) -> Result<u64, String> {
    field(v, key)?
        .as_u64()
        .ok_or_else(|| format!("the Cetus route's '{key}' is not a whole number"))
}

fn address_field(v: &Value, key: &str) -> Result<Address, String> {
    text_field(v, key)?
        .parse()
        .map_err(|_| format!("the Cetus route's '{key}' is not an address"))
}

fn parse_path(v: &Value) -> Result<Path, String> {
    Ok(Path {
        pool: address_field(v, "id")?,
        provider: text_field(v, "provider")?,
        from: normalize_coin_type(&text_field(v, "from")?)?,
        target: normalize_coin_type(&text_field(v, "target")?)?,
        direction: field(v, "direction")?
            .as_bool()
            .ok_or("the Cetus route's 'direction' is not true/false")?,
        amount_in: amount_field(v, "amount_in")?,
        amount_out: amount_field(v, "amount_out")?,
        published_at: address_field(v, "published_at")?,
        fee_rate: sanitize_text_max(&text_field(v, "fee_rate").unwrap_or_default(), 16),
    })
}

/// Reads the aggregator's JSON answer (without trusting it; see `check_route`).
pub fn parse_route(text: &str) -> Result<Route, String> {
    let doc = json::parse(text)
        .map_err(|_| "the Cetus aggregator returned an answer that is not JSON".to_string())?;
    let code = doc.get("code").and_then(Value::as_u64).unwrap_or(0);
    let data = match doc.get("data") {
        Some(d) if code == 200 && !matches!(d, Value::Null) => d,
        _ => {
            let msg = doc.get("msg").and_then(Value::as_str).unwrap_or("no route");
            return Err(format!(
                "Cetus found no route (code {code}): {}",
                sanitize_text_max(msg, 200)
            ));
        }
    };
    let paths = field(data, "paths")?
        .as_array()
        .ok_or("the Cetus route's 'paths' is not a list")?
        .iter()
        .map(parse_path)
        .collect::<Result<Vec<_>, _>>()?;
    let aggregator_package = data
        .get("packages")
        .map(|p| address_field(p, "aggregator_v3"))
        .ok_or("the Cetus route names no aggregator package")??;
    let deviation = data
        .get("deviation_ratio")
        .and_then(|d| match d {
            Value::String(s) | Value::Number(s) => s.parse::<f64>().ok(),
            _ => None,
        })
        .unwrap_or(0.0);
    Ok(Route {
        quote_id: sanitize_text_max(&text_field(data, "request_id")?, 64),
        amount_in: amount_field(data, "amount_in")?,
        amount_out: amount_field(data, "amount_out")?,
        deviation,
        paths,
        aggregator_package,
    })
}

pub fn fetch_route(
    http: &dyn HttpClient,
    settings: &CetusSettings,
    from: &str,
    target: &str,
    amount: u64,
) -> Result<Route, String> {
    let url = route_url(&settings.base_url, from, target, amount);
    let text = http
        .get(&url)
        .map_err(|e| format!("cannot reach the Cetus aggregator: {e}"))?;
    parse_route(&text)
}

// ---- checking a route ---------------------------------------------------------------------------

/// Checks that a route does what was asked: sells exactly the requested coin and amount, ends in
/// the requested coin, uses only Cetus pools, and never spends a coin before it has been produced.
pub fn check_route_shape(
    route: &Route,
    from: &str,
    to: &str,
    amount_in: u64,
) -> Result<(), String> {
    if route.paths.is_empty() || route.paths.len() > MAX_PATHS {
        return Err("the Cetus route has no hops or too many hops".to_string());
    }
    if route.amount_in != amount_in {
        return Err(
            "the Cetus route sells a different amount than requested; refusing".to_string(),
        );
    }
    if route.amount_out == 0 {
        return Err("the Cetus route would deliver nothing".to_string());
    }
    if from == to {
        return Err("from_token and to_token are the same coin".to_string());
    }
    let mut held: HashMap<&str, u128> = HashMap::new();
    held.insert(from, u128::from(amount_in));
    let mut sold_from = 0u128;
    let mut bought_to = 0u128;
    for path in &route.paths {
        if path.provider != PROVIDER {
            return Err(format!(
                "the Cetus route uses provider '{}'; only Cetus pools are supported",
                sanitize_text_max(&path.provider, 24)
            ));
        }
        if path.from == path.target {
            return Err("the Cetus route has a hop between identical coins".to_string());
        }
        if path.target == from {
            return Err("the Cetus route buys back the coin being sold".to_string());
        }
        let available = held.get(path.from.as_str()).copied().unwrap_or(0);
        if u128::from(path.amount_in) > available {
            return Err("the Cetus route spends a coin before it has been produced".to_string());
        }
        held.insert(path.from.as_str(), available - u128::from(path.amount_in));
        *held.entry(path.target.as_str()).or_insert(0) += u128::from(path.amount_out);
        if path.from == from {
            sold_from += u128::from(path.amount_in);
        }
        if path.target == to {
            bought_to += u128::from(path.amount_out);
        }
    }
    if sold_from != u128::from(amount_in) {
        return Err(
            "the Cetus route does not sell exactly the requested amount; refusing".to_string(),
        );
    }
    if bought_to != u128::from(route.amount_out) {
        return Err(
            "the Cetus route's hops do not add up to its quoted output; refusing".to_string(),
        );
    }
    if route.paths.last().map(|p| p.target.as_str()) != Some(to) {
        return Err("the Cetus route does not end in the requested coin".to_string());
    }
    if route.deviation.abs() > MAX_PRICE_DEVIATION {
        return Err(format!(
            "the quoted price is {:.2}% away from the market price; refusing to swap",
            route.deviation.abs() * 100.0
        ));
    }
    Ok(())
}

/// Hop order with a flag for the last hop that spends each coin (it takes the whole balance,
/// like the official SDK, so rounding never strands dust inside the swap).
pub fn last_use_flags(paths: &[Path]) -> Vec<bool> {
    let mut seen: Vec<&str> = vec![];
    let mut flags = vec![false; paths.len()];
    for (i, path) in paths.iter().enumerate().rev() {
        if !seen.contains(&path.from.as_str()) {
            seen.push(path.from.as_str());
            flags[i] = true;
        }
    }
    flags
}

/// A route whose packages and objects have been verified on-chain.
#[derive(Debug, Clone)]
pub struct Verified {
    pub route: Route,
    pub router_package: Address,
    pub aggregator_package: Address,
    pub shared: HashMap<Address, u64>,
}

fn struct_type(repr: &str) -> Option<StructTag> {
    repr.parse().ok()
}

fn is_clmm_type(repr: &str, module: &str, name: &str) -> Option<StructTag> {
    let tag = struct_type(repr)?;
    (*tag.address() == address_const(CLMM_PACKAGE)
        && tag.module().as_str() == module
        && tag.name().as_str() == name)
        .then_some(tag)
}

fn shared_object(
    gql: &Gql<'_>,
    id: &Address,
    module: &str,
    name: &str,
) -> Result<(u64, StructTag), String> {
    let info = gql
        .object(id)?
        .ok_or_else(|| "a Cetus object does not exist on-chain".to_string())?;
    let tag = info
        .type_repr
        .as_deref()
        .and_then(|t| is_clmm_type(t, module, name))
        .ok_or_else(|| format!("a Cetus object is not a Cetus {module}::{name}; refusing"))?;
    let version = info
        .initial_shared_version
        .ok_or("a Cetus object is not shared")?;
    Ok((version, tag))
}

/// Confirms on-chain that the route's code and objects are Cetus's.
pub fn verify_on_chain(gql: &Gql<'_>, route: Route) -> Result<Verified, String> {
    let latest = |original: &str, what: &str| -> Result<Address, String> {
        gql.latest_package(&address_const(original))?
            .ok_or_else(|| format!("the Cetus {what} package was not found on-chain"))
    };
    let router_package = latest(CETUS_ROUTER_PACKAGE, "router")?;
    let aggregator_package = latest(AGGREGATOR_V3_PACKAGE, "aggregator")?;
    if route.aggregator_package != aggregator_package {
        return Err(
            "the Cetus route names an aggregator package that is not Cetus's current one; refusing"
                .to_string(),
        );
    }
    let mut shared = HashMap::new();
    for path in &route.paths {
        if path.published_at != router_package {
            return Err(
                "a hop calls a package that is not Cetus's current router; refusing".to_string(),
            );
        }
        if shared.contains_key(&path.pool) {
            continue;
        }
        let (version, tag) = shared_object(gql, &path.pool, "pool", "Pool")?;
        let expected: Vec<String> = if path.direction {
            vec![path.from.clone(), path.target.clone()]
        } else {
            vec![path.target.clone(), path.from.clone()]
        };
        let actual: Vec<String> = tag.type_params().iter().map(|t| t.to_string()).collect();
        if actual != expected {
            return Err(
                "a hop's pool does not hold the coins the route claims; refusing".to_string(),
            );
        }
        shared.insert(path.pool, version);
    }
    for (id, module, name) in [
        (GLOBAL_CONFIG, "config", "GlobalConfig"),
        (PARTNER, "partner", "Partner"),
    ] {
        let id = address_const(id);
        let (version, _) = shared_object(gql, &id, module, name)?;
        shared.insert(id, version);
    }
    Ok(Verified {
        route,
        router_package,
        aggregator_package,
        shared,
    })
}

// ---- amounts ------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Amounts {
    pub amount_in: u64,
    /// What the pools pay out before the service fee.
    pub gross_out: u64,
    pub fee: u64,
    /// What the wallet should receive: output minus the service fee.
    pub expected_out: u64,
    /// The least it will accept; the contract aborts the swap below this.
    pub min_out: u64,
    pub slippage_bps: u64,
}

pub fn parse_slippage(bps: Option<u64>) -> Result<u64, String> {
    let bps = bps.unwrap_or(DEFAULT_SLIPPAGE_BPS);
    if !(MIN_SLIPPAGE_BPS..=MAX_SLIPPAGE_BPS).contains(&bps) {
        return Err(format!(
            "slippage_bps must be between {MIN_SLIPPAGE_BPS} and {MAX_SLIPPAGE_BPS} (100 = 1%)"
        ));
    }
    Ok(bps)
}

pub fn compute_amounts(route: &Route, fee: Option<&SuiSwapFee>, slippage_bps: u64) -> Amounts {
    let fee_value = fee.map_or(0, |f| f.amount_for(route.amount_out));
    let expected_out = route.amount_out - fee_value;
    let min_out = (u128::from(expected_out) * u128::from(10_000 - slippage_bps) / 10_000) as u64;
    Amounts {
        amount_in: route.amount_in,
        gross_out: route.amount_out,
        fee: fee_value,
        expected_out,
        min_out,
        slippage_bps,
    }
}

// ---- building the transaction -------------------------------------------------------------------

fn function(
    package: Address,
    module: &str,
    name: &str,
    types: Vec<TypeTag>,
) -> Result<Function, String> {
    let module = Identifier::new(module).map_err(|_| "invalid module name".to_string())?;
    let name = Identifier::new(name).map_err(|_| "invalid function name".to_string())?;
    Ok(Function::new(package, module, name).with_type_args(types))
}

fn coin_tag(coin_type: &str) -> Result<TypeTag, String> {
    coin_type
        .parse::<StructTag>()
        .map(TypeTag::from)
        .map_err(|_| "invalid coin type".to_string())
}

/// Appends the swap: open a swap context holding the input coin, call each pool, confirm (which
/// enforces the minimum on-chain and pays the fee), and send the result to `recipient`.
pub fn add_swap(
    tx: &mut TransactionBuilder,
    verified: &Verified,
    amounts: &Amounts,
    fee: Option<&SuiSwapFee>,
    input_coin: Argument,
    recipient: Address,
) -> Result<(), String> {
    let route = &verified.route;
    let from = route.paths[0].from.clone();
    let to = route
        .paths
        .last()
        .map(|p| p.target.clone())
        .ok_or("route has no hops")?;
    let (fee_rate, fee_recipient) = match fee {
        Some(f) => (f.rate(), f.recipient),
        None => (0, Address::ZERO),
    };
    let quote_id = tx.pure(&route.quote_id);
    let max_in = tx.pure(&u64::MAX);
    let quote_in = tx.pure(&amounts.amount_in);
    let quote_out = tx.pure(&amounts.expected_out);
    let slippage = tx.pure(&(amounts.slippage_bps * 100));
    let rate = tx.pure(&fee_rate);
    let receiver = tx.pure(&fee_recipient);
    let context = tx.move_call(
        function(
            verified.aggregator_package,
            "router",
            "new_swap_context_v3",
            vec![coin_tag(&from)?, coin_tag(&to)?],
        )?,
        vec![
            quote_id, max_in, quote_in, quote_out, slippage, input_coin, rate, receiver,
        ],
    );
    let shared = |tx: &mut TransactionBuilder, id: &Address, mutable: bool| {
        let version = verified
            .shared
            .get(id)
            .copied()
            .ok_or("missing shared object version")?;
        Ok::<Argument, String>(tx.object(ObjectInput::shared(*id, version, mutable)))
    };
    let flags = last_use_flags(&route.paths);
    for (path, last_use) in route.paths.iter().zip(flags) {
        let config = shared(tx, &address_const(GLOBAL_CONFIG), true)?;
        let pool = shared(tx, &path.pool, true)?;
        let partner = shared(tx, &address_const(PARTNER), true)?;
        let direction = tx.pure(&path.direction);
        let amount = tx.pure(&if last_use { u64::MAX } else { path.amount_in });
        let clock = tx.object(ObjectInput::shared(address_const(CLOCK), 1, false));
        let (coin_a, coin_b) = if path.direction {
            (&path.from, &path.target)
        } else {
            (&path.target, &path.from)
        };
        tx.move_call(
            function(
                path.published_at,
                "cetus",
                "swap",
                vec![coin_tag(coin_a)?, coin_tag(coin_b)?],
            )?,
            vec![context, config, pool, partner, direction, amount, clock],
        );
    }
    let output = tx.move_call(
        function(
            verified.aggregator_package,
            "router",
            "confirm_swap_v3",
            vec![coin_tag(&to)?],
        )?,
        vec![context],
    );
    let to_address = tx.pure(&recipient);
    tx.transfer_objects(vec![output], to_address);
    Ok(())
}

pub fn route_description(route: &Route) -> Value {
    Value::Array(
        route
            .paths
            .iter()
            .map(|p| {
                Value::object(vec![
                    ("provider", Value::str(&p.provider)),
                    ("pool", Value::String(p.pool.to_string())),
                    ("from", Value::String(coins::fallback_symbol(&p.from))),
                    ("to", Value::String(coins::fallback_symbol(&p.target))),
                    ("fee_rate", Value::str(&p.fee_rate)),
                ])
            })
            .collect(),
    )
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::sui::coins::SUI_COIN_TYPE;
    use crate::sui::network::SuiNetwork;
    use crate::sui::testing::SuiMock;

    pub const USDC: &str =
        "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC";
    pub const BUCK: &str =
        "0xce7ff77a83ea0cb6fd39bd8748e2ec89a3f41e8efdc3f4eb123e0ca37b184db2::buck::BUCK";
    pub const ROUTER_LATEST: &str =
        "0x87b17d7f0b448b83ab934a13eb64b05b60e17d0a371e2a721c241bcf3f5ec9e7";
    pub const AGG_LATEST: &str =
        "0xffd4058af7d6f6d66c335930cced8c91e38ec943e2392232aee27e8127e684e9";
    pub const POOL1: &str = "0x59cf0d333464ad29443d92bfd2ddfd1f794c5830141a5ee4a815d1ef3395bf6c";
    pub const POOL2: &str = "0x4c50ba9d1e60d229800293a4222851c9c3f797aa5ba8a8d32cc67ec7e79fec60";

    pub fn api_answer(amount_out_mid: u64, amount_out: u64) -> String {
        format!(
            r#"{{"code":200,"msg":"Success","data":{{"request_id":"quote-1","amount_in":1000000000,"amount_out":{amount_out},"deviation_ratio":"-0.00008184","paths":[
            {{"id":"{POOL1}","provider":"CETUS","from":"{SUI_COIN_TYPE}","target":"{BUCK}","direction":false,"fee_rate":"0.0005","lot_size":1,"amount_in":1000000000,"amount_out":{amount_out_mid},"published_at":"{ROUTER_LATEST}","extended_details":{{}}}},
            {{"id":"{POOL2}","provider":"CETUS","from":"{BUCK}","target":"{USDC}","direction":false,"fee_rate":"0.0001","lot_size":1,"amount_in":{amount_out_mid},"amount_out":{amount_out},"published_at":"{ROUTER_LATEST}","extended_details":{{}}}}],
            "packages":{{"aggregator_v3":"{AGG_LATEST}"}},"gas":35000}}}}"#
        )
    }

    pub fn route() -> Route {
        parse_route(&api_answer(962_476_346, 960_871)).unwrap()
    }

    #[test]
    fn parses_the_aggregators_answer() {
        let r = route();
        assert_eq!(r.quote_id, "quote-1");
        assert_eq!((r.amount_in, r.amount_out), (1_000_000_000, 960_871));
        assert_eq!(r.paths.len(), 2);
        assert_eq!(r.paths[0].from, SUI_COIN_TYPE);
        assert!(!r.paths[0].direction);
        assert_eq!(r.paths[1].amount_in, 962_476_346);
        assert!((r.deviation + 0.00008184).abs() < 1e-12);
        assert_eq!(r.aggregator_package.to_string(), AGG_LATEST);
    }

    #[test]
    fn answers_without_a_route_become_readable_errors() {
        let err =
            parse_route(r#"{"code":5010,"msg":"liquidity is not enough, no sub_graph found."}"#)
                .unwrap_err();
        assert!(err.contains("no route") && err.contains("5010") && err.contains("liquidity"));
        assert!(parse_route("<html>").is_err());
        assert!(parse_route(r#"{"code":200,"data":null}"#).is_err());
        let long = format!(r#"{{"code":1,"msg":"{}"}}"#, "x".repeat(900));
        assert!(parse_route(&long).unwrap_err().len() < 300);
    }

    #[test]
    fn malformed_routes_are_rejected() {
        let good = api_answer(1, 1);
        for broken in [
            good.replace("\"amount_in\":1000000000", "\"amount_in\":\"abc\""),
            good.replace("\"direction\":false", "\"direction\":\"no\""),
            good.replace("\"published_at\"", "\"published\""),
            good.replace("\"aggregator_v3\"", "\"other\""),
            good.replace(POOL1, "0xnothex"),
        ] {
            assert!(parse_route(&broken).is_err());
        }
    }

    #[test]
    fn request_url_asks_for_cetus_pools_only() {
        let url = route_url("https://api.example/router_v3/", SUI_COIN_TYPE, USDC, 5);
        assert!(url.starts_with("https://api.example/router_v3/find_routes?from=0x0000"));
        assert!(url.contains("&amount=5&by_amount_in=true&providers=CETUS&v="));
        assert!(!url.contains('<') && !url.contains(' '));
        assert_eq!(percent_encode("a<b, c>"), "a%3Cb%2C%20c%3E");
    }

    #[test]
    fn a_good_route_passes_the_shape_check() {
        assert!(check_route_shape(&route(), SUI_COIN_TYPE, USDC, 1_000_000_000).is_ok());
    }

    #[test]
    fn shape_check_refuses_dishonest_or_unsupported_routes() {
        let ok = route();
        let check = |r: &Route| check_route_shape(r, SUI_COIN_TYPE, USDC, 1_000_000_000);

        // wrong sell amount
        assert!(check_route_shape(&ok, SUI_COIN_TYPE, USDC, 999).is_err());
        // wrong target coin
        assert!(check_route_shape(&ok, SUI_COIN_TYPE, BUCK, 1_000_000_000).is_err());
        // other providers are not built here
        let mut r = ok.clone();
        r.paths[1].provider = "FLOWXV3".to_string();
        assert!(check(&r).unwrap_err().contains("only Cetus"));
        // spends more of the middle coin than was produced
        let mut r = ok.clone();
        r.paths[1].amount_in += 1;
        assert!(
            check(&r)
                .unwrap_err()
                .contains("before it has been produced")
        );
        // sells more than requested through a hop
        let mut r = ok.clone();
        r.paths[0].amount_in = 2_000_000_000;
        assert!(check(&r).is_err());
        // output that does not add up
        let mut r = ok.clone();
        r.amount_out += 1;
        assert!(check(&r).unwrap_err().contains("add up"));
        // zero output, empty and too long routes
        let mut r = ok.clone();
        r.amount_out = 0;
        assert!(check(&r).is_err());
        let mut r = ok.clone();
        r.paths.clear();
        assert!(check(&r).is_err());
        let mut r = ok.clone();
        r.paths = vec![ok.paths[0].clone(); MAX_PATHS + 1];
        assert!(check(&r).is_err());
        // buying back the coin being sold
        let mut r = ok.clone();
        r.paths[1].target = SUI_COIN_TYPE.to_string();
        assert!(check(&r).is_err());
        // a hop between identical coins
        let mut r = ok.clone();
        r.paths[1].target = BUCK.to_string();
        assert!(check(&r).is_err());
        // same coin both sides
        assert!(check_route_shape(&ok, SUI_COIN_TYPE, SUI_COIN_TYPE, 1_000_000_000).is_err());
        // price far from the market
        let mut r = ok;
        r.deviation = 0.2;
        assert!(check(&r).unwrap_err().contains("market price"));
    }

    #[test]
    fn split_routes_pass_when_the_parts_add_up() {
        let mut r = route();
        let mut second = r.paths[0].clone();
        r.paths[0].amount_in = 400_000_000;
        r.paths[0].amount_out = 385_000_000;
        second.amount_in = 600_000_000;
        second.amount_out = 577_476_346;
        r.paths.insert(1, second);
        assert!(check_route_shape(&r, SUI_COIN_TYPE, USDC, 1_000_000_000).is_ok());
        // Each coin's final use is flagged, so the last hop out of BUCK takes everything.
        assert_eq!(last_use_flags(&r.paths), vec![false, true, true]);
    }

    #[test]
    fn last_use_flags_mark_the_final_spender_of_each_coin() {
        assert_eq!(last_use_flags(&route().paths), vec![true, true]);
        assert!(last_use_flags(&[]).is_empty());
    }

    #[test]
    fn fees_and_minimums_are_computed_from_the_quote() {
        let r = route();
        let fee = SuiSwapFee::new(&format!("0x{}", "ab".repeat(32)), 70).unwrap();
        assert_eq!(fee.rate(), 7000);
        let a = compute_amounts(&r, Some(&fee), 100);
        assert_eq!(a.gross_out, 960_871);
        assert_eq!(a.fee, 6_726);
        assert_eq!(a.expected_out, 954_145);
        assert_eq!(a.min_out, 944_603);
        let none = compute_amounts(&r, None, 50);
        assert_eq!(
            (none.fee, none.expected_out, none.min_out),
            (0, 960_871, 956_066)
        );
    }

    #[test]
    fn fee_validation() {
        let good = format!("0x{}", "ab".repeat(32));
        assert!(SuiSwapFee::new(&good, 1).is_ok());
        assert!(SuiSwapFee::new(&good, 1000).is_ok());
        assert!(SuiSwapFee::new(&good, 0).is_err());
        assert!(SuiSwapFee::new(&good, 1001).is_err());
        assert!(SuiSwapFee::new("0x2", 70).is_err());
        assert!(SuiSwapFee::new("0xf8564095b5b0EdFc8f334b82BF04a57B27dcA4CD", 70).is_err());
        assert!(SuiSwapFee::new(&format!("0x{}", "00".repeat(32)), 70).is_err());
        assert_eq!(
            fee_amount(u64::MAX, 100_000),
            (u128::from(u64::MAX) / 10) as u64
        );
    }

    #[test]
    fn the_shipped_configuration_is_valid() {
        assert!(CetusSettings::from_config().is_ok());
        assert!(sui_swap_config::CETUS_AGGREGATOR_URL.starts_with("https://"));
    }

    #[test]
    fn slippage_limits() {
        assert_eq!(parse_slippage(None), Ok(100));
        assert_eq!(parse_slippage(Some(5)), Ok(5));
        assert!(parse_slippage(Some(4)).is_err());
        assert!(parse_slippage(Some(1001)).is_err());
    }

    pub fn pool_type(a: &str, b: &str) -> String {
        format!("{CLMM_PACKAGE}::pool::Pool<{a},{b}>")
    }

    /// Answers the on-chain lookups a Cetus route needs: its packages, pools, config and partner.
    pub fn chain(pool1_type: &str) -> SuiMock {
        let object = |ty: &str, version: u64| {
            format!(
                r#"{{"object":{{"version":99,"owner":{{"__typename":"Shared","initialSharedVersion":{version}}},"asMoveObject":{{"contents":{{"type":{{"repr":"{ty}"}}}}}}}}}}"#
            )
        };
        SuiMock::new()
            .data(
                &format!("\"{CETUS_ROUTER_PACKAGE}\""),
                &format!(r#"{{"package":{{"address":"{ROUTER_LATEST}"}}}}"#),
            )
            .data(
                &format!("\"{AGGREGATOR_V3_PACKAGE}\""),
                &format!(r#"{{"package":{{"address":"{AGG_LATEST}"}}}}"#),
            )
            .data(&format!("\"{POOL1}\""), &object(pool1_type, 11))
            .data(&format!("\"{POOL2}\""), &object(&pool_type(USDC, BUCK), 22))
            .data(
                &format!("\"{GLOBAL_CONFIG}\""),
                &object(&format!("{CLMM_PACKAGE}::config::GlobalConfig"), 33),
            )
            .data(
                &format!("\"{PARTNER}\""),
                &object(&format!("{CLMM_PACKAGE}::partner::Partner"), 44),
            )
    }

    pub fn verified() -> Verified {
        let mock = chain(&pool_type(BUCK, SUI_COIN_TYPE));
        verify_on_chain(&Gql::new(&mock, SuiNetwork::Mainnet), route()).unwrap()
    }

    #[test]
    fn verification_collects_the_current_packages_and_shared_versions() {
        let v = verified();
        assert_eq!(v.router_package.to_string(), ROUTER_LATEST);
        assert_eq!(v.aggregator_package.to_string(), AGG_LATEST);
        assert_eq!(v.shared[&address_const(POOL1)], 11);
        assert_eq!(v.shared[&address_const(POOL2)], 22);
        assert_eq!(v.shared[&address_const(GLOBAL_CONFIG)], 33);
        assert_eq!(v.shared[&address_const(PARTNER)], 44);
    }

    #[test]
    fn verification_refuses_foreign_code_and_objects() {
        let run =
            |mock: SuiMock, r: Route| verify_on_chain(&Gql::new(&mock, SuiNetwork::Mainnet), r);
        let good_chain = || chain(&pool_type(BUCK, SUI_COIN_TYPE));

        // a hop calling some other package
        let mut r = route();
        r.paths[0].published_at = address_const("0x1234");
        assert!(
            run(good_chain(), r)
                .unwrap_err()
                .contains("not Cetus's current router")
        );
        // another aggregator package
        let mut r = route();
        r.aggregator_package = address_const("0x1234");
        assert!(
            run(good_chain(), r)
                .unwrap_err()
                .contains("aggregator package")
        );
        // a pool that is not a Cetus pool
        let fake = chain("0x999::pool::Pool<0x1::a::A,0x1::b::B>");
        assert!(
            run(fake, route())
                .unwrap_err()
                .contains("not a Cetus pool::Pool")
        );
        // a Cetus pool holding other coins than the route claims
        let wrong_coins = chain(&pool_type(USDC, USDC));
        assert!(
            run(wrong_coins, route())
                .unwrap_err()
                .contains("does not hold the coins")
        );
        // pool coins in the wrong order for the stated direction
        let mut r = route();
        r.paths[0].direction = true;
        assert!(
            run(good_chain(), r)
                .unwrap_err()
                .contains("does not hold the coins")
        );
        // a missing pool
        let missing = SuiMock::new()
            .data(
                &format!("\"{CETUS_ROUTER_PACKAGE}\""),
                &format!(r#"{{"package":{{"address":"{ROUTER_LATEST}"}}}}"#),
            )
            .data(
                &format!("\"{AGGREGATOR_V3_PACKAGE}\""),
                &format!(r#"{{"package":{{"address":"{AGG_LATEST}"}}}}"#),
            )
            .data("object(address", r#"{"object":null}"#);
        assert!(
            run(missing, route())
                .unwrap_err()
                .contains("does not exist")
        );
        // an unpublished router
        let none = SuiMock::new().data("package(address", r#"{"package":null}"#);
        assert!(run(none, route()).is_err());
    }

    #[test]
    fn the_swap_transaction_calls_the_aggregators_contract_in_order() {
        use crate::sui::tx::tests::{addr, coin, env};
        use crate::sui::tx::{Holdings, assemble};
        use sui_sdk_types::{Command, TransactionKind};

        let v = verified();
        let fee = SuiSwapFee::new(&format!("0x{}", "cd".repeat(32)), 70).unwrap();
        let amounts = compute_amounts(&v.route, Some(&fee), 100);
        let sui = Holdings {
            coins: vec![coin(1, 10_000_000_000)],
            address_balance: 0,
        };
        let recipient = addr(8);
        let (tx, _) = assemble(
            addr(1),
            &env(),
            50_000_000,
            &sui,
            SUI_COIN_TYPE,
            1_000_000_000,
            &sui,
            &|t, input| add_swap(t, &v, &amounts, Some(&fee), input, recipient),
        )
        .unwrap();
        let TransactionKind::ProgrammableTransaction(ptb) = &tx.kind else {
            panic!("not a PTB")
        };
        let calls: Vec<String> = ptb
            .commands
            .iter()
            .map(|c| match c {
                Command::MoveCall(m) => format!("{}::{}", m.module.as_str(), m.function.as_str()),
                Command::SplitCoins(_) => "split".to_string(),
                Command::TransferObjects(_) => "transfer".to_string(),
                _ => "other".to_string(),
            })
            .collect();
        assert_eq!(
            calls,
            vec![
                "split",
                "router::new_swap_context_v3",
                "cetus::swap",
                "cetus::swap",
                "router::confirm_swap_v3",
                "transfer"
            ]
        );
        let Command::MoveCall(open) = &ptb.commands[1] else {
            panic!()
        };
        assert_eq!(open.package.to_string(), AGG_LATEST);
        assert_eq!(open.type_arguments.len(), 2);
        assert_eq!(open.arguments.len(), 8);
        let Command::MoveCall(hop) = &ptb.commands[2] else {
            panic!()
        };
        assert_eq!(hop.package.to_string(), ROUTER_LATEST);
        assert_eq!(hop.arguments.len(), 7);
        // Both hops are `direction: false`, so the pool's coin A is the hop's target.
        assert_eq!(hop.type_arguments[0].to_string(), BUCK);
        assert_eq!(hop.type_arguments[1].to_string(), SUI_COIN_TYPE);
    }
}
