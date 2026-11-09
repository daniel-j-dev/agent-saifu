//! Tests for Solana wallets through the service and the MCP tools, against a scripted Solana node.

use std::cell::RefCell;
use std::collections::HashMap;

use super::*;
use crate::crypto::{base58, base64, ed25519};
use crate::json;
use crate::solana::address::{self as saddr, Pubkey};
use crate::solana::describe::Op;
use crate::solana::network::SolanaNetwork;
use crate::solana::testing::SolMock;
use crate::solana::tokens::testing::{mint_bytes, token_account_bytes};
use crate::solana::tx::{self, Transaction};
use crate::store::testutil::TempDir;
use crate::tools::Toolbox;

mod signing;
mod swap;
mod wc;

const PW: &str = "correct-horse-battery";
const MAINNET_GENESIS: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
const DEVNET_GENESIS: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const ABANDON_ART: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";
const ABANDON_ART_ADDRESS: &str = "3Cy3YNTFywCmxoxt8n7UH6hg6dLo5uACowX3CFceaSnx";
const BLOCKHASH: [u8; 32] = [9u8; 32];

fn key_hex() -> String {
    "11".repeat(32)
}

fn vault() -> SolanaVault {
    SolanaVault::from_private_key(&key_hex(), SolanaNetwork::Mainnet).unwrap()
}

fn owner() -> Pubkey {
    vault().public_key().unwrap()
}

fn owner_text() -> String {
    saddr::to_text(&owner())
}

fn key(n: u8) -> Pubkey {
    [n; 32]
}

fn key_text(n: u8) -> String {
    saddr::to_text(&key(n))
}

fn usdc() -> Pubkey {
    saddr::id(USDC)
}

fn token_program() -> Pubkey {
    saddr::id(saddr::TOKEN_PROGRAM)
}

fn token_2022_program() -> Pubkey {
    saddr::id(saddr::TOKEN_2022_PROGRAM)
}

fn system_program() -> Pubkey {
    saddr::id(saddr::SYSTEM_PROGRAM)
}

// ---- the simulated chain -----------------------------------------------------------------------

#[derive(Clone)]
struct Acct {
    lamports: u64,
    owner: Pubkey,
    data: Vec<u8>,
}

impl Acct {
    fn json(&self) -> String {
        format!(
            r#"{{"lamports":{},"owner":"{}","data":["{}","base64"],"executable":false}}"#,
            self.lamports,
            saddr::to_text(&self.owner),
            base64::encode(&self.data)
        )
    }
}

fn system_account(lamports: u64) -> Acct {
    Acct {
        lamports,
        owner: system_program(),
        data: vec![],
    }
}

fn token_acct(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Acct {
    token_acct_under(&token_program(), mint, owner, amount)
}

fn token_acct_under(program: &Pubkey, mint: &Pubkey, owner: &Pubkey, amount: u64) -> Acct {
    Acct {
        lamports: 2_039_280,
        owner: *program,
        data: token_account_bytes(mint, owner, amount, None),
    }
}

fn mint_acct(decimals: u8, program: &Pubkey) -> Acct {
    Acct {
        lamports: 1_461_600,
        owner: *program,
        data: mint_bytes(decimals),
    }
}

#[derive(Default)]
struct World {
    accounts: HashMap<Pubkey, Acct>,
    /// The state the simulation reports afterwards (accounts not listed stay as they are).
    post: HashMap<Pubkey, Acct>,
    sim_error: Option<String>,
    blockhash_valid: Option<bool>,
    /// Every transaction the wallet submitted, as wire bytes.
    sent: Vec<Vec<u8>>,
    /// What `getSignatureStatuses` answers; `None` means "not found".
    status: Option<String>,
    priority_fee: u64,
}

type Shared = Rc<RefCell<World>>;

const CONFIRMED: &str =
    r#"{"slot":9,"confirmations":null,"err":null,"confirmationStatus":"confirmed"}"#;

fn world() -> Shared {
    let w = Rc::new(RefCell::new(World {
        blockhash_valid: Some(true),
        status: Some(CONFIRMED.to_string()),
        priority_fee: 1_000,
        ..World::default()
    }));
    w.borrow_mut()
        .accounts
        .insert(owner(), system_account(5_000_000_000));
    w
}

#[derive(Clone, Copy, PartialEq)]
enum SendMode {
    Accept,
    Reject,
    Lost,
}

fn addresses_in(list: &Value) -> Vec<Pubkey> {
    list.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|a| a.as_str().and_then(|t| saddr::parse(t).ok()))
                .collect()
        })
        .unwrap_or_default()
}

fn token_entry(address: &Pubkey, acct: &Acct, decimals: u8) -> String {
    let data = crate::solana::tokens::parse_token_account(&acct.data).unwrap();
    format!(
        r#"{{"pubkey":"{}","account":{{"owner":"{}","data":{{"parsed":{{"info":{{"mint":"{}","owner":"{}","tokenAmount":{{"amount":"{}","decimals":{decimals}}}}}}}}}}}}}"#,
        saddr::to_text(address),
        saddr::to_text(&acct.owner),
        saddr::to_text(&data.mint),
        saddr::to_text(&data.owner),
        data.amount
    )
}

/// A node serving `w`. Reads come from `w.accounts`; simulations report `w.post` on top of it.
fn node(w: &Shared, send: SendMode, genesis: &'static str) -> SolMock {
    let (w1, w2, w3, w4, w5, w6, w7, w8, w9) = (
        w.clone(),
        w.clone(),
        w.clone(),
        w.clone(),
        w.clone(),
        w.clone(),
        w.clone(),
        w.clone(),
        w.clone(),
    );
    let mock = SolMock::new()
        .on("getGenesisHash", &format!("\"{genesis}\""))
        .on_with("getBalance", move |p| {
            let key = saddr::parse(p.as_array().unwrap()[0].as_str().unwrap()).unwrap();
            let lamports = w1.borrow().accounts.get(&key).map_or(0, |a| a.lamports);
            format!(r#"{{"context":{{"slot":1}},"value":{lamports}}}"#)
        })
        .on(
            "getLatestBlockhash",
            &format!(
                r#"{{"value":{{"blockhash":"{}","lastValidBlockHeight":500}}}}"#,
                base58::encode(&BLOCKHASH)
            ),
        )
        .on_with("isBlockhashValid", move |_| match w2.borrow().blockhash_valid {
            Some(v) => format!(r#"{{"value":{v}}}"#),
            None => "{}".to_string(),
        })
        .on_with("getAccountInfo", move |p| {
            let key = saddr::parse(p.as_array().unwrap()[0].as_str().unwrap()).unwrap();
            match w3.borrow().accounts.get(&key) {
                Some(a) => format!(r#"{{"value":{}}}"#, a.json()),
                None => r#"{"value":null}"#.to_string(),
            }
        })
        .on_with("getMultipleAccounts", move |p| {
            let list = addresses_in(&p.as_array().unwrap()[0]);
            let w = w4.borrow();
            let items: Vec<String> = list
                .iter()
                .map(|k| w.accounts.get(k).map_or("null".to_string(), Acct::json))
                .collect();
            format!(r#"{{"value":[{}]}}"#, items.join(","))
        })
        .on_with("getTokenAccountsByOwner", move |p| {
            let args = p.as_array().unwrap();
            let owner = saddr::parse(args[0].as_str().unwrap()).unwrap();
            let filter = &args[1];
            let w = w5.borrow();
            let parsed = |a: &Acct| crate::solana::tokens::parse_token_account(&a.data);
            let mut found: Vec<(&Pubkey, &Acct)> = w
                .accounts
                .iter()
                .filter(|(_, a)| parsed(a).is_some_and(|t| t.owner == owner))
                .filter(|(_, a)| match filter.get("programId").and_then(Value::as_str) {
                    Some(program) => saddr::to_text(&a.owner) == program,
                    None => true,
                })
                .filter(|(_, a)| match filter.get("mint").and_then(Value::as_str) {
                    Some(mint) => parsed(a).is_some_and(|t| saddr::to_text(&t.mint) == mint),
                    None => true,
                })
                .collect();
            found.sort_by_key(|(k, _)| **k);
            let items: Vec<String> = found
                .iter()
                .map(|(k, a)| {
                    let mint = parsed(a).unwrap().mint;
                    let decimals = w.accounts.get(&mint).map_or(6, |m| m.data[44]);
                    token_entry(k, a, decimals)
                })
                .collect();
            format!(r#"{{"value":[{}]}}"#, items.join(","))
        })
        .on_with("simulateTransaction", move |p| {
            let w = w6.borrow();
            if let Some(error) = &w.sim_error {
                return format!(
                    r#"{{"value":{{"err":{error},"logs":["Program log: boom"],"accounts":null}}}}"#
                );
            }
            let options = &p.as_array().unwrap()[1];
            let list = options
                .get("accounts")
                .and_then(|a| a.get("addresses"))
                .map(addresses_in)
                .unwrap_or_default();
            let items: Vec<String> = list
                .iter()
                .map(|k| {
                    w.post
                        .get(k)
                        .or_else(|| w.accounts.get(k))
                        .map_or("null".to_string(), Acct::json)
                })
                .collect();
            format!(
                r#"{{"value":{{"err":null,"logs":["Program log: ok"],"unitsConsumed":150,"accounts":[{}]}}}}"#,
                items.join(",")
            )
        })
        .on_with("getSignatureStatuses", move |_| match &w7.borrow().status {
            Some(status) => format!(r#"{{"value":[{status}]}}"#),
            None => r#"{"value":[null]}"#.to_string(),
        })
        .on_with("getRecentPrioritizationFees", move |_| {
            format!(
                r#"[{{"slot":1,"prioritizationFee":{}}}]"#,
                w8.borrow().priority_fee
            )
        })
        .get("ids=solana", Ok(r#"{"solana":{"usd":150.0}}"#.to_string()));
    match send {
        SendMode::Accept => mock.on_with("sendTransaction", move |p| {
            let encoded = p.as_array().unwrap()[0].as_str().unwrap().to_string();
            let bytes = base64::decode(&encoded).unwrap();
            let transaction = Transaction::from_bytes(&bytes).unwrap();
            w9.borrow_mut().sent.push(bytes);
            format!("\"{}\"", transaction.id().unwrap_or_default())
        }),
        SendMode::Reject => mock.on_error(
            "sendTransaction",
            -32002,
            "Transaction simulation failed: Blockhash not found",
        ),
        SendMode::Lost => mock.on_net_error("sendTransaction", "connection reset"),
    }
}

struct Fx {
    _dir: TempDir,
    mock: Rc<SolMock>,
    world: Shared,
    service: WalletService,
}

fn build_with(
    w: Shared,
    send: SendMode,
    genesis: &'static str,
    configure: impl FnOnce(WalletService) -> WalletService,
    extend: impl FnOnce(SolMock) -> SolMock,
) -> Fx {
    let dir = TempDir::new();
    let mock = Rc::new(extend(node(&w, send, genesis)));
    let http: Rc<dyn HttpClient> = mock.clone();
    let service = configure(
        WalletService::new(Store::open(&dir.0).unwrap(), http)
            .with_iterations(vault::MIN_ITERATIONS)
            .with_receipt_wait(Duration::ZERO, 3),
    );
    Fx {
        _dir: dir,
        mock,
        world: w,
        service,
    }
}

fn build(w: Shared) -> Fx {
    build_with(w, SendMode::Accept, MAINNET_GENESIS, |s| s, |m| m)
}

fn import(f: &Fx) {
    f.service
        .import_wallet("solana", "w", PW, None, Some(&key_hex()))
        .unwrap();
}

/// A fixture whose wallet "w" holds 5 SOL.
fn funded() -> Fx {
    let f = build(world());
    import(&f);
    f
}

/// Adds the USDC mint and the wallet's USDC account (`amount`); returns that account's address.
fn give_usdc(f: &Fx, amount: u64) -> Pubkey {
    let account = saddr::associated_token_address(&owner(), &usdc(), &token_program()).unwrap();
    let mut w = f.world.borrow_mut();
    w.accounts.insert(usdc(), mint_acct(6, &token_program()));
    w.accounts
        .insert(account, token_acct(&usdc(), &owner(), amount));
    account
}

fn field<'a>(v: &'a Value, path: &[&str]) -> &'a Value {
    let mut cur = v;
    for key in path {
        cur = cur
            .get(key)
            .unwrap_or_else(|| panic!("no '{key}' in {}", v.to_json()));
    }
    cur
}

fn text<'a>(v: &'a Value, path: &[&str]) -> &'a str {
    match field(v, path) {
        Value::String(s) | Value::Number(s) => s,
        other => panic!("not text: {}", other.to_json()),
    }
}

fn items<'a>(v: &'a Value, path: &[&str]) -> &'a Vec<Value> {
    field(v, path).as_array().unwrap()
}

/// The transaction the wallet submitted last.
fn submitted(f: &Fx) -> Transaction {
    let sent = f.world.borrow().sent.clone();
    Transaction::from_bytes(sent.last().expect("nothing was submitted")).unwrap()
}

fn assert_signed_by_wallet(transaction: &Transaction) {
    assert!(ed25519::verify(
        &owner(),
        transaction.message.raw(),
        &transaction.signatures[0]
    ));
}

fn ops_of(transaction: &Transaction) -> Vec<Op> {
    crate::solana::describe::classify(&transaction.message, &transaction.message.account_keys)
}

fn call(tb: &Toolbox, name: &str, args: &str) -> (bool, Value) {
    let args = json::parse(args).unwrap();
    let result = tb.call(name, Some(&args)).expect("tool exists");
    let is_error = result.get("isError") == Some(&Value::Bool(true));
    let body = result.get("content").and_then(Value::as_array).unwrap()[0]
        .get("text")
        .and_then(Value::as_str)
        .unwrap()
        .to_string();
    (is_error, json::parse(&body).unwrap_or(Value::String(body)))
}

fn transfer_tx(lamports: u64) -> Transaction {
    let ix = tx::system_transfer(&owner(), &key(2), lamports);
    Transaction::unsigned(tx::compile_legacy(&owner(), &[ix], BLOCKHASH).unwrap())
}

// ---- wallet lifecycle -----------------------------------------------------------------------

#[test]
fn create_returns_the_address_and_network_but_no_secrets() {
    let f = build(world());
    let v = f
        .service
        .create_wallet_on("solana", "Main", PW, None)
        .unwrap();
    assert_eq!(text(&v, &["blockchain"]), "Solana");
    assert_eq!(text(&v, &["network"]), "mainnet");
    assert_eq!(text(&v, &["status"]), "created");
    let address = text(&v, &["address"]);
    assert!(saddr::parse(address).is_ok());
    let out = v.to_json();
    assert!(!out.contains("seed") && !out.contains("private"));
    assert_eq!(
        text(&f.service.get_address("main", PW).unwrap(), &["address"]),
        address
    );
    // creating a wallet and reading its address needs no network access at all
    assert!(f.mock.methods().is_empty());
}

#[test]
fn wallets_can_be_bound_to_devnet_or_testnet_and_networks_are_validated() {
    let f = build(world());
    for (name, network) in [("d", "devnet"), ("t", "testnet"), ("m", "mainnet-beta")] {
        let v = f
            .service
            .create_wallet_on("solana", name, PW, Some(network))
            .unwrap();
        let expected = if network == "mainnet-beta" {
            "mainnet"
        } else {
            network
        };
        assert_eq!(text(&v, &["network"]), expected);
    }
    for bad in ["localnet", "solana:unknown", ""] {
        let err = f
            .service
            .create_wallet_on("solana", "x", PW, Some(bad))
            .unwrap_err();
        assert!(err.contains("unsupported Solana"), "{bad}: {err}");
    }
    let err = f
        .service
        .create_wallet_on("ethereum", "E", PW, Some("devnet"))
        .unwrap_err();
    assert!(err.contains("Sui and Solana wallets only"));
    assert!(f.service.create_wallet_on("sol", "alias", PW, None).is_ok());
}

#[test]
fn imports_the_known_seed_phrase_and_every_private_key_format() {
    let f = build(world());
    let v = f
        .service
        .import_wallet("solana", "Seed", PW, Some(ABANDON_ART), None)
        .unwrap();
    assert_eq!(text(&v, &["address"]), ABANDON_ART_ADDRESS);
    assert_eq!(text(&v, &["status"]), "imported");
    // the same key written as Phantom shows it
    let key = vault().keypair_base58().unwrap();
    let v = f
        .service
        .import_wallet("solana", "Phantom", PW, None, Some(key.expose()))
        .unwrap();
    assert_eq!(text(&v, &["address"]), owner_text());
    let v = f
        .service
        .import_wallet("solana", "Hex", PW, None, Some(&key_hex()))
        .unwrap();
    assert_eq!(text(&v, &["address"]), owner_text());
    for (secret, as_seed) in [("not a key", false), ("short words here", true)] {
        let err = f
            .service
            .import_wallet(
                "solana",
                "Bad",
                PW,
                as_seed.then_some(secret),
                (!as_seed).then_some(secret),
            )
            .unwrap_err();
        assert!(!err.contains(secret), "errors must not echo secrets");
    }
    assert!(
        f.service
            .import_wallet("solana", "Both", PW, Some(ABANDON_ART), Some(&key_hex()))
            .is_err()
    );
}

#[test]
fn list_wallets_shows_solana_without_addresses() {
    let f = funded();
    let list = f.service.list_wallets().unwrap();
    let wallet = &items(&list, &["wallets"])[0];
    assert_eq!(text(wallet, &["blockchain"]), "Solana");
    assert!(!list.to_json().contains(&owner_text()));
}

#[test]
fn secrets_are_shown_only_by_the_dedicated_tools() {
    let f = funded();
    let m = f
        .service
        .create_wallet_on("solana", "HD", PW, None)
        .unwrap();
    let hd_address = text(&m, &["address"]).to_string();
    let phrase = f.service.view_seed_phrase("hd", PW).unwrap();
    assert_eq!(
        text(&phrase, &["seed_phrase"]).split_whitespace().count(),
        24
    );
    assert_eq!(text(&phrase, &["derivation_path"]), "m/44'/501'/0'/0'");
    let key = f.service.view_private_key("hd", PW).unwrap();
    let keypair = text(&key, &["private_key"]);
    assert_eq!(base58::decode(keypair).unwrap().len(), 64);
    assert_eq!(text(&key, &["address"]), hd_address);
    assert_eq!(text(&key, &["private_key_hex"]).len(), 66);
    // an imported private key has no seed phrase
    let err = f.service.view_seed_phrase("w", PW).unwrap_err();
    assert!(err.contains("no seed phrase"));
    // the key shown can be imported again and gives the same address
    let again = f
        .service
        .import_wallet("solana", "Again", PW, None, Some(keypair))
        .unwrap();
    assert_eq!(text(&again, &["address"]), hd_address);
}

#[test]
fn export_writes_an_encrypted_zip_and_never_the_plaintext() {
    let f = funded();
    let v = f
        .service
        .export_wallet("w", PW, "export-encryption-key", Some("sol.zip"))
        .unwrap();
    assert_eq!(text(&v, &["contains"]), "private key");
    let bytes = std::fs::read(text(&v, &["file"])).unwrap();
    let as_text = String::from_utf8_lossy(&bytes);
    assert!(!as_text.contains(&owner_text()) && !as_text.contains(&key_hex()));
    assert!(
        f.service
            .export_wallet("w", PW, "export-encryption-key", Some("sol.zip"))
            .is_err(),
        "existing files are never overwritten"
    );
    f.service
        .create_wallet_on("solana", "hd", PW, None)
        .unwrap();
    let v = f
        .service
        .export_wallet("hd", PW, "export-encryption-key", Some("hd.zip"))
        .unwrap();
    assert_eq!(text(&v, &["contains"]), "seed phrase and private key");
}

#[test]
fn rename_and_delete_need_the_password() {
    let f = funded();
    assert!(
        f.service
            .rename_wallet("w", "w2", "wrong-password")
            .is_err()
    );
    f.service.rename_wallet("w", "w2", PW).unwrap();
    assert!(f.service.delete_wallet("w2", PW, false).is_err());
    assert!(
        f.service
            .delete_wallet("w2", "wrong-password", true)
            .is_err()
    );
    f.service.delete_wallet("w2", PW, true).unwrap();
    assert!(items(&f.service.list_wallets().unwrap(), &["wallets"]).is_empty());
}

#[test]
fn wrong_passwords_are_rejected_and_rate_limited() {
    let f = funded();
    for _ in 0..MAX_FAILED_ATTEMPTS {
        assert_eq!(
            f.service.get_address("w", "not-the-password").unwrap_err(),
            "incorrect password"
        );
    }
    let err = f.service.get_address("w", PW).unwrap_err();
    assert!(err.contains("too many failed password attempts"), "{err}");
}

#[test]
fn blockchains_cannot_be_mixed() {
    let f = funded();
    f.service.create_wallet("ethereum", "eth", PW).unwrap();
    // an Ethereum-only tool refuses a Solana wallet before decrypting anything
    let err = f
        .service
        .read_contract("w", PW, 1, "0x00", "f()", &[], None)
        .unwrap_err();
    assert!(err.contains("Solana wallet"), "{err}");
    // and the Solana tools refuse an Ethereum wallet
    let err = f.service.solana_balances("eth", PW).unwrap_err();
    assert!(err.contains("Ethereum wallet"), "{err}");
    assert!(f.mock.methods().is_empty());
}

#[test]
fn networks_show_the_cluster_and_hide_provider_credentials() {
    let f = funded();
    let v = f.service.view_networks("w", PW).unwrap();
    let network = &items(&v, &["networks"])[0];
    assert_eq!(text(network, &["name"]), "mainnet");
    assert_eq!(text(network, &["cluster"]), "mainnet-beta");
    assert_eq!(
        text(network, &["chain"]),
        "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
    );
    assert_eq!(text(network, &["rpc_host"]), "api.mainnet-beta.solana.com");
    assert!(
        !v.to_json().contains("https://api."),
        "the RPC URL is never shown"
    );
    assert!(text(&v, &["note"]).contains("no layer 2"));
}

#[test]
fn the_node_must_serve_the_wallets_cluster() {
    let f = build_with(world(), SendMode::Accept, DEVNET_GENESIS, |s| s, |m| m);
    import(&f);
    let err = f.service.solana_balances("w", PW).unwrap_err();
    assert!(err.contains("does not serve Solana mainnet-beta"), "{err}");
    assert_eq!(f.mock.methods(), ["getGenesisHash"]);
}

#[test]
fn every_operation_leaves_the_vault_locked_on_disk() {
    let f = funded();
    f.service.get_address("w", PW).unwrap();
    f.service.solana_balances("w", PW).unwrap();
    f.service.view_private_key("w", PW).unwrap();
    for entry in std::fs::read_dir(f._dir.0.join("wallets")).unwrap() {
        let content = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(
            !content.contains(&owner_text()),
            "the address must stay encrypted"
        );
        assert!(!content.contains(&key_hex()));
        assert!(
            !content.contains("solanaNetwork"),
            "the cluster is only inside the vault"
        );
        assert!(!content.contains("devnet") && !content.contains("mainnet"));
    }
}

// ---- balances -------------------------------------------------------------------------------

#[test]
fn balances_list_sol_and_tokens_with_values() {
    let f = funded();
    give_usdc(&f, 2_500_000);
    // a second USDC account and an unrecognised token
    {
        let mut w = f.world.borrow_mut();
        w.accounts
            .insert(key(60), token_acct(&usdc(), &owner(), 500_000));
        w.accounts.insert(key(70), mint_acct(9, &token_program()));
        w.accounts
            .insert(key(61), token_acct(&key(70), &owner(), 7_000_000_000));
        w.accounts
            .insert(key(62), token_acct(&key(71), &owner(), 0));
        w.accounts
            .insert(key(63), token_acct(&usdc(), &key(99), 123));
    }
    let v = f.service.solana_balances("w", PW).unwrap();
    assert_eq!(text(&v, &["address"]), owner_text());
    assert_eq!(text(&v, &["network", "name"]), "mainnet");
    let assets = items(&v, &["assets"]);
    assert_eq!(assets.len(), 3, "SOL, USDC and the unknown token");
    let sol = &assets[0];
    assert_eq!(text(sol, &["symbol"]), "SOL");
    assert_eq!(text(sol, &["asset"]), "native");
    assert_eq!(text(sol, &["balance"]), "5");
    assert_eq!(text(sol, &["usdc_value"]), "750.00");
    let usdc_row = assets
        .iter()
        .find(|a| text(a, &["symbol"]) == "USDC")
        .unwrap();
    assert_eq!(text(usdc_row, &["balance"]), "3");
    assert_eq!(text(usdc_row, &["balance_raw"]), "3000000");
    assert_eq!(text(usdc_row, &["token_accounts"]), "2");
    assert_eq!(text(usdc_row, &["usdc_value"]), "3.00");
    let unknown = assets
        .iter()
        .find(|a| text(a, &["asset"]) == key_text(70))
        .unwrap();
    assert_eq!(text(unknown, &["balance"]), "7");
    assert_eq!(unknown.get("usdc_value"), Some(&Value::Null));
    assert_eq!(unknown.get("recognised"), Some(&Value::Bool(false)));
    assert!(!items(&v, &["warnings"]).is_empty());
    assert!(!v.to_json().contains(&key_text(99)));
}

#[test]
fn balances_have_no_values_on_devnet() {
    let f = build_with(world(), SendMode::Accept, DEVNET_GENESIS, |s| s, |m| m);
    f.service
        .create_wallet_on("solana", "dev", PW, Some("devnet"))
        .unwrap();
    let address = saddr::parse(text(
        &f.service.get_address("dev", PW).unwrap(),
        &["address"],
    ))
    .unwrap();
    f.world
        .borrow_mut()
        .accounts
        .insert(address, system_account(2_000_000_000));
    let v = f.service.solana_balances("dev", PW).unwrap();
    let sol = &items(&v, &["assets"])[0];
    assert_eq!(text(sol, &["balance"]), "2");
    assert_eq!(sol.get("usdc_value"), Some(&Value::Null));
    assert!(
        f.mock.get_urls.borrow().is_empty(),
        "no price lookups off mainnet"
    );
}

#[test]
fn an_empty_wallet_still_lists_sol() {
    let w = world();
    w.borrow_mut().accounts.clear();
    let f = build(w);
    import(&f);
    let v = f.service.solana_balances("w", PW).unwrap();
    let assets = items(&v, &["assets"]);
    assert_eq!(assets.len(), 1);
    assert_eq!(text(&assets[0], &["balance"]), "0");
}

// ---- sending --------------------------------------------------------------------------------

fn pay(f: &Fx, lamports: u64, fee: u64) {
    let mut w = f.world.borrow_mut();
    let balance = w.accounts[&owner()].lamports;
    w.post
        .insert(owner(), system_account(balance - lamports - fee));
    w.post.insert(key(2), system_account(lamports));
}

#[test]
fn sending_sol_simulates_signs_and_submits() {
    let f = funded();
    pay(&f, 1_000_000_000, 5_002);
    let v = f
        .service
        .solana_send_asset("w", PW, "native", &key_text(2), "1")
        .unwrap();
    let t = submitted(&f);
    assert_signed_by_wallet(&t);
    assert_eq!(t.message.fee_payer(), &owner());
    assert_eq!(t.message.recent_blockhash, BLOCKHASH);
    let ops = ops_of(&t);
    assert_eq!(ops.len(), 3);
    assert_eq!(ops[0], Op::ComputeLimit(1_480));
    assert_eq!(ops[1], Op::ComputePrice(1_000));
    assert_eq!(
        ops[2],
        Op::SolTransfer {
            from: owner(),
            to: key(2),
            lamports: 1_000_000_000
        }
    );
    assert_eq!(text(&v, &["status"]), "confirmed");
    assert_eq!(text(&v, &["asset"]), "SOL");
    assert_eq!(text(&v, &["amount"]), "1");
    assert_eq!(text(&v, &["from"]), owner_text());
    assert_eq!(text(&v, &["to"]), key_text(2));
    assert_eq!(text(&v, &["network_fee_sol"]), "0.000005002");
    assert_eq!(text(&v, &["transaction_signature"]), t.id().unwrap());
    assert!(text(&v, &["explorer_url"]).ends_with(&t.id().unwrap()));
    // simulated and checked before it was sent
    let methods = f.mock.methods();
    let sim = methods
        .iter()
        .position(|m| m == "simulateTransaction")
        .unwrap();
    let send = methods.iter().position(|m| m == "sendTransaction").unwrap();
    assert!(sim < send);
    assert_eq!(f.mock.count("sendTransaction"), 1);
}

#[test]
fn sending_validates_before_touching_the_wallet_or_network_state() {
    let f = funded();
    for (asset, to, amount, expect) in [
        ("native", saddr::SYSTEM_PROGRAM, "1", "program"),
        ("native", saddr::TOKEN_PROGRAM, "1", "program"),
        ("native", "not-an-address", "1", "'to'"),
        ("native", owner_text().as_str(), "1", "own address"),
        ("native", key_text(2).as_str(), "0", "greater than zero"),
        ("native", key_text(2).as_str(), "-1", "'amount'"),
        ("native", key_text(2).as_str(), "abc", "'amount'"),
        ("native", key_text(2).as_str(), "1.0000000001", "'amount'"),
        ("0x2::sui::SUI", key_text(2).as_str(), "1", "'asset'"),
    ] {
        let err = f
            .service
            .solana_send_asset("w", PW, asset, to, amount)
            .unwrap_err();
        assert!(err.contains(expect), "{asset} {to} {amount}: {err}");
    }
    assert_eq!(f.mock.count("sendTransaction"), 0);
    assert_eq!(f.mock.count("simulateTransaction"), 0);
}

#[test]
fn sending_more_than_the_wallet_holds_is_refused_before_signing() {
    let f = funded();
    let err = f
        .service
        .solana_send_asset("w", PW, "native", &key_text(2), "10")
        .unwrap_err();
    assert!(err.contains("insufficient SOL"), "{err}");
    // sending everything leaves nothing for the fee
    let err = f
        .service
        .solana_send_asset("w", PW, "native", &key_text(2), "5")
        .unwrap_err();
    assert!(err.contains("insufficient SOL"), "{err}");
    assert_eq!(f.mock.count("sendTransaction"), 0);
}

#[test]
fn a_transaction_that_would_fail_is_never_signed_or_sent() {
    let f = funded();
    f.world.borrow_mut().sim_error = Some(r#"{"InstructionError":[0,{"Custom":1}]}"#.to_string());
    let err = f
        .service
        .solana_send_asset("w", PW, "native", &key_text(2), "1")
        .unwrap_err();
    assert!(err.contains("would fail"), "{err}");
    assert_eq!(f.mock.count("sendTransaction"), 0);
    assert!(f.world.borrow().sent.is_empty());
}

#[test]
fn a_rejected_submission_says_nothing_was_spent() {
    let w = world();
    w.borrow_mut().status = None;
    let f = build_with(w, SendMode::Reject, MAINNET_GENESIS, |s| s, |m| m);
    import(&f);
    pay(&f, 1_000_000_000, 5_002);
    let err = f
        .service
        .solana_send_asset("w", PW, "native", &key_text(2), "1")
        .unwrap_err();
    assert!(
        err.contains("rejected") && err.contains("nothing was spent"),
        "{err}"
    );
}

#[test]
fn a_lost_answer_is_resolved_by_looking_the_signature_up() {
    let f = build_with(world(), SendMode::Lost, MAINNET_GENESIS, |s| s, |m| m);
    import(&f);
    pay(&f, 1_000_000_000, 5_002);
    // the transaction did land: reported as submitted
    let v = f
        .service
        .solana_send_asset("w", PW, "native", &key_text(2), "1")
        .unwrap();
    assert_eq!(text(&v, &["status"]), "confirmed");

    // it did not: the outcome is unknown and the agent is told how to find out
    let w = world();
    w.borrow_mut().status = None;
    let f = build_with(w, SendMode::Lost, MAINNET_GENESIS, |s| s, |m| m);
    import(&f);
    pay(&f, 1_000_000_000, 5_002);
    let err = f
        .service
        .solana_send_asset("w", PW, "native", &key_text(2), "1")
        .unwrap_err();
    assert!(
        err.contains("outcome is unknown") && err.contains("get_transaction"),
        "{err}"
    );
    assert!(err.contains("transaction_hash"), "{err}");
}

#[test]
fn unconfirmed_and_failed_submissions_are_reported_honestly() {
    let f = funded();
    pay(&f, 1_000_000_000, 5_002);
    f.world.borrow_mut().status = None;
    let v = f
        .service
        .solana_send_asset("w", PW, "native", &key_text(2), "1")
        .unwrap();
    assert_eq!(text(&v, &["status"]), "submitted_not_yet_confirmed");
    assert!(text(&v, &["note"]).contains("get_transaction"));

    let f = funded();
    pay(&f, 1_000_000_000, 5_002);
    f.world.borrow_mut().status = Some(
        r#"{"slot":9,"confirmations":null,"err":{"InstructionError":[0,"Custom"]},"confirmationStatus":"confirmed"}"#
            .to_string(),
    );
    let v = f
        .service
        .solana_send_asset("w", PW, "native", &key_text(2), "1")
        .unwrap();
    assert_eq!(text(&v, &["status"]), "failed");
    assert!(v.get("error").is_some());
}

fn token_send_world(f: &Fx, source_amount: u64) -> Pubkey {
    let source = give_usdc(f, source_amount);
    f.world.borrow_mut().post.insert(
        source,
        token_acct(&usdc(), &owner(), source_amount.saturating_sub(1_500_000)),
    );
    source
}

#[test]
fn sending_a_token_opens_the_recipients_account_when_needed() {
    let f = funded();
    let source = token_send_world(&f, 5_000_000);
    let recipient_account =
        saddr::associated_token_address(&key(2), &usdc(), &token_program()).unwrap();
    let v = f
        .service
        .solana_send_asset("w", PW, USDC, &key_text(2), "1.5")
        .unwrap();
    let t = submitted(&f);
    assert_signed_by_wallet(&t);
    let ops = ops_of(&t);
    assert_eq!(ops.len(), 4, "{ops:?}");
    assert!(matches!(
        &ops[2],
        Op::CreateAssociatedToken { payer, associated, owner: o, mint, idempotent: true }
            if *payer == owner() && *associated == recipient_account && *o == key(2) && *mint == usdc()
    ));
    assert_eq!(
        ops[3],
        Op::TokenTransfer {
            source,
            destination: recipient_account,
            authority: owner(),
            amount: 1_500_000,
            mint: Some(usdc()),
            decimals: Some(6),
        }
    );
    assert_eq!(text(&v, &["asset"]), "USDC");
    assert_eq!(text(&v, &["amount"]), "1.5");
    assert_eq!(text(&v, &["asset_id"]), USDC);
    assert!(text(&v, &["note"]).contains("opened for them"));
}

#[test]
fn sending_a_token_to_an_existing_account_needs_no_creation() {
    let f = funded();
    token_send_world(&f, 5_000_000);
    let recipient_account =
        saddr::associated_token_address(&key(2), &usdc(), &token_program()).unwrap();
    f.world
        .borrow_mut()
        .accounts
        .insert(recipient_account, token_acct(&usdc(), &key(2), 10));
    let v = f
        .service
        .solana_send_asset("w", PW, USDC, &key_text(2), "1.5")
        .unwrap();
    assert_eq!(ops_of(&submitted(&f)).len(), 3);
    assert!(v.get("note").is_none());
}

#[test]
fn token_sends_use_the_mints_own_decimals_and_token_program() {
    let f = funded();
    // a Token-2022 mint with 9 decimals
    let mint = key(80);
    let source = key(81);
    {
        let mut w = f.world.borrow_mut();
        w.accounts.insert(mint, mint_acct(9, &token_2022_program()));
        w.accounts.insert(
            source,
            token_acct_under(&token_2022_program(), &mint, &owner(), 3_000_000_000),
        );
    }
    f.service
        .solana_send_asset("w", PW, &saddr::to_text(&mint), &key_text(2), "2")
        .unwrap();
    let t = submitted(&f);
    let ops = ops_of(&t);
    let Op::TokenTransfer {
        amount,
        decimals,
        source: s,
        destination,
        ..
    } = ops.last().unwrap()
    else {
        panic!("{ops:?}")
    };
    assert_eq!((*amount, *decimals, *s), (2_000_000_000, Some(9), source));
    assert_eq!(
        *destination,
        saddr::associated_token_address(&key(2), &mint, &token_2022_program()).unwrap()
    );
    let transfer = t.message.instructions.last().unwrap();
    assert_eq!(
        t.message.account_keys[usize::from(transfer.program_id_index)],
        token_2022_program()
    );
}

#[test]
fn token_sends_are_refused_when_the_token_or_balance_is_wrong() {
    let f = funded();
    give_usdc(&f, 1_000_000);
    for (asset, expect) in [
        (USDC, "insufficient USDC"),
        (key_text(99).as_str(), "does not exist"),
        (owner_text().as_str(), "not a token mint"),
        ("nonsense", "'asset'"),
    ] {
        let amount = if asset == USDC { "5" } else { "1" };
        let err = f
            .service
            .solana_send_asset("w", PW, asset, &key_text(2), amount)
            .unwrap_err();
        assert!(err.contains(expect), "{asset}: {err}");
    }
    // balance split over two accounts: neither holds enough alone
    f.world
        .borrow_mut()
        .accounts
        .insert(key(60), token_acct(&usdc(), &owner(), 1_000_000));
    let err = f
        .service
        .solana_send_asset("w", PW, USDC, &key_text(2), "1.5")
        .unwrap_err();
    assert!(err.contains("split over several token accounts"), "{err}");
    assert_eq!(f.mock.count("sendTransaction"), 0);
}

#[test]
fn a_recipients_squatted_token_account_address_is_refused() {
    let f = funded();
    token_send_world(&f, 5_000_000);
    let recipient_account =
        saddr::associated_token_address(&key(2), &usdc(), &token_program()).unwrap();
    f.world
        .borrow_mut()
        .accounts
        .insert(recipient_account, system_account(1));
    let err = f
        .service
        .solana_send_asset("w", PW, USDC, &key_text(2), "1")
        .unwrap_err();
    assert!(err.contains("not a token account"), "{err}");
}

#[test]
fn passwords_never_appear_in_any_output_or_error() {
    let f = funded();
    let secret = "unique-secret-password-123";
    f.service
        .create_wallet_on("solana", "pw", secret, None)
        .unwrap();
    let mut outputs = vec![];
    outputs.push(format!("{:?}", f.service.get_address("pw", secret)));
    outputs.push(format!(
        "{:?}",
        f.service.get_address("pw", "x-wrong-password")
    ));
    outputs.push(format!("{:?}", f.service.solana_balances("pw", secret)));
    outputs.push(format!(
        "{:?}",
        f.service
            .solana_send_asset("pw", secret, "native", &key_text(2), "1")
    ));
    for out in outputs {
        assert!(!out.contains(secret) && !out.contains("x-wrong-password"));
    }
}
