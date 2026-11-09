//! Signing, simulating, looking up and the MCP tools for Solana wallets.

use super::*;
use crate::solana::tx::{AccountMeta, Instruction};

fn b64(transaction: &Transaction) -> String {
    transaction.to_base64()
}

fn sign(f: &Fx, transaction: &Transaction, broadcast: bool) -> Result<Value, String> {
    f.service
        .solana_sign_transaction("w", PW, &b64(transaction), broadcast)
}

#[test]
fn signing_does_not_send_by_default() {
    let f = funded();
    let t = transfer_tx(1_000);
    let v = sign(&f, &t, false).unwrap();
    assert_eq!(text(&v, &["status"]), "signed");
    assert_eq!(text(&v, &["wallet"]), "w");
    assert_eq!(text(&v, &["address"]), owner_text());
    let signed = Transaction::from_base64(text(&v, &["transaction"])).unwrap();
    assert_signed_by_wallet(&signed);
    assert_eq!(
        signed.message, t.message,
        "the message is signed exactly as given"
    );
    assert_eq!(
        text(&v, &["signature"]),
        base58::encode(&signed.signatures[0])
    );
    assert_eq!(items(&v, &["description"]).len(), 1);
    assert_eq!(text(&v, &["dry_run", "status"]), "success");
    assert_eq!(
        text(&v, &["dry_run", "wallet_changes", "sol", "change"]),
        "0"
    );
    assert!(items(&v, &["warnings"]).is_empty());
    assert_eq!(f.mock.count("sendTransaction"), 0);
}

#[test]
fn signing_with_broadcast_submits_after_a_successful_dry_run() {
    let f = funded();
    let t = transfer_tx(1_000);
    let v = sign(&f, &t, true).unwrap();
    assert_eq!(text(&v, &["status"]), "submitted");
    assert_eq!(text(&v, &["execution", "status"]), "confirmed");
    assert_signed_by_wallet(&submitted(&f));
    assert_eq!(f.mock.count("sendTransaction"), 1);
}

#[test]
fn a_failing_dry_run_blocks_broadcast_but_not_plain_signing() {
    let f = funded();
    f.world.borrow_mut().sim_error = Some(r#""InsufficientFundsForRent""#.to_string());
    let t = transfer_tx(1_000);
    let err = sign(&f, &t, true).unwrap_err();
    assert!(err.contains("not sending") && err.contains("rent"), "{err}");
    assert_eq!(f.mock.count("sendTransaction"), 0);
    let v = sign(&f, &t, false).unwrap();
    assert_eq!(text(&v, &["dry_run", "status"]), "failed");
    assert!(
        items(&v, &["warnings"])
            .iter()
            .any(|w| w.as_str().unwrap().contains("dry run"))
    );
}

#[test]
fn transactions_must_name_this_wallet_as_a_signer() {
    let f = funded();
    let ix = tx::system_transfer(&key(5), &key(2), 1);
    let foreign = Transaction::unsigned(tx::compile_legacy(&key(5), &[ix], BLOCKHASH).unwrap());
    let err = sign(&f, &foreign, false).unwrap_err();
    assert!(
        err.contains("not one of the transaction's signers"),
        "{err}"
    );
    assert!(f.world.borrow().sent.is_empty());
}

#[test]
fn transactions_for_another_network_or_expired_are_refused() {
    let f = funded();
    f.world.borrow_mut().blockhash_valid = Some(false);
    let err = sign(&f, &transfer_tx(1), false).unwrap_err();
    assert!(err.contains("blockhash is not valid"), "{err}");
    // a durable nonce transaction has no recent blockhash and is judged by its nonce instruction
    let advance = tx::Instruction {
        program_id: system_program(),
        accounts: vec![
            tx::AccountMeta::writable(key(5), false),
            tx::AccountMeta::readonly(key(6), false),
            tx::AccountMeta::readonly(owner(), true),
        ],
        data: vec![4, 0, 0, 0],
    };
    let ix = tx::system_transfer(&owner(), &key(2), 1);
    let nonce_tx =
        Transaction::unsigned(tx::compile_legacy(&owner(), &[advance, ix], [3u8; 32]).unwrap());
    assert!(sign(&f, &nonce_tx, false).is_ok());
    // if validity cannot be checked the transaction is not signed
    f.world.borrow_mut().blockhash_valid = None;
    assert!(sign(&f, &transfer_tx(1), false).is_err());
}

#[test]
fn another_fee_payer_is_reported_and_blocks_sending() {
    let f = funded();
    let both = Instruction {
        program_id: key(50),
        accounts: vec![AccountMeta::readonly(owner(), true)],
        data: vec![1],
    };
    let t = Transaction::unsigned(tx::compile_legacy(&key(5), &[both], BLOCKHASH).unwrap());
    let v = sign(&f, &t, false).unwrap();
    let signed = Transaction::from_base64(text(&v, &["transaction"])).unwrap();
    assert!(signed.is_signed(1) && !signed.is_signed(0));
    let warnings = field(&v, &["warnings"]).to_json();
    assert!(warnings.contains("not the fee payer") && warnings.contains("more signature"));
    let err = sign(&f, &t, true).unwrap_err();
    assert!(err.contains("someone other than this wallet"), "{err}");
    assert!(f.world.borrow().sent.is_empty());
}

#[test]
fn approvals_and_unknown_programs_are_described_and_flagged() {
    let f = funded();
    let mut data = vec![4u8];
    data.extend_from_slice(&u64::MAX.to_le_bytes());
    let approve = Instruction {
        program_id: token_program(),
        accounts: vec![
            AccountMeta::writable(key(3), false),
            AccountMeta::readonly(key(9), false),
            AccountMeta::readonly(owner(), true),
        ],
        data,
    };
    let unknown = Instruction {
        program_id: key(99),
        accounts: vec![AccountMeta::writable(owner(), true)],
        data: vec![1, 2],
    };
    let t = Transaction::unsigned(
        tx::compile_legacy(&owner(), &[approve, unknown], BLOCKHASH).unwrap(),
    );
    let v = sign(&f, &t, false).unwrap();
    let description = field(&v, &["description"]).to_json();
    assert!(description.contains("UNLIMITED") && description.contains("does not recognise"));
    let warnings = field(&v, &["warnings"]).to_json();
    assert!(warnings.contains("approval is revoked") && warnings.contains("not recognised"));
}

#[test]
fn malformed_transactions_are_refused_before_unlocking() {
    let f = funded();
    for bad in ["", "not base64 !!", "AAAA", &"A".repeat(5000)] {
        let err = f
            .service
            .solana_sign_transaction("w", "wrong-password", bad, false)
            .unwrap_err();
        assert_ne!(err, "incorrect password", "{bad}: validation comes first");
    }
    assert!(f.mock.methods().is_empty());
}

#[test]
fn version_0_transactions_are_signed_after_reading_their_lookup_tables() {
    let f = funded();
    let table = key(60);
    let mut raw = vec![0x80, 1, 0, 1];
    tx::encode_short_len(2, &mut raw);
    raw.extend_from_slice(&owner());
    raw.extend_from_slice(&system_program());
    raw.extend_from_slice(&BLOCKHASH);
    tx::encode_short_len(1, &mut raw);
    // a system transfer from the wallet to the table's entry 0 (loaded as writable)
    let mut data = 2u32.to_le_bytes().to_vec();
    data.extend_from_slice(&7u64.to_le_bytes());
    raw.extend_from_slice(&[1, 2, 0, 2]);
    tx::encode_short_len(data.len(), &mut raw);
    raw.extend_from_slice(&data);
    tx::encode_short_len(1, &mut raw);
    raw.extend_from_slice(&table);
    raw.extend_from_slice(&[1, 0, 0]);
    let message = crate::solana::tx::Message::parse(&raw).unwrap();
    let t = Transaction::unsigned(message);
    let mut table_data = vec![0u8; 56];
    table_data.extend_from_slice(&key(70));
    f.world.borrow_mut().accounts.insert(
        table,
        Acct {
            lamports: 1,
            owner: saddr::id(saddr::ADDRESS_LOOKUP_TABLE_PROGRAM),
            data: table_data,
        },
    );
    let v = sign(&f, &t, false).unwrap();
    let description = field(&v, &["description"]).to_json();
    assert!(description.contains(&key_text(70)), "{description}");
    assert!(items(&v, &["warnings"]).is_empty());
    let signed = Transaction::from_base64(text(&v, &["transaction"])).unwrap();
    assert_signed_by_wallet(&signed);
}

// ---- messages -----------------------------------------------------------------------------------

#[test]
fn messages_are_signed_with_the_raw_ed25519_key() {
    let f = funded();
    let v = f
        .service
        .solana_sign_message("w", PW, "Sign in to example.com", None)
        .unwrap();
    assert_eq!(text(&v, &["address"]), owner_text());
    assert_eq!(text(&v, &["message_preview"]), "Sign in to example.com");
    let signature = base58::decode_fixed::<64>(text(&v, &["signature"])).unwrap();
    assert!(ed25519::verify(
        &owner(),
        b"Sign in to example.com",
        &signature
    ));
    assert_eq!(
        base64::decode(text(&v, &["signature_base64"])).unwrap(),
        signature
    );
    // other encodings of the same bytes give the same signature
    for (message, format) in [
        (hex::encode(b"Sign in to example.com"), "hex"),
        (base64::encode(b"Sign in to example.com"), "base64"),
    ] {
        let again = f
            .service
            .solana_sign_message("w", PW, &message, Some(format))
            .unwrap();
        assert_eq!(text(&again, &["signature"]), text(&v, &["signature"]));
    }
    assert!(
        f.mock.methods().is_empty(),
        "signing a message needs no network"
    );
}

#[test]
fn binary_messages_are_shown_as_such_and_bad_ones_are_refused() {
    let f = funded();
    let v = f
        .service
        .solana_sign_message("w", PW, "0001feff", Some("hex"))
        .unwrap();
    assert_eq!(text(&v, &["message_preview"]), "(binary data)");
    for (message, format) in [
        ("", None),
        ("zz", Some("hex")),
        ("!!", Some("base64")),
        ("x", Some("rot13")),
    ] {
        assert!(
            f.service
                .solana_sign_message("w", PW, message, format)
                .is_err(),
            "{message}"
        );
    }
}

#[test]
fn messages_that_are_valid_transactions_are_never_signed() {
    let f = funded();
    let t = transfer_tx(1_000_000_000);
    for (bytes, format) in [
        (hex::encode(t.message.raw()), "hex"),
        (base64::encode(&t.to_bytes()), "base64"),
    ] {
        let err = f
            .service
            .solana_sign_message("w", PW, &bytes, Some(format))
            .unwrap_err();
        assert!(err.contains("valid Solana transaction"), "{err}");
    }
}

// ---- estimating and simulating --------------------------------------------------------------

#[test]
fn estimating_a_transfer_gives_the_fee_in_sol_and_usd() {
    let f = funded();
    let params = IntentParams {
        to: Some(key_text(2)),
        asset: Some("native".to_string()),
        amount: Some("1".to_string()),
        ..IntentParams::default()
    };
    let v = f.service.solana_estimate_gas("w", PW, &params).unwrap();
    assert_eq!(v.get("will_succeed"), Some(&Value::Bool(true)));
    assert_eq!(text(&v, &["fee_lamports"]), "5002");
    assert_eq!(text(&v, &["fee_sol"]), "0.000005002");
    assert_eq!(text(&v, &["fee_usdc"]), "0.0008");
    assert_eq!(text(&v, &["compute_units"]), "150");
    assert_eq!(f.mock.count("sendTransaction"), 0);
}

#[test]
fn estimating_still_works_on_a_wallet_that_cannot_afford_the_transfer() {
    let w = world();
    w.borrow_mut().accounts.clear();
    let f = build(w);
    import(&f);
    let params = IntentParams {
        to: Some(key_text(2)),
        asset: Some("native".to_string()),
        amount: Some("1".to_string()),
        ..IntentParams::default()
    };
    let v = f.service.solana_estimate_gas("w", PW, &params).unwrap();
    assert_eq!(v.get("will_succeed"), Some(&Value::Bool(false)));
    assert_eq!(text(&v, &["fee_lamports"]), "5000");
    assert!(text(&v, &["error"]).contains("insufficient SOL"));
    let v = f
        .service
        .solana_simulate_transaction("w", PW, &params)
        .unwrap();
    assert_eq!(v.get("success"), Some(&Value::Bool(false)));
    assert_eq!(f.mock.count("sendTransaction"), 0);
}

#[test]
fn simulating_reports_balance_changes_and_never_sends() {
    let f = funded();
    pay(&f, 1_000_000_000, 5_002);
    let params = IntentParams {
        to: Some(key_text(2)),
        asset: Some("native".to_string()),
        amount: Some("1".to_string()),
        ..IntentParams::default()
    };
    let v = f
        .service
        .solana_simulate_transaction("w", PW, &params)
        .unwrap();
    assert_eq!(v.get("success"), Some(&Value::Bool(true)));
    assert_eq!(
        text(&v, &["result", "wallet_changes", "sol", "change"]),
        "-1.000005002"
    );
    assert_eq!(
        text(&v, &["result", "wallet_changes", "sol", "change_lamports"]),
        "-1000005002"
    );
    assert_eq!(f.mock.count("sendTransaction"), 0);
    assert!(f.world.borrow().sent.is_empty());
}

#[test]
fn simulating_a_token_transfer_names_the_token_change() {
    let f = funded();
    token_send_world(&f, 5_000_000);
    let params = IntentParams {
        to: Some(key_text(2)),
        asset: Some(USDC.to_string()),
        amount: Some("1.5".to_string()),
        ..IntentParams::default()
    };
    let v = f
        .service
        .solana_simulate_transaction("w", PW, &params)
        .unwrap();
    let token = &items(&v, &["result", "wallet_changes", "tokens"])[0];
    assert_eq!(text(token, &["symbol"]), "USDC");
    assert_eq!(text(token, &["change"]), "-1.5");
    assert_eq!(text(token, &["change_raw"]), "-1500000");
}

#[test]
fn simulating_given_transaction_bytes_decodes_every_instruction() {
    let f = funded();
    let t = transfer_tx(2_000_000_000);
    let params = IntentParams {
        transaction_bytes: Some(b64(&t)),
        ..IntentParams::default()
    };
    let v = f
        .service
        .solana_simulate_transaction("w", PW, &params)
        .unwrap();
    let decoded = field(&v, &["decoded", "instructions"]).to_json();
    assert!(decoded.contains("2 SOL"), "{decoded}");
    let est = f.service.solana_estimate_gas("w", PW, &params).unwrap();
    assert_eq!(text(&est, &["fee_lamports"]), "5000");
}

#[test]
fn simulation_parameters_are_checked() {
    let f = funded();
    let base = IntentParams {
        to: Some(key_text(2)),
        asset: Some("native".to_string()),
        amount: Some("1".to_string()),
        ..IntentParams::default()
    };
    let t = b64(&transfer_tx(1));
    for (params, expect) in [
        (
            IntentParams {
                value: Some("1".into()),
                ..base.clone()
            },
            "Ethereum parameters",
        ),
        (
            IntentParams {
                data: Some("0x00".into()),
                ..base.clone()
            },
            "Ethereum parameters",
        ),
        (
            IntentParams {
                transaction_bytes: Some(t),
                ..base.clone()
            },
            "not both",
        ),
        (
            IntentParams {
                to: None,
                ..base.clone()
            },
            "'to' is required",
        ),
        (
            IntentParams {
                asset: None,
                ..base.clone()
            },
            "'asset' is required",
        ),
        (
            IntentParams {
                amount: None,
                ..base.clone()
            },
            "'amount' is required",
        ),
    ] {
        let err = f
            .service
            .solana_simulate_transaction("w", PW, &params)
            .unwrap_err();
        assert!(err.contains(expect), "{err}");
    }
}

// ---- looking things up ----------------------------------------------------------------------

fn record() -> String {
    let me = owner_text();
    format!(
        r#"{{"slot":77,"blockTime":1700000001,"transaction":{{"message":{{"accountKeys":["{me}","{}"]}}}},"meta":{{"err":null,"fee":5000,"preBalances":[100000,5],"postBalances":[94000,1005],"preTokenBalances":[{{"accountIndex":1,"mint":"{USDC}","owner":"{me}","uiTokenAmount":{{"amount":"10000000","decimals":6}}}}],"postTokenBalances":[{{"accountIndex":1,"mint":"{USDC}","owner":"{me}","uiTokenAmount":{{"amount":"7500000","decimals":6}}}}]}}}}"#,
        key_text(2)
    )
}

fn signature_text() -> String {
    base58::encode(&[5u8; 64])
}

#[test]
fn a_transaction_is_looked_up_with_the_wallets_changes() {
    let body = record();
    let f = build_with(
        world(),
        SendMode::Accept,
        MAINNET_GENESIS,
        |s| s,
        move |m| m.on("getTransaction", &body),
    );
    import(&f);
    let v = f
        .service
        .solana_get_transaction("w", PW, &signature_text())
        .unwrap();
    assert_eq!(text(&v, &["status"]), "success");
    assert_eq!(text(&v, &["slot"]), "77");
    assert_eq!(text(&v, &["fee_sol"]), "0.000005");
    assert_eq!(text(&v, &["fee_payer"]), owner_text());
    assert_eq!(
        text(&v, &["wallet_changes", "sol", "change_lamports"]),
        "-6000"
    );
    let token = &items(&v, &["wallet_changes", "tokens"])[0];
    assert_eq!(text(token, &["symbol"]), "USDC");
    assert_eq!(text(token, &["change"]), "-2.5");
    assert_eq!(text(&v, &["confirmation"]), "confirmed");
    assert!(text(&v, &["explorer_url"]).contains(&signature_text()));
}

#[test]
fn unknown_and_pending_transactions_are_explained() {
    let f = build_with(
        world(),
        SendMode::Accept,
        MAINNET_GENESIS,
        |s| s,
        |m| m.on("getTransaction", "null"),
    );
    import(&f);
    // seen by the network but not yet readable
    let v = f
        .service
        .solana_get_transaction("w", PW, &signature_text())
        .unwrap();
    assert_eq!(text(&v, &["status"]), "pending");
    // not found anywhere
    f.world.borrow_mut().status = None;
    let err = f
        .service
        .solana_get_transaction("w", PW, &signature_text())
        .unwrap_err();
    assert!(err.contains("no transaction"), "{err}");
    for bad in ["", "abc", "0x1234"] {
        assert!(f.service.solana_get_transaction("w", PW, bad).is_err());
    }
}

#[test]
fn failed_transactions_show_their_error() {
    let me = owner_text();
    let body = format!(
        r#"{{"slot":1,"transaction":{{"message":{{"accountKeys":["{me}"]}}}},"meta":{{"err":{{"InstructionError":[0,"Custom"]}},"fee":5000,"preBalances":[10],"postBalances":[5]}}}}"#
    );
    let f = build_with(
        world(),
        SendMode::Accept,
        MAINNET_GENESIS,
        |s| s,
        move |m| m.on("getTransaction", &body),
    );
    import(&f);
    let v = f
        .service
        .solana_get_transaction("w", PW, &signature_text())
        .unwrap();
    assert_eq!(text(&v, &["status"]), "failed");
    assert!(text(&v, &["error"]).contains("InstructionError"));
}

#[test]
fn history_lists_recent_transactions() {
    let body = format!(
        r#"[{{"signature":"{}","slot":5,"blockTime":1700000000,"err":null,"memo":"[2] hi","confirmationStatus":"finalized"}},{{"signature":"{}","slot":4,"blockTime":null,"err":"AccountNotFound"}}]"#,
        signature_text(),
        base58::encode(&[6u8; 64])
    );
    let f = build_with(
        world(),
        SendMode::Accept,
        MAINNET_GENESIS,
        |s| s,
        move |m| m.on("getSignaturesForAddress", &body),
    );
    import(&f);
    let v = f
        .service
        .solana_history("w", PW, HistoryKind::Transactions, 5)
        .unwrap();
    assert_eq!(text(&v, &["count"]), "2");
    let list = items(&v, &["items"]);
    assert_eq!(text(&list[0], &["status"]), "success");
    assert_eq!(text(&list[0], &["memo"]), "[2] hi");
    assert_eq!(text(&list[1], &["status"]), "failed");
    let params = f.mock.params("getSignaturesForAddress", 0);
    assert_eq!(
        params.as_array().unwrap()[1].get("limit").unwrap().as_u64(),
        Some(5)
    );
    let err = f
        .service
        .solana_history("w", PW, HistoryKind::TokenTransfers, 5)
        .unwrap_err();
    assert!(err.contains("transactions only"));
}

// ---- the MCP tools ----------------------------------------------------------------------------

fn tool_fx() -> (Fx, Toolbox) {
    let dir = TempDir::new();
    let w = world();
    let mock = Rc::new(node(&w, SendMode::Accept, MAINNET_GENESIS));
    let http: Rc<dyn HttpClient> = mock.clone();
    let service = WalletService::new(Store::open(&dir.0).unwrap(), http)
        .with_iterations(vault::MIN_ITERATIONS)
        .with_receipt_wait(Duration::ZERO, 3);
    let fx = Fx {
        _dir: dir,
        mock,
        world: w,
        service: WalletService::new(
            Store::open(&TempDir::new().0).unwrap(),
            Rc::new(SolMock::new()),
        ),
    };
    (fx, Toolbox::new(service))
}

#[test]
fn the_tools_drive_a_solana_wallet_end_to_end() {
    let (f, tb) = tool_fx();
    let creds = format!(r#""wallet_name":"s","password":"{PW}""#);
    let (err, v) = call(
        &tb,
        "import_wallet",
        &format!(
            r#"{{"blockchain":"solana","wallet_name":"s","password":"{PW}","private_key":"{}"}}"#,
            key_hex()
        ),
    );
    assert!(!err, "{}", v.to_json());
    assert_eq!(text(&v, &["address"]), owner_text());
    let (err, v) = call(&tb, "list_wallets", "{}");
    assert!(!err && text(&items(&v, &["wallets"])[0], &["blockchain"]) == "Solana");
    let (err, v) = call(&tb, "get_wallet_address", &format!("{{{creds}}}"));
    assert!(!err && text(&v, &["address"]) == owner_text());
    let (err, v) = call(&tb, "get_wallet_balances", &format!("{{{creds}}}"));
    assert!(!err, "{}", v.to_json());
    assert_eq!(text(&items(&v, &["assets"])[0], &["balance"]), "5");
    let (err, v) = call(&tb, "list_networks", &format!("{{{creds}}}"));
    assert!(!err && text(&items(&v, &["networks"])[0], &["name"]) == "mainnet");
    let (err, v) = call(
        &tb,
        "estimate_gas_fee",
        &format!(
            r#"{{{creds},"to":"{}","asset":"native","amount":"1"}}"#,
            key_text(2)
        ),
    );
    assert!(!err, "{}", v.to_json());
    let (err, v) = call(
        &tb,
        "sign_personal_message",
        &format!(r#"{{{creds},"message":"hello"}}"#),
    );
    assert!(!err && v.get("signature").is_some());
    let t = b64(&transfer_tx(5));
    let (err, v) = call(
        &tb,
        "sign_contract_transaction",
        &format!(r#"{{{creds},"transaction_bytes":"{t}"}}"#),
    );
    assert!(!err, "{}", v.to_json());
    assert_eq!(text(&v, &["status"]), "signed");
    let (err, v) = call(
        &tb,
        "send_asset",
        &format!(
            r#"{{{creds},"asset":"native","to":"{}","amount":"0.5"}}"#,
            key_text(2)
        ),
    );
    assert!(!err, "{}", v.to_json());
    assert_eq!(text(&v, &["status"]), "confirmed");
    drop(f);
}

#[test]
fn chain_ids_and_other_blockchains_options_are_refused_for_solana_wallets() {
    let (_f, tb) = tool_fx();
    call(
        &tb,
        "import_wallet",
        &format!(
            r#"{{"blockchain":"solana","wallet_name":"s","password":"{PW}","private_key":"{}"}}"#,
            key_hex()
        ),
    );
    let creds = format!(r#""wallet_name":"s","password":"{PW}""#);
    let t = b64(&transfer_tx(5));
    const ETH: &str = "0x0000000000000000000000000000000000000001";
    let cases = [
        (
            "get_wallet_balances",
            format!(r#"{{{creds},"chain_id":1}}"#),
            "'chain_id'",
        ),
        (
            "get_wallet_balances",
            format!(r#"{{{creds},"token_contracts":["0x00"]}}"#),
            "token_contracts",
        ),
        (
            "send_asset",
            format!(
                r#"{{{creds},"chain_id":1,"asset":"native","to":"{}","amount":"1"}}"#,
                key_text(2)
            ),
            "'chain_id'",
        ),
        (
            "send_asset",
            format!(
                r#"{{{creds},"gas_limit":21000,"asset":"native","to":"{}","amount":"1"}}"#,
                key_text(2)
            ),
            "gas_limit",
        ),
        (
            "sign_contract_transaction",
            format!(r#"{{{creds},"to":"{}","data":"0x00"}}"#, key_text(2)),
            "for a Solana wallet give only transaction_bytes",
        ),
        (
            "sign_contract_transaction",
            format!(r#"{{{creds}}}"#),
            "transaction_bytes",
        ),
        (
            "get_transaction",
            format!(r#"{{{creds},"chain_id":1,"transaction_hash":"x"}}"#),
            "'chain_id'",
        ),
        (
            "get_transaction_history",
            format!(r#"{{{creds},"chain_id":1}}"#),
            "'chain_id'",
        ),
        (
            "get_transaction_history",
            format!(r#"{{{creds},"type":"token_transfers"}}"#),
            "transactions only",
        ),
        (
            "estimate_gas_fee",
            format!(r#"{{{creds},"chain_id":1,"transaction_bytes":"{t}"}}"#),
            "'chain_id'",
        ),
        (
            "simulate_transaction",
            format!(r#"{{{creds},"chain_id":1}}"#),
            "'chain_id'",
        ),
        (
            "read_smart_contract",
            format!(r#"{{{creds},"chain_id":1,"contract":"{ETH}","function":"f()"}}"#),
            "Solana wallet",
        ),
        (
            "add_token",
            format!(r#"{{{creds},"chain_id":1,"contract":"{ETH}"}}"#),
            "Solana wallet",
        ),
        (
            "add_network",
            format!(
                r#"{{{creds},"network_name":"x","chain_id":1,"rpc_url":"https://x.example","currency_symbol":"X","block_explorer_url":"https://x.example"}}"#
            ),
            "Solana wallet",
        ),
        (
            "revoke_token_approval",
            format!(r#"{{{creds},"chain_id":1,"token":"{ETH}","spender":"{ETH}"}}"#),
            "Solana wallet",
        ),
    ];
    for (tool, args, expect) in cases {
        let (err, out) = call(&tb, tool, &args);
        assert!(
            err && out.to_json().contains(expect),
            "{tool}: {}",
            out.to_json()
        );
    }
}

#[test]
fn the_solana_tool_descriptions_cover_every_shared_tool() {
    let (_f, tb) = tool_fx();
    let list = tb.list();
    let described = |name: &str| -> String {
        items(&list, &["tools"])
            .iter()
            .find(|t| text(t, &["name"]) == name)
            .map(|t| t.to_json())
            .unwrap_or_else(|| panic!("no tool {name}"))
    };
    for name in [
        "create_wallet",
        "import_wallet",
        "get_wallet_address",
        "get_wallet_balances",
        "send_asset",
        "sign_contract_transaction",
        "sign_personal_message",
        "estimate_gas_fee",
        "simulate_transaction",
        "get_transaction",
        "get_transaction_history",
        "list_networks",
        "get_swap_quote",
        "swap_tokens",
        "view_private_key",
        "wc_pair",
        "wc_approve_session",
        "wc_review_request",
        "wc_approve_request",
    ] {
        assert!(
            described(name).contains("Solana"),
            "{name} does not mention Solana"
        );
    }
}
