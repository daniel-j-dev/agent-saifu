//! MCP (Model Context Protocol) message handling over JSON-RPC 2.0.
//!
//! Everything here is pure (string in, string out) so it is easy to unit test.

use crate::json::{self, Value};
use crate::metadata;
use crate::tools::Toolbox;

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

/// Handles one raw JSON-RPC message; returns the serialized response, if any.
/// Parsed arguments and results may hold secrets, so both trees are wiped afterwards.
pub fn handle_message(raw: &str, tools: &Toolbox) -> Option<String> {
    let mut message = match json::parse(raw) {
        Ok(v) => v,
        Err(e) => {
            return Some(error_response(
                &Value::Null,
                PARSE_ERROR,
                &format!("Parse error: {e}"),
            ));
        }
    };
    let mut response = handle_value(&message, tools);
    let out = response.as_ref().map(Value::to_json);
    if let Some(r) = response.as_mut() {
        r.wipe();
    }
    message.wipe();
    out
}

fn handle_value(message: &Value, tools: &Toolbox) -> Option<Value> {
    if !matches!(message, Value::Object(_)) {
        return Some(error_value(
            &Value::Null,
            INVALID_REQUEST,
            "Invalid request",
        ));
    }
    let method = message.get("method");
    let id = message.get("id");

    let Some(method) = method else {
        // Responses from the client carry no method and need no reply.
        if message.get("result").is_some() || message.get("error").is_some() {
            return None;
        }
        return Some(error_value(
            id.unwrap_or(&Value::Null),
            INVALID_REQUEST,
            "Missing method",
        ));
    };
    let Some(method) = method.as_str() else {
        return Some(error_value(
            &Value::Null,
            INVALID_REQUEST,
            "Method must be a string",
        ));
    };

    // Notifications have no id and never get a response.
    let id = id?;
    if !is_valid_id(id) {
        return Some(error_value(&Value::Null, INVALID_REQUEST, "Invalid id"));
    }

    let params = message.get("params");
    Some(match dispatch(method, params, tools) {
        Ok(result) => result_value(id, result),
        Err((code, msg)) => error_value(id, code, &msg),
    })
}

fn is_valid_id(id: &Value) -> bool {
    matches!(id, Value::String(_) | Value::Number(_))
}

type RpcResult = Result<Value, (i64, String)>;

fn dispatch(method: &str, params: Option<&Value>, tools: &Toolbox) -> RpcResult {
    match method {
        "initialize" => Ok(initialize_result(params)),
        "ping" => Ok(Value::Object(vec![])),
        "tools/list" => Ok(tools.list()),
        "tools/call" => tools_call_result(params, tools),
        _ => Err((METHOD_NOT_FOUND, format!("Method not found: {method}"))),
    }
}

pub fn initialize_result(params: Option<&Value>) -> Value {
    let requested = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str);
    Value::object(vec![
        (
            "protocolVersion",
            Value::str(metadata::negotiate_protocol_version(requested)),
        ),
        (
            "capabilities",
            Value::object(vec![("tools", Value::Object(vec![]))]),
        ),
        (
            "serverInfo",
            Value::object(vec![
                ("name", Value::str(metadata::NAME)),
                ("version", Value::str(metadata::VERSION_NUMBER)),
                ("description", Value::str(metadata::DESCRIPTION)),
            ]),
        ),
        ("instructions", Value::String(metadata::info_text())),
    ])
}

fn tools_call_result(params: Option<&Value>, tools: &Toolbox) -> RpcResult {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .ok_or((INVALID_PARAMS, "Missing tool name".to_string()))?;
    let arguments = params.and_then(|p| p.get("arguments"));
    tools.call(name, arguments).ok_or_else(|| {
        (
            INVALID_PARAMS,
            format!(
                "Unknown tool: {}",
                crate::eth::explorer::sanitize_text(name)
            ),
        )
    })
}

fn result_value(id: &Value, result: Value) -> Value {
    Value::object(vec![
        ("jsonrpc", Value::str("2.0")),
        ("id", id.clone()),
        ("result", result),
    ])
}

fn error_value(id: &Value, code: i64, message: &str) -> Value {
    Value::object(vec![
        ("jsonrpc", Value::str("2.0")),
        ("id", id.clone()),
        (
            "error",
            Value::object(vec![
                ("code", Value::int(code)),
                ("message", Value::str(message)),
            ]),
        ),
    ])
}

fn error_response(id: &Value, code: i64, message: &str) -> String {
    error_value(id, code, message).to_json()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testing::toolbox_fixture;

    fn call(raw: &str) -> Value {
        let (_f, tb) = toolbox_fixture();
        json::parse(&handle_message(raw, &tb).expect("expected a response")).unwrap()
    }

    fn send(raw: &str) -> Option<String> {
        let (_f, tb) = toolbox_fixture();
        handle_message(raw, &tb)
    }

    #[test]
    fn initialize_returns_server_info() {
        let r = call(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        );
        assert_eq!(r.get("id"), Some(&Value::int(1)));
        let result = r.get("result").unwrap();
        assert_eq!(
            result.get("protocolVersion").and_then(Value::as_str),
            Some("2024-11-05")
        );
        let info = result.get("serverInfo").unwrap();
        assert_eq!(
            info.get("name").and_then(Value::as_str),
            Some("agent-saifu")
        );
        assert_eq!(info.get("version").and_then(Value::as_str), Some("0.0"));
        assert_eq!(
            info.get("description").and_then(Value::as_str),
            Some("All-in-one crypto wallet for Agents. Built from the ground up in Rust.")
        );
        assert!(result.get("capabilities").unwrap().get("tools").is_some());
    }

    #[test]
    fn initialize_without_params_uses_default_version() {
        let r = call(r#"{"jsonrpc":"2.0","id":"a","method":"initialize"}"#);
        assert_eq!(r.get("id"), Some(&Value::str("a")));
        assert_eq!(
            r.get("result")
                .unwrap()
                .get("protocolVersion")
                .and_then(Value::as_str),
            Some(metadata::MCP_PROTOCOL_VERSION)
        );
    }

    #[test]
    fn tools_list_contains_all_tools() {
        let r = call(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let tools = match r.get("result").unwrap().get("tools").unwrap() {
            Value::Array(t) => t.clone(),
            _ => panic!("tools not an array"),
        };
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert!(
            names.contains(&"get_info")
                && names.contains(&"create_wallet")
                && names.contains(&"send_asset")
        );
        assert!(tools.iter().all(|t| t.get("inputSchema").is_some()));
    }

    #[test]
    fn get_info_tool_returns_metadata() {
        let r = call(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_info","arguments":{}}}"#,
        );
        let result = r.get("result").unwrap();
        assert_eq!(result.get("isError"), Some(&Value::Bool(false)));
        let content = match result.get("content").unwrap() {
            Value::Array(c) => c.clone(),
            _ => panic!("content not an array"),
        };
        let text = content[0].get("text").and_then(Value::as_str).unwrap();
        assert!(
            text.contains("All-in-one crypto wallet for Agents. Built from the ground up in Rust.")
        );
        assert!(text.contains("Version 0.0"));
    }

    #[test]
    fn tool_failures_are_results_with_is_error() {
        let r = call(
            r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"get_wallet_address","arguments":{"wallet_name":"x","password":"pppppppp"}}}"#,
        );
        assert!(r.get("error").is_none());
        assert_eq!(
            r.get("result").unwrap().get("isError"),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn unknown_tool_is_invalid_params() {
        let r = call(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope"}}"#);
        assert_eq!(
            r.get("error").unwrap().get("code"),
            Some(&Value::int(INVALID_PARAMS))
        );
    }

    #[test]
    fn missing_or_non_string_tool_name_is_invalid_params() {
        for raw in [
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call"}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":5}}"#,
        ] {
            let r = call(raw);
            assert_eq!(
                r.get("error").unwrap().get("code"),
                Some(&Value::int(INVALID_PARAMS)),
                "{raw}"
            );
        }
    }

    #[test]
    fn ping_returns_empty_object() {
        let r = call(r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#);
        assert_eq!(r.get("result"), Some(&Value::Object(vec![])));
    }

    #[test]
    fn unknown_method_is_not_found() {
        let r = call(r#"{"jsonrpc":"2.0","id":6,"method":"wallet/steal"}"#);
        assert_eq!(
            r.get("error").unwrap().get("code"),
            Some(&Value::int(METHOD_NOT_FOUND))
        );
        assert_eq!(r.get("id"), Some(&Value::int(6)));
    }

    #[test]
    fn notifications_get_no_response() {
        assert_eq!(
            send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            None
        );
        assert_eq!(send(r#"{"jsonrpc":"2.0","method":"unknown/thing"}"#), None);
    }

    #[test]
    fn client_responses_are_ignored() {
        assert_eq!(send(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#), None);
    }

    #[test]
    fn parse_error_has_null_id() {
        let r = call("{not json");
        assert_eq!(r.get("id"), Some(&Value::Null));
        assert_eq!(
            r.get("error").unwrap().get("code"),
            Some(&Value::int(PARSE_ERROR))
        );
    }

    #[test]
    fn non_object_messages_are_invalid_requests() {
        for raw in [
            "[]",
            "1",
            "\"x\"",
            "null",
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
        ] {
            let r = call(raw);
            assert_eq!(
                r.get("error").unwrap().get("code"),
                Some(&Value::int(INVALID_REQUEST)),
                "{raw}"
            );
        }
    }

    #[test]
    fn invalid_method_or_id_is_invalid_request() {
        for raw in [
            r#"{"jsonrpc":"2.0","id":1,"method":7}"#,
            r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":[1],"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":1}"#,
        ] {
            let r = call(raw);
            assert_eq!(
                r.get("error").unwrap().get("code"),
                Some(&Value::int(INVALID_REQUEST)),
                "{raw}"
            );
        }
    }

    #[test]
    fn numeric_id_round_trips_verbatim() {
        let out = send(r#"{"jsonrpc":"2.0","id":12345678901234567890,"method":"ping"}"#).unwrap();
        assert!(out.contains(r#""id":12345678901234567890"#));
    }

    #[test]
    fn responses_are_single_line() {
        let out =
            send(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_info"}}"#)
                .unwrap();
        assert!(!out.contains('\n'));
    }
}
