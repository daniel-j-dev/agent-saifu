//! Sui's GraphQL API: the only network access a Sui wallet needs besides the swap aggregator.
//! (Sui's JSON-RPC is retired on public nodes; GraphQL serves reads, simulation and submission
//! over plain HTTPS, so no extra transport is needed.)

use sui_sdk_types::{Address, Digest};

use super::network::SuiNetwork;
use crate::eth::explorer::{sanitize_text, sanitize_text_max};
use crate::http::HttpClient;
use crate::json::{self, Value};

/// The node limits how many items per request may need a store lookup (balances carry coin
/// metadata, which does), so balances use smaller pages than coin objects.
const BALANCE_PAGE: i64 = 10;
const OBJECT_PAGE: i64 = 50;
const MAX_PAGES: usize = 10;
const MAX_ERROR_CHARS: usize = 300;
pub const MAX_HISTORY: usize = 50;

pub struct Gql<'a> {
    http: &'a dyn HttpClient,
    url: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinBalance {
    pub coin_type: String,
    pub total: u128,
    pub coin_objects: u128,
    pub address_balance: u128,
    pub symbol: Option<String>,
    pub decimals: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinObject {
    pub id: Address,
    pub version: u64,
    pub digest: Digest,
    pub balance: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinMetadata {
    pub symbol: String,
    pub decimals: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpochInfo {
    pub epoch: u64,
    pub reference_gas_price: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GasSummary {
    pub computation: u64,
    pub storage: u64,
    pub rebate: u64,
}

impl GasSummary {
    /// What the sender pays in total (never below zero).
    pub fn net_cost(&self) -> u64 {
        self.computation
            .saturating_add(self.storage)
            .saturating_sub(self.rebate)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceChange {
    pub owner: String,
    pub coin_type: String,
    pub amount: i128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effects {
    pub digest: Option<String>,
    pub success: bool,
    pub error: Option<String>,
    pub gas: GasSummary,
    pub balance_changes: Vec<BalanceChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionInfo {
    pub digest: String,
    pub sender: Option<String>,
    pub effects: Effects,
    pub timestamp: Option<String>,
    pub epoch: Option<u64>,
    pub checkpoint: Option<u64>,
}

/// Where an object lives; shared objects need their initial shared version in a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    pub version: u64,
    pub digest: Option<Digest>,
    pub initial_shared_version: Option<u64>,
    pub type_repr: Option<String>,
}

fn u128_of(v: Option<&Value>) -> Option<u128> {
    match v? {
        Value::String(s) | Value::Number(s)
            if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) =>
        {
            s.parse().ok()
        }
        _ => None,
    }
}

fn i128_of(v: Option<&Value>) -> Option<i128> {
    match v? {
        Value::String(s) | Value::Number(s) => s.parse().ok(),
        _ => None,
    }
}

fn u64_of(v: Option<&Value>) -> Option<u64> {
    v.and_then(Value::as_u64)
}

fn text_of(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_string)
}

fn page_info(connection: &Value) -> Option<String> {
    let info = connection.get("pageInfo")?;
    if info.get("hasNextPage").and_then(Value::as_bool) == Some(true) {
        return text_of(info.get("endCursor"));
    }
    None
}

fn nodes(connection: Option<&Value>) -> &[Value] {
    connection
        .and_then(|c| c.get("nodes"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn gas_summary(effects: &Value) -> GasSummary {
    let g = effects.get("gasEffects").and_then(|g| g.get("gasSummary"));
    let field = |name: &str| u64_of(g.and_then(|g| g.get(name))).unwrap_or(0);
    GasSummary {
        computation: field("computationCost"),
        storage: field("storageCost"),
        rebate: field("storageRebate"),
    }
}

fn balance_changes(effects: &Value) -> Vec<BalanceChange> {
    effects
        .get("balanceChangesJson")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|c| {
                    Some(BalanceChange {
                        owner: text_of(c.get("address"))?,
                        coin_type: text_of(c.get("coinType"))?,
                        amount: i128_of(c.get("amount"))?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_effects(effects: &Value) -> Result<Effects, String> {
    let success = match effects.get("status").and_then(Value::as_str) {
        Some("SUCCESS") => true,
        Some("FAILURE") => false,
        _ => return Err("the Sui node returned no execution status".to_string()),
    };
    let error = effects
        .get("executionError")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(|m| sanitize_text_max(m, MAX_ERROR_CHARS));
    Ok(Effects {
        digest: text_of(effects.get("digest")),
        success,
        error,
        gas: gas_summary(effects),
        balance_changes: balance_changes(effects),
    })
}

const EFFECTS_FIELDS: &str = "digest status executionError { message } gasEffects { gasSummary { computationCost storageCost storageRebate } } balanceChangesJson";

impl<'a> Gql<'a> {
    pub fn new(http: &'a dyn HttpClient, network: SuiNetwork) -> Gql<'a> {
        Gql {
            http,
            url: network.graphql_url(),
        }
    }

    /// Runs a query and returns its `data`. Server-side errors become readable messages.
    fn run(&self, query: &str, variables: Vec<(&str, Value)>) -> Result<Value, String> {
        let body = Value::object(vec![
            ("query", Value::str(query)),
            ("variables", Value::object(variables)),
        ])
        .to_json();
        let text = self.http.post_json(self.url, &body)?;
        let doc = json::parse(&text)
            .map_err(|_| "the Sui node returned an answer that is not JSON".to_string())?;
        if let Some(first) = doc
            .get("errors")
            .and_then(Value::as_array)
            .and_then(|e| e.first())
        {
            let message = first
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(format!(
                "Sui node error: {}",
                sanitize_text_max(message, MAX_ERROR_CHARS)
            ));
        }
        doc.get("data")
            .cloned()
            .ok_or_else(|| "the Sui node returned no data".to_string())
    }

    pub fn chain_identifier(&self) -> Result<String, String> {
        let data = self.run("{ chainIdentifier }", vec![])?;
        text_of(data.get("chainIdentifier"))
            .ok_or("the Sui node reported no chain identifier".to_string())
    }

    /// Makes sure the endpoint serves the network the wallet is bound to.
    pub fn verify_network(&self, network: SuiNetwork) -> Result<String, String> {
        let reported = self.chain_identifier()?;
        if let Some(expected) = network.expected_chain_identifier()
            && reported != expected
        {
            return Err(format!(
                "the Sui endpoint is not {} (chain identifier {}); refusing to use it",
                network.id(),
                sanitize_text(&reported)
            ));
        }
        Ok(reported)
    }

    pub fn epoch(&self) -> Result<EpochInfo, String> {
        let data = self.run("{ epoch { epochId referenceGasPrice } }", vec![])?;
        let epoch = data.get("epoch").ok_or("the Sui node reported no epoch")?;
        Ok(EpochInfo {
            epoch: u64_of(epoch.get("epochId")).ok_or("the Sui node reported no epoch number")?,
            reference_gas_price: u64_of(epoch.get("referenceGasPrice"))
                .ok_or("the Sui node reported no gas price")?,
        })
    }

    /// Every coin type the address holds (coin objects and address balance combined), and whether
    /// the list was cut short because the address holds a very large number of coin types.
    pub fn balances(&self, owner: &Address) -> Result<(Vec<CoinBalance>, bool), String> {
        const QUERY: &str = "query($a: SuiAddress!, $n: Int!, $after: String) { address(address: $a) { balances(first: $n, after: $after) { pageInfo { hasNextPage endCursor } nodes { coinType { repr } totalBalance coinBalance addressBalance coinMetadata { symbol decimals } } } } }";
        let mut out = vec![];
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let data = self.run(
                QUERY,
                vec![
                    ("a", Value::String(owner.to_string())),
                    ("n", Value::int(BALANCE_PAGE)),
                    ("after", cursor.map_or(Value::Null, Value::String)),
                ],
            )?;
            let Some(connection) = data.get("address").and_then(|a| a.get("balances")) else {
                return Ok((out, false));
            };
            for node in nodes(Some(connection)) {
                if let Some(balance) = parse_balance(node) {
                    out.push(balance);
                }
            }
            cursor = page_info(connection);
            if cursor.is_none() {
                return Ok((out, false));
            }
        }
        Ok((out, true))
    }

    /// One coin type's holdings of the owner, without listing every coin type.
    pub fn address_balance_of(&self, owner: &Address, coin_type: &str) -> Result<u128, String> {
        const QUERY: &str = "query($a: SuiAddress!, $t: String!) { address(address: $a) { balance(coinType: $t) { addressBalance } } }";
        let data = self.run(
            QUERY,
            vec![
                ("a", Value::String(owner.to_string())),
                ("t", Value::str(coin_type)),
            ],
        )?;
        Ok(u128_of(
            data.get("address")
                .and_then(|a| a.get("balance"))
                .and_then(|b| b.get("addressBalance")),
        )
        .unwrap_or(0))
    }

    /// The owner's `Coin<T>` objects, largest first.
    pub fn coin_objects(
        &self,
        owner: &Address,
        coin_type: &str,
    ) -> Result<Vec<CoinObject>, String> {
        const QUERY: &str = "query($a: SuiAddress!, $t: String!, $n: Int!, $after: String) { address(address: $a) { objects(first: $n, after: $after, filter: { type: $t }) { pageInfo { hasNextPage endCursor } nodes { address version digest contents { json } } } } }";
        let object_type = format!("0x2::coin::Coin<{coin_type}>");
        let mut out = vec![];
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES * 2 {
            let data = self.run(
                QUERY,
                vec![
                    ("a", Value::String(owner.to_string())),
                    ("t", Value::String(object_type.clone())),
                    ("n", Value::int(OBJECT_PAGE)),
                    ("after", cursor.map_or(Value::Null, Value::String)),
                ],
            )?;
            let Some(connection) = data.get("address").and_then(|a| a.get("objects")) else {
                break;
            };
            for node in nodes(Some(connection)) {
                out.push(parse_coin_object(node)?);
            }
            cursor = page_info(connection);
            if cursor.is_none() {
                break;
            }
        }
        out.sort_by(|a, b| b.balance.cmp(&a.balance).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    pub fn coin_metadata(&self, coin_type: &str) -> Result<Option<CoinMetadata>, String> {
        let data = self.run(
            "query($t: String!) { coinMetadata(coinType: $t) { symbol decimals } }",
            vec![("t", Value::str(coin_type))],
        )?;
        let Some(meta) = data
            .get("coinMetadata")
            .filter(|m| !matches!(m, Value::Null))
        else {
            return Ok(None);
        };
        let decimals = u64_of(meta.get("decimals")).and_then(|d| u32::try_from(d).ok());
        Ok(decimals.map(|decimals| CoinMetadata {
            symbol: text_of(meta.get("symbol"))
                .map(|s| sanitize_text(&s))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| super::coins::fallback_symbol(coin_type)),
            decimals,
        }))
    }

    pub fn object(&self, id: &Address) -> Result<Option<ObjectInfo>, String> {
        const QUERY: &str = "query($id: SuiAddress!) { object(address: $id) { version digest owner { __typename ... on Shared { initialSharedVersion } } asMoveObject { contents { type { repr } } } } }";
        let data = self.run(QUERY, vec![("id", Value::String(id.to_string()))])?;
        let Some(object) = data.get("object").filter(|o| !matches!(o, Value::Null)) else {
            return Ok(None);
        };
        Ok(Some(ObjectInfo {
            version: u64_of(object.get("version")).ok_or("object has no version")?,
            digest: text_of(object.get("digest")).and_then(|d| d.parse().ok()),
            initial_shared_version: object
                .get("owner")
                .and_then(|o| o.get("initialSharedVersion"))
                .and_then(Value::as_u64),
            type_repr: object
                .get("asMoveObject")
                .and_then(|m| m.get("contents"))
                .and_then(|c| c.get("type"))
                .and_then(|t| text_of(t.get("repr"))),
        }))
    }

    /// The newest published version of a package family, given its original ID.
    pub fn latest_package(&self, original_id: &Address) -> Result<Option<Address>, String> {
        let data = self.run(
            "query($p: SuiAddress!) { package(address: $p) { address } }",
            vec![("p", Value::String(original_id.to_string()))],
        )?;
        data.get("package")
            .and_then(|p| p.get("address"))
            .and_then(Value::as_str)
            .map(|a| {
                a.parse::<Address>()
                    .map_err(|_| "package address is invalid".to_string())
            })
            .transpose()
    }

    /// Dry-runs a transaction. Nothing is signed or executed.
    pub fn simulate(&self, transaction_bcs_base64: &str) -> Result<Effects, String> {
        let query = format!(
            "query($t: JSON!) {{ simulateTransaction(transaction: $t, checksEnabled: true, doGasSelection: false) {{ effects {{ {EFFECTS_FIELDS} }} }} }}"
        );
        let tx = Value::object(vec![(
            "bcs",
            Value::object(vec![("value", Value::str(transaction_bcs_base64))]),
        )]);
        let data = self.run(&query, vec![("t", tx)])?;
        let effects = data
            .get("simulateTransaction")
            .and_then(|s| s.get("effects"))
            .ok_or("the Sui node returned no simulation result")?;
        parse_effects(effects)
    }

    /// Submits a signed transaction.
    pub fn execute(
        &self,
        transaction_bcs_base64: &str,
        signature_base64: &str,
    ) -> Result<Effects, String> {
        let query = format!(
            "mutation($t: Base64!, $s: [Base64!]!) {{ executeTransaction(transactionDataBcs: $t, signatures: $s) {{ effects {{ {EFFECTS_FIELDS} }} }} }}"
        );
        let data = self.run(
            &query,
            vec![
                ("t", Value::str(transaction_bcs_base64)),
                ("s", Value::Array(vec![Value::str(signature_base64)])),
            ],
        )?;
        let effects = data
            .get("executeTransaction")
            .and_then(|s| s.get("effects"))
            .ok_or("the Sui node returned no execution result")?;
        parse_effects(effects)
    }

    pub fn transaction(&self, digest: &str) -> Result<Option<TransactionInfo>, String> {
        let query = format!(
            "query($d: String!) {{ transaction(digest: $d) {{ digest sender {{ address }} effects {{ {EFFECTS_FIELDS} timestamp epoch {{ epochId }} checkpoint {{ sequenceNumber }} }} }} }}"
        );
        let data = self.run(&query, vec![("d", Value::str(digest))])?;
        let Some(tx) = data
            .get("transaction")
            .filter(|t| !matches!(t, Value::Null))
        else {
            return Ok(None);
        };
        parse_transaction(tx).map(Some)
    }

    /// The most recent transactions that touched the address, newest first.
    pub fn history(
        &self,
        owner: &Address,
        limit: usize,
    ) -> Result<(Vec<TransactionInfo>, bool), String> {
        let query = format!(
            "query($a: SuiAddress!, $n: Int!) {{ transactions(last: $n, filter: {{ affectedAddress: $a }}) {{ pageInfo {{ hasPreviousPage }} nodes {{ digest sender {{ address }} effects {{ {EFFECTS_FIELDS} timestamp epoch {{ epochId }} checkpoint {{ sequenceNumber }} }} }} }} }}"
        );
        let n = limit.clamp(1, MAX_HISTORY);
        let data = self.run(
            &query,
            vec![
                ("a", Value::String(owner.to_string())),
                ("n", Value::int(n as i64)),
            ],
        )?;
        let connection = data.get("transactions");
        let mut out = nodes(connection)
            .iter()
            .map(parse_transaction)
            .collect::<Result<Vec<_>, _>>()?;
        out.reverse();
        let more = connection
            .and_then(|c| c.get("pageInfo"))
            .and_then(|p| p.get("hasPreviousPage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok((out, more))
    }
}

fn parse_transaction(tx: &Value) -> Result<TransactionInfo, String> {
    let effects = tx.get("effects").ok_or("transaction has no effects")?;
    Ok(TransactionInfo {
        digest: text_of(tx.get("digest")).ok_or("transaction has no digest")?,
        sender: tx.get("sender").and_then(|s| text_of(s.get("address"))),
        effects: parse_effects(effects)?,
        timestamp: text_of(effects.get("timestamp")),
        epoch: effects.get("epoch").and_then(|e| u64_of(e.get("epochId"))),
        checkpoint: effects
            .get("checkpoint")
            .and_then(|c| u64_of(c.get("sequenceNumber"))),
    })
}

fn parse_balance(node: &Value) -> Option<CoinBalance> {
    let coin_type = text_of(node.get("coinType").and_then(|t| t.get("repr")))?;
    let meta = node
        .get("coinMetadata")
        .filter(|m| !matches!(m, Value::Null));
    Some(CoinBalance {
        total: u128_of(node.get("totalBalance"))?,
        coin_objects: u128_of(node.get("coinBalance")).unwrap_or(0),
        address_balance: u128_of(node.get("addressBalance")).unwrap_or(0),
        symbol: meta
            .and_then(|m| text_of(m.get("symbol")))
            .map(|s| sanitize_text(&s))
            .filter(|s| !s.is_empty()),
        decimals: meta
            .and_then(|m| u64_of(m.get("decimals")))
            .and_then(|d| u32::try_from(d).ok()),
        coin_type,
    })
}

fn parse_coin_object(node: &Value) -> Result<CoinObject, String> {
    let bad = || "the Sui node returned a malformed coin object".to_string();
    Ok(CoinObject {
        id: text_of(node.get("address"))
            .and_then(|a| a.parse().ok())
            .ok_or_else(bad)?,
        version: u64_of(node.get("version")).ok_or_else(bad)?,
        digest: text_of(node.get("digest"))
            .and_then(|d| d.parse().ok())
            .ok_or_else(bad)?,
        balance: node
            .get("contents")
            .and_then(|c| c.get("json"))
            .and_then(|j| u64_of(j.get("balance")))
            .ok_or_else(bad)?,
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::sui::testing::SuiMock;

    pub const OWNER: &str = "0x34729f8dd6d4e9bf5243b327c6462552a2d092808c537b9bbae7ee1a0e080a39";
    pub const COIN_ID: &str = "0xee4a160295f6c5d8c6b410f57fd6aee67710bee4e22bba8de3a48922264a83b4";
    pub const COIN_DIGEST: &str = "3HNMVAajDX19eHDpRjb3D1nfDeTTMbPZC8uJMosrUHL8";

    fn owner() -> Address {
        OWNER.parse().unwrap()
    }

    #[test]
    fn verifies_the_chain_identifier() {
        let ok = SuiMock::new().data(
            "chainIdentifier",
            r#"{"chainIdentifier":"4btiuiMPvEENsttpZC7CZ53DruC3MAgfznDbASZ7DR6S"}"#,
        );
        let gql = Gql::new(&ok, SuiNetwork::Mainnet);
        assert!(gql.verify_network(SuiNetwork::Mainnet).is_ok());
        // A testnet endpoint answering for a mainnet wallet is refused.
        let wrong = SuiMock::new().data(
            "chainIdentifier",
            r#"{"chainIdentifier":"69WiPg3DAQiwdxfncX6wYQ2siKwAe6L9BZthQea3JNMD"}"#,
        );
        let err = Gql::new(&wrong, SuiNetwork::Mainnet)
            .verify_network(SuiNetwork::Mainnet)
            .unwrap_err();
        assert!(err.contains("not mainnet"), "{err}");
        // Devnet has no fixed identifier.
        let dev = SuiMock::new().data("chainIdentifier", r#"{"chainIdentifier":"abc"}"#);
        assert!(
            Gql::new(&dev, SuiNetwork::Devnet)
                .verify_network(SuiNetwork::Devnet)
                .is_ok()
        );
    }

    #[test]
    fn reads_the_epoch_and_gas_price() {
        let mock = SuiMock::new().data(
            "epochId",
            r#"{"epoch":{"epochId":1290,"referenceGasPrice":"100"}}"#,
        );
        let epoch = Gql::new(&mock, SuiNetwork::Mainnet).epoch().unwrap();
        assert_eq!(
            epoch,
            EpochInfo {
                epoch: 1290,
                reference_gas_price: 100
            }
        );
        let broken = SuiMock::new().data("epochId", r#"{"epoch":null}"#);
        assert!(Gql::new(&broken, SuiNetwork::Mainnet).epoch().is_err());
    }

    #[test]
    fn balances_combine_coin_objects_and_address_balance() {
        let mock = SuiMock::new().data(
            "balances(first",
            r#"{"address":{"balances":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
              {"coinType":{"repr":"0x0000000000000000000000000000000000000000000000000000000000000002::sui::SUI"},"totalBalance":"6117617214303","coinBalance":"1000000","addressBalance":"6117616214303","coinMetadata":{"symbol":"SUI","decimals":9}},
              {"coinType":{"repr":"0x1::spam::SPAM"},"totalBalance":"5","coinBalance":"5","addressBalance":"0","coinMetadata":null}]}}}"#,
        );
        let (balances, truncated) = Gql::new(&mock, SuiNetwork::Mainnet)
            .balances(&owner())
            .unwrap();
        assert!(!truncated);
        assert_eq!(balances.len(), 2);
        assert_eq!(balances[0].total, 6_117_617_214_303);
        assert_eq!(balances[0].coin_objects, 1_000_000);
        assert_eq!(balances[0].address_balance, 6_117_616_214_303);
        assert_eq!(balances[0].symbol.as_deref(), Some("SUI"));
        assert_eq!(balances[0].decimals, Some(9));
        assert_eq!(balances[1].symbol, None);
        assert_eq!(balances[1].decimals, None);
    }

    #[test]
    fn balances_follow_pages_and_handle_unknown_addresses() {
        let node = |n: u32| {
            format!(
                r#"{{"coinType":{{"repr":"0x{n}::a::A"}},"totalBalance":"{n}","coinBalance":"{n}","addressBalance":"0","coinMetadata":null}}"#
            )
        };
        let mock = SuiMock::new()
            .data_seq(
                "balances(first",
                vec![
                    format!(r#"{{"address":{{"balances":{{"pageInfo":{{"hasNextPage":true,"endCursor":"c1"}},"nodes":[{}]}}}}}}"#, node(1)),
                    format!(r#"{{"address":{{"balances":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[{}]}}}}}}"#, node(2)),
                ],
            );
        let (all, truncated) = Gql::new(&mock, SuiNetwork::Mainnet)
            .balances(&owner())
            .unwrap();
        assert!(!truncated);
        assert_eq!(all.len(), 2);
        assert!(mock.requests.borrow()[1].1.contains("\"after\":\"c1\""));
        let none = SuiMock::new().data("balances(first", r#"{"address":null}"#);
        assert!(
            Gql::new(&none, SuiNetwork::Mainnet)
                .balances(&owner())
                .unwrap()
                .0
                .is_empty()
        );
    }

    #[test]
    fn endless_balance_lists_are_cut_short_and_reported() {
        let page = r#"{"address":{"balances":{"pageInfo":{"hasNextPage":true,"endCursor":"c"},"nodes":[{"coinType":{"repr":"0x1::a::A"},"totalBalance":"1","coinBalance":"1","addressBalance":"0","coinMetadata":null}]}}}"#;
        let mock = SuiMock::new().data("balances(first", page);
        let (list, truncated) = Gql::new(&mock, SuiNetwork::Mainnet)
            .balances(&owner())
            .unwrap();
        assert!(truncated);
        assert_eq!(list.len(), MAX_PAGES);
    }

    #[test]
    fn one_coin_types_address_balance() {
        let mock = SuiMock::new().data(
            "balance(coinType",
            r#"{"address":{"balance":{"addressBalance":"42"}}}"#,
        );
        assert_eq!(
            Gql::new(&mock, SuiNetwork::Mainnet).address_balance_of(&owner(), "0x2::sui::SUI"),
            Ok(42)
        );
        let none = SuiMock::new().data("balance(coinType", r#"{"address":{"balance":null}}"#);
        assert_eq!(
            Gql::new(&none, SuiNetwork::Mainnet).address_balance_of(&owner(), "0x2::sui::SUI"),
            Ok(0)
        );
    }

    #[test]
    fn coin_objects_are_sorted_largest_first_and_validated() {
        let mock = SuiMock::new().data(
            "objects(first",
            &format!(
                r#"{{"address":{{"objects":{{"pageInfo":{{"hasNextPage":false}},"nodes":[
                {{"address":"{COIN_ID}","version":5,"digest":"{COIN_DIGEST}","contents":{{"json":{{"id":"x","balance":"10"}}}}}},
                {{"address":"0x{}","version":6,"digest":"{COIN_DIGEST}","contents":{{"json":{{"id":"x","balance":"500"}}}}}}]}}}}}}"#,
                "11".repeat(32)
            ),
        );
        let coins = Gql::new(&mock, SuiNetwork::Mainnet)
            .coin_objects(&owner(), "0x2::sui::SUI")
            .unwrap();
        assert_eq!(
            coins.iter().map(|c| c.balance).collect::<Vec<_>>(),
            vec![500, 10]
        );
        let body = mock.requests.borrow()[0].1.clone();
        assert!(body.contains("0x2::coin::Coin<0x2::sui::SUI>"));

        let bad = SuiMock::new().data(
            "objects(first",
            r#"{"address":{"objects":{"nodes":[{"address":"nope","version":1,"digest":"x","contents":{"json":{"balance":"1"}}}]}}}"#,
        );
        assert!(
            Gql::new(&bad, SuiNetwork::Mainnet)
                .coin_objects(&owner(), "0x2::sui::SUI")
                .is_err()
        );
    }

    #[test]
    fn coin_metadata_and_objects_and_packages() {
        let mock = SuiMock::new()
            .data("coinMetadata", r#"{"coinMetadata":{"symbol":"USDC","decimals":6}}"#)
            .data(
                "object(address",
                r#"{"object":{"version":9,"owner":{"__typename":"Shared","initialSharedVersion":3},"asMoveObject":{"contents":{"type":{"repr":"0x1::pool::Pool<0x2::a::A, 0x2::b::B>"}}}}}"#,
            )
            .data("package(address", r#"{"package":{"address":"0x87b17d7f0b448b83ab934a13eb64b05b60e17d0a371e2a721c241bcf3f5ec9e7"}}"#);
        let gql = Gql::new(&mock, SuiNetwork::Mainnet);
        let meta = gql.coin_metadata("0x1::usdc::USDC").unwrap().unwrap();
        assert_eq!(
            meta,
            CoinMetadata {
                symbol: "USDC".to_string(),
                decimals: 6
            }
        );
        let object = gql.object(&"0x6".parse().unwrap()).unwrap().unwrap();
        assert_eq!(object.initial_shared_version, Some(3));
        assert_eq!(object.version, 9);
        assert!(object.type_repr.unwrap().contains("::pool::Pool<"));
        let package = gql
            .latest_package(&"0x2".parse().unwrap())
            .unwrap()
            .unwrap();
        assert!(package.to_string().starts_with("0x87b17d7f"));

        let none = SuiMock::new()
            .data("coinMetadata", r#"{"coinMetadata":null}"#)
            .data("object(address", r#"{"object":null}"#)
            .data("package(address", r#"{"package":null}"#);
        let gql = Gql::new(&none, SuiNetwork::Mainnet);
        assert!(gql.coin_metadata("0x1::a::A").unwrap().is_none());
        assert!(gql.object(&"0x6".parse().unwrap()).unwrap().is_none());
        assert!(
            gql.latest_package(&"0x2".parse().unwrap())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn simulation_reports_status_gas_and_balance_changes() {
        let mock = SuiMock::new().data(
            "simulateTransaction",
            r#"{"simulateTransaction":{"effects":{"digest":"D","status":"SUCCESS","executionError":null,
             "gasEffects":{"gasSummary":{"computationCost":1000000,"storageCost":988000,"storageRebate":978120}},
             "balanceChangesJson":[{"address":"0xabc","coinType":"0x2::sui::SUI","amount":"-1988000"},{"address":"0xdef","coinType":"0x2::sui::SUI","amount":"1000"}]}}}"#,
        );
        let effects = Gql::new(&mock, SuiNetwork::Mainnet)
            .simulate("AAAA")
            .unwrap();
        assert!(effects.success);
        assert_eq!(effects.gas.net_cost(), 1_009_880);
        assert_eq!(effects.balance_changes.len(), 2);
        assert_eq!(effects.balance_changes[0].amount, -1_988_000);
        let body = mock.requests.borrow()[0].1.clone();
        assert!(body.contains("\"bcs\":{\"value\":\"AAAA\"}"));
    }

    #[test]
    fn failed_executions_carry_a_clean_reason() {
        let mock = SuiMock::new().data(
            "simulateTransaction",
            r#"{"simulateTransaction":{"effects":{"status":"FAILURE","executionError":{"message":"MoveAbort\u0007 code 1"},"gasEffects":null,"balanceChangesJson":[]}}}"#,
        );
        let effects = Gql::new(&mock, SuiNetwork::Mainnet)
            .simulate("AAAA")
            .unwrap();
        assert!(!effects.success);
        assert_eq!(effects.error.as_deref(), Some("MoveAbort code 1"));
        assert_eq!(effects.gas, GasSummary::default());
    }

    #[test]
    fn node_errors_are_readable_and_sanitized() {
        let mock = SuiMock::new().errors(
            "simulateTransaction",
            "Balance of gas object 1 is lower\u{1b}[31m",
        );
        let err = Gql::new(&mock, SuiNetwork::Mainnet)
            .simulate("AAAA")
            .unwrap_err();
        assert!(err.starts_with("Sui node error: Balance of gas object"));
        assert!(!err.contains('\u{1b}'));
        let junk = SuiMock::new().raw("simulateTransaction", "<html>");
        assert!(
            Gql::new(&junk, SuiNetwork::Mainnet)
                .simulate("AAAA")
                .is_err()
        );
        let net = SuiMock::new().net_error("simulateTransaction", "request failed: timeout");
        assert_eq!(
            Gql::new(&net, SuiNetwork::Mainnet)
                .simulate("AAAA")
                .unwrap_err(),
            "request failed: timeout"
        );
    }

    #[test]
    fn execution_sends_the_transaction_and_signature() {
        let mock = SuiMock::new().data(
            "executeTransaction",
            r#"{"executeTransaction":{"effects":{"digest":"DIGEST","status":"SUCCESS","executionError":null,"gasEffects":{"gasSummary":{"computationCost":1,"storageCost":2,"storageRebate":1}},"balanceChangesJson":[]}}}"#,
        );
        let effects = Gql::new(&mock, SuiNetwork::Mainnet)
            .execute("TX", "SIG")
            .unwrap();
        assert_eq!(effects.digest.as_deref(), Some("DIGEST"));
        let body = mock.requests.borrow()[0].1.clone();
        assert!(body.contains("\"t\":\"TX\"") && body.contains("\"s\":[\"SIG\"]"));
        assert!(body.contains("mutation"));
    }

    #[test]
    fn transactions_and_history_parse() {
        let tx = format!(
            r#"{{"digest":"D1","sender":{{"address":"{OWNER}"}},"effects":{{"digest":"D1","status":"SUCCESS","executionError":null,"timestamp":"2026-10-24T01:00:00Z","epoch":{{"epochId":7}},"checkpoint":{{"sequenceNumber":99}},"gasEffects":{{"gasSummary":{{"computationCost":1,"storageCost":1,"storageRebate":0}}}},"balanceChangesJson":[]}}}}"#
        );
        let mock = SuiMock::new()
            .data("transaction(digest", &format!(r#"{{"transaction":{tx}}}"#))
            .data(
                "transactions(last",
                &format!(r#"{{"transactions":{{"pageInfo":{{"hasPreviousPage":true}},"nodes":[{tx},{}]}}}}"#, tx.replace("D1", "D2")),
            );
        let gql = Gql::new(&mock, SuiNetwork::Mainnet);
        let info = gql.transaction("D1").unwrap().unwrap();
        assert_eq!(info.sender.as_deref(), Some(OWNER));
        assert_eq!((info.epoch, info.checkpoint), (Some(7), Some(99)));
        assert_eq!(info.timestamp.as_deref(), Some("2026-10-24T01:00:00Z"));
        let (history, more) = gql.history(&owner(), 2).unwrap();
        assert!(more);
        assert_eq!(
            history
                .iter()
                .map(|t| t.digest.as_str())
                .collect::<Vec<_>>(),
            vec!["D2", "D1"],
            "newest first"
        );
        let missing = SuiMock::new().data("transaction(digest", r#"{"transaction":null}"#);
        assert!(
            Gql::new(&missing, SuiNetwork::Mainnet)
                .transaction("x")
                .unwrap()
                .is_none()
        );
    }
}
