//! WalletConnect with a Solana wallet, against a mock relay playing the dapp.

use super::*;
use crate::walletconnect::manager::WcManager;
use crate::walletconnect::proto;
use crate::walletconnect::relay::mock::MockRelay;
use crate::walletconnect::testing::Dapp;

const PROPOSAL_ID: u64 = 1_700_000_000_000_000_002;
const CHAIN: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
const DEVNET_CHAIN: &str = "solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1";
const LEGACY_CHAIN: &str = "solana:4sGjMW1sUnHzSxGspuhpqLDx6wiyjNtZ";

fn solana_ask(chain: &str) -> String {
    format!(
        r#"{{"solana":{{"chains":["{chain}"],"methods":["solana_signTransaction","solana_signAndSendTransaction","solana_signMessage","solana_signAllTransactions","solana_getAccounts"],"events":[]}}}}"#
    )
}

struct Wc {
    f: Fx,
    relay: Rc<MockRelay>,
    dapp: Dapp,
}

fn wc_with(send: SendMode) -> Wc {
    let relay = Rc::new(MockRelay::default());
    let manager = WcManager::new(relay.clone()).with_settle_wait(Duration::ZERO);
    let f = build_with(
        world(),
        send,
        MAINNET_GENESIS,
        |s| s.with_walletconnect(manager),
        |m| m,
    );
    import(&f);
    Wc {
        f,
        relay,
        dapp: Dapp::new(),
    }
}

fn wc() -> Wc {
    wc_with(SendMode::Accept)
}

impl Wc {
    fn propose(&self, required: &str, optional: &str) -> Value {
        self.dapp
            .send_proposal(&self.relay, PROPOSAL_ID, required, optional);
        self.f.service.wc_pair(&self.dapp.uri(), Some(0)).unwrap()
    }

    /// Connects the dapp asking for `chain`; returns the settle message the dapp received.
    fn connect_on(&mut self, chain: &str) -> Value {
        self.propose(&solana_ask(chain), "{}");
        self.f
            .service
            .wc_approve_session_solana(PROPOSAL_ID, "w", PW)
            .unwrap();
        let settle = self.dapp.accept(&self.relay.take_published());
        let id = settle.get("id").cloned().unwrap();
        self.dapp.push_session_message(
            &self.relay,
            proto::build_result(&id, Value::Bool(true)),
            1103,
        );
        settle
    }

    fn connect(&mut self) -> Value {
        self.connect_on(CHAIN)
    }

    fn request(&self, id: u64, chain: &str, method: &str, params: &str) {
        self.dapp.request(&self.relay, id, chain, method, params);
    }

    fn answer(&self) -> Value {
        let published = self.relay.take_published();
        assert_eq!(published.len(), 1, "expected exactly one answer");
        self.dapp.decode_session(&published[0].message)
    }

    fn pending(&self) -> usize {
        self.f
            .service
            .wc_list_requests()
            .unwrap()
            .get("count")
            .and_then(Value::as_u64)
            .unwrap() as usize
    }

    fn approve(&self, id: u64) -> Result<Value, String> {
        self.f.service.wc_approve_request_solana(id, "w", PW)
    }

    fn review(&self, id: u64) -> Result<Value, String> {
        self.f.service.wc_review_request_solana(id, "w", PW)
    }
}

fn tx_params(t: &Transaction) -> String {
    format!(r#"{{"transaction":"{}"}}"#, t.to_base64())
}

fn message_params(message: &[u8], pubkey: &str) -> String {
    format!(
        r#"{{"message":"{}","pubkey":"{pubkey}"}}"#,
        base58::encode(message)
    )
}

#[test]
fn a_solana_proposal_is_connectable_with_a_solana_wallet_only() {
    let w = wc();
    let out = w.propose(&solana_ask(CHAIN), "{}");
    let proposal = &items(&out, &["proposals"])[0];
    assert_eq!(
        proposal.get("can_connect").and_then(Value::as_bool),
        Some(true)
    );
    let with = items(proposal, &["can_connect_with"]);
    assert_eq!(with.len(), 1);
    assert_eq!(with[0].as_str(), Some("Solana (mainnet)"));
    assert!(
        field(proposal, &["solana_required"])
            .get("chains")
            .is_some()
    );
}

#[test]
fn a_proposal_for_a_test_cluster_names_which_clusters_can_connect() {
    let w = wc();
    let both = format!(
        r#"{{"solana":{{"chains":["{CHAIN}","{DEVNET_CHAIN}"],"methods":["solana_signMessage"],"events":[]}}}}"#
    );
    let out = w.propose("{}", &both);
    let with = items(&items(&out, &["proposals"])[0], &["can_connect_with"]);
    assert_eq!(with[0].as_str(), Some("Solana (mainnet, devnet)"));
}

#[test]
fn connecting_grants_the_wallets_cluster_and_shares_its_account() {
    let mut w = wc();
    let settle = w.connect();
    let params = settle.get("params").unwrap();
    let ns = field(params, &["namespaces", "solana"]);
    assert_eq!(items(ns, &["chains"])[0].as_str(), Some(CHAIN));
    assert_eq!(
        items(ns, &["accounts"])[0].as_str(),
        Some(format!("{CHAIN}:{}", owner_text()).as_str())
    );
    let methods: Vec<&str> = items(ns, &["methods"])
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(methods.contains(&"solana_signTransaction") && methods.contains(&"solana_signMessage"));
    let listed = w.f.service.wc_list_sessions().unwrap();
    let session = &items(&listed, &["sessions"])[0];
    assert_eq!(items(session, &["chains"])[0].as_str(), Some(CHAIN));
    assert!(
        !listed.to_json().contains(&owner_text()),
        "sessions never show addresses"
    );
}

#[test]
fn dapps_using_the_legacy_chain_identifier_are_answered_in_kind() {
    let mut w = wc();
    let settle = w.connect_on(LEGACY_CHAIN);
    let ns = field(settle.get("params").unwrap(), &["namespaces", "solana"]);
    assert_eq!(items(ns, &["chains"])[0].as_str(), Some(LEGACY_CHAIN));
    w.request(
        21,
        LEGACY_CHAIN,
        "solana_signMessage",
        &message_params(b"hi", &owner_text()),
    );
    assert_eq!(w.pending(), 1);
    w.approve(21).unwrap();
    assert!(w.answer().get("result").is_some());
}

#[test]
fn proposals_for_other_blockchains_or_clusters_are_refused() {
    let w = wc();
    w.propose(crate::walletconnect::testing::REQUIRED, "{}");
    let err =
        w.f.service
            .wc_approve_session_solana(PROPOSAL_ID, "w", PW)
            .unwrap_err();
    assert!(err.contains("requires Ethereum"), "{err}");

    let w = wc();
    w.propose(&solana_ask(DEVNET_CHAIN), "{}");
    let err =
        w.f.service
            .wc_approve_session_solana(PROPOSAL_ID, "w", PW)
            .unwrap_err();
    assert!(err.contains("solana:EtWTRABZ"), "{err}");

    // an Ethereum wallet cannot take a Solana proposal
    let w = wc();
    w.f.service.create_wallet("ethereum", "eth", PW).unwrap();
    w.propose(&solana_ask(CHAIN), "{}");
    let err =
        w.f.service
            .wc_approve_session(PROPOSAL_ID, "eth", 1, &[], PW)
            .unwrap_err();
    assert!(err.contains("requires Solana"), "{err}");
    let err =
        w.f.service
            .wc_approve_session_sui(PROPOSAL_ID, "eth", PW)
            .unwrap_err();
    assert!(
        err.contains("Sui wallet") || err.contains("wallet"),
        "{err}"
    );
}

#[test]
fn signing_a_message_answers_with_a_base58_signature() {
    let mut w = wc();
    w.connect();
    w.request(
        11,
        CHAIN,
        "solana_signMessage",
        &message_params(b"Sign in to example.com", &owner_text()),
    );
    let listed = w.f.service.wc_list_requests().unwrap();
    assert_eq!(
        text(&items(&listed, &["requests"])[0], &["kind"]),
        "message"
    );
    assert!(
        !listed.to_json().contains(&owner_text()),
        "the signer stays hidden until review"
    );
    let review = w.review(11).unwrap();
    assert_eq!(text(&review, &["chain"]), CHAIN);
    assert_eq!(text(&review, &["from"]), owner_text());
    assert_eq!(
        text(&review, &["details", "text"]),
        "Sign in to example.com"
    );
    assert!(w.relay.take_published().is_empty(), "review sends nothing");

    let report = w.approve(11).unwrap();
    assert_eq!(text(&report, &["status"]), "approved");
    let answer = w.answer();
    let signature =
        base58::decode_fixed::<64>(text(answer.get("result").unwrap(), &["signature"])).unwrap();
    assert!(ed25519::verify(
        &owner(),
        b"Sign in to example.com",
        &signature
    ));
    assert_eq!(w.pending(), 0);
    assert!(
        w.approve(11).is_err(),
        "a request can only be answered once"
    );
}

#[test]
fn a_message_that_is_a_transaction_is_never_signed_for_a_dapp() {
    let mut w = wc();
    w.connect();
    let t = transfer_tx(1_000_000_000);
    w.request(
        12,
        CHAIN,
        "solana_signMessage",
        &message_params(t.message.raw(), &owner_text()),
    );
    assert!(
        w.review(12)
            .unwrap_err()
            .contains("valid Solana transaction")
    );
    assert!(w.approve(12).is_err());
    assert_eq!(
        w.pending(),
        1,
        "the request stays open so it can be rejected"
    );
    assert!(w.relay.take_published().is_empty());
}

#[test]
fn signing_a_transaction_answers_with_the_signature_and_the_signed_transaction() {
    let mut w = wc();
    w.connect();
    let t = transfer_tx(5_000_000);
    w.request(13, CHAIN, "solana_signTransaction", &tx_params(&t));
    let review = w.review(13).unwrap();
    assert_eq!(field(&review, &["will_broadcast"]), &Value::Bool(false));
    assert_eq!(
        field(&review, &["simulation_succeeded"]),
        &Value::Bool(true)
    );
    let report = &items(&review, &["transactions"])[0];
    assert!(
        field(report, &["description"])
            .to_json()
            .contains("0.005 SOL")
    );
    assert!(w.relay.take_published().is_empty());

    w.approve(13).unwrap();
    let answer = w.answer();
    let result = answer.get("result").unwrap();
    let signed = Transaction::from_base64(text(result, &["transaction"])).unwrap();
    assert_signed_by_wallet(&signed);
    assert_eq!(signed.message, t.message);
    assert_eq!(
        text(result, &["signature"]),
        base58::encode(&signed.signatures[0])
    );
    assert!(
        w.f.world.borrow().sent.is_empty(),
        "signing only never submits"
    );
}

#[test]
fn sign_and_send_submits_and_answers_with_the_signature() {
    let mut w = wc();
    w.connect();
    let t = transfer_tx(5_000_000);
    w.request(14, CHAIN, "solana_signAndSendTransaction", &tx_params(&t));
    let review = w.review(14).unwrap();
    assert_eq!(field(&review, &["will_broadcast"]), &Value::Bool(true));
    assert!(w.f.world.borrow().sent.is_empty());

    let report = w.approve(14).unwrap();
    assert_eq!(text(&report, &["status"]), "approved");
    let sent = submitted(&w.f);
    assert_signed_by_wallet(&sent);
    let answer = w.answer();
    assert_eq!(
        text(answer.get("result").unwrap(), &["signature"]),
        sent.id().unwrap()
    );
    assert!(w.approve(14).is_err(), "never submitted twice");
    assert_eq!(w.f.world.borrow().sent.len(), 1);
}

#[test]
fn sign_all_signs_every_transaction_in_order() {
    let mut w = wc();
    w.connect();
    let (a, b) = (transfer_tx(1), transfer_tx(2));
    w.request(
        15,
        CHAIN,
        "solana_signAllTransactions",
        &format!(
            r#"{{"transactions":["{}","{}"]}}"#,
            a.to_base64(),
            b.to_base64()
        ),
    );
    let review = w.review(15).unwrap();
    assert_eq!(items(&review, &["transactions"]).len(), 2);
    w.approve(15).unwrap();
    let answer = w.answer();
    let signed = items(answer.get("result").unwrap(), &["transactions"]);
    assert_eq!(signed.len(), 2);
    for (encoded, original) in signed.iter().zip([&a, &b]) {
        let t = Transaction::from_base64(encoded.as_str().unwrap()).unwrap();
        assert_signed_by_wallet(&t);
        assert_eq!(&t.message, &original.message);
    }
    assert!(w.f.world.borrow().sent.is_empty());
}

#[test]
fn sign_all_signs_nothing_when_one_transaction_is_not_signable() {
    let mut w = wc();
    w.connect();
    let ix = tx::system_transfer(&key(5), &key(2), 1);
    let foreign = Transaction::unsigned(tx::compile_legacy(&key(5), &[ix], BLOCKHASH).unwrap());
    w.request(
        16,
        CHAIN,
        "solana_signAllTransactions",
        &format!(
            r#"{{"transactions":["{}","{}"]}}"#,
            transfer_tx(1).to_base64(),
            foreign.to_base64()
        ),
    );
    let err = w.approve(16).unwrap_err();
    assert!(
        err.contains("not one of the transaction's signers"),
        "{err}"
    );
    assert!(w.relay.take_published().is_empty());
    assert_eq!(w.pending(), 1);
}

#[test]
fn accounts_requests_are_answered_with_the_address() {
    let mut w = wc();
    w.connect();
    w.request(17, CHAIN, "solana_getAccounts", "{}");
    let review = w.review(17).unwrap();
    assert!(text(&review, &["note"]).contains("address"));
    w.approve(17).unwrap();
    let answer = w.answer();
    let account = &items(&answer, &["result"])[0];
    assert_eq!(text(account, &["pubkey"]), owner_text());
}

#[test]
fn the_deprecated_instruction_form_is_understood() {
    let mut w = wc();
    w.connect();
    let params = format!(
        r#"{{"feePayer":"{me}","recentBlockhash":"{}","instructions":[{{"programId":"{}","data":"{}","keys":[{{"pubkey":"{me}","isSigner":true,"isWritable":true}},{{"pubkey":"{}","isSigner":false,"isWritable":true}}]}}]}}"#,
        base58::encode(&BLOCKHASH),
        saddr::SYSTEM_PROGRAM,
        base58::encode(&{
            let mut d = 2u32.to_le_bytes().to_vec();
            d.extend_from_slice(&9u64.to_le_bytes());
            d
        }),
        key_text(2),
        me = owner_text()
    );
    w.request(18, CHAIN, "solana_signTransaction", &params);
    w.approve(18).unwrap();
    let answer = w.answer();
    let signed =
        Transaction::from_base64(text(answer.get("result").unwrap(), &["transaction"])).unwrap();
    assert_signed_by_wallet(&signed);
    assert_eq!(
        ops_of(&signed)[0],
        Op::SolTransfer {
            from: owner(),
            to: key(2),
            lamports: 9
        }
    );
}

#[test]
fn requests_that_do_not_match_the_session_or_wallet_are_refused() {
    let mut w = wc();
    w.connect();
    // another cluster than the session grants: refused by the protocol layer, never shown
    w.request(
        31,
        DEVNET_CHAIN,
        "solana_signMessage",
        &message_params(b"x", &owner_text()),
    );
    assert_eq!(w.pending(), 0);
    let refusal = w.answer();
    assert!(refusal.get("error").is_some());
    // a method that was not granted
    w.request(32, CHAIN, "solana_requestAccounts", "{}");
    assert_eq!(w.pending(), 0);
    assert!(w.answer().get("error").is_some());
    // the dapp names another signer
    w.request(
        33,
        CHAIN,
        "solana_signMessage",
        &message_params(b"x", &key_text(8)),
    );
    let err = w.approve(33).unwrap_err();
    assert!(err.contains("the dapp asked"), "{err}");
    // another wallet than the session's
    f_import_second(&w);
    let err =
        w.f.service
            .wc_approve_request_solana(33, "other", PW)
            .unwrap_err();
    assert!(
        err.contains("belongs to a session connected with wallet 'w'"),
        "{err}"
    );
    assert!(w.relay.take_published().is_empty());
}

fn f_import_second(w: &Wc) {
    w.f.service
        .create_wallet_on("solana", "other", PW, None)
        .unwrap();
}

#[test]
fn an_ethereum_wallet_cannot_answer_a_solana_request() {
    let mut w = wc();
    w.connect();
    w.f.service.create_wallet("ethereum", "eth", PW).unwrap();
    w.request(
        34,
        CHAIN,
        "solana_signMessage",
        &message_params(b"x", &owner_text()),
    );
    let err = w.f.service.wc_approve_request(34, "w", 1, PW).unwrap_err();
    assert!(err.contains("Solana request"), "{err}");
    let err =
        w.f.service
            .wc_approve_request_solana(34, "eth", PW)
            .unwrap_err();
    assert!(
        err.contains("Ethereum wallet") || err.contains("belongs to a session"),
        "{err}"
    );
    assert_eq!(w.pending(), 1);
}

#[test]
fn a_failing_dry_run_is_never_submitted() {
    let mut w = wc();
    w.connect();
    w.f.world.borrow_mut().sim_error = Some(r#""InsufficientFundsForRent""#.to_string());
    w.request(
        35,
        CHAIN,
        "solana_signAndSendTransaction",
        &tx_params(&transfer_tx(1)),
    );
    let review = w.review(35).unwrap();
    assert_eq!(
        field(&review, &["simulation_succeeded"]),
        &Value::Bool(false)
    );
    let err = w.approve(35).unwrap_err();
    assert!(err.contains("not sending"), "{err}");
    assert!(w.f.world.borrow().sent.is_empty());
    assert!(w.relay.take_published().is_empty());
    assert_eq!(
        w.pending(),
        1,
        "the request stays open so it can be rejected"
    );
    // signing only is still possible for a dapp that submits it itself
    w.request(
        36,
        CHAIN,
        "solana_signTransaction",
        &tx_params(&transfer_tx(1)),
    );
    assert!(w.approve(36).is_ok());
}

#[test]
fn a_rejected_submission_closes_the_request_and_tells_the_dapp() {
    let mut w = wc_with(SendMode::Reject);
    w.f.world.borrow_mut().status = None;
    w.connect();
    w.request(
        37,
        CHAIN,
        "solana_signAndSendTransaction",
        &tx_params(&transfer_tx(1)),
    );
    let err = w.approve(37).unwrap_err();
    assert!(
        err.contains("rejected") && err.contains("request has been closed"),
        "{err}"
    );
    assert_eq!(w.pending(), 0);
    assert!(w.answer().get("error").is_some());
}

#[test]
fn requests_are_rejected_without_a_password() {
    let mut w = wc();
    w.connect();
    w.request(
        38,
        CHAIN,
        "solana_signMessage",
        &message_params(b"x", &owner_text()),
    );
    let report =
        w.f.service
            .wc_reject_request(38, Some("no thanks"))
            .unwrap();
    assert_eq!(text(&report, &["status"]), "rejected");
    assert!(w.answer().get("error").is_some());
    assert_eq!(w.pending(), 0);
}

#[test]
fn the_wallet_stays_locked_between_walletconnect_calls() {
    let mut w = wc();
    w.connect();
    w.request(
        39,
        CHAIN,
        "solana_signMessage",
        &message_params(b"x", &owner_text()),
    );
    assert!(
        w.f.service
            .wc_review_request_solana(39, "w", "wrong-password")
            .is_err()
    );
    w.review(39).unwrap();
    w.approve(39).unwrap();
    for entry in std::fs::read_dir(w.f._dir.0.join("wallets")).unwrap() {
        let content = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!content.contains(&owner_text()));
    }
}

#[test]
fn the_tools_route_solana_walletconnect_calls() {
    let relay = Rc::new(MockRelay::default());
    let manager = WcManager::new(relay.clone()).with_settle_wait(Duration::ZERO);
    let dir = TempDir::new();
    let w = world();
    let mock = Rc::new(node(&w, SendMode::Accept, MAINNET_GENESIS));
    let http: Rc<dyn HttpClient> = mock.clone();
    let service = WalletService::new(Store::open(&dir.0).unwrap(), http)
        .with_iterations(vault::MIN_ITERATIONS)
        .with_receipt_wait(Duration::ZERO, 3)
        .with_walletconnect(manager);
    let tb = Toolbox::new(service);
    let dapp = Dapp::new();
    let (err, _) = call(
        &tb,
        "import_wallet",
        &format!(
            r#"{{"blockchain":"solana","wallet_name":"s","password":"{PW}","private_key":"{}"}}"#,
            key_hex()
        ),
    );
    assert!(!err);
    dapp.send_proposal(&relay, PROPOSAL_ID, &solana_ask(CHAIN), "{}");
    let (err, v) = call(
        &tb,
        "wc_pair",
        &format!(r#"{{"uri":"{}","wait_seconds":0}}"#, dapp.uri()),
    );
    assert!(!err, "{}", v.to_json());
    let creds = format!(r#""wallet_name":"s","password":"{PW}""#);
    // Solana wallets give no chain_id
    let (err, v) = call(
        &tb,
        "wc_approve_session",
        &format!(r#"{{"proposal_id":"{PROPOSAL_ID}",{creds},"chain_id":1}}"#),
    );
    assert!(err && v.to_json().contains("'chain_id'"));
    let (err, v) = call(
        &tb,
        "wc_approve_session",
        &format!(r#"{{"proposal_id":"{PROPOSAL_ID}",{creds}}}"#),
    );
    assert!(!err, "{}", v.to_json());
    assert_eq!(text(&v, &["network"]), "mainnet");
    let (err, v) = call(&tb, "wc_list_sessions", "{}");
    assert!(!err && v.to_json().contains(CHAIN));
}
