//! Minimal HTTP abstraction so network code can be tested without real requests.

use std::time::Duration;

pub const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
pub const TIMEOUT: Duration = Duration::from_secs(25);

pub trait HttpClient {
    fn get(&self, url: &str) -> Result<String, String>;
    fn post_json(&self, url: &str, body: &str) -> Result<String, String>;
}

/// Real transport: HTTPS through `ureq` + rustls, no redirects, bounded size and time.
pub struct UreqClient {
    agent: ureq::Agent,
}

impl UreqClient {
    pub fn new() -> UreqClient {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .max_redirects(0)
            .http_status_as_error(false)
            .user_agent("agent-saifu/0.0")
            .build();
        UreqClient {
            agent: config.into(),
        }
    }
}

impl Default for UreqClient {
    fn default() -> Self {
        UreqClient::new()
    }
}

/// `HTTP status 400: <short body>`. APIs usually explain what was wrong in the error body.
/// Web pages (e.g. a bot-protection challenge) are summarised instead of echoed.
pub fn describe(status: u16, body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.starts_with('<') {
        let blocked = trimmed.contains("Just a moment") || trimmed.contains("cloudflare");
        return if blocked {
            format!("HTTP status {status} (blocked by Cloudflare bot protection)")
        } else {
            format!("HTTP status {status} (the server returned a web page instead of data)")
        };
    }
    let snippet: String = trimmed
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_ERROR_SNIPPET_CHARS)
        .collect();
    if snippet.is_empty() {
        format!("HTTP status {status}")
    } else {
        format!("HTTP status {status}: {snippet}")
    }
}

const MAX_ERROR_SNIPPET_CHARS: usize = 300;

impl HttpClient for UreqClient {
    fn get(&self, url: &str) -> Result<String, String> {
        let mut response = self
            .agent
            .get(url)
            .header("accept", "application/json")
            .call()
            .map_err(|e| format!("request failed: {e}"))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_string()
            .map_err(|e| format!("could not read response: {e}"))?;
        if !(200..300).contains(&status) {
            return Err(describe(status, &text));
        }
        Ok(text)
    }

    fn post_json(&self, url: &str, body: &str) -> Result<String, String> {
        let mut response = self
            .agent
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .send(body)
            .map_err(|e| format!("request failed: {e}"))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_string()
            .map_err(|e| format!("could not read response: {e}"))?;
        if !(200..300).contains(&status) {
            return Err(describe(status, &text));
        }
        Ok(text)
    }
}

#[cfg(test)]
pub mod mock {
    use super::HttpClient;
    use std::cell::RefCell;

    /// Replays canned responses in order and records every request.
    pub struct MockHttp {
        responses: RefCell<Vec<Result<String, String>>>,
        pub requests: RefCell<Vec<(String, Option<String>)>>,
    }

    impl MockHttp {
        pub fn new(responses: Vec<Result<String, String>>) -> MockHttp {
            let mut r = responses;
            r.reverse();
            MockHttp {
                responses: RefCell::new(r),
                requests: RefCell::new(vec![]),
            }
        }

        fn next(&self, url: &str, body: Option<&str>) -> Result<String, String> {
            self.requests
                .borrow_mut()
                .push((url.to_string(), body.map(str::to_string)));
            self.responses
                .borrow_mut()
                .pop()
                .unwrap_or_else(|| Err("no mock response left".to_string()))
        }
    }

    impl HttpClient for MockHttp {
        fn get(&self, url: &str) -> Result<String, String> {
            self.next(url, None)
        }

        fn post_json(&self, url: &str, body: &str) -> Result<String, String> {
            self.next(url, Some(body))
        }
    }

    use crate::json::{self, Value};
    use std::collections::HashMap;

    /// Answers JSON-RPC calls by method name (or `method:selector` for `eth_call`) and
    /// GET requests by URL substring. Unknown RPC methods return a "method not found" error.
    #[derive(Default)]
    pub struct RouterMock {
        rpc: RefCell<HashMap<String, String>>,
        gets: RefCell<Vec<(String, Result<String, String>)>>,
        sequences: RefCell<HashMap<String, Vec<String>>>,
        pub requests: RefCell<Vec<String>>,
    }

    impl RouterMock {
        pub fn new() -> RouterMock {
            RouterMock::default()
        }

        /// `result_json` is the raw JSON placed in the `result` field.
        pub fn rpc(self, key: &str, result_json: &str) -> RouterMock {
            self.rpc
                .borrow_mut()
                .insert(key.to_string(), format!(r#"{{"result":{result_json}}}"#));
            self
        }

        pub fn rpc_error(
            self,
            key: &str,
            code: i64,
            message: &str,
            data: Option<&str>,
        ) -> RouterMock {
            let data = data
                .map(|d| format!(r#","data":"{d}""#))
                .unwrap_or_default();
            self.rpc.borrow_mut().insert(
                key.to_string(),
                format!(r#"{{"error":{{"code":{code},"message":"{message}"{data}}}}}"#),
            );
            self
        }

        /// Answers successive calls of `key` with `results` (raw `result` JSON, `ERR:<message>` for a node
        /// error or `NET:<message>` for a transport failure).
        pub fn rpc_sequence(self, key: &str, results: Vec<&str>) -> RouterMock {
            let encoded = results
                .into_iter()
                .map(|r| match r.strip_prefix("ERR:") {
                    // Not a node answer: the request fails before any response arrives.
                    _ if r.starts_with("NET:") => r.to_string(),
                    Some(msg) => format!(r#"{{"error":{{"code":-32003,"message":"{msg}"}}}}"#),
                    None => format!(r#"{{"result":{r}}}"#),
                })
                .collect();
            self.sequences.borrow_mut().insert(key.to_string(), encoded);
            self
        }

        pub fn get(self, url_part: &str, response: Result<String, String>) -> RouterMock {
            self.gets
                .borrow_mut()
                .push((url_part.to_string(), response));
            self
        }

        pub fn sent_methods(&self) -> Vec<String> {
            self.requests.borrow().clone()
        }
    }

    impl HttpClient for RouterMock {
        fn get(&self, url: &str) -> Result<String, String> {
            self.requests.borrow_mut().push(format!("GET {url}"));
            for (part, response) in self.gets.borrow().iter() {
                if url.contains(part.as_str()) {
                    return response.clone();
                }
            }
            Err("HTTP status 404".to_string())
        }

        fn post_json(&self, _url: &str, body: &str) -> Result<String, String> {
            let request = json::parse(body).map_err(|e| e.to_string())?;
            let method = request
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let selector = request
                .get("params")
                .and_then(Value::as_array)
                .and_then(|p| p.first())
                .and_then(|p| p.get("data"))
                .and_then(Value::as_str)
                .filter(|d| d.len() >= 10)
                .map(|d| d[2..10].to_string());
            self.requests.borrow_mut().push(method.clone());
            let queued = {
                let mut seqs = self.sequences.borrow_mut();
                match seqs.get_mut(&method) {
                    Some(list) if !list.is_empty() => Some(list.remove(0)),
                    _ => None,
                }
            };
            if let Some(msg) = queued.as_deref().and_then(|q| q.strip_prefix("NET:")) {
                return Err(msg.to_string());
            }
            let routes = self.rpc.borrow();
            let found = selector
                .as_ref()
                .and_then(|s| routes.get(&format!("{method}:{s}")))
                .or_else(|| routes.get(&method));
            let inner = match (queued, found) {
                (Some(q), _) => q,
                (None, Some(r)) => r.clone(),
                (None, None) => {
                    r#"{"error":{"code":-32601,"message":"method not found"}}"#.to_string()
                }
            };
            let id = request
                .get("id")
                .map(Value::to_json)
                .unwrap_or_else(|| "1".to_string());
            Ok(format!(r#"{{"jsonrpc":"2.0","id":{id},{}"#, &inner[1..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_description_includes_a_short_clean_body() {
        assert_eq!(describe(404, ""), "HTTP status 404");
        assert_eq!(describe(404, "  \n"), "HTTP status 404");
        assert_eq!(
            describe(400, "{\"error\":\"bad\"}\n"),
            "HTTP status 400: {\"error\":\"bad\"}"
        );
        let long = describe(500, &"x".repeat(1000));
        assert_eq!(
            long.chars().count(),
            "HTTP status 500: ".len() + MAX_ERROR_SNIPPET_CHARS
        );
        assert!(!describe(500, "a\u{0}b\u{7}c").contains('\u{0}'));
    }

    #[test]
    fn html_error_pages_are_summarised_not_echoed() {
        let challenge = "<!DOCTYPE html><html><head><title>Just a moment...</title></head></html>";
        let d = describe(403, challenge);
        assert!(d.contains("Cloudflare") && !d.contains("DOCTYPE"));
        let other = describe(502, "<html><body>Bad gateway</body></html>");
        assert!(other.contains("web page") && !other.contains("Bad gateway"));
    }
}
