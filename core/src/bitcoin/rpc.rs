//! Client for an Esplora-compatible Bitcoin HTTP API (Blockstream's and mempool.space's REST
//! API): chain height, fee estimates, an address's balance, unspent coins and history, single
//! transactions, and broadcasting. Everything the server says is treated as untrusted data:
//! it is parsed strictly, its sizes are bounded, and nothing it returns is used to decide how
//! much to sign without the checks in the wallet service.

use super::units::FeeRate;
use crate::crypto::hex;
use crate::eth::explorer::sanitize_text_max;
use crate::http::HttpClient;
use crate::json::{self, Value};

/// How many unspent coins are read for one address.
const MAX_UTXOS: usize = 5_000;
const MAX_TX_INPUTS_READ: usize = 1_000;
const MAX_TX_OUTPUTS_READ: usize = 1_000;
/// The server reports 25 transactions per page.
pub const PAGE_SIZE: usize = 25;
/// No block height is anywhere near this; larger values are a lying server.
const MAX_PLAUSIBLE_HEIGHT: u64 = 50_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressStats {
    pub confirmed: i128,
    pub pending: i128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainStatus {
    pub confirmed: bool,
    pub block_height: Option<u64>,
    pub block_time: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawUtxo {
    pub txid: String,
    pub vout: u32,
    pub value: u64,
    pub status: ChainStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputInfo {
    pub txid: String,
    pub vout: u32,
    pub is_coinbase: bool,
    /// Script and value of the output being spent, as the server reports them.
    pub prevout_script: Option<Vec<u8>>,
    pub prevout_value: Option<u64>,
    pub prevout_address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputInfo {
    pub script: Vec<u8>,
    pub value: u64,
    pub address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxInfo {
    pub txid: String,
    pub fee: Option<u64>,
    pub weight: Option<u64>,
    pub size: Option<u64>,
    pub status: ChainStatus,
    pub inputs: Vec<InputInfo>,
    pub outputs: Vec<OutputInfo>,
}

/// Fee estimates by confirmation target in blocks, sorted by target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeeEstimates(pub Vec<(u32, FeeRate)>);

impl FeeEstimates {
    /// The rate for confirming within `target` blocks: the entry for that target, else the
    /// nearest smaller target (a faster, pricier one), else the smallest target there is.
    pub fn for_target(&self, target: u32) -> Option<(u32, FeeRate)> {
        self.0
            .iter()
            .rev()
            .find(|(t, _)| *t <= target)
            .or_else(|| self.0.first())
            .copied()
    }
}

pub struct EsploraClient<'a> {
    http: &'a dyn HttpClient,
    base: String,
}

fn malformed(what: &str) -> String {
    format!("unexpected response from the Bitcoin API: {what}")
}

fn number(v: &Value, key: &str) -> Option<u64> {
    match v.get(key)? {
        Value::Number(n) => n.parse::<u64>().ok(),
        _ => None,
    }
}

fn parse_status(v: Option<&Value>) -> Result<ChainStatus, String> {
    let v = v.ok_or_else(|| malformed("missing status"))?;
    let confirmed = v
        .get("confirmed")
        .and_then(Value::as_bool)
        .ok_or_else(|| malformed("missing confirmation status"))?;
    let block_height = number(v, "block_height").filter(|h| *h < MAX_PLAUSIBLE_HEIGHT);
    if confirmed && block_height.is_none() {
        return Err(malformed("a confirmed transaction without a block height"));
    }
    Ok(ChainStatus {
        confirmed,
        block_height: if confirmed { block_height } else { None },
        block_time: number(v, "block_time"),
    })
}

fn is_hex_id(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn parse_txid_field(v: &Value, key: &str) -> Result<String, String> {
    let text = v
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| malformed("missing transaction id"))?;
    if !is_hex_id(text) {
        return Err(malformed("a transaction id is not 64 hex digits"));
    }
    Ok(text.to_ascii_lowercase())
}

fn short_text(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(|s| sanitize_text_max(s, 100))
        .filter(|s| !s.is_empty())
}

fn script_field(v: &Value, key: &str) -> Option<Vec<u8>> {
    let text = v.get(key)?.as_str()?;
    if text.len() > 20_000 {
        return None;
    }
    hex::decode(text).ok()
}

pub fn parse_utxos(body: &str) -> Result<Vec<RawUtxo>, String> {
    let doc = json::parse(body).map_err(|_| malformed("not JSON"))?;
    let items = doc.as_array().ok_or_else(|| malformed("not a list"))?;
    if items.len() > MAX_UTXOS {
        return Err("this address has too many unspent coins to list; consolidate them with a wallet that can".to_string());
    }
    items
        .iter()
        .map(|item| {
            Ok(RawUtxo {
                txid: parse_txid_field(item, "txid")?,
                vout: number(item, "vout")
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| malformed("missing output index"))?,
                value: number(item, "value").ok_or_else(|| malformed("missing value"))?,
                status: parse_status(item.get("status"))?,
            })
        })
        .collect()
}

fn stat_sum(stats: &Value, funded: &str, spent: &str) -> Result<i128, String> {
    let funded = number(stats, funded).ok_or_else(|| malformed("missing address statistics"))?;
    let spent = number(stats, spent).ok_or_else(|| malformed("missing address statistics"))?;
    Ok(i128::from(funded) - i128::from(spent))
}

pub fn parse_address_stats(body: &str) -> Result<AddressStats, String> {
    let doc = json::parse(body).map_err(|_| malformed("not JSON"))?;
    let chain = doc
        .get("chain_stats")
        .ok_or_else(|| malformed("missing chain_stats"))?;
    let mempool = doc
        .get("mempool_stats")
        .ok_or_else(|| malformed("missing mempool_stats"))?;
    Ok(AddressStats {
        confirmed: stat_sum(chain, "funded_txo_sum", "spent_txo_sum")?,
        pending: stat_sum(mempool, "funded_txo_sum", "spent_txo_sum")?,
    })
}

pub fn parse_fee_estimates(body: &str) -> Result<FeeEstimates, String> {
    let doc = json::parse(body).map_err(|_| malformed("not JSON"))?;
    let Value::Object(pairs) = doc else {
        return Err(malformed("fee estimates are not an object"));
    };
    let mut out: Vec<(u32, FeeRate)> = pairs
        .iter()
        .filter_map(|(target, rate)| {
            let target = target.parse::<u32>().ok().filter(|t| *t >= 1)?;
            let rate = match rate {
                Value::Number(n) => n.parse::<f64>().ok()?,
                _ => return None,
            };
            Some((target, FeeRate::from_sat_per_vb(rate)?))
        })
        .collect();
    if out.is_empty() {
        return Err(malformed("no fee estimates"));
    }
    out.sort_by_key(|(t, _)| *t);
    Ok(FeeEstimates(out))
}

fn parse_input(v: &Value) -> Result<InputInfo, String> {
    let is_coinbase = v
        .get("is_coinbase")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let prevout = v.get("prevout").filter(|p| !matches!(p, Value::Null));
    Ok(InputInfo {
        txid: if is_coinbase {
            "0".repeat(64)
        } else {
            parse_txid_field(v, "txid")?
        },
        vout: number(v, "vout")
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0),
        is_coinbase,
        prevout_script: prevout.and_then(|p| script_field(p, "scriptpubkey")),
        prevout_value: prevout.and_then(|p| number(p, "value")),
        prevout_address: prevout.and_then(|p| short_text(p.get("scriptpubkey_address"))),
    })
}

fn parse_output(v: &Value) -> Result<OutputInfo, String> {
    Ok(OutputInfo {
        script: script_field(v, "scriptpubkey")
            .ok_or_else(|| malformed("missing output script"))?,
        value: number(v, "value").ok_or_else(|| malformed("missing output value"))?,
        address: short_text(v.get("scriptpubkey_address")),
    })
}

pub fn parse_tx_value(v: &Value) -> Result<TxInfo, String> {
    let inputs = v
        .get("vin")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("missing inputs"))?;
    let outputs = v
        .get("vout")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("missing outputs"))?;
    if inputs.len() > MAX_TX_INPUTS_READ || outputs.len() > MAX_TX_OUTPUTS_READ {
        return Err("this transaction is too large to display".to_string());
    }
    Ok(TxInfo {
        txid: parse_txid_field(v, "txid")?,
        fee: number(v, "fee"),
        weight: number(v, "weight"),
        size: number(v, "size"),
        status: parse_status(v.get("status"))?,
        inputs: inputs.iter().map(parse_input).collect::<Result<_, _>>()?,
        outputs: outputs.iter().map(parse_output).collect::<Result<_, _>>()?,
    })
}

pub fn parse_tx(body: &str) -> Result<TxInfo, String> {
    parse_tx_value(&json::parse(body).map_err(|_| malformed("not JSON"))?)
}

pub fn parse_tx_list(body: &str) -> Result<Vec<TxInfo>, String> {
    let doc = json::parse(body).map_err(|_| malformed("not JSON"))?;
    doc.as_array()
        .ok_or_else(|| malformed("not a list"))?
        .iter()
        .map(parse_tx_value)
        .collect()
}

/// `http` is only used for https URLs, or http to the local machine for a node run there.
pub fn validate_base_url(url: &str) -> Result<String, String> {
    let url = url.trim().trim_end_matches('/');
    let host = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or("the Bitcoin API URL must start with https://")?;
    let host_name = host.split(['/', ':', '?', '#']).next().unwrap_or("");
    let local = ["localhost", "127.0.0.1", "[::1]"].contains(&host_name);
    if url.starts_with("http://") && !local {
        return Err(
            "the Bitcoin API URL must use https (http is allowed for localhost only)".to_string(),
        );
    }
    if host_name.is_empty() || url.contains('@') {
        return Err("the Bitcoin API URL is not valid".to_string());
    }
    Ok(url.to_string())
}

fn is_not_found(error: &str) -> bool {
    error.starts_with("HTTP status 404")
}

impl<'a> EsploraClient<'a> {
    pub fn new(http: &'a dyn HttpClient, base_url: &str) -> Result<EsploraClient<'a>, String> {
        Ok(EsploraClient {
            http,
            base: validate_base_url(base_url)?,
        })
    }

    /// Host only, for messages: a provider URL may carry a secret in its path.
    pub fn host(&self) -> String {
        let rest = self.base.split("://").nth(1).unwrap_or(&self.base);
        rest.split(['/', '?', '#'])
            .next()
            .unwrap_or(rest)
            .to_string()
    }

    fn get(&self, path: &str) -> Result<String, String> {
        self.http
            .get(&format!("{}{path}", self.base))
            .map_err(|e| format!("Bitcoin API request failed: {}", sanitize_text_max(&e, 300)))
    }

    /// `None` when the server answers 404.
    fn get_optional(&self, path: &str) -> Result<Option<String>, String> {
        match self.http.get(&format!("{}{path}", self.base)) {
            Ok(body) => Ok(Some(body)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(format!(
                "Bitcoin API request failed: {}",
                sanitize_text_max(&e, 300)
            )),
        }
    }

    /// The hash of block 0, which identifies the network the server follows.
    pub fn genesis_hash(&self) -> Result<String, String> {
        let body = self.get("/block-height/0")?;
        let hash = body.trim().to_ascii_lowercase();
        if !is_hex_id(&hash) {
            return Err(malformed("the genesis block hash is not 64 hex digits"));
        }
        Ok(hash)
    }

    pub fn tip_height(&self) -> Result<u64, String> {
        let body = self.get("/blocks/tip/height")?;
        body.trim()
            .parse::<u64>()
            .ok()
            .filter(|h| *h > 0 && *h < MAX_PLAUSIBLE_HEIGHT)
            .ok_or_else(|| malformed("the block height is not a plausible number"))
    }

    pub fn fee_estimates(&self) -> Result<FeeEstimates, String> {
        parse_fee_estimates(&self.get("/fee-estimates")?)
    }

    pub fn address_stats(&self, address: &str) -> Result<AddressStats, String> {
        parse_address_stats(&self.get(&format!("/address/{address}"))?)
    }

    pub fn utxos(&self, address: &str) -> Result<Vec<RawUtxo>, String> {
        parse_utxos(&self.get(&format!("/address/{address}/utxo"))?)
    }

    /// `None` when the server does not know the transaction.
    pub fn transaction(&self, txid: &str) -> Result<Option<TxInfo>, String> {
        match self.get_optional(&format!("/tx/{txid}"))? {
            Some(body) => {
                let tx = parse_tx(&body)?;
                if tx.txid != txid.to_ascii_lowercase() {
                    return Err(malformed(
                        "the server answered with a different transaction",
                    ));
                }
                Ok(Some(tx))
            }
            None => Ok(None),
        }
    }

    /// Mempool and confirmed transactions of an address, newest first; pass the last confirmed
    /// transaction id of the previous page to continue.
    pub fn address_transactions(
        &self,
        address: &str,
        after: Option<&str>,
    ) -> Result<Vec<TxInfo>, String> {
        let path = match after {
            Some(last) => format!("/address/{address}/txs/chain/{last}"),
            None => format!("/address/{address}/txs"),
        };
        parse_tx_list(&self.get(&path)?)
    }

    /// Submits a raw transaction. The server's answer is the transaction id, which the caller
    /// compares with the id it computed itself.
    pub fn broadcast(&self, raw_hex: &str) -> Result<String, String> {
        let answer = self
            .http
            .post_text(&format!("{}/tx", self.base), raw_hex)
            .map_err(|e| sanitize_text_max(&e, 300))?;
        Ok(answer.trim().to_ascii_lowercase())
    }
}

/// A rejection from the Bitcoin node behind the API (the API answers 400), as opposed to the
/// request not arriving or the answer being lost.
pub fn is_definite_rejection(error: &str) -> bool {
    error.starts_with("HTTP status 400")
}

/// The node already has the transaction, which for a re-broadcast is the outcome wanted.
pub fn is_already_known(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("already in block chain")
        || lower.contains("already known")
        || lower.contains("already have transaction")
        || lower.contains("txn-already-in-mempool")
        || lower.contains("txn-already-known")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitcoin::testing::EsploraMock;

    const TXID_A: &str = "a8e94088b5f8178f77761ab9b3c8885edcd6b616debe17bd0ffd5c27cbec714a";

    #[test]
    fn urls_are_validated() {
        assert_eq!(
            validate_base_url("https://blockstream.info/api/").unwrap(),
            "https://blockstream.info/api"
        );
        assert!(validate_base_url("http://localhost:3000/api").is_ok());
        assert!(validate_base_url("http://127.0.0.1:3000").is_ok());
        for bad in [
            "http://blockstream.info/api",
            "ftp://x.org",
            "blockstream.info",
            "https://",
            "https://user:pass@x.org/api",
            "",
        ] {
            assert!(validate_base_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_the_host_is_shown() {
        let mock = EsploraMock::new();
        let client = EsploraClient::new(&mock, "https://node.example/secret-key/api").unwrap();
        assert_eq!(client.host(), "node.example");
    }

    #[test]
    fn parses_utxos() {
        let body = format!(
            r#"[{{"txid":"{TXID_A}","vout":1,"status":{{"confirmed":true,"block_height":900000,"block_time":1700000000}},"value":12345}},
               {{"txid":"{TXID_A}","vout":0,"status":{{"confirmed":false}},"value":500}}]"#
        );
        let utxos = parse_utxos(&body).unwrap();
        assert_eq!(utxos.len(), 2);
        assert_eq!(utxos[0].value, 12_345);
        assert_eq!(utxos[0].status.block_height, Some(900_000));
        assert!(!utxos[1].status.confirmed);
        assert_eq!(utxos[1].status.block_height, None);
    }

    #[test]
    fn rejects_malformed_utxos() {
        for body in [
            "not json",
            "{}",
            r#"[{"txid":"zz","vout":0,"value":1,"status":{"confirmed":false}}]"#,
            &format!(r#"[{{"txid":"{TXID_A}","vout":0,"status":{{"confirmed":false}}}}]"#),
            &format!(
                r#"[{{"txid":"{TXID_A}","vout":-1,"value":1,"status":{{"confirmed":false}}}}]"#
            ),
            &format!(r#"[{{"txid":"{TXID_A}","vout":0,"value":1,"status":{{"confirmed":true}}}}]"#),
            &format!(r#"[{{"txid":"{TXID_A}","vout":0,"value":1}}]"#),
        ] {
            assert!(parse_utxos(body).is_err(), "{body}");
        }
    }

    #[test]
    fn parses_address_stats_with_pending_spends() {
        let body = r#"{"address":"x","chain_stats":{"funded_txo_count":2,"funded_txo_sum":150000,"spent_txo_count":1,"spent_txo_sum":50000,"tx_count":3},
            "mempool_stats":{"funded_txo_count":0,"funded_txo_sum":0,"spent_txo_count":1,"spent_txo_sum":20000,"tx_count":1}}"#;
        let stats = parse_address_stats(body).unwrap();
        assert_eq!(stats.confirmed, 100_000);
        assert_eq!(stats.pending, -20_000);
        assert!(parse_address_stats("{}").is_err());
    }

    #[test]
    fn parses_and_picks_fee_estimates() {
        let estimates =
            parse_fee_estimates(r#"{"1":50.5,"3":20,"6":5.25,"144":1.1,"1008":0.5,"x":3,"0":9}"#)
                .unwrap();
        assert_eq!(estimates.0.len(), 5);
        assert_eq!(estimates.for_target(6), Some((6, FeeRate(5_250))));
        // 4 blocks is not listed: the next faster target is used.
        assert_eq!(estimates.for_target(4), Some((3, FeeRate(20_000))));
        assert_eq!(estimates.for_target(5000), Some((1008, FeeRate(500))));
        // A target faster than any listed falls back to the fastest one.
        assert_eq!(estimates.for_target(1), Some((1, FeeRate(50_500))));
        assert!(parse_fee_estimates("{}").is_err());
        assert!(parse_fee_estimates("[]").is_err());
        assert!(parse_fee_estimates(r#"{"1":-3}"#).is_err());
    }

    fn tx_body(confirmed: bool) -> String {
        let status = if confirmed {
            r#"{"confirmed":true,"block_height":974795,"block_time":1793958771}"#
        } else {
            r#"{"confirmed":false}"#
        };
        format!(
            r#"{{"txid":"{TXID_A}","version":2,"locktime":0,
            "vin":[{{"txid":"{TXID_A}","vout":0,"prevout":{{"scriptpubkey":"00147f43c78a42904de31d3337acb1cd974917228649","scriptpubkey_address":"bc1q0apu0zjzjpx7x8fnx7ktrnvhfytj9pjf2vzpun","value":332}},"is_coinbase":false,"sequence":4294967295}}],
            "vout":[{{"scriptpubkey":"00202179e718ea6e1c0668372abab7f60d6a6ec4e2ec35c8d3832a52422826ffe4c5","scriptpubkey_address":"bc1qy9u7wx82dcwqv6ph92at0asddfhvfchvxhyd8qe22fpzsfhlunzs9ky5x2","value":300}}],
            "size":110,"weight":440,"fee":32,"status":{status}}}"#
        )
    }

    #[test]
    fn parses_a_transaction() {
        let tx = parse_tx(&tx_body(true)).unwrap();
        assert_eq!(tx.txid, TXID_A);
        assert_eq!(tx.fee, Some(32));
        assert_eq!(tx.status.block_height, Some(974_795));
        assert_eq!(tx.inputs[0].prevout_value, Some(332));
        assert_eq!(tx.inputs[0].prevout_script.as_ref().unwrap().len(), 22);
        assert_eq!(tx.outputs[0].value, 300);
        assert!(!parse_tx(&tx_body(false)).unwrap().status.confirmed);
    }

    #[test]
    fn coinbase_inputs_have_no_previous_output() {
        let body = format!(
            r#"{{"txid":"{TXID_A}","vin":[{{"is_coinbase":true,"prevout":null,"vout":0}}],"vout":[{{"scriptpubkey":"0014{}","value":625000000}}],"status":{{"confirmed":false}}}}"#,
            "00".repeat(20)
        );
        let tx = parse_tx(&body).unwrap();
        assert!(tx.inputs[0].is_coinbase);
        assert_eq!(tx.inputs[0].prevout_value, None);
    }

    #[test]
    fn a_mismatched_transaction_answer_is_refused() {
        let other = "b".repeat(64);
        let mock = EsploraMock::new().get(&format!("/tx/{other}"), Ok(tx_body(true)));
        let client = EsploraClient::new(&mock, "https://x.test/api").unwrap();
        assert!(
            client
                .transaction(&other)
                .unwrap_err()
                .contains("different transaction")
        );
    }

    #[test]
    fn unknown_transactions_are_none_and_other_errors_surface() {
        let mock = EsploraMock::new()
            .get(
                &format!("/tx/{}", "c".repeat(64)),
                Err("HTTP status 404: Transaction not found".to_string()),
            )
            .get(
                &format!("/tx/{}", "d".repeat(64)),
                Err("HTTP status 500".to_string()),
            );
        let client = EsploraClient::new(&mock, "https://x.test/api").unwrap();
        assert_eq!(client.transaction(&"c".repeat(64)).unwrap(), None);
        assert!(
            client
                .transaction(&"d".repeat(64))
                .unwrap_err()
                .contains("500")
        );
    }

    #[test]
    fn chain_facts_are_checked_for_plausibility() {
        let mock = EsploraMock::new()
            .get(
                "/block-height/0",
                Ok(
                    "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f\n"
                        .to_string(),
                ),
            )
            .get("/blocks/tip/height", Ok("975181".to_string()));
        let client = EsploraClient::new(&mock, "https://x.test/api").unwrap();
        assert!(
            client
                .genesis_hash()
                .unwrap()
                .starts_with("000000000019d668")
        );
        assert_eq!(client.tip_height().unwrap(), 975_181);

        for body in ["0", "-5", "abc", "99999999999999", ""] {
            let mock = EsploraMock::new().get("/blocks/tip/height", Ok(body.to_string()));
            let client = EsploraClient::new(&mock, "https://x.test/api").unwrap();
            assert!(client.tip_height().is_err(), "{body:?}");
        }
        let mock = EsploraMock::new().get("/block-height/0", Ok("<html>".to_string()));
        let client = EsploraClient::new(&mock, "https://x.test/api").unwrap();
        assert!(client.genesis_hash().is_err());
    }

    #[test]
    fn broadcast_posts_the_raw_transaction() {
        let mock = EsploraMock::new().post("/tx", Ok(format!("{TXID_A}\n")));
        let client = EsploraClient::new(&mock, "https://x.test/api").unwrap();
        assert_eq!(client.broadcast("0200abcd").unwrap(), TXID_A);
        assert_eq!(
            mock.posted(),
            vec![("https://x.test/api/tx".to_string(), "0200abcd".to_string())]
        );
    }

    #[test]
    fn rejections_are_told_apart_from_lost_answers() {
        assert!(is_definite_rejection(
            "HTTP status 400: sendrawtransaction RPC error -26: bad-txns"
        ));
        assert!(!is_definite_rejection("request failed: timeout"));
        assert!(!is_definite_rejection("HTTP status 502"));
        assert!(is_already_known(
            "HTTP status 400: sendrawtransaction RPC error -27: Transaction already in block chain"
        ));
        assert!(is_already_known("txn-already-in-mempool"));
        assert!(!is_already_known(
            "HTTP status 400: bad-txns-inputs-missingorspent"
        ));
    }

    #[test]
    fn pages_use_the_chain_cursor() {
        let mock = EsploraMock::new()
            .get("/address/bc1qx/txs/chain/abc", Ok("[]".to_string()))
            .get("/address/bc1qx/txs", Ok(format!("[{}]", tx_body(true))));
        let client = EsploraClient::new(&mock, "https://x.test/api").unwrap();
        assert_eq!(client.address_transactions("bc1qx", None).unwrap().len(), 1);
        assert!(
            client
                .address_transactions("bc1qx", Some("abc"))
                .unwrap()
                .is_empty()
        );
    }
}
