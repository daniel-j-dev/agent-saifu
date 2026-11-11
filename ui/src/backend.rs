//! The bridge to the wallet in `../core`.
//!
//! The wallet's `Toolbox` is not `Send` (it shares state through `Rc`), so it lives on one
//! dedicated thread and the screens talk to it by name and JSON, exactly like an MCP agent does.
//! Calls run one at a time, in order, and the screens never block while one is running.

use std::path::PathBuf;
use std::sync::mpsc;

use agent_saifu_core::json::{self, Value};
use futures_channel::oneshot;

pub type CallResult = Result<Value, String>;

struct Job {
    name: String,
    args: Value,
    reply: oneshot::Sender<CallResult>,
}

/// Tool-like calls the app answers itself instead of passing to the wallet's `Toolbox`.
pub const UNLOCK: &str = "ui_unlock";
pub const REFRESH: &str = "ui_refresh";
pub const FORGET: &str = "ui_forget";
pub const TOKEN_SEARCH: &str = "ui_token_search";

#[derive(Clone)]
pub struct Backend {
    jobs: mpsc::Sender<Job>,
    /// Token search only reads public lists, so it has its own thread and a slow wallet call
    /// (pairing waits for a dapp) never delays it.
    search_jobs: mpsc::Sender<Job>,
}

fn spawn_worker(
    name: &str,
    run: impl FnOnce(mpsc::Receiver<Job>) + Send + 'static,
) -> mpsc::Sender<Job> {
    let (jobs, inbox) = mpsc::channel::<Job>();
    let spawned = std::thread::Builder::new()
        .name(name.into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || run(inbox));
    if let Err(e) = spawned {
        eprintln!("agent-saifu-ui: cannot start the {name} thread: {e}");
    }
    jobs
}

impl Backend {
    pub fn start() -> Backend {
        let jobs = spawn_worker("saifu-wallet", worker);
        // The demo engine answers everything, so it needs only one thread.
        #[cfg(feature = "demo")]
        let search_jobs = jobs.clone();
        #[cfg(not(feature = "demo"))]
        let search_jobs = spawn_worker("saifu-tokens", search_worker);
        Backend { jobs, search_jobs }
    }

    /// Runs one wallet tool. The arguments (which may hold a password) are wiped afterwards.
    pub async fn call(&self, name: &str, args: Value) -> CallResult {
        let (reply, answer) = oneshot::channel();
        let job = Job {
            name: name.to_string(),
            args,
            reply,
        };
        let queue = if name == TOKEN_SEARCH {
            &self.search_jobs
        } else {
            &self.jobs
        };
        queue
            .send(job)
            .map_err(|_| "The wallet is not running.".to_string())?;
        answer
            .await
            .map_err(|_| "The wallet stopped before it answered.".to_string())?
    }
}

fn worker(inbox: mpsc::Receiver<Job>) {
    let engine = Engine::new();
    for mut job in inbox {
        let outcome = engine.run(&job.name, &job.args);
        job.args.wipe();
        let _ = job.reply.send(outcome);
    }
}

#[cfg(feature = "demo")]
use crate::demo::Engine;
#[cfg(not(feature = "demo"))]
fn search_worker(inbox: mpsc::Receiver<Job>) {
    let engine = real::SearchEngine::new();
    for job in inbox {
        let _ = job.reply.send(engine.run(&job.args));
    }
}

#[cfg(not(feature = "demo"))]
use real::Engine;

/// Unwraps an MCP tool result (`content[0].text`, `isError`) into the tool's own JSON.
pub fn decode(result: &Value) -> CallResult {
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(text.to_string());
    }
    json::parse(text).map_err(|e| format!("The wallet sent an answer the app cannot read: {e}"))
}

/// Where wallets live. `AGENT_SAIFU_HOME` wins (so the app and the MCP server can share wallets
/// or be pointed away from real ones); phones use the app's private storage.
pub fn data_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("AGENT_SAIFU_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    match platform_dir() {
        Some(dir) => Ok(dir),
        None => agent_saifu_core::store::default_dir(),
    }
}

#[cfg(target_os = "android")]
fn platform_dir() -> Option<PathBuf> {
    // The process is named after the app's package, whose private files live under /data/data.
    let raw = std::fs::read("/proc/self/cmdline").ok()?;
    let name = raw.split(|b| *b == 0).next()?;
    let package = std::str::from_utf8(name).ok()?.trim();
    (!package.is_empty()).then(|| {
        PathBuf::from("/data/data")
            .join(package)
            .join("files")
            .join("agent-saifu")
    })
}

#[cfg(target_os = "ios")]
fn platform_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join("Documents").join("agent-saifu"))
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn platform_dir() -> Option<PathBuf> {
    None
}

#[cfg(not(feature = "demo"))]
mod real {
    use std::rc::Rc;

    use std::cell::RefCell;
    use std::collections::HashMap;

    use agent_saifu_core::eth::swap::SwapSettings;
    use agent_saifu_core::http::UreqClient;
    use agent_saifu_core::json::Value;
    use agent_saifu_core::service::{PublicView, WalletService};
    use agent_saifu_core::solana::swap::SolanaSwapSettings;
    use agent_saifu_core::store::Store;
    use agent_saifu_core::sui::cetus::CetusSettings;
    use agent_saifu_core::tools::Toolbox;
    use agent_saifu_core::walletconnect::manager::WcManager;
    use agent_saifu_core::walletconnect::relay::build_relay;
    use agent_saifu_core::walletconnect_config;

    use super::{CallResult, FORGET, REFRESH, UNLOCK, data_dir, decode};
    use crate::tokens::{Net, TokenSearch, hits_value};

    pub struct Engine {
        toolbox: Result<Toolbox, String>,
        /// The public side (address, networks, tokens) of every wallet unlocked since the app
        /// started. No key is kept, so a refresh needs no password; it is dropped with the app.
        views: RefCell<HashMap<String, PublicView>>,
    }

    /// What a screen gets when the wallet unlocked but the network did not answer.
    fn unreadable_balances(wallet: &str, address: &str, error: &str) -> Value {
        Value::object(vec![
            ("name", Value::str(wallet)),
            ("address", Value::str(address)),
            ("assets", Value::Array(vec![])),
            ("refresh_error", Value::str(error)),
        ])
    }

    fn wallet_arg(args: &Value) -> Result<&str, String> {
        args.get("wallet_name")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing wallet name".to_string())
    }

    impl Engine {
        pub fn new() -> Engine {
            Engine::at(data_dir())
        }

        pub fn at(dir: Result<std::path::PathBuf, String>) -> Engine {
            Engine {
                toolbox: dir.and_then(|d| build_toolbox(&d)),
                views: RefCell::new(HashMap::new()),
            }
        }

        pub fn run(&self, name: &str, args: &Value) -> CallResult {
            let toolbox = self.toolbox.as_ref().map_err(String::clone)?;
            match name {
                UNLOCK => {
                    let wallet = wallet_arg(args)?;
                    let password = args
                        .get("password")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let view = toolbox.service().public_view(wallet, password)?;
                    let address = view.address();
                    // Keep the view even when the balances cannot be read right now, so a
                    // later refresh works without the password.
                    let balances = toolbox.service().balances_of_view(&view);
                    self.views.borrow_mut().insert(wallet.to_string(), view);
                    Ok(balances.unwrap_or_else(|e| unreadable_balances(wallet, &address, &e)))
                }
                REFRESH => {
                    let wallet = wallet_arg(args)?;
                    let views = self.views.borrow();
                    let view = views
                        .get(wallet)
                        .ok_or("This wallet is locked. Enter its password once to unlock it.")?;
                    toolbox.service().balances_of_view(view)
                }
                FORGET => {
                    self.views.borrow_mut().remove(wallet_arg(args)?);
                    Ok(Value::object(vec![("status", Value::str("forgotten"))]))
                }
                _ => {
                    let result = toolbox
                        .call(name, Some(args))
                        .ok_or_else(|| format!("The wallet has no tool called '{name}'."))?;
                    decode(&result)
                }
            }
        }
    }

    /// Answers `ui_token_search` on its own thread.
    pub struct SearchEngine {
        search: TokenSearch,
    }

    impl SearchEngine {
        pub fn new() -> SearchEngine {
            SearchEngine {
                search: TokenSearch::new(Rc::new(UreqClient::new())),
            }
        }

        pub fn run(&self, args: &Value) -> CallResult {
            let text = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or_default();
            let net = Net::parse(
                text("network"),
                args.get("chain_id").and_then(Value::as_u64),
            )
            .ok_or("Token search is not available for this network.")?;
            self.search
                .search(net, text("query"))
                .map(|hits| hits_value(&hits))
        }
    }

    // Mirrors the wiring in core/src/main.rs so the app and the MCP server behave the same.
    fn build_toolbox(dir: &std::path::Path) -> Result<Toolbox, String> {
        let store = Store::open(dir)?;
        let project_id = std::env::var(walletconnect_config::PROJECT_ID_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| walletconnect_config::WALLETCONNECT_PROJECT_ID.to_string());
        let relay = build_relay(&project_id, walletconnect_config::WALLETCONNECT_RELAY_URL);
        let service = WalletService::new(store, Rc::new(UreqClient::new()))
            .with_swap_settings(SwapSettings::from_config()?)
            .with_solana_swap_settings(SolanaSwapSettings::from_config()?)
            .with_cetus_settings(CetusSettings::from_config()?)
            .with_walletconnect(WcManager::new(relay));
        Ok(Toolbox::new(service))
    }
}

/// Builder for tool arguments.
#[derive(Default)]
pub struct Args(Vec<(String, Value)>);

impl Args {
    pub fn new() -> Args {
        Args::default()
    }

    pub fn text(mut self, key: &str, value: &str) -> Args {
        self.0.push((key.to_string(), Value::str(value)));
        self
    }

    /// Adds the argument only when something was typed.
    pub fn opt(self, key: &str, value: &str) -> Args {
        if value.trim().is_empty() {
            self
        } else {
            self.text(key, value.trim())
        }
    }

    pub fn int(mut self, key: &str, value: u64) -> Args {
        self.0
            .push((key.to_string(), Value::Number(value.to_string())));
        self
    }

    pub fn opt_int(self, key: &str, value: Option<u64>) -> Args {
        match value {
            Some(v) => self.int(key, v),
            None => self,
        }
    }

    pub fn flag(mut self, key: &str, value: bool) -> Args {
        self.0.push((key.to_string(), Value::Bool(value)));
        self
    }

    pub fn value(mut self, key: &str, value: Value) -> Args {
        self.0.push((key.to_string(), value));
        self
    }

    pub fn build(self) -> Value {
        Value::Object(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_results_and_errors() {
        let ok = json::parse(r#"{"content":[{"type":"text","text":"{\"a\":1}"}],"isError":false}"#)
            .unwrap();
        assert_eq!(decode(&ok).unwrap().get("a").unwrap().as_u64(), Some(1));
        let err =
            json::parse(r#"{"content":[{"type":"text","text":"bad password"}],"isError":true}"#)
                .unwrap();
        assert_eq!(decode(&err), Err("bad password".to_string()));
        let junk =
            json::parse(r#"{"content":[{"type":"text","text":"not json"}],"isError":false}"#)
                .unwrap();
        assert!(decode(&junk).unwrap_err().contains("cannot read"));
    }

    #[test]
    fn builds_arguments_skipping_blanks() {
        let v = Args::new()
            .text("wallet_name", "A")
            .opt("fee_rate", "  ")
            .opt("note", " hi ")
            .int("chain_id", 8453)
            .opt_int("limit", None)
            .flag("confirm", true)
            .build();
        assert_eq!(v.get("wallet_name").unwrap().as_str(), Some("A"));
        assert!(v.get("fee_rate").is_none());
        assert_eq!(v.get("note").unwrap().as_str(), Some("hi"));
        assert_eq!(v.get("chain_id").unwrap().as_u64(), Some(8453));
        assert!(v.get("limit").is_none());
        assert_eq!(v.get("confirm").unwrap().as_bool(), Some(true));
    }

    #[cfg(not(feature = "demo"))]
    mod engine {
        use super::*;

        fn args(pairs: &[(&str, &str)]) -> Value {
            Value::Object(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), Value::str(v)))
                    .collect(),
            )
        }

        fn temp_dir() -> std::path::PathBuf {
            let dir = std::env::temp_dir().join(format!("saifu-ui-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            dir
        }

        #[test]
        fn a_refresh_before_any_unlock_asks_for_the_password() {
            let dir = temp_dir();
            let engine = Engine::at(Ok(dir.clone()));
            let err = engine
                .run(REFRESH, &args(&[("wallet_name", "w")]))
                .unwrap_err();
            assert!(err.contains("locked"), "{err}");
            assert!(engine.run(REFRESH, &Value::Object(vec![])).is_err());
            let _ = std::fs::remove_dir_all(dir);
        }

        /// Needs the network: `cargo test live -- --ignored --nocapture`.
        #[test]
        #[ignore = "needs the network and takes a few seconds"]
        fn live_unlock_once_then_refresh_without_the_password() {
            let dir = temp_dir();
            let engine = Engine::at(Ok(dir.clone()));
            let created = engine.run(
                "create_wallet",
                &args(&[
                    ("blockchain", "sui"),
                    ("wallet_name", "w"),
                    ("password", "password-123"),
                    ("network", "devnet"),
                ]),
            );
            let address = created
                .unwrap()
                .get("address")
                .and_then(Value::as_str)
                .unwrap()
                .to_string();
            let wrong = engine.run(
                UNLOCK,
                &args(&[("wallet_name", "w"), ("password", "wrong-password")]),
            );
            assert!(wrong.is_err());
            assert!(
                engine.run(REFRESH, &args(&[("wallet_name", "w")])).is_err(),
                "a failed unlock keeps nothing"
            );
            let unlocked = engine
                .run(
                    UNLOCK,
                    &args(&[("wallet_name", "w"), ("password", "password-123")]),
                )
                .unwrap();
            assert_eq!(
                unlocked.get("address").and_then(Value::as_str),
                Some(address.as_str())
            );
            let refreshed = engine.run(REFRESH, &args(&[("wallet_name", "w")])).unwrap();
            assert_eq!(
                refreshed.get("address").and_then(Value::as_str),
                Some(address.as_str())
            );
            assert!(refreshed.get("assets").and_then(Value::as_array).is_some());
            engine.run(FORGET, &args(&[("wallet_name", "w")])).unwrap();
            assert!(engine.run(REFRESH, &args(&[("wallet_name", "w")])).is_err());
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
