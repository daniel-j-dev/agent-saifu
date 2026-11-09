//! A scripted Solana node and web for tests: answers JSON-RPC calls by method (optionally only
//! when the parameters contain some text) and GET requests by URL part, and records every call.

use std::cell::RefCell;
use std::rc::Rc;

use crate::http::HttpClient;
use crate::json::{self, Value};

#[derive(Clone)]
enum Answer {
    Result(String),
    Error(i64, String),
    Net(String),
    /// Works out the raw `result` JSON from the call's parameters.
    With(Rc<dyn Fn(&Value) -> String>),
}

struct Handler {
    method: String,
    when: Option<String>,
    answers: Vec<Answer>,
}

#[derive(Default)]
pub struct SolMock {
    handlers: RefCell<Vec<Handler>>,
    gets: RefCell<Vec<(String, Result<String, String>)>>,
    /// Every RPC call as `(method, params)`.
    pub calls: RefCell<Vec<(String, Value)>>,
    pub get_urls: RefCell<Vec<String>>,
}

impl SolMock {
    pub fn new() -> SolMock {
        SolMock::default()
    }

    fn add(self, method: &str, when: Option<&str>, answer: Answer) -> SolMock {
        {
            let mut handlers = self.handlers.borrow_mut();
            match handlers
                .iter_mut()
                .find(|h| h.method == method && h.when.as_deref() == when)
            {
                Some(h) => h.answers.push(answer),
                None => handlers.push(Handler {
                    method: method.to_string(),
                    when: when.map(str::to_string),
                    answers: vec![answer],
                }),
            }
        }
        self
    }

    /// Answers `method` with this raw `result` JSON. Calling it again for the same method queues
    /// further answers: they are used in order and the last one repeats.
    pub fn on(self, method: &str, result_json: &str) -> SolMock {
        self.add(method, None, Answer::Result(result_json.to_string()))
    }

    /// Like `on`, but only for calls whose parameters contain `when`.
    pub fn on_when(self, method: &str, when: &str, result_json: &str) -> SolMock {
        self.add(method, Some(when), Answer::Result(result_json.to_string()))
    }

    pub fn on_error(self, method: &str, code: i64, message: &str) -> SolMock {
        self.add(method, None, Answer::Error(code, message.to_string()))
    }

    /// Answers `method` by computing the raw `result` JSON from the call's parameters.
    pub fn on_with(self, method: &str, answer: impl Fn(&Value) -> String + 'static) -> SolMock {
        self.add(method, None, Answer::With(Rc::new(answer)))
    }

    /// A transport failure: the request fails before any answer arrives.
    pub fn on_net_error(self, method: &str, message: &str) -> SolMock {
        self.add(method, None, Answer::Net(message.to_string()))
    }

    pub fn get(self, url_part: &str, response: Result<String, String>) -> SolMock {
        self.gets
            .borrow_mut()
            .push((url_part.to_string(), response));
        self
    }

    pub fn count(&self, method: &str) -> usize {
        self.calls
            .borrow()
            .iter()
            .filter(|(m, _)| m == method)
            .count()
    }

    /// Parameters of the `n`th call of `method`.
    pub fn params(&self, method: &str, n: usize) -> Value {
        self.calls
            .borrow()
            .iter()
            .filter(|(m, _)| m == method)
            .nth(n)
            .map(|(_, p)| p.clone())
            .unwrap_or(Value::Null)
    }

    pub fn methods(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|(m, _)| m.clone()).collect()
    }
}

impl HttpClient for SolMock {
    fn get(&self, url: &str) -> Result<String, String> {
        self.get_urls.borrow_mut().push(url.to_string());
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
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let params_text = params.to_json();
        self.calls.borrow_mut().push((method.clone(), params));
        let answer = {
            let mut handlers = self.handlers.borrow_mut();
            let specific = handlers.iter().position(|h| {
                h.method == method
                    && h.when
                        .as_ref()
                        .is_some_and(|w| params_text.contains(w.as_str()))
            });
            let chosen = specific.or_else(|| {
                handlers
                    .iter()
                    .position(|h| h.method == method && h.when.is_none())
            });
            chosen.map(|i| {
                let h = &mut handlers[i];
                if h.answers.len() > 1 {
                    h.answers.remove(0)
                } else {
                    h.answers[0].clone()
                }
            })
        };
        let id = request
            .get("id")
            .map(Value::to_json)
            .unwrap_or_else(|| "1".to_string());
        let answer = match answer {
            Some(Answer::With(f)) => {
                let params = self
                    .calls
                    .borrow()
                    .last()
                    .map(|(_, p)| p.clone())
                    .unwrap_or(Value::Null);
                Some(Answer::Result(f(&params)))
            }
            other => other,
        };
        match answer {
            Some(Answer::Result(r)) => Ok(format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{r}}}"#)),
            Some(Answer::Error(code, message)) => Ok(format!(
                r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":{code},"message":"{message}"}}}}"#
            )),
            Some(Answer::Net(message)) => Err(message),
            Some(Answer::With(_)) => unreachable!("resolved above"),
            None => Ok(format!(
                r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32601,"message":"method not found"}}}}"#
            )),
        }
    }
}
