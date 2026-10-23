//! Ethereum JSON-RPC client.

use std::cell::Cell;
use std::fmt;

use super::abi;
use super::address::{Address, to_checksum};
use super::journal::Journal;
use super::tx::Fees;
use super::u256::U256;
use crate::crypto::hex;
use crate::http::HttpClient;
use crate::json::{self, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcError {
    Transport(String),
    Node {
        code: i64,
        message: String,
        data: Option<String>,
    },
    Malformed(String),
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RpcError::Transport(m) => write!(f, "network error: {m}"),
            RpcError::Node { code, message, .. } => write!(f, "node error {code}: {message}"),
            RpcError::Malformed(m) => write!(f, "unexpected response from node: {m}"),
        }
    }
}

impl RpcError {
    /// Human-readable revert reason, when the node supplied one.
    pub fn revert_reason(&self) -> Option<String> {
        if let RpcError::Node { message, data, .. } = self {
            if let Some(bytes) = data.as_deref().and_then(|d| hex::decode(d).ok())
                && let Some(reason) = abi::decode_revert_reason(&bytes)
            {
                return Some(reason);
            }
            if message.to_lowercase().contains("revert") {
                return Some(message.clone());
            }
        }
        None
    }
}

#[derive(Debug, Clone, Default)]
pub struct CallRequest {
    pub from: Option<Address>,
    pub to: Option<Address>,
    pub value: U256,
    pub data: Vec<u8>,
    pub gas: Option<u64>,
}

impl CallRequest {
    pub fn to_json(&self) -> Value {
        let mut pairs: Vec<(&str, Value)> = vec![];
        if let Some(from) = &self.from {
            pairs.push(("from", Value::String(to_checksum(from))));
        }
        if let Some(to) = &self.to {
            pairs.push(("to", Value::String(to_checksum(to))));
        }
        if !self.value.is_zero() {
            pairs.push(("value", Value::String(self.value.to_hex_quantity())));
        }
        if !self.data.is_empty() {
            pairs.push(("data", Value::String(hex::encode_prefixed(&self.data))));
        }
        if let Some(gas) = self.gas {
            pairs.push(("gas", Value::String(U256::from_u64(gas).to_hex_quantity())));
        }
        Value::object(pairs)
    }
}

pub struct RpcClient<'a> {
    http: &'a dyn HttpClient,
    url: String,
    next_id: Cell<u64>,
    journal: Option<&'a Journal>,
}

fn quantity(v: &Value, what: &str) -> Result<U256, RpcError> {
    v.as_str()
        .ok_or_else(|| RpcError::Malformed(format!("{what} is not a string")))
        .and_then(|s| {
            U256::from_hex_quantity(s).map_err(|e| RpcError::Malformed(format!("{what}: {e}")))
        })
}

fn quantity_u64(v: &Value, what: &str) -> Result<u64, RpcError> {
    quantity(v, what)?
        .to_u64()
        .ok_or_else(|| RpcError::Malformed(format!("{what} does not fit in 64 bits")))
}

fn hex_bytes(v: &Value, what: &str) -> Result<Vec<u8>, RpcError> {
    v.as_str()
        .ok_or_else(|| RpcError::Malformed(format!("{what} is not a string")))
        .and_then(|s| hex::decode(s).map_err(|e| RpcError::Malformed(format!("{what}: {e}"))))
}

impl<'a> RpcClient<'a> {
    pub fn new(http: &'a dyn HttpClient, url: &str) -> RpcClient<'a> {
        RpcClient {
            http,
            url: url.to_string(),
            next_id: Cell::new(1),
            journal: None,
        }
    }

    /// Makes broadcasts through this client crash-safe: see [`Journal`].
    pub fn with_journal(mut self, journal: &'a Journal) -> RpcClient<'a> {
        self.journal = Some(journal);
        self
    }

    pub fn journal(&self) -> Option<&'a Journal> {
        self.journal
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
        let response = json::parse(&body)
            .map_err(|_| RpcError::Malformed("response is not valid JSON".to_string()))?;
        if let Some(err) = response.get("error") {
            let code = match err.get("code") {
                Some(Value::Number(n)) => n.parse::<i64>().unwrap_or(0),
                _ => 0,
            };
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string();
            let data = err.get("data").and_then(Value::as_str).map(str::to_string);
            return Err(RpcError::Node {
                code,
                message,
                data,
            });
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| RpcError::Malformed("response has neither result nor error".to_string()))
    }

    pub fn chain_id(&self) -> Result<u64, RpcError> {
        quantity_u64(&self.call("eth_chainId", vec![])?, "chain id")
    }

    pub fn block_number(&self) -> Result<u64, RpcError> {
        quantity_u64(&self.call("eth_blockNumber", vec![])?, "block number")
    }

    pub fn balance(&self, address: &Address) -> Result<U256, RpcError> {
        let params = vec![Value::String(to_checksum(address)), Value::str("latest")];
        quantity(&self.call("eth_getBalance", params)?, "balance")
    }

    /// Next nonce including pending transactions.
    pub fn nonce(&self, address: &Address) -> Result<u64, RpcError> {
        let params = vec![Value::String(to_checksum(address)), Value::str("pending")];
        quantity_u64(&self.call("eth_getTransactionCount", params)?, "nonce")
    }

    pub fn gas_price(&self) -> Result<U256, RpcError> {
        quantity(&self.call("eth_gasPrice", vec![])?, "gas price")
    }

    pub fn latest_base_fee(&self) -> Result<Option<U256>, RpcError> {
        let block = self.call(
            "eth_getBlockByNumber",
            vec![Value::str("latest"), Value::Bool(false)],
        )?;
        match block.get("baseFeePerGas") {
            Some(v) => quantity(v, "base fee").map(Some),
            None => Ok(None),
        }
    }

    /// EIP-1559 fees when the chain supports them, otherwise a legacy gas price.
    pub fn suggest_fees(&self) -> Result<Fees, RpcError> {
        match self.latest_base_fee()? {
            Some(base_fee) => {
                let tip = self
                    .call("eth_maxPriorityFeePerGas", vec![])
                    .ok()
                    .and_then(|v| quantity(&v, "priority fee").ok())
                    .unwrap_or_else(|| U256::from_u64(1_000_000_000));
                build_eip1559_fees(&base_fee, &tip)
            }
            None => Ok(Fees::Legacy {
                gas_price: self.gas_price()?,
            }),
        }
    }

    pub fn estimate_gas(&self, request: &CallRequest) -> Result<u64, RpcError> {
        quantity_u64(
            &self.call("eth_estimateGas", vec![request.to_json()])?,
            "gas estimate",
        )
    }

    pub fn eth_call(&self, request: &CallRequest) -> Result<Vec<u8>, RpcError> {
        let result = self.call("eth_call", vec![request.to_json(), Value::str("latest")])?;
        hex_bytes(&result, "call result")
    }

    pub fn send_raw_transaction(&self, raw: &[u8]) -> Result<[u8; 32], RpcError> {
        let result = self.call(
            "eth_sendRawTransaction",
            vec![Value::String(hex::encode_prefixed(raw))],
        )?;
        let bytes = hex_bytes(&result, "transaction hash")?;
        <[u8; 32]>::try_from(bytes.as_slice())
            .map_err(|_| RpcError::Malformed("transaction hash has wrong length".to_string()))
    }

    pub fn transaction_by_hash(&self, hash: &[u8; 32]) -> Result<Option<Value>, RpcError> {
        let v = self.call(
            "eth_getTransactionByHash",
            vec![Value::String(hex::encode_prefixed(hash))],
        )?;
        Ok(if matches!(v, Value::Null) {
            None
        } else {
            Some(v)
        })
    }

    pub fn transaction_receipt(&self, hash: &[u8; 32]) -> Result<Option<Value>, RpcError> {
        let v = self.call(
            "eth_getTransactionReceipt",
            vec![Value::String(hex::encode_prefixed(hash))],
        )?;
        Ok(if matches!(v, Value::Null) {
            None
        } else {
            Some(v)
        })
    }

    /// Unix timestamp of a block, if it can be fetched.
    pub fn block_timestamp(&self, block_number_hex: &str) -> Result<Option<u64>, RpcError> {
        let v = self.call(
            "eth_getBlockByNumber",
            vec![Value::str(block_number_hex), Value::Bool(false)],
        )?;
        match v.get("timestamp") {
            Some(ts) => quantity_u64(ts, "timestamp").map(Some),
            None => Ok(None),
        }
    }
}

/// Max fee = 2 x base fee + tip, the usual headroom for a few blocks of base fee growth.
pub fn build_eip1559_fees(base_fee: &U256, tip: &U256) -> Result<Fees, RpcError> {
    let max_fee = base_fee
        .checked_mul_u64(2)
        .and_then(|v| v.checked_add(tip))
        .ok_or_else(|| RpcError::Malformed("fee overflow".to_string()))?;
    Ok(Fees::Eip1559 {
        max_priority_fee_per_gas: *tip,
        max_fee_per_gas: max_fee,
    })
}

/// Adds 20% headroom to a gas estimate, except for the exact 21000 of a plain transfer.
pub fn with_gas_buffer(estimate: u64) -> u64 {
    if estimate == 21_000 {
        return estimate;
    }
    estimate.saturating_add(estimate / 5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::mock::MockHttp;

    fn ok(result: &str) -> Result<String, String> {
        Ok(format!(r#"{{"jsonrpc":"2.0","id":1,"result":{result}}}"#))
    }

    #[test]
    fn simple_quantity_calls() {
        let http = MockHttp::new(vec![
            ok(r#""0x1""#),
            ok(r#""0x10""#),
            ok(r#""0xde0b6b3a7640000""#),
            ok(r#""0x7""#),
        ]);
        let rpc = RpcClient::new(&http, "https://rpc.test");
        assert_eq!(rpc.chain_id(), Ok(1));
        assert_eq!(rpc.block_number(), Ok(16));
        assert_eq!(
            rpc.balance(&[1u8; 20]).unwrap().to_dec_string(),
            "1000000000000000000"
        );
        assert_eq!(rpc.nonce(&[1u8; 20]), Ok(7));
        let requests = http.requests.borrow();
        assert_eq!(requests[0].0, "https://rpc.test");
        assert!(
            requests[0]
                .1
                .as_ref()
                .unwrap()
                .contains(r#""method":"eth_chainId""#)
        );
        assert!(requests[3].1.as_ref().unwrap().contains(r#""pending""#));
    }

    #[test]
    fn request_ids_increment() {
        let http = MockHttp::new(vec![ok(r#""0x1""#), ok(r#""0x1""#)]);
        let rpc = RpcClient::new(&http, "https://rpc.test");
        rpc.chain_id().unwrap();
        rpc.chain_id().unwrap();
        let reqs = http.requests.borrow();
        assert!(reqs[0].1.as_ref().unwrap().contains(r#""id":1"#));
        assert!(reqs[1].1.as_ref().unwrap().contains(r#""id":2"#));
    }

    #[test]
    fn node_errors_are_surfaced_with_revert_reason() {
        let mut data = vec![0x08, 0xc3, 0x79, 0xa0];
        data.extend(abi::encode_values(&[abi::AbiValue::Str("nope".into())]));
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"error":{{"code":3,"message":"execution reverted","data":"{}"}}}}"#,
            hex::encode_prefixed(&data)
        );
        let http = MockHttp::new(vec![Ok(body)]);
        let rpc = RpcClient::new(&http, "https://rpc.test");
        let err = rpc.eth_call(&CallRequest::default()).unwrap_err();
        assert_eq!(err.revert_reason(), Some("nope".to_string()));
        assert!(err.to_string().contains("execution reverted"));
    }

    #[test]
    fn revert_reason_falls_back_to_message() {
        let err = RpcError::Node {
            code: -32000,
            message: "execution reverted: ERC20: x".into(),
            data: None,
        };
        assert_eq!(
            err.revert_reason(),
            Some("execution reverted: ERC20: x".to_string())
        );
        let other = RpcError::Node {
            code: -32000,
            message: "nonce too low".into(),
            data: None,
        };
        assert_eq!(other.revert_reason(), None);
        assert_eq!(RpcError::Transport("x".into()).revert_reason(), None);
    }

    #[test]
    fn malformed_responses_are_rejected() {
        let http = MockHttp::new(vec![
            Ok("not json".to_string()),
            Ok(r#"{"jsonrpc":"2.0","id":1}"#.to_string()),
            ok(r#"123"#),
            ok(r#""0xzz""#),
            ok(r#""0x10000000000000000""#),
            Err("connection refused".to_string()),
        ]);
        let rpc = RpcClient::new(&http, "https://rpc.test");
        assert!(matches!(rpc.chain_id(), Err(RpcError::Malformed(_))));
        assert!(matches!(rpc.chain_id(), Err(RpcError::Malformed(_))));
        assert!(matches!(rpc.chain_id(), Err(RpcError::Malformed(_))));
        assert!(matches!(rpc.chain_id(), Err(RpcError::Malformed(_))));
        assert!(matches!(rpc.chain_id(), Err(RpcError::Malformed(_))));
        assert!(matches!(rpc.chain_id(), Err(RpcError::Transport(_))));
    }

    #[test]
    fn eip1559_fee_suggestion() {
        let http = MockHttp::new(vec![
            ok(r#"{"baseFeePerGas":"0x3b9aca00"}"#),
            ok(r#""0x59682f00""#),
        ]);
        let rpc = RpcClient::new(&http, "https://rpc.test");
        match rpc.suggest_fees().unwrap() {
            Fees::Eip1559 {
                max_priority_fee_per_gas,
                max_fee_per_gas,
            } => {
                assert_eq!(max_priority_fee_per_gas, U256::from_u64(1_500_000_000));
                assert_eq!(
                    max_fee_per_gas,
                    U256::from_u64(2_000_000_000 + 1_500_000_000)
                );
            }
            other => panic!("expected EIP-1559 fees, got {other:?}"),
        }
    }

    #[test]
    fn tip_falls_back_when_method_missing() {
        let err =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}"#
                .to_string();
        let http = MockHttp::new(vec![ok(r#"{"baseFeePerGas":"0x64"}"#), Ok(err)]);
        let rpc = RpcClient::new(&http, "https://rpc.test");
        match rpc.suggest_fees().unwrap() {
            Fees::Eip1559 {
                max_priority_fee_per_gas,
                ..
            } => {
                assert_eq!(max_priority_fee_per_gas, U256::from_u64(1_000_000_000));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn legacy_fees_when_no_base_fee() {
        let http = MockHttp::new(vec![ok(r#"{"number":"0x1"}"#), ok(r#""0x77359400""#)]);
        let rpc = RpcClient::new(&http, "https://rpc.test");
        assert_eq!(
            rpc.suggest_fees().unwrap(),
            Fees::Legacy {
                gas_price: U256::from_u64(2_000_000_000)
            }
        );
    }

    #[test]
    fn gas_buffer() {
        assert_eq!(with_gas_buffer(21_000), 21_000);
        assert_eq!(with_gas_buffer(100_000), 120_000);
        assert_eq!(with_gas_buffer(0), 0);
        assert_eq!(with_gas_buffer(u64::MAX), u64::MAX);
    }

    #[test]
    fn call_request_json_omits_empty_fields() {
        assert_eq!(CallRequest::default().to_json().to_json(), "{}");
        let req = CallRequest {
            from: Some([1u8; 20]),
            to: Some([2u8; 20]),
            value: U256::from_u64(255),
            data: vec![0xab],
            gas: Some(21000),
        };
        let json = req.to_json();
        assert_eq!(json.get("value").and_then(Value::as_str), Some("0xff"));
        assert_eq!(json.get("data").and_then(Value::as_str), Some("0xab"));
        assert_eq!(json.get("gas").and_then(Value::as_str), Some("0x5208"));
        assert!(json.get("from").is_some() && json.get("to").is_some());
    }

    #[test]
    fn transaction_lookups_handle_null() {
        let http = MockHttp::new(vec![
            ok("null"),
            ok(r#"{"status":"0x1"}"#),
            ok(r#"{"timestamp":"0x65"}"#),
        ]);
        let rpc = RpcClient::new(&http, "https://rpc.test");
        assert_eq!(rpc.transaction_by_hash(&[0u8; 32]), Ok(None));
        assert!(rpc.transaction_receipt(&[0u8; 32]).unwrap().is_some());
        assert_eq!(rpc.block_timestamp("0x1"), Ok(Some(101)));
    }

    #[test]
    fn send_raw_returns_hash() {
        let hash = format!("\"{}\"", hex::encode_prefixed(&[0xaau8; 32]));
        let http = MockHttp::new(vec![ok(&hash), ok(r#""0x1234""#)]);
        let rpc = RpcClient::new(&http, "https://rpc.test");
        assert_eq!(rpc.send_raw_transaction(&[1, 2, 3]), Ok([0xaa; 32]));
        assert!(matches!(
            rpc.send_raw_transaction(&[1]),
            Err(RpcError::Malformed(_))
        ));
        assert!(
            http.requests.borrow()[0]
                .1
                .as_ref()
                .unwrap()
                .contains("0x010203")
        );
    }
}
