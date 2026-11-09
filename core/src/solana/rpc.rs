//! Solana JSON-RPC client: reading balances and accounts, simulating, sending and looking up
//! transactions. Everything a node says is treated as untrusted data.

use std::cell::Cell;
use std::fmt;

use super::address::{self, Pubkey};
use crate::crypto::{base58, base64};
use crate::eth::explorer::sanitize_text_max;
use crate::http::HttpClient;
use crate::json::{self, Value};

const MAX_LOGS: usize = 40;
/// `getMultipleAccounts` accepts at most this many addresses per request.
pub const MAX_ACCOUNTS_PER_CALL: usize = 100;

#[derive(Debug, Clone, PartialEq)]
pub enum RpcError {
    Transport(String),
    Node {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    Malformed(String),
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RpcError::Transport(m) => write!(f, "network error: {}", sanitize_text_max(m, 300)),
            RpcError::Node { code, message, .. } => {
                write!(
                    f,
                    "Solana node error {code}: {}",
                    sanitize_text_max(message, 300)
                )
            }
            RpcError::Malformed(m) => write!(
                f,
                "unexpected response from the Solana node: {}",
                sanitize_text_max(m, 200)
            ),
        }
    }
}

impl From<RpcError> for String {
    fn from(e: RpcError) -> String {
        e.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    pub lamports: u64,
    pub owner: Pubkey,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenAccount {
    pub address: Pubkey,
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
    pub decimals: u8,
    pub program: Pubkey,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Simulation {
    pub error: Option<String>,
    pub logs: Vec<String>,
    pub units_consumed: Option<u64>,
    /// The requested accounts as they would be after the transaction.
    pub accounts: Vec<Option<AccountInfo>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureStatus {
    pub slot: u64,
    pub confirmations: Option<u64>,
    pub error: Option<String>,
    /// `processed`, `confirmed` or `finalized`.
    pub confirmation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureInfo {
    pub signature: String,
    pub slot: u64,
    pub block_time: Option<i64>,
    pub error: Option<String>,
    pub memo: Option<String>,
    pub confirmation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenBalance {
    pub account_index: usize,
    pub mint: Pubkey,
    pub owner: Option<Pubkey>,
    pub amount: u128,
    pub decimals: u8,
}

/// A confirmed transaction as the node records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionRecord {
    pub slot: u64,
    pub block_time: Option<i64>,
    pub fee: u64,
    pub error: Option<String>,
    /// Static account keys followed by the ones loaded from lookup tables.
    pub accounts: Vec<Pubkey>,
    pub pre_balances: Vec<u64>,
    pub post_balances: Vec<u64>,
    pub pre_tokens: Vec<TokenBalance>,
    pub post_tokens: Vec<TokenBalance>,
}

pub struct SolanaRpc<'a> {
    http: &'a dyn HttpClient,
    url: String,
    next_id: Cell<u64>,
}

fn malformed(what: &str) -> RpcError {
    RpcError::Malformed(what.to_string())
}

fn number(v: Option<&Value>, what: &str) -> Result<u64, RpcError> {
    v.and_then(Value::as_u64)
        .ok_or_else(|| malformed(&format!("{what} is missing or not a number")))
}

fn pubkey(v: Option<&Value>, what: &str) -> Result<Pubkey, RpcError> {
    let text = v
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(&format!("{what} is missing")))?;
    address::parse(text).map_err(|_| malformed(&format!("{what} is not an address")))
}

/// Short, readable text for the `err` field the node reports for a failed transaction.
pub fn describe_tx_error(err: &Value) -> String {
    let text = match err {
        Value::String(s) => match s.as_str() {
            "BlockhashNotFound" => {
                "the transaction's blockhash expired; build it again".to_string()
            }
            "InsufficientFundsForFee" => "not enough SOL to pay the network fee".to_string(),
            "InsufficientFundsForRent" => {
                "an account would end up with too little SOL to stay open (rent)".to_string()
            }
            "AccountNotFound" => "the fee payer account has no SOL or does not exist".to_string(),
            "AlreadyProcessed" => "this transaction was already processed".to_string(),
            other => other.to_string(),
        },
        other => other.to_json(),
    };
    sanitize_text_max(&text, 200)
}

fn error_of(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) => None,
        Some(e) => Some(describe_tx_error(e)),
    }
}

fn parse_account(v: &Value) -> Result<Option<AccountInfo>, RpcError> {
    if matches!(v, Value::Null) {
        return Ok(None);
    }
    let encoded = v
        .get("data")
        .and_then(Value::as_array)
        .and_then(|d| d.first())
        .and_then(Value::as_str)
        .ok_or_else(|| malformed("account data is missing"))?;
    Ok(Some(AccountInfo {
        lamports: number(v.get("lamports"), "account balance")?,
        owner: pubkey(v.get("owner"), "account owner")?,
        data: base64::decode(encoded).map_err(|_| malformed("account data is not base64"))?,
    }))
}

fn parse_accounts(list: &Value) -> Result<Vec<Option<AccountInfo>>, RpcError> {
    list.as_array()
        .ok_or_else(|| malformed("accounts are not a list"))?
        .iter()
        .map(parse_account)
        .collect()
}

fn parse_token_account(entry: &Value) -> Option<TokenAccount> {
    let account = entry.get("account")?;
    let info = account.get("data")?.get("parsed")?.get("info")?;
    let amount = info.get("tokenAmount")?;
    Some(TokenAccount {
        address: address::parse(entry.get("pubkey")?.as_str()?).ok()?,
        mint: address::parse(info.get("mint")?.as_str()?).ok()?,
        owner: address::parse(info.get("owner")?.as_str()?).ok()?,
        amount: amount.get("amount")?.as_u64()?,
        decimals: u8::try_from(amount.get("decimals")?.as_u64()?).ok()?,
        program: address::parse(account.get("owner")?.as_str()?).ok()?,
    })
}

fn parse_token_balances(list: Option<&Value>) -> Vec<TokenBalance> {
    list.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|b| {
            Some(TokenBalance {
                account_index: usize::try_from(b.get("accountIndex")?.as_u64()?).ok()?,
                mint: address::parse(b.get("mint")?.as_str()?).ok()?,
                owner: b
                    .get("owner")
                    .and_then(Value::as_str)
                    .and_then(|o| address::parse(o).ok()),
                amount: b
                    .get("uiTokenAmount")?
                    .get("amount")?
                    .as_str()?
                    .parse::<u128>()
                    .ok()?,
                decimals: u8::try_from(b.get("uiTokenAmount")?.get("decimals")?.as_u64()?).ok()?,
            })
        })
        .collect()
}

fn parse_u64_list(v: Option<&Value>) -> Vec<u64> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .collect()
}

fn parse_status(v: &Value) -> Option<SignatureStatus> {
    if matches!(v, Value::Null) {
        return None;
    }
    Some(SignatureStatus {
        slot: v.get("slot")?.as_u64()?,
        confirmations: v.get("confirmations").and_then(Value::as_u64),
        error: error_of(v.get("err")),
        confirmation: v
            .get("confirmationStatus")
            .and_then(Value::as_str)
            .map(|s| sanitize_text_max(s, 20)),
    })
}

fn parse_record(v: &Value) -> Result<TransactionRecord, RpcError> {
    let meta = v
        .get("meta")
        .ok_or_else(|| malformed("transaction has no metadata"))?;
    let keys = v
        .get("transaction")
        .and_then(|t| t.get("message"))
        .and_then(|m| m.get("accountKeys"))
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("transaction has no account keys"))?;
    let mut accounts = vec![];
    for key in keys {
        accounts.push(pubkey(Some(key), "account key")?);
    }
    if let Some(loaded) = meta.get("loadedAddresses") {
        for group in ["writable", "readonly"] {
            for key in loaded
                .get(group)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                accounts.push(pubkey(Some(key), "loaded account key")?);
            }
        }
    }
    Ok(TransactionRecord {
        slot: number(v.get("slot"), "slot")?,
        block_time: v
            .get("blockTime")
            .and_then(Value::as_u64)
            .and_then(|t| i64::try_from(t).ok()),
        fee: number(meta.get("fee"), "fee")?,
        error: error_of(meta.get("err")),
        accounts,
        pre_balances: parse_u64_list(meta.get("preBalances")),
        post_balances: parse_u64_list(meta.get("postBalances")),
        pre_tokens: parse_token_balances(meta.get("preTokenBalances")),
        post_tokens: parse_token_balances(meta.get("postTokenBalances")),
    })
}

impl<'a> SolanaRpc<'a> {
    pub fn new(http: &'a dyn HttpClient, url: &str) -> SolanaRpc<'a> {
        SolanaRpc {
            http,
            url: url.to_string(),
            next_id: Cell::new(1),
        }
    }

    pub fn call(&self, method: &str, params: Vec<Value>) -> Result<Value, RpcError> {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let request = Value::object(vec![
            ("jsonrpc", Value::str("2.0")),
            ("id", Value::Number(id.to_string())),
            ("method", Value::str(method)),
            ("params", Value::Array(params)),
        ]);
        let body = self
            .http
            .post_json(&self.url, &request.to_json())
            .map_err(RpcError::Transport)?;
        let response = json::parse(&body).map_err(|_| malformed("response is not valid JSON"))?;
        if let Some(err) = response.get("error") {
            return Err(RpcError::Node {
                code: match err.get("code") {
                    Some(Value::Number(n)) => n.parse::<i64>().unwrap_or(0),
                    _ => 0,
                },
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string(),
                data: err.get("data").cloned(),
            });
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| malformed("response has neither result nor error"))
    }

    fn config(pairs: Vec<(&str, Value)>) -> Value {
        Value::object(pairs)
    }

    fn commitment() -> (&'static str, Value) {
        ("commitment", Value::str("confirmed"))
    }

    /// The hash of the cluster's first block; identifies which cluster the node serves.
    pub fn genesis_hash(&self) -> Result<String, RpcError> {
        self.call("getGenesisHash", vec![])?
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| malformed("genesis hash is not text"))
    }

    pub fn balance(&self, owner: &Pubkey) -> Result<u64, RpcError> {
        let result = self.call(
            "getBalance",
            vec![
                Value::String(address::to_text(owner)),
                Self::config(vec![Self::commitment()]),
            ],
        )?;
        number(result.get("value"), "balance")
    }

    /// A recent blockhash and the last block height at which a transaction using it is valid.
    pub fn latest_blockhash(&self) -> Result<([u8; 32], u64), RpcError> {
        let result = self.call(
            "getLatestBlockhash",
            vec![Self::config(vec![Self::commitment()])],
        )?;
        let value = result
            .get("value")
            .ok_or_else(|| malformed("blockhash is missing"))?;
        let hash = base58::decode_fixed::<32>(
            value
                .get("blockhash")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed("blockhash is missing"))?,
        )
        .map_err(|_| malformed("blockhash is not valid"))?;
        Ok((
            hash,
            number(value.get("lastValidBlockHeight"), "block height")?,
        ))
    }

    /// Whether a blockhash is recent enough for a transaction using it to be accepted.
    pub fn is_blockhash_valid(&self, hash: &[u8; 32]) -> Result<bool, RpcError> {
        let result = self.call(
            "isBlockhashValid",
            vec![
                Value::String(base58::encode(hash)),
                Self::config(vec![Self::commitment()]),
            ],
        )?;
        result
            .get("value")
            .and_then(Value::as_bool)
            .ok_or_else(|| malformed("blockhash validity is missing"))
    }

    pub fn account(&self, key: &Pubkey) -> Result<Option<AccountInfo>, RpcError> {
        let result = self.call(
            "getAccountInfo",
            vec![
                Value::String(address::to_text(key)),
                Self::config(vec![("encoding", Value::str("base64")), Self::commitment()]),
            ],
        )?;
        parse_account(result.get("value").unwrap_or(&Value::Null))
    }

    /// Several accounts in order; `None` for ones that do not exist.
    pub fn accounts(&self, keys: &[Pubkey]) -> Result<Vec<Option<AccountInfo>>, RpcError> {
        let mut out = vec![];
        for chunk in keys.chunks(MAX_ACCOUNTS_PER_CALL) {
            let list = Value::Array(
                chunk
                    .iter()
                    .map(|k| Value::String(address::to_text(k)))
                    .collect(),
            );
            let result = self.call(
                "getMultipleAccounts",
                vec![
                    list,
                    Self::config(vec![("encoding", Value::str("base64")), Self::commitment()]),
                ],
            )?;
            let found = parse_accounts(result.get("value").unwrap_or(&Value::Null))?;
            if found.len() != chunk.len() {
                return Err(malformed("the node returned the wrong number of accounts"));
            }
            out.extend(found);
        }
        Ok(out)
    }

    fn token_accounts_filtered(
        &self,
        owner: &Pubkey,
        filter: Value,
    ) -> Result<Vec<TokenAccount>, RpcError> {
        let result = self.call(
            "getTokenAccountsByOwner",
            vec![
                Value::String(address::to_text(owner)),
                filter,
                Self::config(vec![
                    ("encoding", Value::str("jsonParsed")),
                    Self::commitment(),
                ]),
            ],
        )?;
        Ok(result
            .get("value")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(parse_token_account)
            .filter(|t| t.owner == *owner)
            .collect())
    }

    /// Every SPL Token and Token-2022 account the owner holds.
    pub fn token_accounts(&self, owner: &Pubkey) -> Result<Vec<TokenAccount>, RpcError> {
        let mut all = vec![];
        for program in [address::TOKEN_PROGRAM, address::TOKEN_2022_PROGRAM] {
            let filter = Value::object(vec![("programId", Value::str(program))]);
            all.extend(self.token_accounts_filtered(owner, filter)?);
        }
        Ok(all)
    }

    pub fn token_accounts_for_mint(
        &self,
        owner: &Pubkey,
        mint: &Pubkey,
    ) -> Result<Vec<TokenAccount>, RpcError> {
        let filter = Value::object(vec![("mint", Value::String(address::to_text(mint)))]);
        let mut found = self.token_accounts_filtered(owner, filter)?;
        found.retain(|t| t.mint == *mint);
        Ok(found)
    }

    /// Dry-runs a serialized transaction without checking signatures and with a fresh blockhash,
    /// returning the state of `addresses` as it would be afterwards.
    pub fn simulate(&self, tx_bytes: &[u8], addresses: &[Pubkey]) -> Result<Simulation, RpcError> {
        let mut options = vec![
            ("encoding", Value::str("base64")),
            ("sigVerify", Value::Bool(false)),
            ("replaceRecentBlockhash", Value::Bool(true)),
            Self::commitment(),
        ];
        if !addresses.is_empty() {
            options.push((
                "accounts",
                Value::object(vec![
                    ("encoding", Value::str("base64")),
                    (
                        "addresses",
                        Value::Array(
                            addresses
                                .iter()
                                .map(|k| Value::String(address::to_text(k)))
                                .collect(),
                        ),
                    ),
                ]),
            ));
        }
        let result = self.call(
            "simulateTransaction",
            vec![
                Value::String(base64::encode(tx_bytes)),
                Self::config(options),
            ],
        )?;
        let value = result
            .get("value")
            .ok_or_else(|| malformed("simulation has no result"))?;
        let logs: Vec<String> = value
            .get("logs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|l| sanitize_text_max(l, 200))
            .collect();
        let start = logs.len().saturating_sub(MAX_LOGS);
        let mut error = error_of(value.get("err"));
        if let Some(e) = &mut error
            && let Some(line) = logs
                .iter()
                .rev()
                .find(|l| l.contains("failed") || l.contains("Error"))
        {
            e.push_str(&format!(" ({line})"));
        }
        let accounts = match value.get("accounts") {
            Some(list) if !matches!(list, Value::Null) => parse_accounts(list)?,
            _ => vec![],
        };
        if !addresses.is_empty() && error.is_none() && accounts.len() != addresses.len() {
            return Err(malformed(
                "the simulation returned the wrong number of accounts",
            ));
        }
        Ok(Simulation {
            error,
            logs: logs[start..].to_vec(),
            units_consumed: value.get("unitsConsumed").and_then(Value::as_u64),
            accounts,
        })
    }

    /// Submits a signed transaction; returns its signature. The node checks it by simulating
    /// first (preflight).
    pub fn send(&self, tx_bytes: &[u8]) -> Result<String, RpcError> {
        let result = self.call(
            "sendTransaction",
            vec![
                Value::String(base64::encode(tx_bytes)),
                Self::config(vec![
                    ("encoding", Value::str("base64")),
                    ("skipPreflight", Value::Bool(false)),
                    ("preflightCommitment", Value::str("confirmed")),
                    ("maxRetries", Value::int(5)),
                ]),
            ],
        )?;
        result
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| malformed("the node did not return a signature"))
    }

    pub fn signature_statuses(
        &self,
        signatures: &[String],
    ) -> Result<Vec<Option<SignatureStatus>>, RpcError> {
        let result = self.call(
            "getSignatureStatuses",
            vec![
                Value::Array(signatures.iter().map(|s| Value::str(s)).collect()),
                Self::config(vec![("searchTransactionHistory", Value::Bool(true))]),
            ],
        )?;
        let list = result
            .get("value")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed("statuses are missing"))?;
        Ok(list.iter().map(parse_status).collect())
    }

    pub fn signatures_for_address(
        &self,
        owner: &Pubkey,
        limit: usize,
    ) -> Result<Vec<SignatureInfo>, RpcError> {
        let result = self.call(
            "getSignaturesForAddress",
            vec![
                Value::String(address::to_text(owner)),
                Self::config(vec![
                    ("limit", Value::int(limit as i64)),
                    Self::commitment(),
                ]),
            ],
        )?;
        Ok(result
            .as_array()
            .ok_or_else(|| malformed("signatures are not a list"))?
            .iter()
            .filter_map(|s| {
                Some(SignatureInfo {
                    signature: s.get("signature")?.as_str()?.to_string(),
                    slot: s.get("slot")?.as_u64()?,
                    block_time: s
                        .get("blockTime")
                        .and_then(Value::as_u64)
                        .and_then(|t| i64::try_from(t).ok()),
                    error: error_of(s.get("err")),
                    memo: s
                        .get("memo")
                        .and_then(Value::as_str)
                        .map(|m| sanitize_text_max(m, 200)),
                    confirmation: s
                        .get("confirmationStatus")
                        .and_then(Value::as_str)
                        .map(|c| sanitize_text_max(c, 20)),
                })
            })
            .collect())
    }

    /// A transaction by signature; `None` when the node does not have it.
    pub fn transaction(&self, signature: &str) -> Result<Option<TransactionRecord>, RpcError> {
        let result = self.call(
            "getTransaction",
            vec![
                Value::str(signature),
                Self::config(vec![
                    ("encoding", Value::str("json")),
                    ("maxSupportedTransactionVersion", Value::int(0)),
                    Self::commitment(),
                ]),
            ],
        )?;
        if matches!(result, Value::Null) {
            return Ok(None);
        }
        parse_record(&result).map(Some)
    }

    /// The typical priority fee in micro-lamports per compute unit lately paid on this cluster
    /// (the median of the recent samples). Zero when unknown.
    pub fn recent_priority_fee(&self) -> u64 {
        let Ok(result) = self.call("getRecentPrioritizationFees", vec![]) else {
            return 0;
        };
        let mut fees: Vec<u64> = result
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e.get("prioritizationFee").and_then(Value::as_u64))
            .collect();
        if fees.is_empty() {
            return 0;
        }
        fees.sort_unstable();
        fees[fees.len() / 2]
    }
}

/// The signature of a transaction, as base58 text, validated to be 64 bytes.
pub fn parse_signature(text: &str) -> Result<String, String> {
    let trimmed = text.trim();
    let bytes = base58::decode_fixed::<64>(trimmed).map_err(|_| {
        "a Solana transaction signature is a base58 string of 64 bytes (87 or 88 characters)"
            .to_string()
    })?;
    Ok(base58::encode(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::mock::RouterMock;

    const OWNER: &str = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";

    fn rpc<'a>(mock: &'a RouterMock) -> SolanaRpc<'a> {
        SolanaRpc::new(mock, "https://example.invalid")
    }

    fn owner() -> Pubkey {
        address::parse(OWNER).unwrap()
    }

    #[test]
    fn reads_balances_and_genesis_hash() {
        let mock = RouterMock::new()
            .rpc("getBalance", r#"{"context":{"slot":1},"value":2500000}"#)
            .rpc("getGenesisHash", "\"abc\"");
        assert_eq!(rpc(&mock).balance(&owner()).unwrap(), 2_500_000);
        assert_eq!(rpc(&mock).genesis_hash().unwrap(), "abc");
    }

    #[test]
    fn node_errors_and_bad_answers_are_reported() {
        let mock = RouterMock::new()
            .rpc_error("getBalance", 429, "Too many requests", None)
            .rpc("getGenesisHash", "7");
        let err = rpc(&mock).balance(&owner()).unwrap_err();
        assert!(matches!(err, RpcError::Node { code: 429, .. }));
        assert!(err.to_string().contains("Too many requests"));
        assert!(matches!(
            rpc(&mock).genesis_hash().unwrap_err(),
            RpcError::Malformed(_)
        ));
        let unknown = RouterMock::new();
        assert!(rpc(&unknown).balance(&owner()).is_err());
    }

    #[test]
    fn reads_the_latest_blockhash() {
        let hash = base58::encode(&[7u8; 32]);
        let mock = RouterMock::new().rpc(
            "getLatestBlockhash",
            &format!(r#"{{"value":{{"blockhash":"{hash}","lastValidBlockHeight":900}}}}"#),
        );
        assert_eq!(rpc(&mock).latest_blockhash().unwrap(), ([7u8; 32], 900));
        let bad = RouterMock::new().rpc(
            "getLatestBlockhash",
            r#"{"value":{"blockhash":"nope","lastValidBlockHeight":900}}"#,
        );
        assert!(rpc(&bad).latest_blockhash().is_err());
    }

    #[test]
    fn checks_whether_a_blockhash_is_still_valid() {
        let valid = RouterMock::new().rpc("isBlockhashValid", r#"{"value":true}"#);
        assert!(rpc(&valid).is_blockhash_valid(&[1u8; 32]).unwrap());
        let expired = RouterMock::new().rpc("isBlockhashValid", r#"{"value":false}"#);
        assert!(!rpc(&expired).is_blockhash_valid(&[1u8; 32]).unwrap());
        let broken = RouterMock::new().rpc("isBlockhashValid", "{}");
        assert!(rpc(&broken).is_blockhash_valid(&[1u8; 32]).is_err());
    }

    fn account_json(lamports: u64, owner: &str, data: &[u8]) -> String {
        format!(
            r#"{{"lamports":{lamports},"owner":"{owner}","data":["{}","base64"],"executable":false}}"#,
            base64::encode(data)
        )
    }

    #[test]
    fn reads_accounts_and_missing_accounts() {
        let one = account_json(5, address::SYSTEM_PROGRAM, &[1, 2, 3]);
        let mock = RouterMock::new()
            .rpc("getAccountInfo", &format!(r#"{{"value":{one}}}"#))
            .rpc(
                "getMultipleAccounts",
                &format!(r#"{{"value":[{one},null]}}"#),
            );
        let account = rpc(&mock).account(&owner()).unwrap().unwrap();
        assert_eq!(account.lamports, 5);
        assert_eq!(account.data, [1, 2, 3]);
        assert_eq!(account.owner, [0u8; 32]);
        let many = rpc(&mock).accounts(&[owner(), owner()]).unwrap();
        assert_eq!(many.len(), 2);
        assert!(many[0].is_some() && many[1].is_none());

        let none = RouterMock::new().rpc("getAccountInfo", r#"{"value":null}"#);
        assert!(rpc(&none).account(&owner()).unwrap().is_none());
        let short = RouterMock::new().rpc("getMultipleAccounts", r#"{"value":[null]}"#);
        assert!(rpc(&short).accounts(&[owner(), owner()]).is_err());
    }

    pub(crate) fn token_entry(
        address: &Pubkey,
        mint: &str,
        owner: &str,
        amount: u64,
        decimals: u8,
    ) -> String {
        format!(
            r#"{{"pubkey":"{}","account":{{"owner":"{}","data":{{"parsed":{{"info":{{"mint":"{mint}","owner":"{owner}","tokenAmount":{{"amount":"{amount}","decimals":{decimals}}}}}}}}}}}}}"#,
            address::to_text(address),
            address::TOKEN_PROGRAM
        )
    }

    #[test]
    fn reads_token_accounts_and_ignores_foreign_ones() {
        let usdc = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        let mine = token_entry(&[1u8; 32], usdc, OWNER, 1_500_000, 6);
        let theirs = token_entry(&[2u8; 32], usdc, address::SYSTEM_PROGRAM, 9, 6);
        let mock = RouterMock::new().rpc(
            "getTokenAccountsByOwner",
            &format!(r#"{{"value":[{mine},{theirs},{{"junk":1}}]}}"#),
        );
        let found = rpc(&mock)
            .token_accounts_for_mint(&owner(), &address::id(usdc))
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].amount, 1_500_000);
        assert_eq!(found[0].decimals, 6);
        assert_eq!(found[0].address, [1u8; 32]);
        assert_eq!(found[0].program, address::id(address::TOKEN_PROGRAM));
        // both token programs are asked for when listing everything
        let all = rpc(&mock).token_accounts(&owner()).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn simulation_reports_errors_logs_and_accounts() {
        let acct = account_json(10, address::SYSTEM_PROGRAM, &[]);
        let ok = RouterMock::new().rpc(
            "simulateTransaction",
            &format!(
                r#"{{"value":{{"err":null,"logs":["Program log: hi"],"unitsConsumed":150,"accounts":[{acct},null]}}}}"#
            ),
        );
        let sim = rpc(&ok).simulate(&[1, 2, 3], &[owner(), owner()]).unwrap();
        assert!(sim.error.is_none());
        assert_eq!(sim.units_consumed, Some(150));
        assert_eq!(sim.logs, ["Program log: hi"]);
        assert_eq!(sim.accounts.len(), 2);
        assert!(sim.accounts[1].is_none());

        let failing = RouterMock::new().rpc(
            "simulateTransaction",
            r#"{"value":{"err":{"InstructionError":[0,{"Custom":1}]},"logs":["Program log: Error: insufficient funds","Program X failed: custom program error: 0x1"],"unitsConsumed":10,"accounts":null}}"#,
        );
        let sim = rpc(&failing).simulate(&[1], &[]).unwrap();
        let error = sim.error.unwrap();
        assert!(
            error.contains("InstructionError") && error.contains("failed"),
            "{error}"
        );

        let blockhash = RouterMock::new().rpc(
            "simulateTransaction",
            r#"{"value":{"err":"BlockhashNotFound","logs":null,"accounts":null}}"#,
        );
        let sim = rpc(&blockhash).simulate(&[1], &[]).unwrap();
        assert!(sim.error.unwrap().contains("blockhash expired"));
        assert!(sim.logs.is_empty());

        let wrong_count = RouterMock::new().rpc(
            "simulateTransaction",
            r#"{"value":{"err":null,"logs":[],"accounts":[null]}}"#,
        );
        assert!(
            rpc(&wrong_count)
                .simulate(&[1], &[owner(), owner()])
                .is_err()
        );
    }

    #[test]
    fn simulation_requests_ask_for_unsigned_checks_and_accounts() {
        struct Spy(std::cell::RefCell<Vec<String>>);
        impl HttpClient for Spy {
            fn get(&self, _: &str) -> Result<String, String> {
                Err("no".into())
            }
            fn post_json(&self, _: &str, body: &str) -> Result<String, String> {
                self.0.borrow_mut().push(body.to_string());
                Ok(r#"{"result":{"value":{"err":null,"logs":[],"accounts":[null]}}}"#.to_string())
            }
        }
        let spy = Spy(Default::default());
        SolanaRpc::new(&spy, "https://x")
            .simulate(&[9, 9], &[owner()])
            .unwrap();
        let body = spy.0.borrow()[0].clone();
        assert!(body.contains("\"sigVerify\":false"));
        assert!(body.contains("\"replaceRecentBlockhash\":true"));
        assert!(body.contains(OWNER));
        assert!(body.contains(&base64::encode(&[9, 9])));
    }

    #[test]
    fn send_returns_the_signature_and_surfaces_preflight_failures() {
        let sig = base58::encode(&[5u8; 64]);
        let ok = RouterMock::new().rpc("sendTransaction", &format!("\"{sig}\""));
        assert_eq!(rpc(&ok).send(&[1]).unwrap(), sig);
        let rejected = RouterMock::new().rpc_error(
            "sendTransaction",
            -32002,
            "Transaction simulation failed: Blockhash not found",
            None,
        );
        let err = rpc(&rejected).send(&[1]).unwrap_err();
        assert!(matches!(err, RpcError::Node { code: -32002, .. }));
        let lost = RouterMock::new().rpc_sequence("sendTransaction", vec!["NET:timeout"]);
        assert!(matches!(
            rpc(&lost).send(&[1]).unwrap_err(),
            RpcError::Transport(_)
        ));
    }

    #[test]
    fn signature_statuses_parse_found_and_missing() {
        let mock = RouterMock::new().rpc(
            "getSignatureStatuses",
            r#"{"value":[null,{"slot":9,"confirmations":3,"err":null,"confirmationStatus":"confirmed"},{"slot":10,"confirmations":null,"err":{"InstructionError":[1,"Custom"]},"confirmationStatus":"finalized"}]}"#,
        );
        let statuses = rpc(&mock)
            .signature_statuses(&["a".into(), "b".into(), "c".into()])
            .unwrap();
        assert!(statuses[0].is_none());
        let found = statuses[1].as_ref().unwrap();
        assert_eq!((found.slot, found.confirmations), (9, Some(3)));
        assert_eq!(found.confirmation.as_deref(), Some("confirmed"));
        assert!(found.error.is_none());
        assert!(statuses[2].as_ref().unwrap().error.is_some());
    }

    #[test]
    fn signature_history_parses() {
        let mock = RouterMock::new().rpc(
            "getSignaturesForAddress",
            r#"[{"signature":"s1","slot":5,"blockTime":1700000000,"err":null,"memo":"[4] hi","confirmationStatus":"finalized"},{"signature":"s2","slot":4,"blockTime":null,"err":"AccountNotFound"},{"bad":true}]"#,
        );
        let items = rpc(&mock).signatures_for_address(&owner(), 10).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].block_time, Some(1_700_000_000));
        assert_eq!(items[0].memo.as_deref(), Some("[4] hi"));
        assert!(items[1].error.as_deref().unwrap().contains("no SOL"));
    }

    #[test]
    fn transactions_parse_with_balance_changes() {
        let me = OWNER;
        let other = address::to_text(&[2u8; 32]);
        let usdc = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        let record = format!(
            r#"{{"slot":77,"blockTime":1700000001,"transaction":{{"message":{{"accountKeys":["{me}","{other}"]}}}},"meta":{{"err":null,"fee":5000,"preBalances":[100,5],"postBalances":[50,55],"preTokenBalances":[{{"accountIndex":1,"mint":"{usdc}","owner":"{me}","uiTokenAmount":{{"amount":"10","decimals":6}}}}],"postTokenBalances":[{{"accountIndex":1,"mint":"{usdc}","owner":"{me}","uiTokenAmount":{{"amount":"4","decimals":6}}}}],"loadedAddresses":{{"writable":["{other}"],"readonly":[]}}}}}}"#
        );
        let mock = RouterMock::new().rpc("getTransaction", &record);
        let t = rpc(&mock).transaction("sig").unwrap().unwrap();
        assert_eq!((t.slot, t.fee), (77, 5000));
        assert_eq!(t.accounts.len(), 3);
        assert_eq!(t.pre_balances, [100, 5]);
        assert_eq!(t.post_balances, [50, 55]);
        assert_eq!(t.pre_tokens[0].amount, 10);
        assert_eq!(t.post_tokens[0].amount, 4);
        assert_eq!(t.post_tokens[0].owner, Some(owner()));
        assert!(t.error.is_none());
        let missing = RouterMock::new().rpc("getTransaction", "null");
        assert!(rpc(&missing).transaction("sig").unwrap().is_none());
        let broken = RouterMock::new().rpc("getTransaction", r#"{"slot":1}"#);
        assert!(rpc(&broken).transaction("sig").is_err());
    }

    #[test]
    fn priority_fee_is_the_median_and_failures_mean_zero() {
        let mock = RouterMock::new().rpc(
            "getRecentPrioritizationFees",
            r#"[{"slot":1,"prioritizationFee":0},{"slot":2,"prioritizationFee":900},{"slot":3,"prioritizationFee":100},{"slot":4,"prioritizationFee":500},{"slot":5,"prioritizationFee":50}]"#,
        );
        assert_eq!(rpc(&mock).recent_priority_fee(), 100);
        assert_eq!(rpc(&RouterMock::new()).recent_priority_fee(), 0);
        let empty = RouterMock::new().rpc("getRecentPrioritizationFees", "[]");
        assert_eq!(rpc(&empty).recent_priority_fee(), 0);
    }

    #[test]
    fn signatures_are_validated() {
        let sig = base58::encode(&[5u8; 64]);
        assert_eq!(parse_signature(&format!(" {sig} ")).unwrap(), sig);
        for bad in [
            "",
            "abc",
            &base58::encode(&[5u8; 63]),
            "0OIl",
            &"1".repeat(200),
        ] {
            assert!(parse_signature(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn error_text_is_readable_and_bounded() {
        assert!(describe_tx_error(&Value::str("InsufficientFundsForFee")).contains("network fee"));
        assert!(describe_tx_error(&Value::str("Weird\u{1b}Thing")).len() < 50);
        let long = Value::String("x".repeat(1000));
        assert!(describe_tx_error(&long).chars().count() <= 201);
    }
}
