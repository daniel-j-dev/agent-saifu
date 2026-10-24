//! Test double for Sui's GraphQL API and the Cetus aggregator: answers by a fragment of the
//! request body (or URL) and records every request.

use std::cell::RefCell;

use crate::http::HttpClient;

enum Reply {
    Ok(String),
    Net(String),
}

struct Route {
    needle: String,
    replies: Vec<Reply>,
}

#[derive(Default)]
pub struct SuiMock {
    posts: RefCell<Vec<Route>>,
    gets: RefCell<Vec<Route>>,
    /// `(url, body)`; the body is empty for GET requests.
    pub requests: RefCell<Vec<(String, String)>>,
}

fn pick(routes: &RefCell<Vec<Route>>, haystack: &str) -> Option<Result<String, String>> {
    let mut routes = routes.borrow_mut();
    let route = routes.iter_mut().find(|r| haystack.contains(&r.needle))?;
    let reply = if route.replies.len() > 1 {
        route.replies.remove(0)
    } else {
        match route.replies.first()? {
            Reply::Ok(s) => Reply::Ok(s.clone()),
            Reply::Net(s) => Reply::Net(s.clone()),
        }
    };
    Some(match reply {
        Reply::Ok(s) => Ok(s),
        Reply::Net(s) => Err(s),
    })
}

impl SuiMock {
    pub fn new() -> SuiMock {
        SuiMock::default()
    }

    fn post(self, needle: &str, replies: Vec<Reply>) -> SuiMock {
        self.posts.borrow_mut().push(Route {
            needle: needle.to_string(),
            replies,
        });
        self
    }

    /// Answers requests whose body contains `needle` with `{"data": <data_json>}`.
    pub fn data(self, needle: &str, data_json: &str) -> SuiMock {
        self.post(
            needle,
            vec![Reply::Ok(format!(r#"{{"data":{data_json}}}"#))],
        )
    }

    /// Successive answers; the last one repeats.
    pub fn data_seq(self, needle: &str, data_jsons: Vec<String>) -> SuiMock {
        let replies = data_jsons
            .into_iter()
            .map(|d| Reply::Ok(format!(r#"{{"data":{d}}}"#)))
            .collect();
        self.post(needle, replies)
    }

    pub fn errors(self, needle: &str, message: &str) -> SuiMock {
        let escaped = crate::json::escape_string(message);
        self.post(
            needle,
            vec![Reply::Ok(format!(
                r#"{{"data":null,"errors":[{{"message":{escaped}}}]}}"#
            ))],
        )
    }

    pub fn raw(self, needle: &str, text: &str) -> SuiMock {
        self.post(needle, vec![Reply::Ok(text.to_string())])
    }

    pub fn net_error(self, needle: &str, message: &str) -> SuiMock {
        self.post(needle, vec![Reply::Net(message.to_string())])
    }

    /// Answers GET requests whose URL contains `url_part`.
    pub fn get(self, url_part: &str, body: &str) -> SuiMock {
        self.gets.borrow_mut().push(Route {
            needle: url_part.to_string(),
            replies: vec![Reply::Ok(body.to_string())],
        });
        self
    }

    pub fn get_error(self, url_part: &str, message: &str) -> SuiMock {
        self.gets.borrow_mut().push(Route {
            needle: url_part.to_string(),
            replies: vec![Reply::Net(message.to_string())],
        });
        self
    }

    /// Appends another mock's routes after this one's (earlier routes win).
    pub fn extend(self, other: SuiMock) -> SuiMock {
        self.posts
            .borrow_mut()
            .append(&mut other.posts.borrow_mut());
        self.gets.borrow_mut().append(&mut other.gets.borrow_mut());
        self
    }

    pub fn post_count(&self, needle: &str) -> usize {
        self.requests
            .borrow()
            .iter()
            .filter(|(_, body)| body.contains(needle))
            .count()
    }

    pub fn get_urls(&self) -> Vec<String> {
        self.requests
            .borrow()
            .iter()
            .filter(|(_, body)| body.is_empty())
            .map(|(url, _)| url.clone())
            .collect()
    }

    /// The bodies of POST requests containing `needle`.
    pub fn bodies(&self, needle: &str) -> Vec<String> {
        self.requests
            .borrow()
            .iter()
            .filter(|(_, body)| body.contains(needle))
            .map(|(_, body)| body.clone())
            .collect()
    }
}

impl HttpClient for SuiMock {
    fn get(&self, url: &str) -> Result<String, String> {
        self.requests
            .borrow_mut()
            .push((url.to_string(), String::new()));
        pick(&self.gets, url).unwrap_or_else(|| Err("HTTP status 404".to_string()))
    }

    fn post_json(&self, url: &str, body: &str) -> Result<String, String> {
        self.requests
            .borrow_mut()
            .push((url.to_string(), body.to_string()));
        pick(&self.posts, body)
            .unwrap_or_else(|| Err("unexpected request: no mock route".to_string()))
    }
}
