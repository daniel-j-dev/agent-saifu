//! Tests for Bitcoin wallets through the service and the MCP tools, against a scripted
//! Esplora server.

use std::rc::Rc;

use super::*;
use crate::bitcoin::address::Address;
use crate::bitcoin::network::BitcoinNetwork;
use crate::bitcoin::testing::EsploraMock;
use crate::bitcoin::tx::Transaction;
use crate::crypto::hex;
use crate::crypto::ripemd160::hash160;
use crate::json;
use crate::store::testutil::TempDir;
use crate::tools::Toolbox;

const PW: &str = "correct-horse-battery";
const MAINNET_GENESIS: &str = "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f";
const TESTNET_GENESIS: &str = "00000000da84f2bafbbc53dee25a72ae507ff4914b867c565be350b0da8bf043";
const SIGNET_GENESIS: &str = "00000008819873e925422c1ff0f99f7cc9bbb232af63a077a480a3633bee1ef6";
const ABANDON_ART: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";
const ABANDON_ART_ADDRESS: &str = "bc1qzmtrqsfuaf6l6kkcsseumq26ukaphfj9skkug6";
const RECIPIENT: &str = "bc1q9vza2e8x573nczrlzms0wvx3gsqjx7vavgkx0l";
const TIP: u64 = 900_000;
const FEES: &str = r#"{"1":20,"3":10,"6":5,"144":1,"1008":0.5}"#;

fn key_hex() -> String {
    "11".repeat(32)
}

fn vault() -> BitcoinVault {
    BitcoinVault::from_private_key(&key_hex(), BitcoinNetwork::Mainnet).unwrap()
}

fn wallet_address() -> String {
    vault().address().unwrap()
}

fn wallet_script() -> Vec<u8> {
    vault().address_kind().unwrap().script_pubkey()
}

fn recipient_script() -> Vec<u8> {
    Address::parse(RECIPIENT, BitcoinNetwork::Mainnet)
        .unwrap()
        .script_pubkey()
}

fn other_txid(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

// ---- the simulated server --------------------------------------------------------------------

#[derive(Clone)]
struct Coin {
    txid: String,
    vout: u32,
    value: u64,
    confirmed: bool,
}

fn coin(n: u8, value: u64) -> Coin {
    Coin {
        txid: other_txid(n),
        vout: 0,
        value,
        confirmed: true,
    }
}

fn pending_coin(n: u8, value: u64) -> Coin {
    Coin {
        confirmed: false,
        ..coin(n, value)
    }
}

#[derive(Clone, Copy)]
enum Post {
    Accept,
    Reject,
    Lost,
    WrongId,
    AlreadyKnown,
}

fn txid_of_raw(raw_hex: &str) -> String {
    Transaction::deserialize(&hex::decode(raw_hex).unwrap())
        .unwrap()
        .txid_hex()
}

struct Node {
    coins: Vec<Coin>,
    genesis: &'static str,
    address: String,
    fees: String,
    post: Post,
    extra: Vec<(String, Result<String, String>)>,
}

impl Node {
    fn new(coins: Vec<Coin>) -> Node {
        Node {
            coins,
            genesis: MAINNET_GENESIS,
            address: wallet_address(),
            fees: FEES.to_string(),
            post: Post::Accept,
            extra: vec![],
        }
    }

    fn post(mut self, post: Post) -> Node {
        self.post = post;
        self
    }

    fn genesis(mut self, genesis: &'static str) -> Node {
        self.genesis = genesis;
        self
    }

    fn address(mut self, address: &str) -> Node {
        self.address = address.to_string();
        self
    }

    fn fees(mut self, fees: &str) -> Node {
        self.fees = fees.to_string();
        self
    }

    fn get(mut self, suffix: &str, answer: Result<String, String>) -> Node {
        self.extra.push((suffix.to_string(), answer));
        self
    }

    fn build(self) -> EsploraMock {
        let sum = |confirmed: bool| -> u64 {
            self.coins
                .iter()
                .filter(|c| c.confirmed == confirmed)
                .map(|c| c.value)
                .sum()
        };
        let (confirmed, pending) = (sum(true), sum(false));
        let utxos = self
            .coins
            .iter()
            .map(|c| {
                let status = if c.confirmed {
                    format!(
                        r#"{{"confirmed":true,"block_height":{},"block_time":1700000000}}"#,
                        TIP - 5
                    )
                } else {
                    r#"{"confirmed":false}"#.to_string()
                };
                format!(
                    r#"{{"txid":"{}","vout":{},"status":{status},"value":{}}}"#,
                    c.txid, c.vout, c.value
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let stats = format!(
            r#"{{"chain_stats":{{"funded_txo_sum":{confirmed},"spent_txo_sum":0}},"mempool_stats":{{"funded_txo_sum":{pending},"spent_txo_sum":0}}}}"#
        );
        let mut mock = EsploraMock::new()
            .get("/block-height/0", Ok(self.genesis.to_string()))
            .get("/blocks/tip/height", Ok(TIP.to_string()))
            .get("/fee-estimates", Ok(self.fees))
            .get(
                &format!("/address/{}/utxo", self.address),
                Ok(format!("[{utxos}]")),
            )
            .get(&format!("/address/{}", self.address), Ok(stats));
        for (suffix, answer) in self.extra {
            mock = mock.get(&suffix, answer);
        }
        match self.post {
            Post::Accept => mock.post_with("/tx", |raw| Ok(txid_of_raw(raw))),
            Post::Reject => mock.post(
                "/tx",
                Err(
                    "HTTP status 400: sendrawtransaction RPC error -26: min relay fee not met"
                        .to_string(),
                ),
            ),
            Post::Lost => mock.post("/tx", Err("request failed: timed out".to_string())),
            Post::WrongId => mock.post("/tx", Ok("ff".repeat(32))),
            Post::AlreadyKnown => mock.post(
                "/tx",
                Err("HTTP status 400: txn-already-in-mempool".to_string()),
            ),
        }
    }
}

struct Fx {
    dir: TempDir,
    mock: Rc<EsploraMock>,
    service: WalletService,
}

fn fx(node: Node) -> Fx {
    let dir = TempDir::new();
    let mock = Rc::new(node.build());
    let http: Rc<dyn HttpClient> = mock.clone();
    let service = WalletService::new(Store::open(&dir.0).unwrap(), http)
        .with_iterations(vault::MIN_ITERATIONS);
    service
        .import_wallet("bitcoin", "w", PW, None, Some(&key_hex()))
        .unwrap();
    Fx { dir, mock, service }
}

fn funded(coins: Vec<Coin>) -> Fx {
    fx(Node::new(coins))
}

fn rich() -> Fx {
    funded(vec![coin(1, 1_000_000)])
}

fn pending_files(f: &Fx) -> Vec<String> {
    std::fs::read_dir(f.dir.0.join("pending"))
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Hands a pending-transaction record from one fixture to another, as if the same wallet had
/// been used again after a restart.
fn carry_record(from: &Fx, to: &Fx) {
    let name = pending_files(from).remove(0);
    std::fs::create_dir_all(to.dir.0.join("pending")).unwrap();
    std::fs::copy(
        from.dir.0.join("pending").join(&name),
        to.dir.0.join("pending").join(&name),
    )
    .unwrap();
}

fn field<'a>(v: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(v, |v, key| {
        v.get(key)
            .unwrap_or_else(|| panic!("no '{key}' in {}", v.to_json()))
    })
}

fn text<'a>(v: &'a Value, path: &[&str]) -> &'a str {
    match field(v, path) {
        Value::String(s) | Value::Number(s) => s,
        other => panic!("not text: {}", other.to_json()),
    }
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

fn sent_tx(f: &Fx) -> Transaction {
    let posted = f.mock.posted();
    let (_, raw) = posted.last().expect("nothing was broadcast");
    Transaction::deserialize(&hex::decode(raw).unwrap()).unwrap()
}

fn fee_rate(rate: &str) -> BitcoinFee {
    BitcoinFee {
        fee_rate: Some(rate.to_string()),
        confirmation_target: None,
    }
}

fn send(f: &Fx, to: &str, amount: &str, rate: Option<&str>) -> Result<Value, String> {
    let fee = rate.map(fee_rate).unwrap_or_default();
    f.service
        .bitcoin_send_asset("w", PW, "native", to, amount, &fee)
}

fn params(to: Option<&str>, amount: Option<&str>) -> IntentParams {
    IntentParams {
        to: to.map(str::to_string),
        amount: amount.map(str::to_string),
        ..IntentParams::default()
    }
}

// ---- wallet lifecycle -----------------------------------------------------------------------

#[test]
fn create_returns_the_address_and_network_but_no_secrets() {
    let f = funded(vec![]);
    let v = f
        .service
        .create_wallet_on("bitcoin", "Main", PW, None)
        .unwrap();
    assert_eq!(text(&v, &["blockchain"]), "Bitcoin");
    assert_eq!(text(&v, &["network"]), "mainnet");
    assert_eq!(text(&v, &["status"]), "created");
    let address = text(&v, &["address"]);
    assert!(
        address.starts_with("bc1q") && address.len() == 42,
        "{address}"
    );
    let out = v.to_json();
    assert!(!out.contains("seed") && !out.contains("private"));
    assert_eq!(
        text(&f.service.get_address("main", PW).unwrap(), &["address"]),
        address
    );
    // creating a wallet and reading its address needs no network access at all
    assert!(f.mock.get_urls.borrow().is_empty());
}

#[test]
fn wallets_can_be_bound_to_test_networks_and_networks_are_validated() {
    let f = funded(vec![]);
    for (name, network, prefix) in [
        ("t", "testnet", "tb1q"),
        ("s", "signet", "tb1q"),
        ("m", "bitcoin", "bc1q"),
    ] {
        let v = f
            .service
            .create_wallet_on("btc", name, PW, Some(network))
            .unwrap();
        assert!(text(&v, &["address"]).starts_with(prefix), "{network}");
        assert_eq!(text(&v, &["blockchain"]), "Bitcoin");
    }
    for bad in ["regtest", "devnet", "testnet3", ""] {
        let err = f
            .service
            .create_wallet_on("bitcoin", "x", PW, Some(bad))
            .unwrap_err();
        assert!(err.contains("Bitcoin"), "{bad}: {err}");
    }
    assert!(
        f.service
            .create_wallet_on("bitcoin", "short", "x", None)
            .is_err()
    );
}

#[test]
fn imports_the_known_seed_phrase_and_every_private_key_format() {
    let f = funded(vec![]);
    let v = f
        .service
        .import_wallet("bitcoin", "Seed", PW, Some(ABANDON_ART), None)
        .unwrap();
    assert_eq!(text(&v, &["address"]), ABANDON_ART_ADDRESS);
    assert_eq!(text(&v, &["status"]), "imported");
    let wif = vault().wif().unwrap();
    let v = f
        .service
        .import_wallet("bitcoin", "Wif", PW, None, Some(wif.expose()))
        .unwrap();
    assert_eq!(text(&v, &["address"]), wallet_address());
    let v = f
        .service
        .import_wallet("bitcoin", "Hex", PW, None, Some(&key_hex()))
        .unwrap();
    assert_eq!(text(&v, &["address"]), wallet_address());
    for (secret, as_seed) in [("not a key", false), ("short words here", true)] {
        let err = f
            .service
            .import_wallet(
                "bitcoin",
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
            .import_wallet("bitcoin", "Both", PW, Some(ABANDON_ART), Some(&key_hex()))
            .is_err()
    );
}

#[test]
fn a_testnet_key_cannot_be_imported_into_a_mainnet_wallet() {
    let f = funded(vec![]);
    let testnet = BitcoinVault::from_private_key(&key_hex(), BitcoinNetwork::Testnet).unwrap();
    let wif = testnet.wif().unwrap();
    let err = f
        .service
        .import_wallet("bitcoin", "T", PW, None, Some(wif.expose()))
        .unwrap_err();
    assert!(err.contains("different Bitcoin network"), "{err}");
    assert!(
        f.service
            .import_wallet_on(
                "bitcoin",
                "T",
                PW,
                None,
                Some(wif.expose()),
                Some("testnet")
            )
            .is_ok()
    );
}

#[test]
fn secrets_are_shown_only_with_the_password_and_in_bitcoin_formats() {
    let f = funded(vec![]);
    f.service
        .import_wallet("bitcoin", "Seed", PW, Some(ABANDON_ART), None)
        .unwrap();
    let v = f.service.view_seed_phrase("seed", PW).unwrap();
    assert_eq!(text(&v, &["seed_phrase"]), ABANDON_ART);
    assert_eq!(text(&v, &["derivation_path"]), "m/84'/0'/0'/0/0");
    let v = f.service.view_private_key("seed", PW).unwrap();
    assert_eq!(text(&v, &["address"]), ABANDON_ART_ADDRESS);
    let wif = text(&v, &["private_key"]);
    assert!(wif.starts_with('K') || wif.starts_with('L'), "{wif}");
    let imported = BitcoinVault::from_private_key(wif, BitcoinNetwork::Mainnet).unwrap();
    assert_eq!(imported.address().unwrap(), ABANDON_ART_ADDRESS);
    assert_eq!(text(&v, &["private_key_hex"]).len(), 64);
    assert_eq!(
        f.service
            .view_private_key("seed", "wrong-password")
            .unwrap_err(),
        "incorrect password"
    );
    // a wallet imported from a private key has no seed phrase
    let err = f.service.view_seed_phrase("w", PW).unwrap_err();
    assert!(err.contains("no seed phrase"), "{err}");
}

#[test]
fn export_writes_an_encrypted_zip_without_leaking_secrets() {
    let f = funded(vec![]);
    f.service
        .import_wallet("bitcoin", "Seed", PW, Some(ABANDON_ART), None)
        .unwrap();
    let v = f
        .service
        .export_wallet("seed", PW, "a-long-export-key", Some("btc.zip"))
        .unwrap();
    assert_eq!(text(&v, &["contains"]), "seed phrase and private key");
    let bytes = std::fs::read(text(&v, &["file"])).unwrap();
    let as_text = String::from_utf8_lossy(&bytes).to_string();
    assert!(!as_text.contains("abandon") && !as_text.contains(ABANDON_ART_ADDRESS));
    assert!(
        f.service
            .export_wallet("seed", PW, "a-long-export-key", Some("btc.zip"))
            .is_err(),
        "exports are never overwritten"
    );
}

#[test]
fn rename_and_delete_need_the_password_and_bitcoin_wallets_are_listed() {
    let f = funded(vec![]);
    assert!(f.service.rename_wallet("w", "x", "wrong-password").is_err());
    f.service.rename_wallet("w", "Renamed", PW).unwrap();
    let listed = f.service.list_wallets().unwrap().to_json();
    assert!(listed.contains("Renamed") && listed.contains("Bitcoin"));
    assert!(f.service.delete_wallet("renamed", PW, false).is_err());
    f.service.delete_wallet("renamed", PW, true).unwrap();
    assert!(f.service.get_address("renamed", PW).is_err());
}

#[test]
fn five_wrong_passwords_lock_the_wallet_like_every_other_chain() {
    let f = funded(vec![]);
    for _ in 0..5 {
        assert_eq!(
            f.service.get_address("w", "wrong-password").unwrap_err(),
            "incorrect password"
        );
    }
    let err = f.service.get_address("w", PW).unwrap_err();
    assert!(err.contains("too many failed password attempts"), "{err}");
}

#[test]
fn the_vault_file_holds_no_address_and_is_a_metamask_style_vault() {
    let f = funded(vec![]);
    let dir = f.dir.0.join("wallets");
    let mut files = std::fs::read_dir(dir).unwrap();
    let path = files.next().unwrap().unwrap().path();
    let content = std::fs::read_to_string(path).unwrap();
    assert!(!content.contains(&wallet_address()) && !content.contains("bc1"));
    for key in ["\"data\"", "\"iv\"", "\"salt\"", "\"bitcoin\""] {
        assert!(content.contains(key), "{key}");
    }
}

#[test]
fn bitcoin_wallets_are_refused_by_other_chains_tools() {
    let f = funded(vec![]);
    let err = f.service.unlock("w", PW).map(drop).unwrap_err();
    assert!(err.contains("is a Bitcoin wallet"), "{err}");
    let err = f.service.unlock_sui("w", PW).map(drop).unwrap_err();
    assert!(err.contains("needs a Sui wallet"), "{err}");
    f.service.create_wallet("ethereum", "eth", PW).unwrap();
    let err = f.service.unlock_bitcoin("eth", PW).map(drop).unwrap_err();
    assert!(err.contains("needs a Bitcoin wallet"), "{err}");
}

// ---- balances -------------------------------------------------------------------------------

#[test]
fn a_public_view_refreshes_balances_without_the_password() {
    let f = fx(Node::new(vec![coin(1, 150_000), pending_coin(2, 50_000)]));
    let unlocked = f.service.bitcoin_balances("w", PW).unwrap();
    let view = f.service.public_view("w", PW).unwrap();
    assert_eq!(view.address(), text(&unlocked, &["address"]));
    assert_eq!(f.service.balances_of_view(&view).unwrap(), unlocked);
    assert!(f.service.public_view("w", "wrong-password").is_err());
}

#[test]
fn balances_show_confirmed_and_pending_amounts_with_a_usd_value() {
    let f = fx(
        Node::new(vec![coin(1, 150_000), pending_coin(2, 50_000)]).get(
            "/simple/price?ids=bitcoin&vs_currencies=usd",
            Ok(r#"{"bitcoin":{"usd":60000}}"#.to_string()),
        ),
    );
    let v = f.service.bitcoin_balances("w", PW).unwrap();
    let asset = &field(&v, &["assets"]).as_array().unwrap()[0];
    assert_eq!(text(asset, &["symbol"]), "BTC");
    assert_eq!(text(asset, &["balance"]), "0.002");
    assert_eq!(text(asset, &["confirmed_balance"]), "0.0015");
    assert_eq!(text(asset, &["unconfirmed_change"]), "+0.0005");
    assert_eq!(text(asset, &["balance_raw"]), "200000");
    assert_eq!(text(asset, &["usdc_value"]), "120.00");
    assert_eq!(text(&v, &["address"]), wallet_address());
    assert_eq!(text(&v, &["network", "name"]), "mainnet");
}

#[test]
fn an_empty_wallet_has_a_zero_balance_and_a_missing_price_is_null() {
    let f = funded(vec![]);
    let v = f.service.bitcoin_balances("w", PW).unwrap();
    let asset = &field(&v, &["assets"]).as_array().unwrap()[0];
    assert_eq!(text(asset, &["balance"]), "0");
    assert_eq!(field(asset, &["usdc_value"]), &Value::Null);
}

#[test]
fn test_networks_have_no_usd_value_and_use_their_own_api() {
    let dir = TempDir::new();
    let signet_wallet = BitcoinVault::from_private_key(&key_hex(), BitcoinNetwork::Signet).unwrap();
    let node = Node::new(vec![coin(1, 100_000)])
        .genesis(SIGNET_GENESIS)
        .address(&signet_wallet.address().unwrap());
    let mock = Rc::new(node.build());
    let http: Rc<dyn HttpClient> = mock.clone();
    let service = WalletService::new(Store::open(&dir.0).unwrap(), http)
        .with_iterations(vault::MIN_ITERATIONS);
    service
        .import_wallet_on("bitcoin", "s", PW, None, Some(&key_hex()), Some("signet"))
        .unwrap();
    let v = service.bitcoin_balances("s", PW).unwrap();
    let asset = &field(&v, &["assets"]).as_array().unwrap()[0];
    assert_eq!(field(asset, &["usdc_value"]), &Value::Null);
    let urls = mock.get_urls.borrow();
    assert!(!urls.iter().any(|u| u.contains("coingecko")));
    assert!(urls.iter().any(|u| u.contains("mempool.space/signet/api")));
}

#[test]
fn a_server_on_the_wrong_network_is_refused() {
    let f = fx(Node::new(vec![coin(1, 100_000)]).genesis(TESTNET_GENESIS));
    let err = f.service.bitcoin_balances("w", PW).unwrap_err();
    assert!(err.contains("does not serve Bitcoin mainnet"), "{err}");
    let err = send(&f, RECIPIENT, "0.0005", None).unwrap_err();
    assert!(err.contains("does not serve"), "{err}");
    assert!(f.mock.posted().is_empty());
}

#[test]
fn impossible_balances_from_the_server_are_refused() {
    let lying = EsploraMock::new()
        .get("/block-height/0", Ok(MAINNET_GENESIS.to_string()))
        .get(
            &format!("/address/{}", wallet_address()),
            Ok(r#"{"chain_stats":{"funded_txo_sum":0,"spent_txo_sum":5},"mempool_stats":{"funded_txo_sum":0,"spent_txo_sum":0}}"#.to_string()),
        );
    let dir = TempDir::new();
    let http: Rc<dyn HttpClient> = Rc::new(lying);
    let service = WalletService::new(Store::open(&dir.0).unwrap(), http)
        .with_iterations(vault::MIN_ITERATIONS);
    service
        .import_wallet("bitcoin", "w", PW, None, Some(&key_hex()))
        .unwrap();
    let err = service.bitcoin_balances("w", PW).unwrap_err();
    assert!(err.contains("impossible"), "{err}");
}

// ---- sending --------------------------------------------------------------------------------

#[test]
fn a_payment_is_built_signed_and_broadcast_correctly() {
    let f = funded(vec![coin(1, 100_000)]);
    let v = send(&f, RECIPIENT, "0.0005", Some("2")).unwrap();
    let tx = sent_tx(&f);

    // what was broadcast
    assert_eq!(tx.version, 2);
    assert_eq!(tx.lock_time, TIP as u32, "anti fee sniping");
    assert_eq!(tx.inputs.len(), 1);
    assert_eq!(tx.inputs[0].sequence, 0xffff_fffd, "replaceable");
    let mut spent = tx.inputs[0].previous.txid;
    spent.reverse();
    assert_eq!(hex::encode(&spent), other_txid(1));
    assert_eq!(tx.outputs.len(), 2);
    assert_eq!(tx.outputs[0].value, 50_000);
    assert_eq!(tx.outputs[0].script_pubkey, recipient_script());
    assert_eq!(
        tx.outputs[1].script_pubkey,
        wallet_script(),
        "change returns to the wallet"
    );
    let fee = 100_000 - tx.total_output_value().unwrap();
    assert_eq!(fee, 282, "141 vbytes at 2 sat/vB");
    assert_eq!(tx.outputs[1].value, 100_000 - 50_000 - 282);

    // the signature is valid for the wallet's key and the coin's value
    assert!(tx.verify_p2wpkh_input(0, 100_000));
    assert!(
        !tx.verify_p2wpkh_input(0, 100_001),
        "a signature commits to the value"
    );
    assert_eq!(
        hash160(&tx.inputs[0].witness[1]).to_vec(),
        wallet_script()[2..].to_vec(),
        "signed by the key behind the wallet's address"
    );
    assert!(tx.vsize() <= 141);

    // what was reported
    assert_eq!(text(&v, &["status"]), "broadcast");
    assert_eq!(text(&v, &["txid"]), tx.txid_hex());
    assert_eq!(text(&v, &["amount"]), "0.0005");
    assert_eq!(text(&v, &["network_fee_sats"]), "282");
    assert_eq!(text(&v, &["fee_rate_sat_per_vb"]), "2");
    assert_eq!(text(&v, &["to"]), RECIPIENT);
    assert_eq!(text(&v, &["from"]), wallet_address());
    assert!(text(&v, &["explorer_url"]).ends_with(&tx.txid_hex()));
    assert_eq!(field(&v, &["replaceable"]), &Value::Bool(true));

    // nothing is left pending after a clean broadcast
    assert!(pending_files(&f).is_empty());
}

#[test]
fn the_default_fee_comes_from_the_networks_estimate_for_six_blocks() {
    let f = funded(vec![coin(1, 1_000_000)]);
    let v = send(&f, RECIPIENT, "0.001", None).unwrap();
    assert_eq!(text(&v, &["fee_rate_sat_per_vb"]), "5");
    assert_eq!(text(&v, &["confirmation_target_blocks"]), "6");
    assert_eq!(text(&v, &["network_fee_sats"]), (141 * 5).to_string());
}

#[test]
fn a_confirmation_target_picks_another_estimate() {
    let f = funded(vec![coin(1, 1_000_000)]);
    let fee = BitcoinFee {
        fee_rate: None,
        confirmation_target: Some(1),
    };
    let v = f
        .service
        .bitcoin_send_asset("w", PW, "native", RECIPIENT, "0.001", &fee)
        .unwrap();
    assert_eq!(text(&v, &["fee_rate_sat_per_vb"]), "20");
    for bad in [0u64, 1009] {
        let fee = BitcoinFee {
            fee_rate: None,
            confirmation_target: Some(bad),
        };
        assert!(
            f.service
                .bitcoin_send_asset("w", PW, "native", RECIPIENT, "0.001", &fee)
                .is_err()
        );
    }
}

#[test]
fn fee_choices_are_validated_before_anything_is_signed() {
    let f = rich();
    let both = BitcoinFee {
        fee_rate: Some("5".to_string()),
        confirmation_target: Some(3),
    };
    let err = f
        .service
        .bitcoin_send_asset("w", PW, "native", RECIPIENT, "0.001", &both)
        .unwrap_err();
    assert!(
        err.contains("either fee_rate or confirmation_target"),
        "{err}"
    );
    for (rate, expect) in [
        ("0.5", "at least 1 sat/vB"),
        ("1001", "above 1,000 sat/vB"),
        ("fast", "not a fee rate"),
        ("-3", "not a fee rate"),
    ] {
        let err = send(&f, RECIPIENT, "0.001", Some(rate)).unwrap_err();
        assert!(err.contains(expect), "{rate}: {err}");
    }
    assert!(f.mock.posted().is_empty());
}

#[test]
fn an_absurd_fee_estimate_from_the_server_is_not_followed() {
    let f = fx(Node::new(vec![coin(1, 1_000_000)]).fees(r#"{"6":50000,"1":60000}"#));
    let err = send(&f, RECIPIENT, "0.001", None).unwrap_err();
    assert!(err.contains("implausibly high"), "{err}");
    assert!(f.mock.posted().is_empty());
    // choosing the rate yourself is still possible
    assert!(send(&f, RECIPIENT, "0.001", Some("5")).is_ok());
}

#[test]
fn estimates_below_the_minimum_relay_fee_are_raised_to_it() {
    let f = fx(Node::new(vec![coin(1, 1_000_000)]).fees(r#"{"6":0.1}"#));
    let v = send(&f, RECIPIENT, "0.001", None).unwrap();
    assert_eq!(text(&v, &["fee_rate_sat_per_vb"]), "1");
    let tx = sent_tx(&f);
    assert!(1_000_000 - tx.total_output_value().unwrap() >= tx.vsize());
}

#[test]
fn invalid_requests_are_refused_before_any_wallet_work() {
    let f = rich();
    for (to, amount, expect) in [
        ("", "0.001", "address is empty"),
        ("hello", "0.001", "not a valid Bitcoin mainnet address"),
        (
            "0x742d35Cc6634C0532925a3b844Bc454e4438f44e",
            "0.001",
            "not a valid",
        ),
        (
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
            "0.001",
            "not Bitcoin mainnet",
        ),
        (
            "bc1zw508d6qejxtdg4y5r3zarvaryvaxxpcs",
            "0.001",
            "not supported yet",
        ),
        (RECIPIENT, "0", "greater than zero"),
        (RECIPIENT, "-1", "not an amount"),
        (RECIPIENT, "1e-3", "not an amount"),
        (RECIPIENT, "0.000000001", "8 decimal places"),
        (RECIPIENT, "22000000", "21 million"),
    ] {
        let err = send(&f, to, amount, Some("2")).unwrap_err();
        assert!(err.contains(expect), "{to} {amount}: {err}");
    }
    assert!(f.mock.posted().is_empty());
}

#[test]
fn sending_to_the_wallets_own_address_is_refused() {
    let f = rich();
    let err = send(&f, &wallet_address(), "0.001", None).unwrap_err();
    assert!(err.contains("own address"), "{err}");
}

#[test]
fn only_bitcoin_can_be_sent() {
    let f = rich();
    let fee = BitcoinFee::default();
    let err = f
        .service
        .bitcoin_send_asset(
            "w",
            PW,
            "0xdAC17F958D2ee523a2206206994597C13D831ec7",
            RECIPIENT,
            "1",
            &fee,
        )
        .unwrap_err();
    assert!(err.contains("only bitcoin"), "{err}");
    for asset in ["native", "BTC", "bitcoin"] {
        assert!(
            f.service
                .bitcoin_send_asset("w", PW, asset, RECIPIENT, "0.001", &fee)
                .is_ok(),
            "{asset}"
        );
    }
}

#[test]
fn dust_and_insufficient_funds_are_refused_with_the_numbers() {
    let f = funded(vec![coin(1, 10_000)]);
    let err = send(&f, RECIPIENT, "0.00000293", Some("1")).unwrap_err();
    assert!(err.contains("dust") && err.contains("294"), "{err}");
    let err = send(&f, RECIPIENT, "0.001", Some("1")).unwrap_err();
    assert!(
        err.contains("insufficient funds") && err.contains("0.0001"),
        "{err}"
    );
    // the amount fits but the fee does not
    let err = send(&f, RECIPIENT, "0.0000999", Some("1")).unwrap_err();
    assert!(err.contains("insufficient funds"), "{err}");
    assert!(f.mock.posted().is_empty());
    let empty = funded(vec![]);
    assert!(
        send(&empty, RECIPIENT, "0.001", None)
            .unwrap_err()
            .contains("insufficient funds")
    );
}

#[test]
fn a_fee_larger_than_the_payment_needs_an_explicit_rate() {
    let f = funded(vec![coin(1, 1_000_000)]);
    // 5 sat/vB * 141 = 705 sats of fee for a 600-sat payment
    let err = send(&f, RECIPIENT, "0.000006", None).unwrap_err();
    assert!(
        err.contains("higher than the amount") && err.contains("fee_rate"),
        "{err}"
    );
    assert!(send(&f, RECIPIENT, "0.000006", Some("5")).is_ok());
}

#[test]
fn several_coins_are_combined_and_all_are_signed() {
    let f = funded(vec![coin(1, 30_000), coin(2, 30_000), coin(3, 30_000)]);
    let v = send(&f, RECIPIENT, "0.0007", Some("1")).unwrap();
    let tx = sent_tx(&f);
    assert_eq!(tx.inputs.len(), 3);
    for index in 0..3 {
        assert!(tx.verify_p2wpkh_input(index, 30_000), "input {index}");
    }
    assert_eq!(text(&v, &["inputs_spent"]), "3");
    let fee = 90_000 - tx.total_output_value().unwrap();
    assert!(fee >= tx.vsize(), "at least 1 sat/vB");
}

#[test]
fn small_change_is_added_to_the_fee_with_a_warning() {
    let f = funded(vec![coin(1, 50_300)]);
    let v = send(&f, RECIPIENT, "0.0005", Some("1")).unwrap();
    let tx = sent_tx(&f);
    assert_eq!(tx.outputs.len(), 1, "no dust change output");
    assert_eq!(field(&v, &["change"]), &Value::Null);
    assert!(
        v.to_json()
            .contains("too small to be worth a change output")
    );
    assert_eq!(tx.total_output_value().unwrap(), 50_000);
}

fn parent_json(txid: &str, input_script: &str, input_value: u64, value: u64) -> String {
    format!(
        r#"{{"txid":"{txid}","vin":[{{"txid":"{}","vout":0,"prevout":{{"scriptpubkey":"{input_script}","value":{input_value}}},"is_coinbase":false}}],"vout":[{{"scriptpubkey":"{}","value":{value}}}],"size":100,"weight":400,"fee":100,"status":{{"confirmed":false}}}}"#,
        other_txid(9),
        hex::encode(&wallet_script())
    )
}

#[test]
fn unconfirmed_payments_from_others_are_not_spent_but_own_change_is() {
    // coin 2 is an unconfirmed payment from someone else; coin 3 is the wallet's own change.
    let own_parent = other_txid(3);
    let foreign_parent = other_txid(2);
    let own_script = hex::encode(&wallet_script());
    let foreign_script = format!("0014{}", "ab".repeat(20));
    let f = fx(
        Node::new(vec![pending_coin(2, 500_000), pending_coin(3, 400_000)])
            .get(
                &format!("/tx/{foreign_parent}"),
                Ok(parent_json(
                    &foreign_parent,
                    &foreign_script,
                    500_100,
                    500_000,
                )),
            )
            .get(
                &format!("/tx/{own_parent}"),
                Ok(parent_json(&own_parent, &own_script, 400_100, 400_000)),
            ),
    );
    let v = send(&f, RECIPIENT, "0.003", Some("1")).unwrap();
    let tx = sent_tx(&f);
    assert_eq!(tx.inputs.len(), 1);
    let mut spent = tx.inputs[0].previous.txid;
    spent.reverse();
    assert_eq!(
        hex::encode(&spent),
        own_parent,
        "only the wallet's own change was spent"
    );
    assert!(v.to_json().contains("unconfirmed change"));

    // with only the foreign payment, the wallet explains why it cannot pay
    let f = fx(Node::new(vec![pending_coin(2, 500_000)]).get(
        &format!("/tx/{foreign_parent}"),
        Ok(parent_json(
            &foreign_parent,
            &foreign_script,
            500_100,
            500_000,
        )),
    ));
    let err = send(&f, RECIPIENT, "0.003", Some("1")).unwrap_err();
    assert!(
        err.contains("unconfirmed") && err.contains("0.005"),
        "{err}"
    );
    assert!(f.mock.posted().is_empty());
}

#[test]
fn an_unconfirmed_coin_whose_parent_cannot_be_read_is_not_trusted() {
    let f = funded(vec![pending_coin(2, 500_000)]);
    let err = send(&f, RECIPIENT, "0.003", Some("1")).unwrap_err();
    assert!(err.contains("unconfirmed"), "{err}");
}

#[test]
fn duplicate_and_zero_value_coins_from_the_server_are_ignored() {
    let f = funded(vec![coin(1, 100_000), coin(1, 100_000), coin(2, 0)]);
    send(&f, RECIPIENT, "0.0005", Some("1")).unwrap();
    assert_eq!(sent_tx(&f).inputs.len(), 1);
    // the duplicate is not counted twice: 0.0015 BTC is more than the one real coin holds
    let err = send(&f, RECIPIENT, "0.0015", Some("1")).unwrap_err();
    assert!(err.contains("insufficient funds"), "{err}");
}

// ---- broadcast outcomes ---------------------------------------------------------------------

#[test]
fn a_rejected_broadcast_reports_the_reason_and_leaves_nothing_pending() {
    let f = fx(Node::new(vec![coin(1, 100_000)]).post(Post::Reject));
    let err = send(&f, RECIPIENT, "0.0005", Some("1")).unwrap_err();
    assert!(
        err.contains("rejected") && err.contains("nothing was sent"),
        "{err}"
    );
    assert!(pending_files(&f).is_empty());
}

#[test]
fn a_node_that_already_knows_the_transaction_counts_as_success() {
    let f = fx(Node::new(vec![coin(1, 100_000)]).post(Post::AlreadyKnown));
    let v = send(&f, RECIPIENT, "0.0005", Some("1")).unwrap();
    assert_eq!(text(&v, &["status"]), "broadcast");
    assert!(v.to_json().contains("already knew"));
    assert!(pending_files(&f).is_empty());
}

#[test]
fn a_lost_broadcast_answer_is_never_paid_twice() {
    let f = fx(Node::new(vec![coin(1, 100_000)]).post(Post::Lost));
    let err = send(&f, RECIPIENT, "0.0005", Some("1")).unwrap_err();
    assert!(
        err.contains("outcome is unknown") && err.contains("do not send again"),
        "{err}"
    );
    let first = sent_tx(&f);
    assert!(err.contains(&first.txid_hex()));
    assert_eq!(
        pending_files(&f).len(),
        1,
        "the signed transaction was recorded first"
    );
    assert!(
        !pending_files(&f)[0].contains(&wallet_address()),
        "the record's name shows no address"
    );

    // The next send looks the earlier transaction up; the node does not have it, so the
    // identical bytes are submitted again and nothing new is built.
    let again = fx(Node::new(vec![coin(1, 100_000)]));
    carry_record(&f, &again);
    let report = send(&again, RECIPIENT, "0.0005", Some("1")).unwrap();
    assert_eq!(text(&report, &["status"]), "previous_transaction_resent");
    assert_eq!(text(&report, &["txid"]), first.txid_hex());
    assert_eq!(sent_tx(&again), first, "the identical signed transaction");
    assert_eq!(again.mock.posted().len(), 1, "no second payment was built");
    assert!(pending_files(&again).is_empty());
}

#[test]
fn a_lost_answer_for_a_transaction_the_node_has_is_reported_as_accepted() {
    // The first run tells us the id the deterministic transaction will have.
    let probe = fx(Node::new(vec![coin(1, 100_000)]));
    let txid = text(
        &send(&probe, RECIPIENT, "0.0005", Some("1")).unwrap(),
        &["txid"],
    )
    .to_string();
    let known = format!(r#"{{"txid":"{txid}","vin":[],"vout":[],"status":{{"confirmed":false}}}}"#);
    let f = fx(Node::new(vec![coin(1, 100_000)])
        .post(Post::Lost)
        .get(&format!("/tx/{txid}"), Ok(known)));
    let v = send(&f, RECIPIENT, "0.0005", Some("1")).unwrap();
    assert_eq!(text(&v, &["status"]), "broadcast");
    assert_eq!(text(&v, &["txid"]), txid);
    assert!(pending_files(&f).is_empty());
}

#[test]
fn an_earlier_transaction_the_node_already_has_stops_a_second_send() {
    let probe = fx(Node::new(vec![coin(1, 100_000)]).post(Post::Lost));
    let err = send(&probe, RECIPIENT, "0.0005", Some("1")).unwrap_err();
    let txid = sent_tx(&probe).txid_hex();
    assert!(err.contains(&txid));

    let known = format!(r#"{{"txid":"{txid}","vin":[],"vout":[],"status":{{"confirmed":false}}}}"#);
    let f = fx(Node::new(vec![coin(1, 100_000)]).get(&format!("/tx/{txid}"), Ok(known)));
    carry_record(&probe, &f);
    let report = send(&f, RECIPIENT, "0.0005", Some("1")).unwrap();
    assert_eq!(text(&report, &["status"]), "previous_transaction_found");
    assert!(f.mock.posted().is_empty(), "nothing new was sent");
    assert!(pending_files(&f).is_empty());
}

#[test]
fn if_the_earlier_transaction_cannot_be_checked_nothing_is_sent() {
    let probe = fx(Node::new(vec![coin(1, 100_000)]).post(Post::Lost));
    let _ = send(&probe, RECIPIENT, "0.0005", Some("1")).unwrap_err();
    let txid = sent_tx(&probe).txid_hex();
    let f = fx(Node::new(vec![coin(1, 100_000)])
        .get(&format!("/tx/{txid}"), Err("HTTP status 503".to_string())));
    carry_record(&probe, &f);
    let err = send(&f, RECIPIENT, "0.0005", Some("1")).unwrap_err();
    assert!(
        err.contains("unknown outcome") && err.contains("nothing was sent"),
        "{err}"
    );
    assert!(f.mock.posted().is_empty());
    assert_eq!(pending_files(&f).len(), 1, "the record is kept");
}

#[test]
fn an_earlier_transaction_the_network_rejects_is_discarded() {
    let probe = fx(Node::new(vec![coin(1, 100_000)]).post(Post::Lost));
    let _ = send(&probe, RECIPIENT, "0.0005", Some("1")).unwrap_err();
    let f = fx(Node::new(vec![coin(1, 100_000)]).post(Post::Reject));
    carry_record(&probe, &f);
    let err = send(&f, RECIPIENT, "0.0005", Some("1")).unwrap_err();
    assert!(
        err.contains("never accepted") && err.contains("discarded"),
        "{err}"
    );
    assert!(pending_files(&f).is_empty());
}

#[test]
fn a_different_transaction_id_from_the_server_is_an_error_and_keeps_the_record() {
    let f = fx(Node::new(vec![coin(1, 100_000)]).post(Post::WrongId));
    let err = send(&f, RECIPIENT, "0.0005", Some("1")).unwrap_err();
    assert!(err.contains("different transaction id"), "{err}");
    assert_eq!(pending_files(&f).len(), 1);
}

// ---- estimates and dry runs -----------------------------------------------------------------

#[test]
fn estimating_a_payment_reports_the_fee_without_signing_or_sending() {
    let f = funded(vec![coin(1, 100_000)]);
    let v = f
        .service
        .bitcoin_estimate_gas(
            "w",
            PW,
            &params(Some(RECIPIENT), Some("0.0005")),
            &fee_rate("3.5"),
        )
        .unwrap();
    assert_eq!(text(&v, &["fee_rate_sat_per_vb"]), "3.5");
    assert_eq!(text(&v, &["estimated_vsize"]), "141");
    assert_eq!(text(&v, &["network_fee_sats"]), "494");
    assert_eq!(text(&v, &["network_fee_btc"]), "0.00000494");
    assert_eq!(field(&v, &["network_fee_usdc"]), &Value::Null);
    assert!(f.mock.posted().is_empty());
}

#[test]
fn estimating_without_a_payment_lists_the_rates() {
    let f = funded(vec![]);
    let v = f
        .service
        .bitcoin_estimate_gas("w", PW, &params(None, None), &BitcoinFee::default())
        .unwrap();
    assert_eq!(text(&v, &["fee_rate_sat_per_vb"]), "5");
    assert_eq!(text(&v, &["estimated_vsize"]), "141");
    let rows = field(&v, &["rates_by_confirmation_target"])
        .as_array()
        .unwrap();
    assert_eq!(text(&rows[0], &["confirmation_target_blocks"]), "1");
    assert_eq!(text(&rows[0], &["fee_rate_sat_per_vb"]), "20");
    // 0.5 sat/vB is raised to the 1 sat/vB minimum
    assert_eq!(text(rows.last().unwrap(), &["fee_rate_sat_per_vb"]), "1");
}

#[test]
fn estimate_arguments_are_checked() {
    let f = rich();
    let none = BitcoinFee::default();
    for (p, expect) in [
        (
            IntentParams {
                data: Some("0x00".to_string()),
                ..params(Some(RECIPIENT), Some("0.1"))
            },
            "no smart contracts",
        ),
        (
            IntentParams {
                transaction_bytes: Some("AA==".to_string()),
                ..params(None, None)
            },
            "no smart contracts",
        ),
        (params(Some(RECIPIENT), None), "both 'to' and 'amount'"),
        (
            IntentParams {
                asset: Some("0xabc".to_string()),
                ..params(Some(RECIPIENT), Some("0.1"))
            },
            "only bitcoin",
        ),
    ] {
        let err = f
            .service
            .bitcoin_estimate_gas("w", PW, &p, &none)
            .unwrap_err();
        assert!(err.contains(expect), "{err}");
    }
}

#[test]
fn simulating_signs_in_memory_and_reports_what_would_happen() {
    let f = funded(vec![coin(1, 100_000)]);
    let v = f
        .service
        .bitcoin_simulate_transaction(
            "w",
            PW,
            &params(Some(RECIPIENT), Some("0.0005")),
            &fee_rate("2"),
        )
        .unwrap();
    assert_eq!(field(&v, &["would_succeed"]), &Value::Bool(true));
    assert_eq!(text(&v, &["wallet_balance_change"]), "-0.00050282");
    assert_eq!(text(&v, &["change"]), "0.00049718");
    assert_eq!(text(&v, &["lock_time"]), TIP.to_string());
    assert!(f.mock.posted().is_empty(), "nothing was sent");
    assert!(pending_files(&f).is_empty());
    // the transaction id it reports is the one a real send then produces
    let sent = send(&f, RECIPIENT, "0.0005", Some("2")).unwrap();
    assert_eq!(text(&sent, &["txid"]), text(&v, &["txid"]));
}

#[test]
fn simulating_an_unaffordable_payment_reports_failure_instead_of_erroring() {
    let f = funded(vec![coin(1, 10_000)]);
    let none = BitcoinFee::default();
    let v = f
        .service
        .bitcoin_simulate_transaction("w", PW, &params(Some(RECIPIENT), Some("0.01")), &none)
        .unwrap();
    assert_eq!(field(&v, &["would_succeed"]), &Value::Bool(false));
    assert!(text(&v, &["error"]).contains("insufficient funds"));
    let dust = f
        .service
        .bitcoin_simulate_transaction("w", PW, &params(Some(RECIPIENT), Some("0.000001")), &none)
        .unwrap();
    assert!(text(&dust, &["error"]).contains("dust"));
    // an invalid address is a bad request, not a failed simulation
    assert!(
        f.service
            .bitcoin_simulate_transaction("w", PW, &params(Some("nonsense"), Some("0.01")), &none)
            .is_err()
    );
    assert!(
        f.service
            .bitcoin_simulate_transaction("w", PW, &params(None, None), &none)
            .is_err()
    );
}

// ---- messages -------------------------------------------------------------------------------

#[test]
fn messages_are_signed_with_bip322_and_need_no_network() {
    let f = funded(vec![]);
    f.service
        .import_wallet(
            "bitcoin",
            "vector",
            PW,
            None,
            Some("L3VFeEujGtevx9w18HD1fhRbCH67Az2dpCymeRE1SoPK6XQtaN2k"),
        )
        .unwrap();
    let v = f
        .service
        .bitcoin_sign_message("vector", PW, "Hello World", None)
        .unwrap();
    assert_eq!(
        text(&v, &["address"]),
        "bc1q9vza2e8x573nczrlzms0wvx3gsqjx7vavgkx0l"
    );
    // BIP 322's published signature for this key and message (plain RFC 6979 nonce)
    assert_eq!(
        text(&v, &["signature"]),
        "smpAkgwRQIhAOzyynlqt93lOKJr+wmmxIens//zPzl9tqIOua93wO6MAiBi5n5EyAcPScOjf1lAqIUIQtr3zKNeavYabHyR8eGhowEhAsfxIAMZZEKUPYWI4BruhAQjzFT8FSFSajuFwrDL1Yhy"
    );
    assert_eq!(
        text(&v, &["signature_without_prefix"]),
        &text(&v, &["signature"])[3..]
    );
    assert_eq!(text(&v, &["message_preview"]), "Hello World");
    assert_eq!(
        text(&v, &["message_hash"]),
        "f0eb03b1a75ac6d9847f55c624a99169b5dccba2a31f5b23bea77ba270de0a7a"
    );
    assert!(f.mock.get_urls.borrow().is_empty());
}

#[test]
fn message_formats_and_errors() {
    let f = funded(vec![]);
    let signed = f
        .service
        .bitcoin_sign_message("w", PW, "00ff", Some("hex"))
        .unwrap();
    assert_eq!(text(&signed, &["message_preview"]), "(binary data)");
    assert!(f.service.bitcoin_sign_message("w", PW, "", None).is_err());
    assert!(
        f.service
            .bitcoin_sign_message("w", PW, "zz", Some("hex"))
            .is_err()
    );
    assert!(
        f.service
            .bitcoin_sign_message("w", "bad-password", "hi", None)
            .is_err()
    );
}

// ---- lookups --------------------------------------------------------------------------------

fn tx_body(txid: &str, confirmed: bool, our_input: bool) -> String {
    let wallet = hex::encode(&wallet_script());
    let other = format!("0014{}", "cd".repeat(20));
    let status = if confirmed {
        format!(
            r#"{{"confirmed":true,"block_height":{},"block_time":1790000000}}"#,
            TIP - 9
        )
    } else {
        r#"{"confirmed":false}"#.to_string()
    };
    let input_script = if our_input { &wallet } else { &other };
    format!(
        r#"{{"txid":"{txid}","vin":[{{"txid":"{}","vout":0,"prevout":{{"scriptpubkey":"{input_script}","scriptpubkey_address":"x","value":100000}},"is_coinbase":false}}],
        "vout":[{{"scriptpubkey":"{other}","scriptpubkey_address":"bc1qother","value":60000}},{{"scriptpubkey":"{wallet}","value":39000}}],
        "size":141,"weight":562,"fee":1000,"status":{status}}}"#,
        other_txid(7)
    )
}

#[test]
fn a_transaction_is_described_from_the_wallets_point_of_view() {
    let txid = other_txid(5);
    let f = fx(Node::new(vec![]).get(&format!("/tx/{txid}"), Ok(tx_body(&txid, true, true))));
    let v = f
        .service
        .bitcoin_get_transaction("w", PW, &txid.to_uppercase())
        .unwrap();
    assert_eq!(text(&v, &["status"]), "confirmed");
    assert_eq!(text(&v, &["confirmations"]), "10");
    assert_eq!(text(&v, &["direction"]), "sent");
    assert_eq!(text(&v, &["wallet_balance_change"]), "-0.00061");
    assert_eq!(text(&v, &["fee_btc"]), "0.00001");
    assert_eq!(text(&v, &["virtual_size"]), "141");
    let outputs = field(&v, &["outputs"]).as_array().unwrap();
    assert_eq!(outputs.len(), 2);
    assert_eq!(field(&outputs[1], &["is_this_wallet"]), &Value::Bool(true));
    assert!(text(&v, &["explorer_url"]).ends_with(&txid));
}

#[test]
fn received_and_pending_transactions_are_recognised() {
    let txid = other_txid(5);
    let f = fx(Node::new(vec![]).get(&format!("/tx/{txid}"), Ok(tx_body(&txid, false, false))));
    let v = f.service.bitcoin_get_transaction("w", PW, &txid).unwrap();
    assert_eq!(text(&v, &["status"]), "pending");
    assert_eq!(text(&v, &["confirmations"]), "0");
    assert_eq!(text(&v, &["direction"]), "received");
    assert_eq!(text(&v, &["wallet_balance_change"]), "+0.00039");
}

#[test]
fn unknown_and_malformed_transaction_ids() {
    let f = funded(vec![]);
    let v = f
        .service
        .bitcoin_get_transaction("w", PW, &other_txid(8))
        .unwrap();
    assert_eq!(text(&v, &["status"]), "not_found");
    for bad in ["", "abc", "0x1234", &"g".repeat(64), &"ab".repeat(33)] {
        assert!(
            f.service.bitcoin_get_transaction("w", PW, bad).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn history_lists_transactions_newest_first_with_the_wallets_change() {
    let (a, b, c) = (other_txid(5), other_txid(6), other_txid(7));
    let page = format!(
        "[{},{},{}]",
        tx_body(&a, false, false),
        tx_body(&b, true, true),
        tx_body(&c, true, false)
    );
    let f = fx(Node::new(vec![]).get(&format!("/address/{}/txs", wallet_address()), Ok(page)));
    let v = f
        .service
        .bitcoin_history("w", PW, HistoryKind::Transactions, 20)
        .unwrap();
    let txs = field(&v, &["transactions"]).as_array().unwrap();
    assert_eq!(txs.len(), 3);
    assert_eq!(text(&txs[0], &["status"]), "pending");
    assert_eq!(text(&txs[1], &["direction"]), "sent");
    assert_eq!(text(&txs[2], &["direction"]), "received");
    let limited = f
        .service
        .bitcoin_history("w", PW, HistoryKind::Transactions, 2)
        .unwrap();
    assert_eq!(
        field(&limited, &["transactions"]).as_array().unwrap().len(),
        2
    );
    let err = f
        .service
        .bitcoin_history("w", PW, HistoryKind::TokenTransfers, 20)
        .unwrap_err();
    assert!(err.contains("no tokens"), "{err}");
}

#[test]
fn history_follows_pages_until_the_limit() {
    let wallet = wallet_address();
    let id = |page: u8, i: u8| format!("{page:02x}{i:02x}").repeat(16);
    let page = |n: u8| {
        let items: Vec<String> = (0..25u8).map(|i| tx_body(&id(n, i), true, false)).collect();
        format!("[{}]", items.join(","))
    };
    let f = fx(Node::new(vec![])
        .get(
            &format!("/address/{wallet}/txs/chain/{}", id(1, 24)),
            Ok(page(2)),
        )
        .get(&format!("/address/{wallet}/txs"), Ok(page(1))));
    let v = f
        .service
        .bitcoin_history("w", PW, HistoryKind::Transactions, 40)
        .unwrap();
    assert_eq!(text(&v, &["count"]), "40");
}

// ---- the MCP tools --------------------------------------------------------------------------

fn toolbox(f: Fx) -> (Toolbox, Rc<EsploraMock>, TempDir) {
    let Fx { dir, mock, service } = f;
    (Toolbox::new(service), mock, dir)
}

#[test]
fn tools_dispatch_to_bitcoin_wallets() {
    let (tb, mock, _dir) = toolbox(rich());
    let args = format!(r#"{{"wallet_name":"w","password":"{PW}"}}"#);
    let (error, v) = call(&tb, "get_wallet_address", &args);
    assert!(!error);
    assert_eq!(text(&v, &["address"]), wallet_address());
    let (error, v) = call(&tb, "get_wallet_balances", &args);
    assert!(!error, "{}", v.to_json());
    let asset = &field(&v, &["assets"]).as_array().unwrap()[0];
    assert_eq!(text(asset, &["symbol"]), "BTC");
    assert_eq!(text(asset, &["balance"]), "0.01");
    let (error, v) = call(&tb, "list_networks", &args);
    assert!(!error);
    assert_eq!(
        text(&field(&v, &["networks"]).as_array().unwrap()[0], &["name"]),
        "mainnet"
    );
    let (error, v) = call(&tb, "view_private_key", &args);
    assert!(!error);
    assert!(text(&v, &["private_key"]).len() >= 51);
    let (error, _) = call(&tb, "view_seed_phrase", &args);
    assert!(error, "a key-imported wallet has no seed phrase");
    let (error, _) = call(
        &tb,
        "send_asset",
        &format!(
            r#"{{"wallet_name":"w","password":"{PW}","asset":"native","to":"{RECIPIENT}","amount":"0.001","fee_rate":"2"}}"#
        ),
    );
    assert!(!error);
    assert_eq!(mock.posted().len(), 1);
}

#[test]
fn tools_refuse_what_bitcoin_cannot_do() {
    let (tb, mock, _dir) = toolbox(rich());
    let base = format!(r#""wallet_name":"w","password":"{PW}""#);
    let evm = r#""chain_id":1,"contract":"0x0000000000000000000000000000000000000001""#;
    for (tool, extra, expect) in [
        ("get_wallet_balances", r#","chain_id":1"#.to_string(), "chain_id"),
        (
            "get_wallet_balances",
            r#","token_contracts":["0x1"]"#.to_string(),
            "Ethereum option",
        ),
        (
            "send_asset",
            r#","chain_id":1,"asset":"native","to":"x","amount":"1""#.to_string(),
            "chain_id",
        ),
        (
            "send_asset",
            r#","gas_limit":21000,"asset":"native","to":"x","amount":"1""#.to_string(),
            "Ethereum option",
        ),
        (
            "sign_contract_transaction",
            r#","transaction_bytes":"AA==""#.to_string(),
            "PSBT",
        ),
        (
            "get_swap_quote",
            r#","from_token":"native","to_token":"native","amount":"1""#.to_string(),
            "swaps are not available",
        ),
        (
            "swap_tokens",
            r#","from_token":"native","to_token":"native","amount":"1""#.to_string(),
            "swaps are not available",
        ),
        (
            "add_network",
            r#","network_name":"x","chain_id":1,"rpc_url":"https://x.org","currency_symbol":"X","block_explorer_url":"https://x.org""#.to_string(),
            "Bitcoin wallet",
        ),
        ("add_token", format!(",{evm}"), "Bitcoin wallet"),
        (
            "read_smart_contract",
            format!(r#",{evm},"function":"f()""#),
            "Bitcoin wallet",
        ),
        ("wc_approve_session", r#","proposal_id":"1""#.to_string(), "WalletConnect"),
        ("wc_review_request", r#","request_id":"1""#.to_string(), "WalletConnect"),
        ("wc_approve_request", r#","request_id":"1""#.to_string(), "WalletConnect"),
        (
            "get_transaction_history",
            r#","chain_id":1"#.to_string(),
            "chain_id",
        ),
        (
            "get_transaction",
            r#","chain_id":1,"transaction_hash":"x""#.to_string(),
            "chain_id",
        ),
        (
            "estimate_gas_fee",
            r#","value":"1","to":"x""#.to_string(),
            "Bitcoin has no smart contracts",
        ),
    ] {
        let (error, v) = call(&tb, tool, &format!("{{{base}{extra}}}"));
        assert!(error, "{tool} should fail");
        assert!(v.to_json().contains(expect), "{tool}: {}", v.to_json());
    }
    assert!(mock.posted().is_empty());
}

#[test]
fn bitcoin_only_fee_options_are_refused_for_other_chains() {
    let f = rich();
    f.service.create_wallet("ethereum", "eth", PW).unwrap();
    f.service
        .import_wallet("solana", "sol", PW, None, Some(&key_hex()))
        .unwrap();
    let (tb, _mock, _dir) = toolbox(f);
    for wallet in ["eth", "sol"] {
        for tool in ["send_asset", "estimate_gas_fee", "simulate_transaction"] {
            let (error, v) = call(
                &tb,
                tool,
                &format!(
                    r#"{{"wallet_name":"{wallet}","password":"{PW}","fee_rate":"5","asset":"native","to":"x","amount":"1"}}"#
                ),
            );
            let message = v.to_json();
            assert!(
                error && message.contains("for Bitcoin wallets"),
                "{tool} {wallet}: {message}"
            );
        }
    }
}

#[test]
fn tools_accept_fee_options_and_price_a_payment() {
    let (tb, _mock, _dir) = toolbox(fx(Node::new(vec![coin(1, 100_000)]).get(
        &format!("/address/{}/txs", wallet_address()),
        Ok("[]".to_string()),
    )));
    let (error, v) = call(
        &tb,
        "estimate_gas_fee",
        &format!(
            r#"{{"wallet_name":"w","password":"{PW}","to":"{RECIPIENT}","amount":"0.0005","confirmation_target":1}}"#
        ),
    );
    assert!(!error, "{}", v.to_json());
    assert_eq!(text(&v, &["fee_rate_sat_per_vb"]), "20");
    let (error, v) = call(
        &tb,
        "simulate_transaction",
        &format!(
            r#"{{"wallet_name":"w","password":"{PW}","to":"{RECIPIENT}","amount":"0.0005","fee_rate":"1.5"}}"#
        ),
    );
    assert!(!error, "{}", v.to_json());
    assert_eq!(field(&v, &["would_succeed"]), &Value::Bool(true));
    let (error, v) = call(
        &tb,
        "sign_personal_message",
        &format!(r#"{{"wallet_name":"w","password":"{PW}","message":"hello"}}"#),
    );
    assert!(!error);
    assert_eq!(text(&v, &["standard"]), "BIP 322 simple (P2WPKH)");
    let (error, _) = call(
        &tb,
        "get_transaction_history",
        &format!(r#"{{"wallet_name":"w","password":"{PW}","limit":5}}"#),
    );
    assert!(!error);
}

#[test]
fn the_tool_list_documents_bitcoin() {
    let (tb, _mock, _dir) = toolbox(funded(vec![]));
    let listing = tb.list().to_json();
    for needle in [
        "\\\"bitcoin\\\"",
        "fee_rate",
        "confirmation_target",
        "BIP 322",
        "native SegWit",
    ] {
        assert!(listing.contains(needle), "{needle}");
    }
}

#[test]
fn passwords_and_keys_never_appear_in_bitcoin_results() {
    let f = funded(vec![coin(1, 100_000)]);
    let results = [
        send(&f, RECIPIENT, "0.0005", Some("1")).map(|v| v.to_json()),
        f.service.bitcoin_balances("w", PW).map(|v| v.to_json()),
        f.service
            .bitcoin_sign_message("w", PW, "hi", None)
            .map(|v| v.to_json()),
        f.service
            .bitcoin_estimate_gas(
                "w",
                PW,
                &params(Some(RECIPIENT), Some("0.0005")),
                &BitcoinFee::default(),
            )
            .map(|v| v.to_json()),
        f.service.bitcoin_networks("w", PW).map(|v| v.to_json()),
    ];
    let wif = vault().wif().unwrap();
    for result in results {
        let out = result.unwrap();
        assert!(!out.contains(PW) && !out.contains(&key_hex()) && !out.contains(wif.expose()));
    }
}

#[test]
fn the_wallet_is_locked_again_after_every_call() {
    let f = rich();
    send(&f, RECIPIENT, "0.001", Some("1")).unwrap();
    // every call needs the password anew
    assert!(f.service.bitcoin_balances("w", "").is_err());
    assert!(f.service.bitcoin_balances("w", "wrong-password").is_err());
    assert!(f.service.bitcoin_balances("w", PW).is_ok());
}
