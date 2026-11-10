//! A scripted Esplora server for tests: answers GET and POST requests by the end of the URL,
//! can answer the same request differently each time, and records every request.

use std::cell::RefCell;
use std::rc::Rc;

use crate::http::HttpClient;

type Answers = Vec<Result<String, String>>;

#[derive(Default)]
pub struct EsploraMock {
    gets: RefCell<Vec<(String, Answers)>>,
    posts: RefCell<Vec<(String, Answers)>>,
    #[allow(clippy::type_complexity)]
    dynamic_posts: RefCell<Vec<(String, Rc<dyn Fn(&str) -> Result<String, String>>)>>,
    pub get_urls: RefCell<Vec<String>>,
    posted: RefCell<Vec<(String, String)>>,
}

fn add(routes: &RefCell<Vec<(String, Answers)>>, suffix: &str, answer: Result<String, String>) {
    let mut routes = routes.borrow_mut();
    match routes.iter_mut().find(|(s, _)| s == suffix) {
        Some((_, answers)) => answers.push(answer),
        None => routes.push((suffix.to_string(), vec![answer])),
    }
}

fn next(routes: &RefCell<Vec<(String, Answers)>>, url: &str) -> Option<Result<String, String>> {
    let mut routes = routes.borrow_mut();
    let (_, answers) = routes.iter_mut().find(|(s, _)| url.ends_with(s.as_str()))?;
    if answers.len() > 1 {
        Some(answers.remove(0))
    } else {
        answers.first().cloned()
    }
}

impl EsploraMock {
    pub fn new() -> EsploraMock {
        EsploraMock::default()
    }

    /// Answers GET requests whose URL ends with `suffix`. Registering the same suffix again
    /// queues a further answer: they are used in order and the last one repeats.
    pub fn get(self, suffix: &str, answer: Result<String, String>) -> EsploraMock {
        add(&self.gets, suffix, answer);
        self
    }

    pub fn post(self, suffix: &str, answer: Result<String, String>) -> EsploraMock {
        add(&self.posts, suffix, answer);
        self
    }

    /// Answers POST requests by computing the answer from the request body.
    pub fn post_with(
        self,
        suffix: &str,
        answer: impl Fn(&str) -> Result<String, String> + 'static,
    ) -> EsploraMock {
        self.dynamic_posts
            .borrow_mut()
            .push((suffix.to_string(), Rc::new(answer)));
        self
    }

    /// Every POST as `(url, body)`.
    pub fn posted(&self) -> Vec<(String, String)> {
        self.posted.borrow().clone()
    }

    pub fn count_gets(&self, suffix: &str) -> usize {
        self.get_urls
            .borrow()
            .iter()
            .filter(|u| u.ends_with(suffix))
            .count()
    }
}

impl HttpClient for EsploraMock {
    fn get(&self, url: &str) -> Result<String, String> {
        self.get_urls.borrow_mut().push(url.to_string());
        next(&self.gets, url).unwrap_or_else(|| Err("HTTP status 404: not found".to_string()))
    }

    fn post_json(&self, _url: &str, _body: &str) -> Result<String, String> {
        Err("unexpected JSON POST".to_string())
    }

    fn post_text(&self, url: &str, body: &str) -> Result<String, String> {
        self.posted
            .borrow_mut()
            .push((url.to_string(), body.to_string()));
        let dynamic = self
            .dynamic_posts
            .borrow()
            .iter()
            .find(|(suffix, _)| url.ends_with(suffix.as_str()))
            .map(|(_, f)| f.clone());
        if let Some(answer) = dynamic {
            return answer(body);
        }
        next(&self.posts, url).unwrap_or_else(|| Err("HTTP status 404: not found".to_string()))
    }
}
