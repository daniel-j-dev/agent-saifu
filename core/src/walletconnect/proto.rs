//! JSON-RPC messages of the Sign protocol and the relay tag / TTL each one is published with.

use crate::crypto::random;
use crate::json::{self, Value};

use super::now_secs;

pub const METHOD_PROPOSE: &str = "wc_sessionPropose";
pub const METHOD_SETTLE: &str = "wc_sessionSettle";
pub const METHOD_UPDATE: &str = "wc_sessionUpdate";
pub const METHOD_EXTEND: &str = "wc_sessionExtend";
pub const METHOD_REQUEST: &str = "wc_sessionRequest";
pub const METHOD_EVENT: &str = "wc_sessionEvent";
pub const METHOD_DELETE: &str = "wc_sessionDelete";
pub const METHOD_PING: &str = "wc_sessionPing";

pub const FIVE_MINUTES: u64 = 300;
pub const ONE_DAY: u64 = 86_400;

pub const USER_REJECTED: i64 = 5000;
pub const USER_REJECTED_CHAINS: i64 = 5001;
pub const USER_REJECTED_METHODS: i64 = 5002;
pub const UNSUPPORTED_CHAINS: i64 = 5100;
pub const UNSUPPORTED_METHODS: i64 = 5101;
pub const USER_DISCONNECTED: i64 = 6000;
pub const SESSION_SETTLEMENT_FAILED: i64 = 7000;
pub const UNAUTHORIZED_METHOD: i64 = 3001;
pub const UNAUTHORIZED_CHAIN: i64 = 3005;
pub const REQUEST_EXPIRED: i64 = 8000;
pub const INVALID_REQUEST: i64 = 1001;

/// How a message is published to the relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delivery {
    pub ttl: u64,
    pub tag: u64,
    pub prompt: bool,
}

const fn delivery(ttl: u64, tag: u64, prompt: bool) -> Delivery {
    Delivery { ttl, tag, prompt }
}

/// Delivery settings for a request we send.
pub fn request_delivery(method: &str) -> Option<Delivery> {
    Some(match method {
        METHOD_PROPOSE => delivery(FIVE_MINUTES, 1100, true),
        METHOD_SETTLE => delivery(FIVE_MINUTES, 1102, false),
        METHOD_UPDATE => delivery(ONE_DAY, 1104, false),
        METHOD_EXTEND => delivery(ONE_DAY, 1106, false),
        METHOD_REQUEST => delivery(FIVE_MINUTES, 1108, true),
        METHOD_EVENT => delivery(FIVE_MINUTES, 1110, true),
        METHOD_DELETE => delivery(ONE_DAY, 1112, false),
        METHOD_PING => delivery(30, 1114, false),
        _ => return None,
    })
}

/// Delivery settings for a response to a request of `method`.
pub fn response_delivery(method: &str) -> Option<Delivery> {
    Some(match method {
        METHOD_PROPOSE => delivery(FIVE_MINUTES, 1101, false),
        METHOD_SETTLE => delivery(FIVE_MINUTES, 1103, false),
        METHOD_UPDATE => delivery(ONE_DAY, 1105, false),
        METHOD_EXTEND => delivery(ONE_DAY, 1107, false),
        METHOD_REQUEST => delivery(FIVE_MINUTES, 1109, false),
        METHOD_EVENT => delivery(FIVE_MINUTES, 1111, false),
        METHOD_DELETE => delivery(ONE_DAY, 1113, false),
        METHOD_PING => delivery(30, 1115, false),
        _ => return None,
    })
}

/// Rejecting a proposal uses its own tag.
pub const PROPOSE_REJECT: Delivery = delivery(FIVE_MINUTES, 1120, false);

#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Response {
        id: Value,
        outcome: Result<Value, RpcError>,
    },
}

pub fn parse_message(text: &str) -> Result<Message, String> {
    let doc = json::parse(text).map_err(|e| format!("not JSON: {e}"))?;
    let id = match doc.get("id") {
        Some(id @ Value::Number(_)) => id.clone(),
        _ => return Err("message has no numeric id".to_string()),
    };
    if let Some(method) = doc.get("method").and_then(Value::as_str) {
        return Ok(Message::Request {
            id,
            method: method.to_string(),
            params: doc.get("params").cloned().unwrap_or(Value::Null),
        });
    }
    if let Some(error) = doc.get("error") {
        return Ok(Message::Response {
            id,
            outcome: Err(RpcError {
                code: error
                    .get("code")
                    .and_then(|c| match c {
                        Value::Number(n) => n.parse::<i64>().ok(),
                        _ => None,
                    })
                    .unwrap_or(0),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(200)
                    .collect(),
            }),
        });
    }
    match doc.get("result") {
        Some(result) => Ok(Message::Response {
            id,
            outcome: Ok(result.clone()),
        }),
        None => Err("message is neither a request nor a response".to_string()),
    }
}

/// Message ids are 13 digits of milliseconds plus 3 digits of entropy, as the SDKs do. They must
/// stay below 2^53: JavaScript dapps parse ids as doubles and would otherwise echo a rounded id.
pub fn new_message_id() -> Result<u64, String> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(now_secs() * 1000);
    let entropy = u32::from_be_bytes(random::random_array::<4>()?) % 1_000;
    Ok(millis * 1_000 + u64::from(entropy))
}

pub fn build_request(id: u64, method: &str, params: Value) -> String {
    Value::object(vec![
        ("id", Value::Number(id.to_string())),
        ("jsonrpc", Value::str("2.0")),
        ("method", Value::str(method)),
        ("params", params),
    ])
    .to_json()
}

pub fn build_result(id: &Value, result: Value) -> String {
    Value::object(vec![
        ("id", id.clone()),
        ("jsonrpc", Value::str("2.0")),
        ("result", result),
    ])
    .to_json()
}

pub fn build_error(id: &Value, code: i64, message: &str) -> String {
    Value::object(vec![
        ("id", id.clone()),
        ("jsonrpc", Value::str("2.0")),
        (
            "error",
            Value::object(vec![
                ("code", Value::int(code)),
                ("message", Value::str(message)),
            ]),
        ),
    ])
    .to_json()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_and_ttls_follow_the_spec_table() {
        let d = request_delivery(METHOD_PROPOSE).unwrap();
        assert_eq!((d.ttl, d.tag, d.prompt), (300, 1100, true));
        assert_eq!(response_delivery(METHOD_PROPOSE).unwrap().tag, 1101);
        assert_eq!(PROPOSE_REJECT.tag, 1120);
        assert_eq!(request_delivery(METHOD_SETTLE).unwrap().tag, 1102);
        assert_eq!(response_delivery(METHOD_SETTLE).unwrap().tag, 1103);
        assert_eq!(request_delivery(METHOD_REQUEST).unwrap().tag, 1108);
        assert_eq!(response_delivery(METHOD_REQUEST).unwrap().tag, 1109);
        assert_eq!(request_delivery(METHOD_DELETE).unwrap().ttl, ONE_DAY);
        assert_eq!(request_delivery(METHOD_DELETE).unwrap().tag, 1112);
        assert_eq!(request_delivery(METHOD_PING).unwrap().tag, 1114);
        assert_eq!(response_delivery(METHOD_PING).unwrap().tag, 1115);
        assert_eq!(request_delivery("nope"), None);
        assert_eq!(response_delivery("nope"), None);
    }

    #[test]
    fn parses_requests_results_and_errors() {
        let req = parse_message(
            r#"{"id":1700000000000123456,"jsonrpc":"2.0","method":"wc_sessionRequest","params":{"a":1}}"#,
        )
        .unwrap();
        match req {
            Message::Request { id, method, params } => {
                assert_eq!(id, Value::Number("1700000000000123456".into()));
                assert_eq!(method, "wc_sessionRequest");
                assert!(params.get("a").is_some());
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse_message(r#"{"id":5,"jsonrpc":"2.0","result":true}"#).unwrap(),
            Message::Response {
                id: Value::int(5),
                outcome: Ok(Value::Bool(true))
            }
        );
        assert_eq!(
            parse_message(r#"{"id":5,"error":{"code":5000,"message":"no\nway"}}"#).unwrap(),
            Message::Response {
                id: Value::int(5),
                outcome: Err(RpcError {
                    code: 5000,
                    message: "noway".into()
                })
            }
        );
    }

    #[test]
    fn rejects_malformed_messages() {
        for bad in [
            "nope",
            r#"{"jsonrpc":"2.0","method":"x"}"#,
            r#"{"id":"abc","method":"x"}"#,
            r#"{"id":1}"#,
            "[]",
        ] {
            assert!(parse_message(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn builders_round_trip_through_the_parser() {
        let id = new_message_id().unwrap();
        assert!(id > 1_000_000_000_000_000 && id < (1u64 << 53));
        let text = build_request(id, METHOD_PING, Value::object(vec![]));
        match parse_message(&text).unwrap() {
            Message::Request {
                id: got, method, ..
            } => {
                assert_eq!(got.as_u64(), Some(id));
                assert_eq!(method, METHOD_PING);
            }
            other => panic!("{other:?}"),
        }
        let ok = build_result(&Value::int(9), Value::Bool(true));
        assert!(matches!(
            parse_message(&ok).unwrap(),
            Message::Response { outcome: Ok(_), .. }
        ));
        let err = build_error(&Value::int(9), USER_REJECTED, "User rejected.");
        match parse_message(&err).unwrap() {
            Message::Response {
                outcome: Err(e), ..
            } => assert_eq!((e.code, e.message.as_str()), (5000, "User rejected.")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn message_ids_are_unique_enough() {
        assert_ne!(new_message_id().unwrap(), new_message_id().unwrap());
    }
}
