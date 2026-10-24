//! Tests for Sui wallets through the service and the MCP tools, against a scripted Sui node.

use sui_crypto::SuiVerifier;
use sui_crypto::ed25519::Ed25519VerifyingKey;
use sui_sdk_types::{Command, Input, TransactionKind, UserSignature};

use super::*;
use crate::crypto::base64;
use crate::json;
use crate::store::testutil::TempDir;
use crate::sui::cetus::tests::{
    AGG_LATEST, BUCK, ROUTER_LATEST, USDC, api_answer, chain, pool_type,
};
use crate::sui::coins::SUI_COIN_TYPE;
use crate::sui::testing::SuiMock;
use crate::sui::tx;
use crate::tools::Toolbox;

const PW: &str = "correct-horse-battery";
const MAINNET: &str = "4btiuiMPvEENsttpZC7CZ53DruC3MAgfznDbASZ7DR6S";
const TESTNET: &str = "69WiPg3DAQiwdxfncX6wYQ2siKwAe6L9BZthQea3JNMD";
const COMPUTATION: u64 = 1_000_000;
const STORAGE: u64 = 2_000_000;
const REBATE: u64 = 1_500_000;
const ABANDON_ART: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";

fn key_hex() -> String {
    "11".repeat(32)
}

fn vault() -> SuiVault {
    SuiVault::from_private_key(&key_hex(), crate::sui::network::SuiNetwork::Mainnet).unwrap()
}

fn wallet_address() -> String {
    vault().address().unwrap().to_string()
}

fn other_address() -> String {
    format!("0x{}", "cd".repeat(32))
}

fn coin_node(id: u8, balance: u64) -> String {
    format!(
        r#"{{"address":"0x{}","version":{},"digest":"3HNMVAajDX19eHDpRjb3D1nfDeTTMbPZC8uJMosrUHL8","contents":{{"json":{{"id":"x","balance":"{balance}"}}}}}}"#,
        format!("{id:02x}").repeat(32),
        100 + u64::from(id)
    )
}

fn coins_data(coins: &[(u8, u64)]) -> String {
    let nodes: Vec<String> = coins.iter().map(|(i, b)| coin_node(*i, *b)).collect();
    format!(
        r#"{{"address":{{"objects":{{"pageInfo":{{"hasNextPage":false}},"nodes":[{}]}}}}}}"#,
        nodes.join(",")
    )
}

fn change(owner: &str, coin_type: &str, amount: i128) -> String {
    format!(r#"{{"address":"{owner}","coinType":"{coin_type}","amount":"{amount}"}}"#)
}

fn effects_data(field: &str, status: &str, error: Option<&str>, changes: &[String]) -> String {
    let error = error.map_or("null".to_string(), |e| format!(r#"{{"message":"{e}"}}"#));
    format!(
        r#"{{"{field}":{{"effects":{{"digest":"FAKEDIGEST","status":"{status}","executionError":{error},"gasEffects":{{"gasSummary":{{"computationCost":{COMPUTATION},"storageCost":{STORAGE},"storageRebate":{REBATE}}}}},"balanceChangesJson":[{}]}}}}}}"#,
        changes.join(",")
    )
}

fn sui_loss(amount: i128) -> String {
    change(&wallet_address(), SUI_COIN_TYPE, -amount)
}

/// A healthy mainnet node: the wallet holds `sui_coins` (id, balance) and `address_balance`.
fn node(sui_coins: &[(u8, u64)], address_balance: u128) -> SuiMock {
    SuiMock::new()
        .data(
            "chainIdentifier",
            &format!(r#"{{"chainIdentifier":"{MAINNET}"}}"#),
        )
        .data(
            "referenceGasPrice",
            r#"{"epoch":{"epochId":1290,"referenceGasPrice":"100"}}"#,
        )
        .data(
            &format!("\"t\":\"0x2::coin::Coin<{SUI_COIN_TYPE}>\""),
            &coins_data(sui_coins),
        )
        .data(
            &format!("\"t\":\"0x2::coin::Coin<{USDC}>\""),
            &coins_data(&[(0x40, 5_000_000)]),
        )
        .data(
            "balance(coinType",
            &format!(r#"{{"address":{{"balance":{{"addressBalance":"{address_balance}"}}}}}}"#),
        )
        .data(
            "coinMetadata(coinType",
            r#"{"coinMetadata":{"symbol":"USDC","decimals":6}}"#,
        )
        .data(
            "simulateTransaction",
            &effects_data(
                "simulateTransaction",
                "SUCCESS",
                None,
                &[sui_loss(1_500_000)],
            ),
        )
        .data(
            "executeTransaction",
            &effects_data(
                "executeTransaction",
                "SUCCESS",
                None,
                &[sui_loss(1_500_000)],
            ),
        )
        .get("simple/price?ids=sui", r#"{"sui":{"usd":2.0}}"#)
}

struct Fx {
    _dir: TempDir,
    mock: Rc<SuiMock>,
    service: WalletService,
}

fn build(mock: SuiMock, configure: impl FnOnce(WalletService) -> WalletService) -> Fx {
    let dir = TempDir::new();
    let mock = Rc::new(mock);
    let http: Rc<dyn HttpClient> = mock.clone();
    let service = configure(
        WalletService::new(Store::open(&dir.0).unwrap(), http)
            .with_iterations(vault::MIN_ITERATIONS),
    );
    Fx {
        _dir: dir,
        mock,
        service,
    }
}

fn import(f: &Fx) {
    f.service
        .import_wallet("sui", "w", PW, None, Some(&key_hex()))
        .unwrap();
}

/// A fixture whose wallet "w" holds a 5 SUI coin object.
fn funded() -> Fx {
    let f = build(node(&[(1, 5_000_000_000)], 0), |s| s);
    import(&f);
    f
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

/// The transaction and signature the wallet submitted last.
fn submitted(f: &Fx) -> (sui_sdk_types::Transaction, UserSignature) {
    let bodies = f.mock.bodies("executeTransaction");
    let doc = json::parse(bodies.last().expect("nothing was submitted")).unwrap();
    let vars = doc.get("variables").unwrap();
    let transaction = tx::decode(vars.get("t").and_then(Value::as_str).unwrap()).unwrap();
    let signature = UserSignature::from_base64(
        vars.get("s")
            .and_then(Value::as_array)
            .and_then(|a| a[0].as_str())
            .unwrap(),
    )
    .unwrap();
    (transaction, signature)
}

fn assert_signed_by_wallet(transaction: &sui_sdk_types::Transaction, signature: &UserSignature) {
    let key = vault().signer().unwrap();
    let verifier = Ed25519VerifyingKey::new(&key.public_key()).unwrap();
    verifier.verify_transaction(transaction, signature).unwrap();
}

fn commands(transaction: &sui_sdk_types::Transaction) -> Vec<Command> {
    match &transaction.kind {
        TransactionKind::ProgrammableTransaction(p) => p.commands.clone(),
        _ => panic!("not a PTB"),
    }
}

// ---- wallet lifecycle ---------------------------------------------------------------------------

#[test]
fn creating_a_sui_wallet_returns_a_sui_address_and_its_network() {
    let f = build(SuiMock::new(), |s| s);
    let out = f.service.create_wallet("sui", "S", PW).unwrap();
    assert_eq!(text(&out, &["blockchain"]), "Sui");
    assert_eq!(text(&out, &["network"]), "mainnet");
    let address = text(&out, &["address"]);
    assert!(address.starts_with("0x") && address.len() == 66);
    assert_eq!(text(&out, &["status"]), "created");
    let shown = f.service.get_address("S", PW).unwrap();
    assert_eq!(text(&shown, &["address"]), address);
    assert_eq!(text(&shown, &["network"]), "mainnet");
    assert!(
        f.mock.requests.borrow().is_empty(),
        "creating a wallet needs no network access"
    );
}

#[test]
fn wallets_can_be_bound_to_testnet_or_devnet_and_networks_are_validated() {
    let f = build(SuiMock::new(), |s| s);
    let out = f
        .service
        .create_wallet_on("sui", "T", PW, Some("testnet"))
        .unwrap();
    assert_eq!(text(&out, &["network"]), "testnet");
    assert_eq!(
        text(
            &f.service
                .create_wallet_on("SUI", "D", PW, Some("sui:devnet"))
                .unwrap(),
            &["network"]
        ),
        "devnet"
    );
    let err = f
        .service
        .create_wallet_on("sui", "X", PW, Some("moonnet"))
        .unwrap_err();
    assert!(err.contains("unsupported Sui network"));
    let err = f
        .service
        .create_wallet_on("ethereum", "E", PW, Some("testnet"))
        .unwrap_err();
    assert!(err.contains("Sui wallets only"));
    assert!(
        !f.service
            .list_wallets()
            .unwrap()
            .to_json()
            .contains("\"X\"")
    );
}

#[test]
fn the_same_seed_gives_different_wallets_per_blockchain() {
    let f = build(SuiMock::new(), |s| s);
    let eth = f
        .service
        .import_wallet("ethereum", "E", PW, Some(ABANDON_ART), None)
        .unwrap();
    let sui = f
        .service
        .import_wallet("sui", "S", PW, Some(ABANDON_ART), None)
        .unwrap();
    assert!(text(&eth, &["address"]).len() == 42 && text(&sui, &["address"]).len() == 66);
    let listed = f.service.list_wallets().unwrap();
    let names: Vec<(&str, &str)> = field(&listed, &["wallets"])
        .as_array()
        .unwrap()
        .iter()
        .map(|w| (text(w, &["name"]), text(w, &["blockchain"])))
        .collect();
    assert_eq!(names, vec![("E", "Ethereum"), ("S", "Sui")]);
}

#[test]
fn importing_sui_keys_in_every_format() {
    let f = build(SuiMock::new(), |s| s);
    let reference = vault();
    let bech32 = reference.suiprivkey().unwrap();
    let a = f
        .service
        .import_wallet("sui", "a", PW, None, Some(bech32.expose()))
        .unwrap();
    let b = f
        .service
        .import_wallet("sui", "b", PW, None, Some(&key_hex()))
        .unwrap();
    assert_eq!(text(&a, &["address"]), wallet_address());
    assert_eq!(text(&b, &["address"]), wallet_address());
    assert_eq!(text(&a, &["status"]), "imported");
}

#[test]
fn import_validation_for_sui() {
    let f = build(SuiMock::new(), |s| s);
    for (seed, key) in [(None, None), (Some(ABANDON_ART), Some("11".repeat(32)))] {
        let err = f
            .service
            .import_wallet("sui", "w", PW, seed, key.as_deref())
            .unwrap_err();
        assert!(err.contains("exactly one of"), "{err}");
    }
    assert!(
        f.service
            .import_wallet("sui", "w", PW, Some("abandon abandon"), None)
            .is_err()
    );
    assert!(
        f.service
            .import_wallet("sui", "w", PW, None, Some("suiprivkey1qqqq"))
            .is_err()
    );
    assert!(
        f.service
            .import_wallet("sui", "w", "short", None, Some(&key_hex()))
            .is_err()
    );
    import(&f);
    let err = f
        .service
        .import_wallet("sui", "W", PW, None, Some(&key_hex()))
        .unwrap_err();
    assert!(err.contains("already exists"), "{err}");
    // An Ethereum import of a Sui key is pointed to the right blockchain.
    let bech32 = vault().suiprivkey().unwrap();
    let err = f
        .service
        .import_wallet("ethereum", "e", PW, None, Some(bech32.expose()))
        .unwrap_err();
    assert!(
        err.contains("blockchain to \"sui\"") || err.contains("set blockchain"),
        "{err}"
    );
}

#[test]
fn the_wallet_file_holds_no_address_key_or_network() {
    let f = build(SuiMock::new(), |s| s);
    let out = f
        .service
        .import_wallet_on("sui", "w", PW, Some(ABANDON_ART), None, Some("testnet"))
        .unwrap();
    let address = text(&out, &["address"]).to_string();
    let mut files = 0;
    for entry in std::fs::read_dir(f._dir.0.join("wallets")).unwrap() {
        let content = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        files += 1;
        assert!(content.contains("\"blockchain\":\"sui\""));
        for secret in [address.as_str(), "abandon", "testnet", "784"] {
            assert!(!content.contains(secret), "wallet file leaks {secret}");
        }
    }
    assert_eq!(files, 1);
}

#[test]
fn wrong_passwords_are_rate_limited_for_sui_wallets_too() {
    let f = build(SuiMock::new(), |s| s);
    import(&f);
    for _ in 0..MAX_FAILED_ATTEMPTS {
        assert_eq!(
            f.service.get_address("w", "wrong-password").unwrap_err(),
            "incorrect password"
        );
    }
    let err = f.service.get_address("w", PW).unwrap_err();
    assert!(err.contains("too many failed password attempts"), "{err}");
}

#[test]
fn each_chains_tools_refuse_the_other_chains_wallets() {
    let f = build(SuiMock::new(), |s| s);
    import(&f);
    f.service.create_wallet("ethereum", "e", PW).unwrap();
    // Ethereum-only operations on a Sui wallet.
    let err = f
        .service
        .add_token("w", PW, 1, &format!("0x{}", "22".repeat(20)))
        .unwrap_err();
    assert!(
        err.contains("is a Sui wallet, but this operation needs a Ethereum wallet"),
        "{err}"
    );
    // Sui-only operations on an Ethereum wallet.
    let err = f.service.sui_balances("e", PW).unwrap_err();
    assert!(
        err.contains("is a Ethereum wallet, but this operation needs a Sui wallet"),
        "{err}"
    );
    assert!(
        f.mock.requests.borrow().is_empty(),
        "refusals happen before any network access"
    );
}

#[test]
fn secrets_are_shown_only_by_their_tools_and_in_sui_formats() {
    let f = build(SuiMock::new(), |s| s);
    f.service
        .import_wallet("sui", "w", PW, Some(ABANDON_ART), None)
        .unwrap();
    let phrase = f.service.view_seed_phrase("w", PW).unwrap();
    assert_eq!(text(&phrase, &["seed_phrase"]), ABANDON_ART);
    assert_eq!(text(&phrase, &["derivation_path"]), "m/44'/784'/0'/0'/0'");
    let key = f.service.view_private_key("w", PW).unwrap();
    assert!(text(&key, &["private_key"]).starts_with("suiprivkey1"));
    assert_eq!(text(&key, &["private_key_hex"]).len(), 66);
    assert_eq!(text(&key, &["scheme"]), "ed25519");
    let address = f.service.get_address("w", PW).unwrap().to_json();
    assert!(!address.contains("abandon") && !address.contains("suiprivkey"));
    // The exported key re-imports to the same address.
    let imported = f
        .service
        .import_wallet("sui", "copy", PW, None, Some(text(&key, &["private_key"])))
        .unwrap();
    assert_eq!(
        text(&imported, &["address"]),
        text(&f.service.get_address("w", PW).unwrap(), &["address"])
    );
    // A key import has no seed phrase.
    assert!(
        f.service
            .view_seed_phrase("copy", PW)
            .unwrap_err()
            .contains("no seed phrase")
    );
}

#[test]
fn exporting_a_sui_wallet_encrypts_key_and_phrase() {
    let f = build(SuiMock::new(), |s| s);
    f.service
        .import_wallet("sui", "w", PW, Some(ABANDON_ART), None)
        .unwrap();
    let out = f
        .service
        .export_wallet("w", PW, "export-encryption-key", Some("sui.zip"))
        .unwrap();
    assert_eq!(text(&out, &["contains"]), "seed phrase and private key");
    let archive = std::fs::read(text(&out, &["file"])).unwrap();
    let raw = String::from_utf8_lossy(&archive);
    assert!(
        !raw.contains("abandon") && !raw.contains("suiprivkey"),
        "the archive must be encrypted"
    );
    // Decrypt it with the export key: the payload names the blockchain and network.
    let start = raw.find("{\"data\"").expect("sealed vault JSON in the zip");
    let end = raw[start..].find("}}}").unwrap() + start + 3;
    let sealed = vault::EncryptedVault::from_json(&json::parse(&raw[start..end]).unwrap()).unwrap();
    let plain = vault::decrypt("export-encryption-key", &sealed).unwrap();
    let doc = json::parse(std::str::from_utf8(plain.expose()).unwrap()).unwrap();
    assert_eq!(text(&doc, &["blockchain"]), "sui");
    assert_eq!(text(&doc, &["network"]), "mainnet");
    assert_eq!(text(&doc, &["mnemonic"]), ABANDON_ART);
    assert!(text(&doc, &["privateKey"]).starts_with("suiprivkey1"));
    assert_eq!(
        text(&doc, &["address"]),
        text(&f.service.get_address("w", PW).unwrap(), &["address"])
    );
}

#[test]
fn renaming_and_deleting_work_with_the_password() {
    let f = build(SuiMock::new(), |s| s);
    import(&f);
    assert!(
        f.service
            .rename_wallet("w", "renamed", "wrong-password")
            .is_err()
    );
    f.service.rename_wallet("w", "renamed", PW).unwrap();
    assert_eq!(
        text(&f.service.get_address("renamed", PW).unwrap(), &["address"]),
        wallet_address()
    );
    assert!(f.service.delete_wallet("renamed", PW, false).is_err());
    f.service.delete_wallet("renamed", PW, true).unwrap();
    assert!(f.service.get_address("renamed", PW).is_err());
}

#[test]
fn list_networks_shows_the_one_bound_network() {
    let f = build(SuiMock::new(), |s| s);
    import(&f);
    let out = f.service.view_networks("w", PW).unwrap();
    let networks = field(&out, &["networks"]).as_array().unwrap();
    assert_eq!(networks.len(), 1);
    assert_eq!(text(&networks[0], &["name"]), "mainnet");
    assert_eq!(text(&networks[0], &["chain"]), "sui:mainnet");
}

// ---- balances ---------------------------------------------------------------------------------------

fn balances_node() -> SuiMock {
    SuiMock::new()
        .data(
            "balances(first",
            &format!(
                r#"{{"address":{{"balances":{{"pageInfo":{{"hasNextPage":false}},"nodes":[
                {{"coinType":{{"repr":"{USDC}"}},"totalBalance":"2500000","coinBalance":"2500000","addressBalance":"0","coinMetadata":{{"symbol":"USDC","decimals":6}}}},
                {{"coinType":{{"repr":"{SUI_COIN_TYPE}"}},"totalBalance":"5000000000","coinBalance":"3000000000","addressBalance":"2000000000","coinMetadata":{{"symbol":"SUI","decimals":9}}}},
                {{"coinType":{{"repr":"0x1::spam::SPAM"}},"totalBalance":"0","coinBalance":"0","addressBalance":"0","coinMetadata":null}},
                {{"coinType":{{"repr":"0x2::mystery::MYST"}},"totalBalance":"7","coinBalance":"7","addressBalance":"0","coinMetadata":null}}]}}}}}}"#
            ),
        )
        .extend(node(&[], 0))
}

#[test]
fn balances_list_every_coin_with_values_and_hide_empty_ones() {
    let f = build(balances_node(), |s| s);
    import(&f);
    let out = f.service.sui_balances("w", PW).unwrap();
    let assets = field(&out, &["assets"]).as_array().unwrap();
    let symbols: Vec<&str> = assets.iter().map(|a| text(a, &["symbol"])).collect();
    assert_eq!(
        symbols,
        vec!["SUI", "USDC", "MYST"],
        "SUI first; the empty SPAM coin is hidden"
    );
    let sui = &assets[0];
    assert_eq!(text(sui, &["balance"]), "5");
    assert_eq!(text(sui, &["balance_raw"]), "5000000000");
    assert_eq!(text(sui, &["coin_objects_raw"]), "3000000000");
    assert_eq!(text(sui, &["address_balance_raw"]), "2000000000");
    assert_eq!(text(sui, &["asset"]), "native");
    assert_eq!(text(sui, &["usdc_value"]), "10.00");
    let usdc = &assets[1];
    assert_eq!(text(usdc, &["balance"]), "2.5");
    assert_eq!(text(usdc, &["usdc_value"]), "2.50");
    assert_eq!(text(usdc, &["asset"]), USDC);
    // A coin without metadata has unknown decimals and no value, and says so.
    let myst = &assets[2];
    assert_eq!(text(myst, &["balance"]), "7");
    assert_eq!(field(myst, &["decimals"]), &Value::Null);
    assert_eq!(field(myst, &["usdc_value"]), &Value::Null);
    assert!(
        field(&out, &["warnings"])
            .to_json()
            .contains("no on-chain metadata")
    );
    assert_eq!(text(&out, &["network", "name"]), "mainnet");
    assert_eq!(text(&out, &["address"]), wallet_address());
}

#[test]
fn an_empty_wallet_still_shows_zero_sui() {
    let f = build(
        SuiMock::new()
            .data("balances(first", r#"{"address":null}"#)
            .extend(node(&[], 0)),
        |s| s,
    );
    import(&f);
    let out = f.service.sui_balances("w", PW).unwrap();
    let assets = field(&out, &["assets"]).as_array().unwrap();
    assert_eq!(assets.len(), 1);
    assert_eq!(text(&assets[0], &["balance"]), "0");
}

#[test]
fn a_wrong_network_endpoint_is_refused() {
    let wrong = SuiMock::new().data(
        "chainIdentifier",
        &format!(r#"{{"chainIdentifier":"{TESTNET}"}}"#),
    );
    let f = build(wrong, |s| s);
    import(&f);
    let err = f.service.sui_balances("w", PW).unwrap_err();
    assert!(err.contains("not mainnet"), "{err}");
}

#[test]
fn testnet_balances_are_not_valued() {
    let mock = SuiMock::new()
        .data("chainIdentifier", &format!(r#"{{"chainIdentifier":"{TESTNET}"}}"#))
        .data(
            "balances(first",
            &format!(
                r#"{{"address":{{"balances":{{"pageInfo":{{"hasNextPage":false}},"nodes":[{{"coinType":{{"repr":"{SUI_COIN_TYPE}"}},"totalBalance":"1000000000","coinBalance":"1000000000","addressBalance":"0","coinMetadata":{{"symbol":"SUI","decimals":9}}}}]}}}}}}"#
            ),
        );
    let f = build(mock, |s| s);
    f.service
        .create_wallet_on("sui", "t", PW, Some("testnet"))
        .unwrap();
    let out = f.service.sui_balances("t", PW).unwrap();
    assert_eq!(
        field(
            &field(&out, &["assets"]).as_array().unwrap()[0],
            &["usdc_value"]
        ),
        &Value::Null
    );
    assert!(
        f.mock.get_urls().is_empty(),
        "no price lookups for test networks"
    );
    assert_eq!(text(&out, &["network", "name"]), "testnet");
}

// ---- sending -------------------------------------------------------------------------------------------

#[test]
fn sending_sui_dry_runs_signs_and_submits_exactly_what_it_signed() {
    let f = funded();
    let out = f
        .service
        .sui_send_asset("w", PW, "native", &other_address(), "1.5")
        .unwrap();
    assert_eq!(text(&out, &["status"]), "success");
    assert!(text(&out, &["transaction_digest"]).len() > 20);
    assert_eq!(text(&out, &["to"]), other_address());
    assert_eq!(text(&out, &["amount"]), "1.5");
    assert_eq!(text(&out, &["asset"]), "SUI");
    assert!(text(&out, &["explorer_url"]).starts_with("https://suiscan.xyz/mainnet/tx/"));
    assert_eq!(text(&out, &["gas", "net_mist"]), "1500000");
    assert_eq!(
        f.mock.post_count("simulateTransaction"),
        2,
        "a sizing dry run and a final one"
    );
    assert_eq!(f.mock.post_count("executeTransaction"), 1);

    let (transaction, signature) = submitted(&f);
    assert_signed_by_wallet(&transaction, &signature);
    assert_eq!(
        text(&out, &["transaction_digest"]),
        tx::digest_of(&transaction)
    );
    assert_eq!(transaction.sender.to_string(), wallet_address());
    // Gas budget = computation + 1000 * price + storage - rebate.
    assert_eq!(
        transaction.gas_payment.budget,
        COMPUTATION + 1000 * 100 + STORAGE - REBATE
    );
    assert_eq!(transaction.gas_payment.price, 100);
    let cmds = commands(&transaction);
    assert!(matches!(&cmds[0], Command::SplitCoins(_)));
    let TransactionKind::ProgrammableTransaction(ptb) = &transaction.kind else {
        panic!()
    };
    // The amount is exactly 1.5 SUI and the recipient is the one asked for.
    assert!(
        ptb.inputs
            .iter()
            .any(|i| matches!(i, Input::Pure(b) if b == &1_500_000_000u64.to_le_bytes().to_vec()))
    );
    let recipient = other_address().parse::<sui_sdk_types::Address>().unwrap();
    assert!(
        ptb.inputs
            .iter()
            .any(|i| matches!(i, Input::Pure(b) if b.as_slice() == recipient.as_bytes()))
    );
}

#[test]
fn the_wallet_is_locked_again_after_sending() {
    let f = funded();
    f.service
        .sui_send_asset("w", PW, "native", &other_address(), "1")
        .unwrap();
    assert_eq!(
        f.service.get_address("w", "wrong-password").unwrap_err(),
        "incorrect password"
    );
}

#[test]
fn sending_a_token_uses_its_decimals_and_coin_objects() {
    let f = funded();
    let out = f
        .service
        .sui_send_asset("w", PW, USDC, &other_address(), "1.25")
        .unwrap();
    assert_eq!(text(&out, &["asset"]), "USDC");
    assert_eq!(text(&out, &["amount"]), "1.25");
    let (transaction, signature) = submitted(&f);
    assert_signed_by_wallet(&transaction, &signature);
    let TransactionKind::ProgrammableTransaction(ptb) = &transaction.kind else {
        panic!()
    };
    assert!(
        ptb.inputs
            .iter()
            .any(|i| matches!(i, Input::Pure(b) if b == &1_250_000u64.to_le_bytes().to_vec()))
    );
    assert!(matches!(&commands(&transaction)[0], Command::SplitCoins(_)));
}

#[test]
fn a_wallet_holding_only_an_address_balance_pays_gas_from_it() {
    let f = build(node(&[], 10_000_000_000), |s| s);
    import(&f);
    f.service
        .sui_send_asset("w", PW, "native", &other_address(), "1")
        .unwrap();
    let (transaction, signature) = submitted(&f);
    assert_signed_by_wallet(&transaction, &signature);
    assert!(transaction.gas_payment.objects.is_empty());
    assert!(matches!(
        transaction.expiration,
        sui_sdk_types::TransactionExpiration::ValidDuring {
            min_epoch: Some(1290),
            max_epoch: Some(1290),
            ..
        }
    ));
}

#[test]
fn sending_is_validated_before_anything_is_signed() {
    let f = funded();
    let send =
        |asset: &str, to: &str, amount: &str| f.service.sui_send_asset("w", PW, asset, to, amount);
    for (asset, to, amount, expect) in [
        ("native", "0x2", "1", "64 hex"),
        ("native", &other_address()[..60], "1", "64 hex"),
        (
            "native",
            &format!("0x{}", "00".repeat(32)),
            "1",
            "zero address",
        ),
        ("native", &other_address(), "0", "greater than zero"),
        ("native", &other_address(), "-1", "amount"),
        ("native", &other_address(), "abc", "amount"),
        ("native", &other_address(), "0.0000000001", "amount"),
        ("bogus", &other_address(), "1", "'asset'"),
        ("native", &other_address(), "50", "insufficient"),
        (USDC, &other_address(), "9999", "insufficient"),
    ] {
        let err = send(asset, to, amount).unwrap_err();
        assert!(err.contains(expect), "{asset} {to} {amount}: {err}");
    }
    assert_eq!(f.mock.post_count("executeTransaction"), 0);
    assert_eq!(
        f.service
            .sui_send_asset("w", "", "native", &other_address(), "1")
            .unwrap_err(),
        "password is required"
    );
}

#[test]
fn coins_without_metadata_cannot_be_sent_because_their_decimals_are_unknown() {
    let mock = SuiMock::new()
        .data("coinMetadata(coinType", r#"{"coinMetadata":null}"#)
        .extend(node(&[(1, 5_000_000_000)], 0));
    let f = build(mock, |s| s);
    import(&f);
    let err = f
        .service
        .sui_send_asset("w", PW, "0x2::mystery::MYST", &other_address(), "1")
        .unwrap_err();
    assert!(err.contains("no on-chain metadata"), "{err}");
    assert_eq!(f.mock.post_count("executeTransaction"), 0);
}

#[test]
fn a_transaction_that_would_fail_is_never_signed() {
    let failing = effects_data(
        "simulateTransaction",
        "FAILURE",
        Some("InsufficientCoinBalance"),
        &[],
    );
    let f = build(
        SuiMock::new()
            .data("simulateTransaction", &failing)
            .extend(node(&[(1, 5_000_000_000)], 0)),
        |s| s,
    );
    import(&f);
    let err = f
        .service
        .sui_send_asset("w", PW, "native", &other_address(), "1")
        .unwrap_err();
    assert!(
        err.contains("would fail") && err.contains("InsufficientCoinBalance"),
        "{err}"
    );
    assert_eq!(f.mock.post_count("executeTransaction"), 0);
}

#[test]
fn a_rejected_submission_is_reported_as_not_executed() {
    let f = build(
        SuiMock::new()
            .errors(
                "executeTransaction",
                "Transaction validator signing failed: object version stale",
            )
            .data("transaction(digest", r#"{"transaction":null}"#)
            .extend(node(&[(1, 5_000_000_000)], 0)),
        |s| s,
    );
    import(&f);
    let err = f
        .service
        .sui_send_asset("w", PW, "native", &other_address(), "1")
        .unwrap_err();
    assert!(
        err.starts_with("the Sui network rejected the transaction:"),
        "{err}"
    );
    assert!(err.contains("stale") && err.contains("not found on-chain"));
    assert_eq!(f.mock.post_count("transaction(digest"), 1);
}

#[test]
fn a_node_error_for_a_transaction_that_did_execute_is_not_called_a_rejection() {
    let tx_found = r#"{"transaction":{"digest":"X","sender":{"address":"0xabc"},"effects":{"digest":"X","status":"SUCCESS","executionError":null,"timestamp":null,"epoch":null,"checkpoint":null,"gasEffects":{"gasSummary":{"computationCost":1,"storageCost":1,"storageRebate":0}},"balanceChangesJson":[]}}}"#;
    let f = build(
        SuiMock::new()
            .errors(
                "executeTransaction",
                "request timed out waiting for finality",
            )
            .data("transaction(digest", tx_found)
            .extend(node(&[(1, 5_000_000_000)], 0)),
        |s| s,
    );
    import(&f);
    let out = f
        .service
        .sui_send_asset("w", PW, "native", &other_address(), "1")
        .unwrap();
    assert_eq!(text(&out, &["status"]), "success");
    // With no way to look it up the outcome is unknown, not "rejected".
    let g = build(
        SuiMock::new()
            .errors("executeTransaction", "timed out")
            .errors("transaction(digest", "node overloaded")
            .extend(node(&[(1, 5_000_000_000)], 0)),
        |s| s,
    );
    import(&g);
    let err = g
        .service
        .sui_send_asset("w", PW, "native", &other_address(), "1")
        .unwrap_err();
    assert!(err.contains("outcome is unknown"), "{err}");
}

#[test]
fn an_on_chain_failure_is_reported_not_hidden() {
    let f = build(
        SuiMock::new()
            .data(
                "executeTransaction",
                &effects_data(
                    "executeTransaction",
                    "FAILURE",
                    Some("MoveAbort 3"),
                    &[sui_loss(900_000)],
                ),
            )
            .extend(node(&[(1, 5_000_000_000)], 0)),
        |s| s,
    );
    import(&f);
    let out = f
        .service
        .sui_send_asset("w", PW, "native", &other_address(), "1")
        .unwrap();
    assert_eq!(text(&out, &["status"]), "failed");
    assert_eq!(text(&out, &["error"]), "MoveAbort 3");
}

fn digest_lookup_node(found: bool) -> SuiMock {
    let tx = if found {
        r#"{"transaction":{"digest":"X","sender":{"address":"0xabc"},"effects":{"digest":"X","status":"SUCCESS","executionError":null,"timestamp":null,"epoch":null,"checkpoint":null,"gasEffects":{"gasSummary":{"computationCost":1,"storageCost":1,"storageRebate":0}},"balanceChangesJson":[]}}}"#
    } else {
        r#"{"transaction":null}"#
    };
    SuiMock::new()
        .net_error("executeTransaction", "request failed: timed out")
        .data("transaction(digest", tx)
        .extend(node(&[(1, 5_000_000_000)], 0))
}

#[test]
fn a_lost_answer_is_resolved_by_looking_the_transaction_up() {
    let f = build(digest_lookup_node(true), |s| s);
    import(&f);
    let out = f
        .service
        .sui_send_asset("w", PW, "native", &other_address(), "1")
        .unwrap();
    assert_eq!(text(&out, &["status"]), "success");
    let (transaction, _) = submitted(&f);
    assert!(f.mock.bodies("transaction(digest")[0].contains(&tx::digest_of(&transaction)));
}

#[test]
fn if_the_outcome_stays_unknown_the_digest_is_returned_and_retrying_is_discouraged() {
    let f = build(digest_lookup_node(false), |s| s);
    import(&f);
    let err = f
        .service
        .sui_send_asset("w", PW, "native", &other_address(), "1")
        .unwrap_err();
    let (transaction, _) = submitted(&f);
    assert!(
        err.contains("outcome is unknown") && err.contains(&tx::digest_of(&transaction)),
        "{err}"
    );
    assert!(err.contains("get_transaction"));
}

// ---- signing --------------------------------------------------------------------------------------------

fn own_transaction(budget: u64) -> sui_sdk_types::Transaction {
    let sui = tx::Holdings {
        coins: vec![crate::sui::gql::CoinObject {
            id: sui_sdk_types::Address::new([1; 32]),
            version: 7,
            digest: sui_sdk_types::Digest::new([2; 32]),
            balance: 5_000_000_000,
        }],
        address_balance: 0,
    };
    let env = tx::GasEnv {
        price: 100,
        epoch: 1290,
        chain: MAINNET.parse().unwrap(),
        nonce: 1,
    };
    tx::assemble(
        vault().address().unwrap(),
        &env,
        budget,
        &sui,
        SUI_COIN_TYPE,
        1_000,
        &sui,
        &tx::transfer_body(other_address().parse().unwrap()),
    )
    .unwrap()
    .0
}

#[test]
fn signing_transaction_bytes_returns_the_wallet_standard_output() {
    let f = funded();
    let transaction = own_transaction(5_000_000);
    let bytes = tx::encode(&transaction).unwrap();
    let out = f
        .service
        .sui_sign_transaction("w", PW, &bytes, false)
        .unwrap();
    assert_eq!(text(&out, &["status"]), "signed");
    assert_eq!(text(&out, &["bytes"]), bytes);
    assert_eq!(
        text(&out, &["transaction_digest"]),
        tx::digest_of(&transaction)
    );
    let signature = UserSignature::from_base64(text(&out, &["signature"])).unwrap();
    assert_signed_by_wallet(&transaction, &signature);
    assert_eq!(text(&out, &["dry_run", "status"]), "success");
    assert!(
        field(&out, &["description", "commands"])
            .as_array()
            .unwrap()
            .len()
            == 2
    );
    assert_eq!(
        f.mock.post_count("executeTransaction"),
        0,
        "signing alone sends nothing"
    );
}

#[test]
fn signing_with_broadcast_submits_after_a_successful_dry_run() {
    let f = funded();
    let transaction = own_transaction(5_000_000);
    let out = f
        .service
        .sui_sign_transaction("w", PW, &tx::encode(&transaction).unwrap(), true)
        .unwrap();
    assert_eq!(text(&out, &["status"]), "submitted");
    assert_eq!(text(&out, &["execution", "status"]), "success");
    let (sent, signature) = submitted(&f);
    assert_eq!(sent, transaction);
    assert_signed_by_wallet(&sent, &signature);
}

#[test]
fn broadcast_is_refused_when_the_dry_run_fails_but_plain_signing_warns() {
    let failing = effects_data(
        "simulateTransaction",
        "FAILURE",
        Some("ObjectVersionStale"),
        &[],
    );
    let f = build(
        SuiMock::new()
            .data("simulateTransaction", &failing)
            .extend(node(&[(1, 5_000_000_000)], 0)),
        |s| s,
    );
    import(&f);
    let bytes = tx::encode(&own_transaction(5_000_000)).unwrap();
    let err = f
        .service
        .sui_sign_transaction("w", PW, &bytes, true)
        .unwrap_err();
    assert!(
        err.contains("dry run") && err.contains("ObjectVersionStale"),
        "{err}"
    );
    assert_eq!(f.mock.post_count("executeTransaction"), 0);
    let signed = f
        .service
        .sui_sign_transaction("w", PW, &bytes, false)
        .unwrap();
    assert_eq!(text(&signed, &["status"]), "signed");
    assert!(
        field(&signed, &["warnings"])
            .to_json()
            .contains("dry run of this transaction fails")
    );
}

#[test]
fn an_unavailable_dry_run_blocks_broadcast_only() {
    let f = build(
        SuiMock::new()
            .errors("simulateTransaction", "node overloaded")
            .extend(node(&[(1, 5_000_000_000)], 0)),
        |s| s,
    );
    import(&f);
    let bytes = tx::encode(&own_transaction(5_000_000)).unwrap();
    assert!(
        f.service
            .sui_sign_transaction("w", PW, &bytes, true)
            .is_err()
    );
    let signed = f
        .service
        .sui_sign_transaction("w", PW, &bytes, false)
        .unwrap();
    assert_eq!(text(&signed, &["dry_run", "status"]), "unavailable");
}

#[test]
fn transactions_from_other_senders_chains_or_sponsors_are_refused() {
    let f = funded();
    // Another sender.
    let mut other = own_transaction(5_000_000);
    other.sender = sui_sdk_types::Address::new([9; 32]);
    other.gas_payment.owner = other.sender;
    let err = f
        .service
        .sui_sign_transaction("w", PW, &tx::encode(&other).unwrap(), false)
        .unwrap_err();
    assert!(err.contains("sender is not this wallet"), "{err}");
    // A sponsored transaction.
    let mut sponsored = own_transaction(5_000_000);
    sponsored.gas_payment.owner = sui_sdk_types::Address::new([9; 32]);
    let err = f
        .service
        .sui_sign_transaction("w", PW, &tx::encode(&sponsored).unwrap(), false)
        .unwrap_err();
    assert!(err.contains("gas owner"), "{err}");
    // A transaction valid on another network.
    let mut foreign = own_transaction(5_000_000);
    foreign.expiration = sui_sdk_types::TransactionExpiration::ValidDuring {
        min_epoch: Some(1),
        max_epoch: Some(1),
        min_timestamp: None,
        max_timestamp: None,
        chain: TESTNET.parse().unwrap(),
        nonce: 1,
    };
    let err = f
        .service
        .sui_sign_transaction("w", PW, &tx::encode(&foreign).unwrap(), false)
        .unwrap_err();
    assert!(err.contains("different Sui network"), "{err}");
    // Garbage.
    for bad in ["", "not base64!", "AAAA"] {
        assert!(
            f.service.sui_sign_transaction("w", PW, bad, false).is_err(),
            "{bad:?}"
        );
    }
    assert_eq!(f.mock.post_count("executeTransaction"), 0);
}

#[test]
fn personal_messages_are_signed_with_the_personal_message_intent() {
    let f = build(SuiMock::new(), |s| s);
    import(&f);
    let out = f
        .service
        .sui_sign_message("w", PW, "hello sui", None)
        .unwrap();
    assert_eq!(text(&out, &["message_preview"]), "hello sui");
    assert_eq!(text(&out, &["address"]), wallet_address());
    assert_eq!(text(&out, &["intent"]), "PersonalMessage");
    assert_eq!(
        base64::decode(text(&out, &["bytes"])).unwrap(),
        b"hello sui"
    );
    let signature = UserSignature::from_base64(text(&out, &["signature"])).unwrap();
    let key = vault().signer().unwrap();
    let verifier = Ed25519VerifyingKey::new(&key.public_key()).unwrap();
    verifier
        .verify_personal_message(
            &sui_sdk_types::PersonalMessage(b"hello sui".as_slice().into()),
            &signature,
        )
        .unwrap();
    let hex_signed = f
        .service
        .sui_sign_message("w", PW, "0xdeadbeef", Some("hex"))
        .unwrap();
    assert_eq!(text(&hex_signed, &["message_preview"]), "(binary data)");
    assert_eq!(
        base64::decode(text(&hex_signed, &["bytes"])).unwrap(),
        vec![0xde, 0xad, 0xbe, 0xef]
    );
    assert!(f.service.sui_sign_message("w", PW, "", None).is_err());
    assert!(
        f.service
            .sui_sign_message("w", "wrong-password", "x", None)
            .is_err()
    );
    assert!(
        f.mock.requests.borrow().is_empty(),
        "message signing needs no network access"
    );
}

// ---- estimating, simulating and looking things up ---------------------------------------------------------

fn transfer_params(amount: &str) -> IntentParams {
    IntentParams {
        to: Some(other_address()),
        asset: Some("native".to_string()),
        amount: Some(amount.to_string()),
        ..IntentParams::default()
    }
}

#[test]
fn gas_estimates_come_from_a_dry_run_and_sign_nothing() {
    let f = funded();
    let out = f
        .service
        .sui_estimate_gas("w", PW, &transfer_params("1"))
        .unwrap();
    assert_eq!(field(&out, &["will_succeed"]), &Value::Bool(true));
    assert_eq!(text(&out, &["gas", "net_mist"]), "1500000");
    assert_eq!(text(&out, &["gas", "net_sui"]), "0.0015");
    assert_eq!(text(&out, &["fee_usdc"]), "0.0030");
    assert_eq!(
        text(&out, &["gas_budget_mist"]),
        (COMPUTATION + 100_000 + STORAGE - REBATE).to_string()
    );
    assert_eq!(f.mock.post_count("executeTransaction"), 0);
}

#[test]
fn estimating_given_transaction_bytes() {
    let f = funded();
    let params = IntentParams {
        transaction_bytes: Some(tx::encode(&own_transaction(7_000_000)).unwrap()),
        ..IntentParams::default()
    };
    let out = f.service.sui_estimate_gas("w", PW, &params).unwrap();
    assert_eq!(text(&out, &["gas_budget_mist"]), "7000000");
    assert_eq!(
        field(&out, &["decoded", "transaction", "commands"])
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn simulating_reports_the_wallets_balance_changes() {
    let f = funded();
    let out = f
        .service
        .sui_simulate_transaction("w", PW, &transfer_params("1"))
        .unwrap();
    assert_eq!(field(&out, &["success"]), &Value::Bool(true));
    let changes = field(&out, &["result", "wallet_changes"])
        .as_array()
        .unwrap();
    assert_eq!(text(&changes[0], &["symbol"]), "SUI");
    assert_eq!(text(&changes[0], &["change"]), "-0.0015");
}

#[test]
fn a_failing_simulation_is_reported_not_raised_for_given_bytes() {
    let failing = effects_data("simulateTransaction", "FAILURE", Some("MoveAbort 9"), &[]);
    let f = build(
        SuiMock::new()
            .data("simulateTransaction", &failing)
            .extend(node(&[(1, 5_000_000_000)], 0)),
        |s| s,
    );
    import(&f);
    let params = IntentParams {
        transaction_bytes: Some(tx::encode(&own_transaction(5_000_000)).unwrap()),
        ..IntentParams::default()
    };
    let out = f
        .service
        .sui_simulate_transaction("w", PW, &params)
        .unwrap();
    assert_eq!(field(&out, &["success"]), &Value::Bool(false));
    assert_eq!(text(&out, &["result", "error"]), "MoveAbort 9");
}

#[test]
fn estimate_and_simulate_inputs_are_checked() {
    let f = funded();
    let bytes = tx::encode(&own_transaction(5_000_000)).unwrap();
    for (params, expect) in [
        (
            IntentParams {
                value: Some("1".into()),
                ..transfer_params("1")
            },
            "Ethereum parameters",
        ),
        (
            IntentParams {
                data: Some("0x00".into()),
                ..transfer_params("1")
            },
            "Ethereum parameters",
        ),
        (
            IntentParams {
                transaction_bytes: Some(bytes.clone()),
                ..transfer_params("1")
            },
            "not both",
        ),
        (
            IntentParams {
                to: None,
                ..transfer_params("1")
            },
            "'to' is required",
        ),
        (
            IntentParams {
                asset: None,
                ..transfer_params("1")
            },
            "'asset' is required",
        ),
        (
            IntentParams {
                amount: None,
                ..transfer_params("1")
            },
            "'amount' is required",
        ),
    ] {
        let err = f.service.sui_estimate_gas("w", PW, &params).unwrap_err();
        assert!(err.contains(expect), "{expect}: {err}");
        assert!(
            f.service
                .sui_simulate_transaction("w", PW, &params)
                .is_err()
        );
    }
}

#[test]
fn looking_up_a_transaction_and_the_history() {
    let me = wallet_address();
    let tx_json = |digest: &str| {
        format!(
            r#"{{"digest":"{digest}","sender":{{"address":"{me}"}},"effects":{{"digest":"{digest}","status":"SUCCESS","executionError":null,"timestamp":"2026-10-24T01:00:00Z","epoch":{{"epochId":7}},"checkpoint":{{"sequenceNumber":99}},"gasEffects":{{"gasSummary":{{"computationCost":1000,"storageCost":2000,"storageRebate":500}}}},"balanceChangesJson":[{}]}}}}"#,
            change(&me, SUI_COIN_TYPE, -2_500_000_000)
        )
    };
    let digest = tx::digest_of(&own_transaction(5_000_000));
    let mock = SuiMock::new()
        .data(
            "transaction(digest",
            &format!(r#"{{"transaction":{}}}"#, tx_json(&digest)),
        )
        .data(
            "transactions(last",
            &format!(
                r#"{{"transactions":{{"pageInfo":{{"hasPreviousPage":false}},"nodes":[{},{}]}}}}"#,
                tx_json("OLDER"),
                tx_json("NEWER")
            ),
        )
        .extend(node(&[], 0));
    let f = build(mock, |s| s);
    import(&f);
    let out = f.service.sui_get_transaction("w", PW, &digest).unwrap();
    assert_eq!(text(&out, &["status"]), "success");
    assert_eq!(text(&out, &["checkpoint"]), "99");
    assert_eq!(text(&out, &["epoch"]), "7");
    assert_eq!(text(&out, &["network"]), "mainnet");
    assert_eq!(text(&out, &["gas", "net_mist"]), "2500");
    let changes = field(&out, &["wallet_changes"]).as_array().unwrap();
    assert_eq!(text(&changes[0], &["change"]), "-2.5");
    assert!(text(&out, &["explorer_url"]).ends_with(&digest));

    let history = f
        .service
        .sui_history("w", PW, HistoryKind::Transactions, 10)
        .unwrap();
    assert_eq!(field(&history, &["count"]).as_u64(), Some(2));
    let items = field(&history, &["items"]).as_array().unwrap();
    assert_eq!(
        text(&items[0], &["transaction_digest"]),
        "NEWER",
        "newest first"
    );
    assert_eq!(field(&history, &["has_more"]), &Value::Bool(false));
    let err = f
        .service
        .sui_history("w", PW, HistoryKind::TokenTransfers, 10)
        .unwrap_err();
    assert!(err.contains("Ethereum option"));
}

#[test]
fn unknown_or_malformed_digests_are_explained() {
    let f = build(
        SuiMock::new()
            .data("transaction(digest", r#"{"transaction":null}"#)
            .extend(node(&[], 0)),
        |s| s,
    );
    import(&f);
    let missing = tx::digest_of(&own_transaction(5_000_000));
    let err = f
        .service
        .sui_get_transaction("w", PW, &missing)
        .unwrap_err();
    assert!(err.contains("has no transaction"), "{err}");
    for bad in ["", "0x1234", "not a digest!", "abc"] {
        let err = f.service.sui_get_transaction("w", PW, bad).unwrap_err();
        assert!(err.contains("base58"), "{bad}: {err}");
    }
}

// ---- swaps -------------------------------------------------------------------------------------------------

fn swap_params() -> SwapParams {
    SwapParams {
        from_token: "native".to_string(),
        to_token: USDC.to_string(),
        amount: "1".to_string(),
        ..SwapParams::default()
    }
}

/// A node that also serves the Cetus aggregator for a 1 SUI to USDC route (960871 USDC units).
fn swap_node(simulated_usdc: i128) -> SuiMock {
    let me = wallet_address();
    let effects = effects_data(
        "simulateTransaction",
        "SUCCESS",
        None,
        &[sui_loss(1_001_500_000), change(&me, USDC, simulated_usdc)],
    );
    SuiMock::new()
        .data("simulateTransaction", &effects)
        .get("simple/price?ids=sui", r#"{"sui":{"usd":0.97}}"#)
        .get("find_routes", &api_answer(962_476_346, 960_871))
        .extend(chain(&pool_type(BUCK, SUI_COIN_TYPE)))
        .extend(node(&[(1, 5_000_000_000)], 0))
}

#[test]
fn a_quote_far_below_the_market_price_is_refused_even_if_the_api_calls_it_fine() {
    // CoinGecko says 1 SUI is worth 2 USD, but the aggregator offers 0.96 USDC.
    let f = swap_service(
        SuiMock::new()
            .get("simple/price?ids=sui", r#"{"sui":{"usd":2.0}}"#)
            .extend(swap_node(955_000)),
        None,
    );
    let err = f
        .service
        .sui_swap_tokens("w", PW, &swap_params())
        .unwrap_err();
    assert!(err.contains("below the market price"), "{err}");
    assert_eq!(f.mock.post_count("executeTransaction"), 0);
    let quote_err = f
        .service
        .sui_swap_quote("w", PW, &swap_params())
        .unwrap_err();
    assert!(quote_err.contains("below the market price"));
}

#[test]
fn without_an_independent_price_a_swap_needs_min_receive() {
    let f = swap_service(
        SuiMock::new()
            .get_error("simple/price", "boom")
            .extend(swap_node(955_000)),
        None,
    );
    let err = f
        .service
        .sui_swap_tokens("w", PW, &swap_params())
        .unwrap_err();
    assert!(err.contains("needs min_receive"), "{err}");
    assert_eq!(f.mock.post_count("executeTransaction"), 0);
    let q = f.service.sui_swap_quote("w", PW, &swap_params()).unwrap();
    assert!(text(&q, &["price_check"]).contains("not checked"));
    let with_floor = SwapParams {
        min_receive: Some("0.9".to_string()),
        ..swap_params()
    };
    assert!(f.service.sui_swap_tokens("w", PW, &with_floor).is_ok());
}

#[test]
fn a_checked_price_is_reported_and_needs_no_floor() {
    let f = swap_service(swap_node(955_000), None);
    let q = f.service.sui_swap_quote("w", PW, &swap_params()).unwrap();
    assert!(text(&q, &["price_check"]).contains("within 5%"));
}

fn swap_service(mock: SuiMock, fee: Option<crate::sui::cetus::SuiSwapFee>) -> Fx {
    let f = build(mock, |s| {
        s.with_cetus_settings(crate::sui::cetus::CetusSettings::new(fee))
    });
    import(&f);
    f
}

fn fee() -> crate::sui::cetus::SuiSwapFee {
    crate::sui::cetus::SuiSwapFee::new(&format!("0x{}", "ab".repeat(32)), 70).unwrap()
}

#[test]
fn a_swap_quote_previews_amounts_fees_and_a_dry_run_without_signing() {
    let f = swap_service(swap_node(955_000), Some(fee()));
    let q = f.service.sui_swap_quote("w", PW, &swap_params()).unwrap();
    assert_eq!(text(&q, &["aggregator"]), "Cetus");
    assert_eq!(text(&q, &["from", "symbol"]), "SUI");
    assert_eq!(text(&q, &["from", "amount"]), "1");
    assert_eq!(text(&q, &["from", "token"]), "native");
    assert_eq!(text(&q, &["to", "symbol"]), "USDC");
    // 960871 less the 0.7% fee (6726) = 954145 expected; 1% slippage gives 944603.
    assert_eq!(text(&q, &["to", "expected_amount_raw"]), "954145");
    assert_eq!(text(&q, &["to", "minimum_amount_raw"]), "944603");
    assert_eq!(text(&q, &["to", "expected_amount"]), "0.954145");
    assert_eq!(text(&q, &["fees", "service_fee", "percent"]), "0.7");
    assert_eq!(text(&q, &["fees", "service_fee", "amount"]), "0.006726");
    assert_eq!(
        text(&q, &["fees", "service_fee", "recipient"]),
        format!("0x{}", "ab".repeat(32))
    );
    assert_eq!(text(&q, &["dry_run", "would_receive"]), "0.955");
    assert_eq!(field(&q, &["dry_run", "success"]), &Value::Bool(true));
    assert_eq!(text(&q, &["fees", "network_fee", "net_mist"]), "1500000");
    assert_eq!(field(&q, &["route"]).as_array().unwrap().len(), 2);
    assert_eq!(text(&q, &["wallet"]), "w");
    assert_eq!(f.mock.post_count("executeTransaction"), 0);
    let asked = f.mock.get_urls();
    assert!(
        asked
            .iter()
            .any(|u| u.contains("providers=CETUS") && u.contains("amount=1000000000")),
        "{asked:?}"
    );
}

#[test]
fn a_quote_without_a_fee_recipient_charges_no_fee() {
    let f = swap_service(swap_node(955_000), None);
    let q = f.service.sui_swap_quote("w", PW, &swap_params()).unwrap();
    assert_eq!(field(&q, &["fees", "service_fee"]), &Value::Null);
    assert_eq!(text(&q, &["to", "expected_amount_raw"]), "960871");
}

#[test]
fn a_quote_reports_but_does_not_fail_on_low_balance() {
    let f = build(
        SuiMock::new()
            .get("simple/price?ids=sui", r#"{"sui":{"usd":0.97}}"#)
            .get("find_routes", &api_answer(962_476_346, 960_871))
            .extend(chain(&pool_type(BUCK, SUI_COIN_TYPE)))
            .extend(node(&[(1, 400_000_000)], 0)),
        |s| s,
    );
    import(&f);
    let q = f.service.sui_swap_quote("w", PW, &swap_params()).unwrap();
    assert!(
        field(&q, &["warnings"])
            .to_json()
            .contains("insufficient SUI balance")
    );
    assert_eq!(field(&q, &["dry_run", "success"]), &Value::Bool(false));
    assert_eq!(f.mock.post_count("simulateTransaction"), 0);
}

#[test]
fn swapping_builds_signs_and_submits_the_aggregators_transaction() {
    let f = swap_service(swap_node(955_000), Some(fee()));
    let out = f.service.sui_swap_tokens("w", PW, &swap_params()).unwrap();
    assert_eq!(text(&out, &["status"]), "submitted");
    assert_eq!(text(&out, &["transaction", "status"]), "success");
    assert_eq!(f.mock.post_count("executeTransaction"), 1);
    let (transaction, signature) = submitted(&f);
    assert_signed_by_wallet(&transaction, &signature);
    assert_eq!(
        text(&out, &["transaction", "transaction_digest"]),
        tx::digest_of(&transaction)
    );

    let calls: Vec<String> = commands(&transaction)
        .iter()
        .map(|c| match c {
            Command::MoveCall(m) => format!("{}::{}", m.module.as_str(), m.function.as_str()),
            Command::SplitCoins(_) => "split".to_string(),
            Command::TransferObjects(_) => "transfer".to_string(),
            _ => "other".to_string(),
        })
        .collect();
    assert_eq!(
        calls,
        vec![
            "split",
            "router::new_swap_context_v3",
            "cetus::swap",
            "cetus::swap",
            "router::confirm_swap_v3",
            "transfer"
        ]
    );
    let TransactionKind::ProgrammableTransaction(ptb) = &transaction.kind else {
        panic!()
    };
    let Command::MoveCall(open) = &ptb.commands[1] else {
        panic!()
    };
    assert_eq!(open.package.to_string(), AGG_LATEST);
    let Command::MoveCall(hop) = &ptb.commands[2] else {
        panic!()
    };
    assert_eq!(hop.package.to_string(), ROUTER_LATEST);
    // The fee rate (7000 millionths) and the fee recipient are in the transaction; so is the
    // expected output, and the amount sold.
    let pure = |bytes: Vec<u8>| {
        ptb.inputs
            .iter()
            .any(|i| matches!(i, Input::Pure(b) if b == &bytes))
    };
    assert!(pure(7000u32.to_le_bytes().to_vec()), "fee rate");
    assert!(pure(vec![0xab; 32]), "fee recipient");
    assert!(
        pure(954_145u64.to_le_bytes().to_vec()),
        "quoted output net of the fee"
    );
    assert!(
        pure(10_000u64.to_le_bytes().to_vec()),
        "slippage 1% in millionths"
    );
    assert!(pure(1_000_000_000u64.to_le_bytes().to_vec()), "amount sold");
    assert!(
        pure({
            let mut v = vec![b"quote-1".len() as u8];
            v.extend_from_slice(b"quote-1");
            v
        }),
        "quote id"
    );
    // The bought coins go to the wallet.
    let wallet = vault().address().unwrap();
    assert!(
        ptb.inputs
            .iter()
            .any(|i| matches!(i, Input::Pure(b) if b.as_slice() == wallet.as_bytes()))
    );
}

#[test]
fn a_swap_can_deliver_to_another_address() {
    let params = SwapParams {
        to_address: Some(other_address()),
        ..swap_params()
    };
    // The dry run shows the other address receiving the coins, not the wallet.
    let other = other_address();
    let mock_effects = effects_data(
        "simulateTransaction",
        "SUCCESS",
        None,
        &[sui_loss(1_001_500_000), change(&other, USDC, 960_871)],
    );
    let g = swap_service(
        SuiMock::new()
            .data("simulateTransaction", &mock_effects)
            .extend(swap_node(0)),
        None,
    );
    let out = g.service.sui_swap_tokens("w", PW, &params).unwrap();
    assert_eq!(text(&out, &["to", "recipient"]), other);
    assert!(
        field(&out, &["warnings"])
            .to_json()
            .contains("different address")
    );
    let (transaction, _) = submitted(&g);
    let TransactionKind::ProgrammableTransaction(ptb) = &transaction.kind else {
        panic!()
    };
    let recipient = other.parse::<sui_sdk_types::Address>().unwrap();
    assert!(
        ptb.inputs
            .iter()
            .any(|i| matches!(i, Input::Pure(b) if b.as_slice() == recipient.as_bytes()))
    );
}

#[test]
fn the_minimum_the_dry_run_must_meet_blocks_a_bad_swap() {
    let f = swap_service(swap_node(900_000), None);
    let err = f
        .service
        .sui_swap_tokens("w", PW, &swap_params())
        .unwrap_err();
    assert!(
        err.contains("dry run shows the swap would deliver only 0.9")
            && err.contains("nothing was signed"),
        "{err}"
    );
    assert_eq!(f.mock.post_count("executeTransaction"), 0);
}

#[test]
fn min_receive_is_a_floor_on_the_guaranteed_minimum() {
    let f = swap_service(swap_node(955_000), None);
    let strict = SwapParams {
        min_receive: Some("0.96".to_string()),
        ..swap_params()
    };
    let err = f.service.sui_swap_tokens("w", PW, &strict).unwrap_err();
    assert!(err.contains("below your min_receive"), "{err}");
    assert_eq!(f.mock.post_count("executeTransaction"), 0);
    let fine = SwapParams {
        min_receive: Some("0.95".to_string()),
        ..swap_params()
    };
    assert!(f.service.sui_swap_tokens("w", PW, &fine).is_ok());
}

#[test]
fn insufficient_balance_stops_a_swap_before_any_signing() {
    let f = build(
        SuiMock::new()
            .get("simple/price?ids=sui", r#"{"sui":{"usd":0.97}}"#)
            .get("find_routes", &api_answer(962_476_346, 960_871))
            .extend(chain(&pool_type(BUCK, SUI_COIN_TYPE)))
            .extend(node(&[(1, 400_000_000)], 0)),
        |s| s,
    );
    import(&f);
    let err = f
        .service
        .sui_swap_tokens("w", PW, &swap_params())
        .unwrap_err();
    assert!(err.contains("insufficient SUI balance"), "{err}");
    assert_eq!(f.mock.post_count("simulateTransaction"), 0);
    assert_eq!(f.mock.post_count("executeTransaction"), 0);
}

#[test]
fn swap_requests_are_validated_before_any_request() {
    let f = swap_service(swap_node(955_000), None);
    let p = |change: &dyn Fn(&mut SwapParams)| {
        let mut p = swap_params();
        change(&mut p);
        f.service.sui_swap_quote("w", PW, &p).unwrap_err()
    };
    assert!(p(&|p| p.from_chain_id = 1).contains("Ethereum parameters"));
    assert!(p(&|p| p.to_chain_id = 1).contains("Ethereum parameters"));
    assert!(p(&|p| p.to_token = "native".into()).contains("same coin"));
    assert!(p(&|p| p.from_token = "nonsense".into()).contains("'from_token'"));
    assert!(p(&|p| p.amount = "0".into()).contains("'amount'"));
    assert!(p(&|p| p.slippage_bps = Some(2)).contains("slippage_bps"));
    assert!(p(&|p| p.slippage_bps = Some(5000)).contains("slippage_bps"));
    assert!(p(&|p| p.to_address = Some("0x1".into())).contains("'to_address'"));
    assert!(p(&|p| p.to_address = Some(format!("0x{}", "00".repeat(32)))).contains("zero address"));
    assert!(p(&|p| p.min_receive = Some("x".into())).contains("'min_receive'"));
    assert!(
        f.mock.get_urls().is_empty(),
        "nothing was asked of the aggregator"
    );
}

#[test]
fn swaps_are_mainnet_only() {
    let mock = SuiMock::new().data(
        "chainIdentifier",
        &format!(r#"{{"chainIdentifier":"{TESTNET}"}}"#),
    );
    let f = build(mock, |s| s);
    f.service
        .create_wallet_on("sui", "t", PW, Some("testnet"))
        .unwrap();
    let err = f
        .service
        .sui_swap_quote("t", PW, &swap_params())
        .unwrap_err();
    assert!(
        err.contains("only available on Sui mainnet") && err.contains("testnet"),
        "{err}"
    );
    assert!(f.mock.get_urls().is_empty());
}

#[test]
fn a_dishonest_aggregator_cannot_redirect_the_swap() {
    // The route claims a hop through a package that is not Cetus's current router.
    let tampered =
        api_answer(962_476_346, 960_871).replace(ROUTER_LATEST, &format!("0x{}", "99".repeat(32)));
    let f = build(
        SuiMock::new()
            .get("find_routes", &tampered)
            .extend(chain(&pool_type(BUCK, SUI_COIN_TYPE)))
            .extend(node(&[(1, 5_000_000_000)], 0)),
        |s| s,
    );
    import(&f);
    let err = f
        .service
        .sui_swap_tokens("w", PW, &swap_params())
        .unwrap_err();
    assert!(err.contains("not Cetus's current router"), "{err}");
    assert_eq!(f.mock.post_count("executeTransaction"), 0);
    // A route selling a different amount than asked.
    let other_amount = api_answer(962_476_346, 960_871).replace(
        "\"amount_in\":1000000000,\"amount_out\":962476346",
        "\"amount_in\":1000000001,\"amount_out\":962476346",
    );
    let g = build(
        SuiMock::new()
            .get("find_routes", &other_amount)
            .extend(chain(&pool_type(BUCK, SUI_COIN_TYPE)))
            .extend(node(&[(1, 5_000_000_000)], 0)),
        |s| s,
    );
    import(&g);
    assert!(g.service.sui_swap_tokens("w", PW, &swap_params()).is_err());
    assert_eq!(g.mock.post_count("executeTransaction"), 0);
}

#[test]
fn aggregator_failures_are_readable_and_send_nothing() {
    let f = build(
        SuiMock::new()
            .get(
                "find_routes",
                r#"{"code":5010,"msg":"liquidity is not enough, no sub_graph found."}"#,
            )
            .extend(node(&[(1, 5_000_000_000)], 0)),
        |s| s,
    );
    import(&f);
    let err = f
        .service
        .sui_swap_tokens("w", PW, &swap_params())
        .unwrap_err();
    assert!(
        err.contains("no route") && err.contains("liquidity"),
        "{err}"
    );
    let down = build(
        SuiMock::new()
            .get_error("find_routes", "HTTP status 503")
            .extend(node(&[(1, 5_000_000_000)], 0)),
        |s| s,
    );
    import(&down);
    let err = down
        .service
        .sui_swap_tokens("w", PW, &swap_params())
        .unwrap_err();
    assert!(err.contains("cannot reach the Cetus aggregator"), "{err}");
    assert_eq!(down.mock.post_count("executeTransaction"), 0);
}

#[test]
fn swapping_a_token_for_sui_counts_gas_against_the_received_sui() {
    // Sell 5 USDC for SUI: the dry run's SUI change includes the gas the wallet pays.
    let me = wallet_address();
    // A one-hop USDC to SUI route through the first pool.
    let one_hop = format!(
        r#"{{"code":200,"msg":"Success","data":{{"request_id":"q2","amount_in":5000000,"amount_out":2500000000,"deviation_ratio":"0.0001","paths":[{{"id":"{pool}","provider":"CETUS","from":"{USDC}","target":"{SUI_COIN_TYPE}","direction":true,"fee_rate":"0.0025","lot_size":1,"amount_in":5000000,"amount_out":2500000000,"published_at":"{ROUTER_LATEST}","extended_details":{{}}}}],"packages":{{"aggregator_v3":"{AGG_LATEST}"}},"gas":1}}}}"#,
        pool = crate::sui::cetus::tests::POOL1
    );
    let effects = effects_data(
        "simulateTransaction",
        "SUCCESS",
        None,
        &[
            change(
                &me,
                SUI_COIN_TYPE,
                2_500_000_000 - i128::from(COMPUTATION + STORAGE - REBATE),
            ),
            change(&me, USDC, -5_000_000),
        ],
    );
    let f = build(
        SuiMock::new()
            .data("simulateTransaction", &effects)
            .get("find_routes", &one_hop)
            .extend(chain(&pool_type(USDC, SUI_COIN_TYPE)))
            .extend(node(&[(1, 5_000_000_000)], 0)),
        |s| s,
    );
    import(&f);
    let params = SwapParams {
        from_token: USDC.to_string(),
        to_token: "native".to_string(),
        amount: "5".to_string(),
        ..SwapParams::default()
    };
    let out = f.service.sui_swap_tokens("w", PW, &params).unwrap();
    assert_eq!(text(&out, &["from", "symbol"]), "USDC");
    assert_eq!(text(&out, &["to", "token"]), "native");
    let (transaction, signature) = submitted(&f);
    assert_signed_by_wallet(&transaction, &signature);
    // The USDC coin object is sold whole, so no split is needed before the swap context opens.
    let TransactionKind::ProgrammableTransaction(ptb) = &transaction.kind else {
        panic!()
    };
    assert!(
        matches!(&ptb.commands[0], Command::MoveCall(m) if m.function.as_str() == "new_swap_context_v3")
    );
}

// ---- MCP tools ---------------------------------------------------------------------------------------------------

fn toolbox(mock: SuiMock) -> (Fx, Toolbox) {
    let dir = TempDir::new();
    let mock = Rc::new(mock);
    let http: Rc<dyn HttpClient> = mock.clone();
    let service = WalletService::new(Store::open(&dir.0).unwrap(), http)
        .with_iterations(vault::MIN_ITERATIONS);
    let shared = Fx {
        _dir: dir,
        mock,
        service: WalletService::new(
            Store::open(&TempDir::new().0).unwrap(),
            Rc::new(SuiMock::new()),
        ),
    };
    (shared, Toolbox::new(service))
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

#[test]
fn the_tools_drive_a_sui_wallet_end_to_end() {
    let (f, tb) = toolbox(node(&[(1, 5_000_000_000)], 0));
    let (err, out) = call(
        &tb,
        "create_wallet",
        r#"{"blockchain":"sui","wallet_name":"S","password":"correct-horse-battery"}"#,
    );
    assert!(!err, "{}", out.to_json());
    assert_eq!(text(&out, &["network"]), "mainnet");
    let address = text(&out, &["address"]).to_string();
    let (err, out) = call(
        &tb,
        "get_wallet_address",
        r#"{"wallet_name":"S","password":"correct-horse-battery"}"#,
    );
    assert!(!err);
    assert_eq!(text(&out, &["address"]), address);
    let (err, out) = call(
        &tb,
        "send_asset",
        &format!(
            r#"{{"wallet_name":"S","password":"correct-horse-battery","asset":"native","to":"{}","amount":"0.5"}}"#,
            other_address()
        ),
    );
    assert!(!err, "{}", out.to_json());
    assert_eq!(text(&out, &["status"]), "success");
    assert_eq!(f.mock.post_count("executeTransaction"), 1);
    let (err, out) = call(
        &tb,
        "view_private_key",
        r#"{"wallet_name":"S","password":"correct-horse-battery"}"#,
    );
    assert!(!err);
    assert!(text(&out, &["private_key"]).starts_with("suiprivkey1"));
    let (err, out) = call(
        &tb,
        "list_networks",
        r#"{"wallet_name":"S","password":"correct-horse-battery"}"#,
    );
    assert!(!err);
    assert_eq!(field(&out, &["networks"]).as_array().unwrap().len(), 1);
}

#[test]
fn sui_wallets_reject_ethereum_arguments_and_tools() {
    let (_f, tb) = toolbox(node(&[(1, 5_000_000_000)], 0));
    let make = r#"{"blockchain":"sui","wallet_name":"S","password":"correct-horse-battery"}"#;
    assert!(!call(&tb, "create_wallet", make).0);
    let base = r#""wallet_name":"S","password":"correct-horse-battery""#;
    for (tool, extra, expect) in [
        (
            "send_asset",
            r#","chain_id":1,"asset":"native","to":"0x2","amount":"1""#,
            "chain_id",
        ),
        (
            "send_asset",
            r#","gas_limit":21000,"asset":"native","to":"0x2","amount":"1""#,
            "gas_limit",
        ),
        ("get_wallet_balances", r#","chain_id":1"#, "chain_id"),
        (
            "get_wallet_balances",
            r#","token_contracts":["0x22"]"#,
            "token_contracts",
        ),
        (
            "get_transaction",
            r#","chain_id":1,"transaction_hash":"x""#,
            "chain_id",
        ),
        ("get_transaction_history", r#","chain_id":1"#, "chain_id"),
        (
            "estimate_gas_fee",
            r#","chain_id":1,"to":"0x2""#,
            "chain_id",
        ),
        (
            "simulate_transaction",
            r#","chain_id":1,"to":"0x2""#,
            "chain_id",
        ),
        (
            "sign_contract_transaction",
            r#","chain_id":1,"transaction_bytes":"AAAA""#,
            "chain_id",
        ),
        (
            "sign_contract_transaction",
            r#","to":"0x2","transaction_bytes":"AAAA""#,
            "Ethereum options",
        ),
        (
            "sign_contract_transaction",
            r#","broadcast":true"#,
            "transaction_bytes",
        ),
        (
            "get_swap_quote",
            r#","from_chain_id":1,"from_token":"native","to_token":"0x2::a::B","amount":"1""#,
            "Ethereum parameters",
        ),
        (
            "add_network",
            r#","network_name":"Base","chain_id":8453,"rpc_url":"https://x.example","currency_symbol":"ETH","block_explorer_url":"https://x.example""#,
            "Sui wallet",
        ),
        (
            "add_token",
            r#","chain_id":1,"contract":"0x2222222222222222222222222222222222222222""#,
            "Sui wallet",
        ),
        (
            "read_smart_contract",
            r#","chain_id":1,"contract":"0x2222222222222222222222222222222222222222","function":"f()""#,
            "Sui wallet",
        ),
        (
            "revoke_token_approval",
            r#","chain_id":1,"token":"0x2222222222222222222222222222222222222222","spender":"0x3333333333333333333333333333333333333333""#,
            "Sui wallet",
        ),
    ] {
        let (err, out) = call(&tb, tool, &format!("{{{base}{extra}}}"));
        assert!(err, "{tool} should fail");
        assert!(out.to_json().contains(expect), "{tool}: {}", out.to_json());
    }
}

#[test]
fn ethereum_wallets_still_need_their_chain_id_and_reject_sui_arguments() {
    let (_f, tb) = toolbox(SuiMock::new());
    assert!(
        !call(
            &tb,
            "create_wallet",
            r#"{"blockchain":"ethereum","wallet_name":"E","password":"correct-horse-battery"}"#
        )
        .0
    );
    let base = r#""wallet_name":"E","password":"correct-horse-battery""#;
    let (err, out) = call(
        &tb,
        "send_asset",
        &format!(r#"{{{base},"asset":"native","to":"0x2","amount":"1"}}"#),
    );
    assert!(
        err && out.to_json().contains("'chain_id'") && out.to_json().contains("Ethereum wallets")
    );
    let (err, out) = call(
        &tb,
        "get_swap_quote",
        &format!(r#"{{{base},"from_token":"native","to_token":"0x2","amount":"1"}}"#),
    );
    assert!(err && out.to_json().contains("from_chain_id"));
    let (err, out) = call(
        &tb,
        "sign_contract_transaction",
        &format!(r#"{{{base},"chain_id":1,"transaction_bytes":"AAAA"}}"#),
    );
    assert!(err && out.to_json().contains("Sui wallets"));
    let (err, out) = call(
        &tb,
        "estimate_gas_fee",
        &format!(r#"{{{base},"chain_id":1,"transaction_bytes":"AAAA"}}"#),
    );
    assert!(err && out.to_json().contains("Sui wallets"));
    let (err, out) = call(
        &tb,
        "create_wallet",
        r#"{"blockchain":"ethereum","wallet_name":"E2","password":"correct-horse-battery","network":"testnet"}"#,
    );
    assert!(err && out.to_json().contains("Sui wallets only"));
}

#[test]
fn sign_personal_message_works_for_both_blockchains() {
    let (_f, tb) = toolbox(SuiMock::new());
    assert!(
        !call(
            &tb,
            "create_wallet",
            r#"{"blockchain":"ethereum","wallet_name":"E","password":"correct-horse-battery"}"#
        )
        .0
    );
    assert!(
        !call(
            &tb,
            "create_wallet",
            r#"{"blockchain":"sui","wallet_name":"S","password":"correct-horse-battery"}"#
        )
        .0
    );
    let (err, eth) = call(
        &tb,
        "sign_personal_message",
        r#"{"wallet_name":"E","password":"correct-horse-battery","message":"hi"}"#,
    );
    assert!(!err, "{}", eth.to_json());
    assert_eq!(text(&eth, &["standard"]), "EIP-191 personal_sign");
    assert_eq!(text(&eth, &["signature"]).len(), 2 + 130);
    let (err, sui) = call(
        &tb,
        "sign_personal_message",
        r#"{"wallet_name":"S","password":"correct-horse-battery","message":"hi"}"#,
    );
    assert!(!err, "{}", sui.to_json());
    assert_eq!(text(&sui, &["intent"]), "PersonalMessage");
    let (err, _) = call(
        &tb,
        "sign_personal_message",
        r#"{"wallet_name":"S","password":"correct-horse-battery","message":"hi","message_format":"rot13"}"#,
    );
    assert!(err);
}

#[test]
fn schemas_describe_which_tools_work_with_which_wallet() {
    let (_f, tb) = toolbox(SuiMock::new());
    let list = tb.list();
    let tools = list.get("tools").and_then(Value::as_array).unwrap();
    let schema = |name: &str| {
        tools
            .iter()
            .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
            .unwrap_or_else(|| panic!("tool {name}"))
            .clone()
    };
    let required = |name: &str| -> Vec<String> {
        schema(name)
            .get("inputSchema")
            .and_then(|s| s.get("required"))
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap().to_string())
            .collect()
    };
    // Tools that work for both blockchains do not demand an Ethereum chain ID.
    for name in [
        "send_asset",
        "sign_contract_transaction",
        "estimate_gas_fee",
        "simulate_transaction",
        "get_transaction",
        "get_transaction_history",
    ] {
        assert!(!required(name).contains(&"chain_id".to_string()), "{name}");
        assert!(
            schema(name).to_json().contains("Sui"),
            "{name} documents Sui"
        );
    }
    // Ethereum-only tools still require it and say so.
    for name in [
        "read_smart_contract",
        "add_token",
        "remove_token",
        "revoke_token_approval",
        "delete_network",
    ] {
        assert!(required(name).contains(&"chain_id".to_string()), "{name}");
    }
    for name in [
        "read_smart_contract",
        "add_network",
        "update_network",
        "delete_network",
        "add_token",
        "remove_token",
        "revoke_token_approval",
    ] {
        let description = schema(name)
            .get("description")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        assert!(description.contains("Ethereum"), "{name}: {description}");
    }
    assert!(required("sign_personal_message").contains(&"message".to_string()));
    assert!(schema("create_wallet").to_json().contains("\\\"sui\\\""));
    assert!(!required("create_wallet").contains(&"network".to_string()));
}

#[test]
fn passwords_and_secrets_never_appear_in_sui_output_or_errors() {
    let secret_pw = "unmistakable-password-12345";
    let f = build(node(&[(1, 5_000_000_000)], 0), |s| s);
    f.service
        .import_wallet("sui", "w", secret_pw, None, Some(&key_hex()))
        .unwrap();
    let mut outputs: Vec<String> = vec![];
    let mut record = |r: Result<Value, String>| match r {
        Ok(v) => outputs.push(v.to_json()),
        Err(e) => outputs.push(e),
    };
    record(f.service.get_address("w", secret_pw));
    record(f.service.sui_balances("w", secret_pw));
    record(
        f.service
            .sui_send_asset("w", secret_pw, "native", &other_address(), "1"),
    );
    record(
        f.service
            .sui_send_asset("w", secret_pw, "native", "bad", "1"),
    );
    record(f.service.sui_sign_message("w", secret_pw, "m", None));
    record(f.service.sui_swap_quote("w", secret_pw, &swap_params()));
    record(f.service.get_address("w", "wrong-unmistakable"));
    for out in &outputs {
        assert!(!out.contains(secret_pw), "{out}");
        assert!(!out.contains(&key_hex()), "{out}");
    }
}

mod wc;
