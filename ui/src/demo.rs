//! Canned wallet answers for reviewing the screens (`cargo run --features demo`).
//! No keys exist, nothing touches the network and any password of 8+ characters is accepted.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use agent_saifu_core::json::{self, Value};

use crate::backend::{CallResult, FORGET, REFRESH, TOKEN_SEARCH, UNLOCK};
use crate::tokens::{Hit, hits_value, rank};

pub struct Engine {
    polls: Cell<u32>,
    request_open: RefCell<bool>,
    wallets: RefCell<Vec<(String, String)>>,
    refreshes: Cell<u32>,
}

fn parse(text: &str) -> Value {
    json::parse(text).expect("demo data is valid JSON")
}

impl Engine {
    pub fn new() -> Engine {
        let wallets = [
            ("Everyday wallet", "Ethereum"),
            ("Sui wallet", "Sui"),
            ("Solana wallet", "Solana"),
            ("Bitcoin wallet", "Bitcoin"),
        ];
        Engine {
            polls: Cell::new(0),
            refreshes: Cell::new(0),
            request_open: RefCell::new(true),
            wallets: RefCell::new(
                wallets
                    .iter()
                    .map(|(n, c)| (n.to_string(), c.to_string()))
                    .collect(),
            ),
        }
    }

    pub fn run(&self, name: &str, args: &Value) -> CallResult {
        std::thread::sleep(Duration::from_millis(350));
        let wallet = args
            .get("wallet_name")
            .and_then(Value::as_str)
            .unwrap_or("Everyday wallet")
            .to_string();
        let chain = self
            .wallets
            .borrow()
            .iter()
            .find(|(n, _)| *n == wallet)
            .map(|(_, c)| c.clone())
            .unwrap_or_else(|| "Ethereum".into());
        if name == TOKEN_SEARCH {
            let text = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or_default();
            return Ok(hits_value(&rank(
                &demo_tokens(text("network")),
                text("query"),
            )));
        }
        let needs_password = args.get("password").is_some();
        if needs_password
            && args
                .get("password")
                .and_then(Value::as_str)
                .is_some_and(|p| p.chars().count() < 8)
        {
            return Err("wrong password".to_string());
        }
        Ok(match name {
            "get_info" => parse(
                r#"{"description":"All-in-one crypto wallet for Agents. Built from the ground up in Rust.","version":"Version 0.0"}"#,
            ),
            "list_wallets" => {
                let items: Vec<String> = self
                    .wallets
                    .borrow()
                    .iter()
                    .map(|(n, c)| format!(r#"{{"name":"{n}","blockchain":"{c}"}}"#))
                    .collect();
                parse(&format!(r#"{{"wallets":[{}]}}"#, items.join(",")))
            }
            "create_wallet" | "import_wallet" => {
                let new = args
                    .get("wallet_name")
                    .and_then(Value::as_str)
                    .unwrap_or("New");
                let chain = args
                    .get("blockchain")
                    .and_then(Value::as_str)
                    .unwrap_or("ethereum");
                let display = match chain {
                    "sui" => "Sui",
                    "solana" => "Solana",
                    "bitcoin" => "Bitcoin",
                    _ => "Ethereum",
                };
                self.wallets
                    .borrow_mut()
                    .push((new.to_string(), display.to_string()));
                parse(&format!(
                    r#"{{"name":"{new}","blockchain":"{display}","address":"0x3c7cb8F6c7e321CDe057F50Aa3D455D9D3C5cc21","status":"created"}}"#
                ))
            }
            "get_wallet_address" => {
                parse(r#"{"name":"demo","address":"0x3c7cb8F6c7e321CDe057F50Aa3D455D9D3C5cc21"}"#)
            }
            "get_wallet_balances" => balances(&chain),
            UNLOCK => balances(&chain),
            REFRESH => {
                // Each refresh finds a little more ETH so the screens visibly update.
                self.refreshes.set(self.refreshes.get() + 1);
                let more = self.refreshes.get();
                let text = balances(&chain).to_json().replace(
                    "\"balance\":\"2.45\"",
                    &format!("\"balance\":\"2.45{more}\""),
                );
                parse(&text)
            }
            FORGET => parse(r#"{"status":"forgotten"}"#),
            "view_seed_phrase" => parse(&format!(
                r#"{{"name":"{wallet}","seed_phrase":"{}","warning":"Anyone with these words controls the wallet."}}"#,
                [
                    "abandon", "ability", "able", "about", "above", "absent", "absorb", "abstract",
                    "absurd", "abuse", "access", "accident", "account", "accuse", "achieve",
                    "acid", "acoustic", "acquire", "across", "act", "action", "actor", "actress",
                    "actual"
                ]
                .join(" ")
            )),
            "view_private_key" => parse(
                r#"{"name":"demo","private_key":"0x0000000000000000000000000000000000000000000000000000000000000001","warning":"Never share it."}"#,
            ),
            "wc_list_requests" => {
                self.polls.set(self.polls.get() + 1);
                if self.polls.get() >= 3 && *self.request_open.borrow() {
                    parse(DEMO_REQUESTS)
                } else {
                    parse(r#"{"count":0,"requests":[]}"#)
                }
            }
            "wc_list_sessions" => parse(DEMO_SESSIONS),
            "wc_list_proposals" => parse(r#"{"count":0,"proposals":[]}"#),
            "wc_review_request" => parse(DEMO_REVIEW),
            "wc_approve_request" => {
                *self.request_open.borrow_mut() = false;
                parse(
                    r#"{"status":"approved","request_id":"1","method":"eth_sendTransaction","wallet":"Everyday wallet","dapp_notified":true,"transaction_hash":"0x9f2c4a77d1e0b6c3a5f8e2d4b7a91c0e3d5f6a8b2c4e6f80a1b3c5d7e9f1a2b3"}"#,
                )
            }
            "wc_reject_request" => {
                *self.request_open.borrow_mut() = false;
                parse(r#"{"request_id":"1","status":"rejected","dapp_notified":true}"#)
            }
            "simulate_transaction" | "estimate_gas_fee" => parse(
                r#"{"chain_id":1,"network":"Ethereum Mainnet","success":true,"currency":"ETH","max_fee":"0.0000842","expected_fee":"0.0000421","expected_fee_usdc":"0.13","warnings":[]}"#,
            ),
            "get_swap_quote" => parse(DEMO_QUOTE),
            "list_networks" => parse(
                r#"{"name":"demo","networks":[{"name":"Ethereum Mainnet","chainId":1,"rpcUrl":"https://ethereum-rpc.publicnode.com","currencySymbol":"ETH","blockExplorerUrl":"https://eth.blockscout.com","builtin":true},{"name":"Base","chainId":8453,"rpcUrl":"https://mainnet.base.org","currencySymbol":"ETH","blockExplorerUrl":"https://base.blockscout.com","builtin":false}]}"#,
            ),
            "get_transaction_history" => parse(
                r#"{"count":2,"has_more":false,"items":[{"transaction_hash":"0xaa11bb22cc33dd44ee55ff6600112233445566778899aabbccddeeff00112233","timestamp":"2026-11-10T14:16:00Z","status":"ok","value":"0.12","from":"0x3c7cb8F6c7e321CDe057F50Aa3D455D9D3C5cc21","to":"0x000000000000000000000000000000000000dEaD"},{"transaction_hash":"0xbb22cc33dd44ee55ff6600112233445566778899aabbccddeeff001122334455","timestamp":"2026-11-08T09:02:00Z","status":"ok","value":"0.5"}]}"#,
            ),
            _ => parse(&format!(
                r#"{{"status":"ok","note":"Demo mode: {name} did nothing."}}"#
            )),
        })
    }
}

fn balances(chain: &str) -> Value {
    match chain {
        "Sui" => parse(
            r#"{"address":"0x134494b08e9938e8587bdde95580c738c7484c18828e834d1bb4ca2bdb52afe6","network":{"name":"mainnet"},"assets":[{"symbol":"SUI","asset":"native","balance":"1840","usdc_value":"3496.00"}]}"#,
        ),
        "Solana" => parse(
            r#"{"address":"GjvrWJBWbrGJYzQaNKfXvaZ7xh1Qa4wGLTobeLyWaJcs","network":{"name":"mainnet"},"assets":[{"symbol":"SOL","asset":"native","name":"Solana","balance":"14.2","usdc_value":"2215.20"},{"symbol":"USDC","asset":"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v","name":"USD Coin","balance":"120","usdc_value":"120.00"}]}"#,
        ),
        "Bitcoin" => parse(
            r#"{"address":"bc1q903fe508nkn2n4d9u7qprg55m3dvcx0cretxpg","network":{"name":"mainnet"},"assets":[{"symbol":"BTC","asset":"native","name":"Bitcoin","balance":"0.042","usdc_value":"3528.00"}]}"#,
        ),
        _ => parse(
            r#"{"address":"0x3c7cb8F6c7e321CDe057F50Aa3D455D9D3C5cc21","networks":[{"chain_id":1,"network":"Ethereum Mainnet","assets":[{"symbol":"ETH","asset":"native","balance":"2.45","usdc_value":"7291.80","chain_id":1},{"symbol":"USDC","asset":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","balance":"950","usdc_value":"950.00","chain_id":1}]}]}"#,
        ),
    }
}

const DEMO_REQUESTS: &str = r#"{"count":1,"requests":[{"request_id":"1","session_topic":"topic-demo","dapp":{"name":"Uniswap","url":"https://app.uniswap.org","description":"Swap tokens"},"method":"eth_sendTransaction","kind":"transaction","chain_id":1,"expires_in_seconds":280,"details":{"to":"0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD","value_native":"0.05","data_bytes":228,"function_selector":"0x3593564c"}}]}"#;

const DEMO_SESSIONS: &str = r#"{"count":1,"sessions":[{"session_topic":"topic-demo","wallet":"Everyday wallet","dapp":{"name":"Uniswap","url":"https://app.uniswap.org","description":"Swap tokens"},"chain_ids":[1],"methods":["eth_sendTransaction","personal_sign"],"confirmed_by_dapp":true,"expires_in_seconds":86000}]}"#;

const DEMO_REVIEW: &str = r#"{"request_id":"1","wallet":"Everyday wallet","from":"0x3c7cb8F6c7e321CDe057F50Aa3D455D9D3C5cc21","dapp":{"name":"Uniswap","url":"https://app.uniswap.org","description":"Swap tokens"},"method":"eth_sendTransaction","kind":"transaction","chain_id":1,"network":"Ethereum Mainnet","expires_in_seconds":270,"will_broadcast":true,"simulation_succeeded":true,"expected_outcome":["Sends 0.05 ETH from your wallet to 0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD","Network fee: at most 0.0000842 ETH (about 0.13 USDC expected)"],"simulation":{"success":true,"native_balance":"2.45","max_fee":"0.0000842","expected_fee":"0.0000421","currency":"ETH","expected_fee_usdc":"0.13","warnings":[]}}"#;

const DEMO_QUOTE: &str = r#"{"type":"same_chain","from":{"chain_id":1,"network":"Ethereum Mainnet","token":"native","symbol":"ETH","amount":"0.1"},"to":{"token":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","symbol":"USDC","expected_amount":"297.62","minimum_amount":"294.64"},"fees":{"service_fee":{"percent":"0.7","amount":"0.0007","symbol":"ETH"},"network_fee":{"amount":"0.00014","currency":"ETH"}},"route":["Uniswap V3"],"price_impact_percent":"0.03","requirements":{"approval_needed":false,"sufficient_balance":true},"warnings":[]}"#;

fn hit(symbol: &str, name: &str, address: &str, verified: bool, note: &str) -> Hit {
    Hit {
        symbol: symbol.to_string(),
        name: name.to_string(),
        address: address.to_string(),
        verified,
        note: note.to_string(),
    }
}

fn demo_tokens(network: &str) -> Vec<Hit> {
    match network {
        "sui" => vec![
            hit("SUI", "Sui", "native", true, ""),
            hit(
                "USDC",
                "USDC",
                "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC",
                true,
                "$9.0M liquidity",
            ),
            hit(
                "CETUS",
                "Cetus Protocol",
                "0x06864a6f921804860930db6ddbe2e16acdf8504495ea7481637a1c8b9a8fe54b::cetus::CETUS",
                true,
                "$1.3M liquidity",
            ),
            hit(
                "NAVX",
                "NAVI Protocol",
                "0xa99b8952d4f7d947ea77fe0ecdcc9e5fc0bcab2841d6e2a5aa00c3044e5544b5::navx::NAVX",
                true,
                "$167K liquidity",
            ),
            hit(
                "DEEP",
                "DeepBook Token",
                "0xdeeb7a4662eec9f2f3def03fb937a663dddaa2e215b8078a284d026b7946c270::deep::DEEP",
                true,
                "$164K liquidity",
            ),
        ],
        "solana" => vec![
            hit("SOL", "Wrapped SOL", "native", true, ""),
            hit(
                "USDC",
                "USD Coin",
                "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                true,
                "$90M liquidity",
            ),
            hit(
                "USDT",
                "Tether USD",
                "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
                true,
                "$40M liquidity",
            ),
            hit(
                "JUP",
                "Jupiter",
                "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN",
                true,
                "$12M liquidity",
            ),
            hit(
                "Bonk",
                "Bonk",
                "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263",
                true,
                "$5.9M liquidity",
            ),
            hit(
                "Bonkey",
                "Bonkey",
                "Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                false,
                "$3K liquidity",
            ),
        ],
        _ => vec![
            hit("ETH", "Ethereum", "native", true, ""),
            hit(
                "USDC",
                "USD Coin",
                "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
                true,
                "",
            ),
            hit(
                "USDT",
                "Tether USD",
                "0xdac17f958d2ee523a2206206994597c13d831ec7",
                true,
                "",
            ),
            hit(
                "DAI",
                "Dai Stablecoin",
                "0x6b175474e89094c44da98b954eedeac495271d0f",
                true,
                "",
            ),
            hit(
                "WBTC",
                "Wrapped Bitcoin",
                "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599",
                true,
                "",
            ),
            hit(
                "UNI",
                "Uniswap",
                "0x1f9840a85d5af5bf1d1762f925bdaddc4201f984",
                true,
                "",
            ),
            hit(
                "PEPE",
                "Pepe",
                "0x6982508145454ce325ddbe47a25d4ec3d2311933",
                true,
                "",
            ),
            hit(
                "USDCX",
                "USD Coin Fork",
                "0x1111111111111111111111111111111111111111",
                false,
                "",
            ),
        ],
    }
}
