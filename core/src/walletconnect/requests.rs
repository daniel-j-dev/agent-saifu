//! Turns the raw `wc_sessionRequest` payloads into typed actions and reviewable descriptions.

use crate::crypto::hex;
use crate::eth::address::{Address, parse_address};
use crate::eth::explorer::sanitize_text_max;
use crate::eth::sign::MAX_MESSAGE_BYTES;
use crate::eth::typed_data::{self, TypedData};
use crate::eth::u256::U256;
use crate::json::Value;

pub const METHOD_SEND_TRANSACTION: &str = "eth_sendTransaction";
pub const METHOD_SIGN_TRANSACTION: &str = "eth_signTransaction";
pub const METHOD_PERSONAL_SIGN: &str = "personal_sign";
pub const METHOD_TYPED_DATA: &str = "eth_signTypedData";
pub const METHOD_TYPED_DATA_V4: &str = "eth_signTypedData_v4";

pub const METHOD_SUI_GET_ACCOUNTS: &str = "sui_getAccounts";
pub const METHOD_SUI_SIGN_TRANSACTION: &str = "sui_signTransaction";
pub const METHOD_SUI_SIGN_AND_EXECUTE: &str = "sui_signAndExecuteTransaction";
pub const METHOD_SUI_PERSONAL_MESSAGE: &str = "sui_signPersonalMessage";

const MAX_CALL_DATA: usize = 128 * 1024;
const MAX_TEXT_PREVIEW: usize = 2000;
const MAX_VALUE_TEXT: usize = 200;
const MAX_PREVIEW_ITEMS: usize = 20;
const MAX_PREVIEW_DEPTH: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxRequest {
    pub from: Address,
    pub to: Option<Address>,
    pub value: U256,
    pub data: Vec<u8>,
    /// Fee, gas and nonce fields the dapp supplied. They are never used: the wallet chooses.
    pub ignored_fields: Vec<String>,
    pub chain_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuiKind {
    GetAccounts,
    SignTransaction,
    SignAndExecute,
    SignPersonalMessage,
}

/// A Sui request. `address` is what the dapp believes the signer is; it is `None` only for
/// `sui_getAccounts`, which carries no parameters.
#[derive(Debug, Clone)]
pub struct SuiRequest {
    pub kind: SuiKind,
    pub address: Option<sui_sdk_types::Address>,
    /// The transaction for the two transaction kinds, decoded from base64 BCS `TransactionData`.
    pub transaction: Option<Box<sui_sdk_types::Transaction>>,
    /// The personal message, taken as UTF-8 text.
    pub message: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum Action {
    Sui(SuiRequest),
    SendTransaction(TxRequest),
    SignTransaction(TxRequest),
    PersonalSign {
        message: Vec<u8>,
        address: Address,
    },
    SignTypedData {
        data: Box<TypedData>,
        address: Address,
    },
}

impl Action {
    /// The Ethereum signer the dapp named; `None` for Sui requests.
    pub fn signer(&self) -> Option<&Address> {
        Some(match self {
            Action::Sui(_) => return None,
            Action::SendTransaction(tx) | Action::SignTransaction(tx) => &tx.from,
            Action::PersonalSign { address, .. } | Action::SignTypedData { address, .. } => address,
        })
    }
}

/// Hex quantity (`0x1a`), `0x`, or plain decimal digits.
fn parse_quantity(text: &str) -> Result<U256, String> {
    let text = text.trim();
    if text == "0x" || text == "0X" {
        return Ok(U256::ZERO);
    }
    if text.starts_with("0x") || text.starts_with("0X") {
        U256::from_hex_quantity(text)
    } else {
        U256::from_dec_str(text)
    }
}

fn field_text<'a>(tx: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match tx.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(Value::Number(n)) => Ok(Some(n)),
        Some(_) => Err(format!("transaction field '{key}' is not valid")),
    }
}

pub fn parse_transaction(tx: &Value) -> Result<TxRequest, String> {
    if !matches!(tx, Value::Object(_)) {
        return Err("the transaction must be an object".to_string());
    }
    let from = field_text(tx, "from")?.ok_or("the transaction has no 'from' address")?;
    let from = parse_address(from).map_err(|e| format!("'from': {e}"))?;
    let to = match field_text(tx, "to")? {
        None | Some("") | Some("0x") => None,
        Some(to) => Some(parse_address(to).map_err(|e| format!("'to': {e}"))?),
    };
    let value = match field_text(tx, "value")? {
        Some(v) => parse_quantity(v).map_err(|e| format!("'value': {e}"))?,
        None => U256::ZERO,
    };
    let data_text = field_text(tx, "data")?.or(field_text(tx, "input")?);
    let data = match data_text {
        Some(d) => hex::decode(d).map_err(|e| format!("'data': {e}"))?,
        None => vec![],
    };
    if data.len() > MAX_CALL_DATA {
        return Err("the transaction data is too large".to_string());
    }
    let chain_id = match field_text(tx, "chainId")? {
        Some(c) => Some(
            parse_quantity(c)
                .map_err(|e| format!("'chainId': {e}"))?
                .to_u64()
                .ok_or("'chainId' is too large")?,
        ),
        None => None,
    };
    let ignored_fields = [
        "gas",
        "gasLimit",
        "gasPrice",
        "maxFeePerGas",
        "maxPriorityFeePerGas",
        "nonce",
        "type",
    ]
    .iter()
    .filter(|k| tx.get(k).is_some())
    .map(|k| k.to_string())
    .collect();
    Ok(TxRequest {
        from,
        to,
        value,
        data,
        ignored_fields,
        chain_id,
    })
}

fn is_address(text: &str) -> bool {
    parse_address(text).is_ok()
}

fn params_list<'a>(params: &'a Value, method: &str) -> Result<&'a Vec<Value>, String> {
    params
        .as_array()
        .ok_or_else(|| format!("{method} parameters must be a list"))
}

fn parse_personal_sign(params: &Value) -> Result<Action, String> {
    let list = params_list(params, METHOD_PERSONAL_SIGN)?;
    let (first, second) = match (list.first(), list.get(1)) {
        (Some(Value::String(a)), Some(Value::String(b))) => (a.as_str(), b.as_str()),
        _ => return Err("personal_sign needs a message and an address".to_string()),
    };
    // Spec order is [message, address]; a few dapps send [address, message].
    let (message, address) = if !is_address(second) && is_address(first) {
        (second, first)
    } else {
        (first, second)
    };
    let address = parse_address(address).map_err(|e| format!("signer address: {e}"))?;
    let bytes = if message.starts_with("0x") || message.starts_with("0X") {
        hex::decode(message).unwrap_or_else(|_| message.as_bytes().to_vec())
    } else {
        message.as_bytes().to_vec()
    };
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err("the message is too large".to_string());
    }
    Ok(Action::PersonalSign {
        message: bytes,
        address,
    })
}

fn parse_typed(method: &str, params: &Value) -> Result<Action, String> {
    let list = params_list(params, method)?;
    let (first, second) = match (list.first(), list.get(1)) {
        (Some(a), Some(b)) => (a, b),
        _ => return Err(format!("{method} needs an address and the typed data")),
    };
    let is_addr = |v: &Value| v.as_str().is_some_and(is_address);
    let (address, data) = if !is_addr(first) && is_addr(second) {
        (second, first)
    } else {
        (first, second)
    };
    let address = parse_address(address.as_str().ok_or("signer address is not a string")?)
        .map_err(|e| format!("signer address: {e}"))?;
    Ok(Action::SignTypedData {
        data: Box::new(typed_data::parse(data)?),
        address,
    })
}

/// Sui parameters are an object `{transaction | message, address}`; a one-element list holding
/// that object is tolerated.
fn parse_sui(method: &str, params: &Value) -> Result<Action, String> {
    let object = match params {
        Value::Array(list) if list.len() == 1 => &list[0],
        other => other,
    };
    let text = |key: &str| object.get(key).and_then(Value::as_str);
    let kind = match method {
        METHOD_SUI_GET_ACCOUNTS => {
            return Ok(Action::Sui(SuiRequest {
                kind: SuiKind::GetAccounts,
                address: None,
                transaction: None,
                message: vec![],
            }));
        }
        METHOD_SUI_SIGN_TRANSACTION => SuiKind::SignTransaction,
        METHOD_SUI_SIGN_AND_EXECUTE => SuiKind::SignAndExecute,
        _ => SuiKind::SignPersonalMessage,
    };
    if !matches!(object, Value::Object(_)) {
        return Err(format!("{method} parameters must be an object"));
    }
    let address = crate::sui::coins::parse_address(
        text("address").ok_or_else(|| format!("{method} needs the signer 'address'"))?,
    )
    .map_err(|e| format!("signer address: {e}"))?;
    if kind == SuiKind::SignPersonalMessage {
        let message = text("message").ok_or("sui_signPersonalMessage needs a 'message'")?;
        if message.len() > crate::sui::tx::MAX_MESSAGE_BYTES {
            return Err("the message is too large".to_string());
        }
        return Ok(Action::Sui(SuiRequest {
            kind,
            address: Some(address),
            transaction: None,
            message: message.as_bytes().to_vec(),
        }));
    }
    let transaction = crate::sui::tx::decode(
        text("transaction").ok_or_else(|| format!("{method} needs the 'transaction'"))?,
    )?;
    Ok(Action::Sui(SuiRequest {
        kind,
        address: Some(address),
        transaction: Some(Box::new(transaction)),
        message: vec![],
    }))
}

pub fn parse_request(method: &str, params: &Value) -> Result<Action, String> {
    match method {
        METHOD_SEND_TRANSACTION | METHOD_SIGN_TRANSACTION => {
            let list = params_list(params, method)?;
            let tx = parse_transaction(list.first().ok_or("the request has no transaction")?)?;
            Ok(if method == METHOD_SEND_TRANSACTION {
                Action::SendTransaction(tx)
            } else {
                Action::SignTransaction(tx)
            })
        }
        METHOD_SUI_GET_ACCOUNTS
        | METHOD_SUI_SIGN_TRANSACTION
        | METHOD_SUI_SIGN_AND_EXECUTE
        | METHOD_SUI_PERSONAL_MESSAGE => parse_sui(method, params),
        METHOD_PERSONAL_SIGN => parse_personal_sign(params),
        METHOD_TYPED_DATA | METHOD_TYPED_DATA_V4 => parse_typed(method, params),
        other => Err(format!(
            "'{}' is not a supported method",
            sanitize_text_max(other, 60)
        )),
    }
}

/// Printable text, or `None` when the bytes are binary (a hash, say) and cannot be read.
pub fn readable_text(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    if text.is_empty()
        || text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return None;
    }
    Some(text.to_string())
}

/// Shows what a `personal_sign` request would sign.
pub fn describe_message(bytes: &[u8]) -> Value {
    let mut warnings: Vec<String> = vec![];
    let mut pairs = vec![("length_bytes", Value::int(bytes.len() as i64))];
    match readable_text(bytes) {
        Some(text) => {
            let shown: String = text.chars().take(MAX_TEXT_PREVIEW).collect();
            pairs.push(("text", Value::String(shown)));
            if text.chars().count() > MAX_TEXT_PREVIEW {
                warnings.push("the message is longer than the preview".to_string());
            }
        }
        None => {
            pairs.push((
                "hex",
                Value::String(hex::encode_prefixed(&bytes[..bytes.len().min(256)])),
            ));
            warnings.push(
                "the message is not readable text, so you cannot see what you are signing; only sign it if you know exactly what it is"
                    .to_string(),
            );
        }
    }
    pairs.push((
        "warnings",
        Value::Array(warnings.into_iter().map(Value::String).collect()),
    ));
    Value::object(pairs)
}

/// Third-party JSON, shortened and stripped of control characters for display.
pub fn preview_json(value: &Value, depth: usize) -> Value {
    if depth > MAX_PREVIEW_DEPTH {
        return Value::str("...");
    }
    match value {
        Value::String(s) => Value::String(sanitize_text_max(s, MAX_VALUE_TEXT)),
        Value::Number(n) => Value::String(sanitize_text_max(n, MAX_VALUE_TEXT)),
        Value::Array(items) => {
            let mut shown: Vec<Value> = items
                .iter()
                .take(MAX_PREVIEW_ITEMS)
                .map(|v| preview_json(v, depth + 1))
                .collect();
            if items.len() > MAX_PREVIEW_ITEMS {
                shown.push(Value::String(format!(
                    "... {} more",
                    items.len() - MAX_PREVIEW_ITEMS
                )));
            }
            Value::Array(shown)
        }
        Value::Object(pairs) => Value::Object(
            pairs
                .iter()
                .take(MAX_PREVIEW_ITEMS * 2)
                .map(|(k, v)| (sanitize_text_max(k, 60), preview_json(v, depth + 1)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn looks_unlimited(value: &Value) -> bool {
    let text = match value {
        Value::String(s) | Value::Number(s) => s.as_str(),
        _ => return false,
    };
    let magnitude = if text.starts_with("0x") || text.starts_with("0X") {
        hex::decode(text)
            .ok()
            .and_then(|b| U256::from_be_bytes(&b).ok())
    } else {
        U256::from_dec_str(text).ok()
    };
    magnitude.is_some_and(|m| m.to_be_bytes()[0] >= 0x80)
}

fn contains_unlimited(value: &Value, depth: usize) -> bool {
    if depth > MAX_PREVIEW_DEPTH {
        return false;
    }
    match value {
        Value::Object(pairs) => pairs
            .iter()
            .any(|(_, v)| looks_unlimited(v) || contains_unlimited(v, depth + 1)),
        Value::Array(items) => items.iter().any(|v| contains_unlimited(v, depth + 1)),
        _ => false,
    }
}

/// Shows what an EIP-712 request would sign, with warnings for the well-known risky kinds.
pub fn describe_typed_data(data: &TypedData) -> Value {
    let mut warnings: Vec<String> = vec![];
    let primary = data.primary_type.to_lowercase();
    if primary.contains("permit") {
        warnings.push(
            "a permit signature lets the spender move your tokens without any further approval from you"
                .to_string(),
        );
    }
    if ["order", "listing", "bid", "offer"]
        .iter()
        .any(|k| primary.contains(k))
    {
        warnings.push(
            "this looks like a marketplace order; check what you give up and what you receive"
                .to_string(),
        );
    }
    if contains_unlimited(&data.message, 0) {
        warnings.push("the message contains a very large amount (possibly unlimited)".to_string());
    }
    Value::object(vec![
        (
            "primary_type",
            Value::String(sanitize_text_max(&data.primary_type, 60)),
        ),
        ("domain", preview_json(&data.domain, 0)),
        ("message", preview_json(&data.message, 0)),
        (
            "warnings",
            Value::Array(warnings.into_iter().map(Value::String).collect()),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    const ME: &str = "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed";
    const YOU: &str = "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359";

    fn parse(method: &str, params: &str) -> Result<Action, String> {
        parse_request(method, &json::parse(params).unwrap())
    }

    #[test]
    fn parses_a_send_transaction_request() {
        let action = parse(
            "eth_sendTransaction",
            &format!(
                r#"[{{"from":"{ME}","to":"{YOU}","value":"0xde0b6b3a7640000","data":"0xa9059cbb","gas":"0x5208","maxFeePerGas":"0x1","nonce":"0x3","chainId":"0x1"}}]"#
            ),
        )
        .unwrap();
        let Action::SendTransaction(tx) = action else {
            panic!("wrong kind")
        };
        assert_eq!(tx.from, parse_address(ME).unwrap());
        assert_eq!(tx.to, Some(parse_address(YOU).unwrap()));
        assert_eq!(tx.value.to_dec_string(), "1000000000000000000");
        assert_eq!(tx.data, vec![0xa9, 0x05, 0x9c, 0xbb]);
        assert_eq!(tx.chain_id, Some(1));
        assert_eq!(tx.ignored_fields, vec!["gas", "maxFeePerGas", "nonce"]);
    }

    #[test]
    fn transaction_defaults_and_aliases() {
        let Action::SignTransaction(tx) = parse(
            "eth_signTransaction",
            &format!(r#"[{{"from":"{ME}","to":"{YOU}","input":"0x1234","value":"0x"}}]"#),
        )
        .unwrap() else {
            panic!("wrong kind")
        };
        assert!(tx.value.is_zero());
        assert_eq!(tx.data, vec![0x12, 0x34]);
        assert!(tx.ignored_fields.is_empty());
        let Action::SendTransaction(tx) = parse(
            "eth_sendTransaction",
            &format!(r#"[{{"from":"{ME}","value":"15"}}]"#),
        )
        .unwrap() else {
            panic!("wrong kind")
        };
        assert_eq!(tx.to, None);
        assert_eq!(tx.value.to_dec_string(), "15");
    }

    #[test]
    fn bad_transactions_are_rejected() {
        for bad in [
            "[]",
            "{}",
            "[\"x\"]",
            &format!(r#"[{{"to":"{YOU}"}}]"#),
            r#"[{"from":"0x12"}]"#,
            &format!(r#"[{{"from":"{ME}","to":"nope"}}]"#),
            &format!(r#"[{{"from":"{ME}","value":"0xzz"}}]"#),
            &format!(r#"[{{"from":"{ME}","data":"0xabc"}}]"#),
            &format!(r#"[{{"from":"{ME}","chainId":"0xffffffffffffffffff"}}]"#),
            &format!(r#"[{{"from":"{ME}","value":[1]}}]"#),
        ] {
            assert!(parse("eth_sendTransaction", bad).is_err(), "{bad}");
        }
        let big = format!(
            r#"[{{"from":"{ME}","data":"0x{}"}}]"#,
            "00".repeat(MAX_CALL_DATA + 1)
        );
        assert!(
            parse("eth_sendTransaction", &big)
                .unwrap_err()
                .contains("too large")
        );
    }

    #[test]
    fn personal_sign_accepts_either_parameter_order() {
        let hex_msg = format!(r#"["0x48656c6c6f","{ME}"]"#);
        let Action::PersonalSign { message, address } = parse("personal_sign", &hex_msg).unwrap()
        else {
            panic!("wrong kind")
        };
        assert_eq!(message, b"Hello");
        assert_eq!(address, parse_address(ME).unwrap());
        let swapped = format!(r#"["{ME}","plain text"]"#);
        let Action::PersonalSign { message, address } = parse("personal_sign", &swapped).unwrap()
        else {
            panic!("wrong kind")
        };
        assert_eq!(message, b"plain text");
        assert_eq!(address, parse_address(ME).unwrap());
    }

    #[test]
    fn personal_sign_rejects_bad_input() {
        assert!(parse("personal_sign", r#"["0x00"]"#).is_err());
        assert!(parse("personal_sign", r#"["a","b"]"#).is_err());
        assert!(parse("personal_sign", r#"{"a":1}"#).is_err());
        let huge = format!(r#"["{}","{ME}"]"#, "a".repeat(MAX_MESSAGE_BYTES + 1));
        assert!(parse("personal_sign", &huge).is_err());
    }

    #[test]
    fn typed_data_accepts_string_or_object_and_either_order() {
        let doc = r#"{"types":{"T":[{"name":"a","type":"string"}]},"primaryType":"T","domain":{"name":"x"},"message":{"a":"b"}}"#;
        let as_string = json::Value::String(doc.to_string()).to_json();
        let Action::SignTypedData { data, address } =
            parse("eth_signTypedData_v4", &format!(r#"["{ME}",{as_string}]"#)).unwrap()
        else {
            panic!("wrong kind")
        };
        assert_eq!(data.primary_type, "T");
        assert_eq!(address, parse_address(ME).unwrap());
        assert!(parse("eth_signTypedData", &format!(r#"[{doc},"{ME}"]"#)).is_ok());
        assert!(parse("eth_signTypedData_v4", &format!(r#"["{ME}"]"#)).is_err());
        assert!(parse("eth_signTypedData_v4", &format!(r#"["{ME}","not json"]"#)).is_err());
    }

    #[test]
    fn unsupported_methods_are_rejected() {
        let err = parse("eth_sign", &format!(r#"["{ME}","0x00"]"#)).unwrap_err();
        assert!(err.contains("not a supported method"));
        assert!(parse("wallet_addEthereumChain", "[]").is_err());
    }

    #[test]
    fn signer_is_reported_for_every_action() {
        let Action::SendTransaction(_) =
            parse("eth_sendTransaction", &format!(r#"[{{"from":"{ME}"}}]"#)).unwrap()
        else {
            panic!()
        };
        let a = parse("personal_sign", &format!(r#"["0x00","{YOU}"]"#)).unwrap();
        assert_eq!(a.signer(), Some(&parse_address(YOU).unwrap()));
    }

    #[test]
    fn readable_messages_are_shown_and_binary_ones_are_flagged() {
        let text = describe_message(b"Sign in to Example\nNonce: 12345");
        assert_eq!(
            text.get("text").and_then(Value::as_str),
            Some("Sign in to Example\nNonce: 12345")
        );
        assert!(text.get("warnings").unwrap().as_array().unwrap().is_empty());
        let binary = describe_message(&[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(
            binary.get("hex").and_then(Value::as_str),
            Some("0xdeadbeef")
        );
        assert!(binary.get("text").is_none());
        assert_eq!(binary.get("warnings").unwrap().as_array().unwrap().len(), 1);
        assert!(readable_text(b"").is_none());
        assert!(readable_text(b"a\x00b").is_none());
    }

    #[test]
    fn long_messages_are_truncated_with_a_warning() {
        let long = "a".repeat(MAX_TEXT_PREVIEW + 10);
        let shown = describe_message(long.as_bytes());
        assert_eq!(
            shown.get("text").and_then(Value::as_str).unwrap().len(),
            MAX_TEXT_PREVIEW
        );
        assert_eq!(shown.get("warnings").unwrap().as_array().unwrap().len(), 1);
    }

    #[test]
    fn previews_strip_control_characters_and_limit_size() {
        let v = json::parse(r#"{"a\u0007b":"x\u001b[31my","list":[1,2,3],"n":12}"#).unwrap();
        let p = preview_json(&v, 0);
        assert_eq!(p.get("ab").and_then(Value::as_str), Some("x[31my"));
        assert_eq!(p.get("n").and_then(Value::as_str), Some("12"));
        let many = Value::Array((0..50).map(Value::int).collect());
        let shown = preview_json(&many, 0);
        let items = shown.as_array().unwrap();
        assert_eq!(items.len(), MAX_PREVIEW_ITEMS + 1);
        assert_eq!(items[MAX_PREVIEW_ITEMS].as_str(), Some("... 30 more"));
        let mut deep = Value::int(1);
        for _ in 0..20 {
            deep = Value::Array(vec![deep]);
        }
        assert!(preview_json(&deep, 0).to_json().contains("..."));
    }

    fn typed(primary: &str, message: &str) -> TypedData {
        let doc = format!(
            r#"{{"types":{{"{primary}":[{{"name":"value","type":"uint256"}}]}},"primaryType":"{primary}","domain":{{"name":"Token"}},"message":{message}}}"#
        );
        typed_data::parse(&Value::String(doc)).unwrap()
    }

    #[test]
    fn typed_data_warnings_cover_permits_orders_and_unlimited_amounts() {
        let permit = describe_typed_data(&typed("Permit", r#"{"value":"1"}"#));
        assert_eq!(permit.get("warnings").unwrap().as_array().unwrap().len(), 1);
        let order = describe_typed_data(&typed("OrderComponents", r#"{"value":"1"}"#));
        assert!(
            order.get("warnings").unwrap().as_array().unwrap()[0]
                .as_str()
                .unwrap()
                .contains("marketplace")
        );
        let unlimited = describe_typed_data(&typed(
            "Mail",
            r#"{"value":"115792089237316195423570985008687907853269984665640564039457584007913129639935"}"#,
        ));
        assert!(
            unlimited.get("warnings").unwrap().as_array().unwrap()[0]
                .as_str()
                .unwrap()
                .contains("very large")
        );
        let plain = describe_typed_data(&typed("Mail", r#"{"value":"5"}"#));
        assert!(
            plain
                .get("warnings")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            plain.get("primary_type").and_then(Value::as_str),
            Some("Mail")
        );
        assert_eq!(
            plain
                .get("domain")
                .and_then(|d| d.get("name"))
                .and_then(Value::as_str),
            Some("Token")
        );
    }
}
