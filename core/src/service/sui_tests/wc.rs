//! WalletConnect with a Sui wallet, against a mock relay playing the dapp.

use std::time::Duration;

use super::*;
use crate::walletconnect::manager::WcManager;
use crate::walletconnect::proto;
use crate::walletconnect::relay::mock::MockRelay;
use crate::walletconnect::testing::Dapp;

const PROPOSAL_ID: u64 = 1_700_000_000_000_000_001;
const SUI_ASK: &str = r#"{"sui":{"chains":["sui:mainnet"],"methods":["sui_signTransaction","sui_signAndExecuteTransaction","sui_signPersonalMessage"],"events":[]}}"#;

struct Wc {
    f: Fx,
    relay: Rc<MockRelay>,
    dapp: Dapp,
}

fn wc() -> Wc {
    let relay = Rc::new(MockRelay::default());
    let manager = WcManager::new(relay.clone()).with_settle_wait(Duration::ZERO);
    let f = build(node(&[(1, 5_000_000_000)], 0), |s| {
        s.with_walletconnect(manager)
    });
    import(&f);
    Wc {
        f,
        relay,
        dapp: Dapp::new(),
    }
}

impl Wc {
    fn propose(&self, required: &str, optional: &str) -> Value {
        self.dapp
            .send_proposal(&self.relay, PROPOSAL_ID, required, optional);
        self.f.service.wc_pair(&self.dapp.uri(), Some(0)).unwrap()
    }

    /// Connects the dapp; returns the settle message the dapp received.
    fn connect(&mut self) -> Value {
        self.propose(SUI_ASK, "{}");
        self.f
            .service
            .wc_approve_session_sui(PROPOSAL_ID, "w", PW)
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
}

fn tx_params(bytes: &str, address: &str) -> String {
    format!(r#"{{"transaction":"{bytes}","address":"{address}"}}"#)
}

#[test]
fn a_sui_proposal_is_connectable_with_a_sui_wallet_only() {
    let w = wc();
    let out = w.propose(SUI_ASK, "{}");
    let proposal = &out.get("proposals").unwrap().as_array().unwrap()[0];
    assert_eq!(
        proposal.get("can_connect").and_then(Value::as_bool),
        Some(true)
    );
    let with = field(proposal, &["can_connect_with"]).as_array().unwrap();
    assert_eq!(with.len(), 1);
    assert_eq!(with[0].as_str(), Some("Sui"));
}

#[test]
fn connecting_grants_the_wallets_network_and_shares_its_accounts() {
    let mut w = wc();
    let settle = w.connect();
    let params = settle.get("params").unwrap();
    let ns = field(params, &["namespaces", "sui"]);
    assert_eq!(
        field(ns, &["chains"]).as_array().unwrap()[0].as_str(),
        Some("sui:mainnet")
    );
    assert_eq!(
        field(ns, &["accounts"]).as_array().unwrap()[0].as_str(),
        Some(format!("sui:mainnet:{}", wallet_address()).as_str())
    );
    let methods = field(ns, &["methods"]).as_array().unwrap();
    assert!(
        methods
            .iter()
            .any(|m| m.as_str() == Some("sui_getAccounts"))
    );
    let props = text(params, &["sessionProperties", "sui_getAccounts"]);
    let accounts = json::parse(props).unwrap();
    let account = &accounts.as_array().unwrap()[0];
    assert_eq!(text(account, &["address"]), wallet_address());
    let pubkey = base64::decode(text(account, &["pubkey"])).unwrap();
    let key = vault().signer().unwrap().public_key();
    assert_eq!(pubkey[0], 0);
    assert_eq!(&pubkey[1..], key.as_bytes());
    let listed = w.f.service.wc_list_sessions().unwrap();
    let session = &field(&listed, &["sessions"]).as_array().unwrap()[0];
    assert_eq!(
        field(session, &["chains"]).as_array().unwrap()[0].as_str(),
        Some("sui:mainnet")
    );
}

#[test]
fn an_ethereum_only_dapp_is_not_connected_to_a_sui_wallet() {
    let w = wc();
    w.propose(crate::walletconnect::testing::REQUIRED, "{}");
    let err =
        w.f.service
            .wc_approve_session_sui(PROPOSAL_ID, "w", PW)
            .unwrap_err();
    assert!(err.contains("requires Ethereum"), "{err}");
}

#[test]
fn a_dapp_needing_another_network_is_refused() {
    let w = wc();
    w.propose(
        r#"{"sui":{"chains":["sui:testnet"],"methods":["sui_signTransaction"],"events":[]}}"#,
        "{}",
    );
    let err =
        w.f.service
            .wc_approve_session_sui(PROPOSAL_ID, "w", PW)
            .unwrap_err();
    assert!(err.contains("sui:testnet"), "{err}");
}

#[test]
fn signing_a_transaction_answers_with_signature_and_bytes() {
    let mut w = wc();
    w.connect();
    let transaction = own_transaction(5_000_000);
    let bytes = tx::encode(&transaction).unwrap();
    w.request(
        11,
        "sui:mainnet",
        "sui_signTransaction",
        &tx_params(&bytes, &wallet_address()),
    );
    assert_eq!(w.pending(), 1);
    let review = w.f.service.wc_review_request_sui(11, "w", PW).unwrap();
    assert_eq!(text(&review, &["chain"]), "sui:mainnet");
    assert_eq!(field(&review, &["will_broadcast"]), &Value::Bool(false));
    assert_eq!(
        field(&review, &["simulation_succeeded"]),
        &Value::Bool(true)
    );
    assert!(w.relay.take_published().is_empty(), "review sends nothing");

    w.f.service.wc_approve_request_sui(11, "w", PW).unwrap();
    let answer = w.answer();
    let result = answer.get("result").unwrap();
    assert_eq!(text(result, &["transactionBytes"]), bytes);
    let signature = UserSignature::from_base64(text(result, &["signature"])).unwrap();
    assert_signed_by_wallet(&transaction, &signature);
    assert!(w.f.mock.bodies("executeTransaction").is_empty());
    assert_eq!(w.pending(), 0);
}

#[test]
fn sign_and_execute_submits_and_answers_with_the_digest() {
    let mut w = wc();
    w.connect();
    let transaction = own_transaction(5_000_000);
    let bytes = tx::encode(&transaction).unwrap();
    w.request(
        12,
        "sui:mainnet",
        "sui_signAndExecuteTransaction",
        &tx_params(&bytes, &wallet_address()),
    );
    let review = w.f.service.wc_review_request_sui(12, "w", PW).unwrap();
    assert_eq!(field(&review, &["will_broadcast"]), &Value::Bool(true));
    assert!(w.f.mock.bodies("executeTransaction").is_empty());

    w.f.service.wc_approve_request_sui(12, "w", PW).unwrap();
    let answer = w.answer();
    assert_eq!(
        text(answer.get("result").unwrap(), &["digest"]),
        tx::digest_of(&transaction)
    );
    let (sent, signature) = submitted(&w.f);
    assert_eq!(tx::digest_of(&sent), tx::digest_of(&transaction));
    assert_signed_by_wallet(&sent, &signature);
}

#[test]
fn a_failing_dry_run_is_never_submitted() {
    let relay = Rc::new(MockRelay::default());
    let manager = WcManager::new(relay.clone()).with_settle_wait(Duration::ZERO);
    let failing = SuiMock::new()
        .data(
            "simulateTransaction",
            &effects_data("simulateTransaction", "FAILURE", Some("MoveAbort"), &[]),
        )
        .extend(node(&[(1, 5_000_000_000)], 0));
    let f = build(failing, |s| s.with_walletconnect(manager));
    import(&f);
    let mut w = Wc {
        f,
        relay,
        dapp: Dapp::new(),
    };
    w.connect();
    let bytes = tx::encode(&own_transaction(5_000_000)).unwrap();
    w.request(
        13,
        "sui:mainnet",
        "sui_signAndExecuteTransaction",
        &tx_params(&bytes, &wallet_address()),
    );
    let err = w.f.service.wc_approve_request_sui(13, "w", PW).unwrap_err();
    assert!(err.contains("not sending"), "{err}");
    assert!(w.f.mock.bodies("executeTransaction").is_empty());
    assert!(w.relay.take_published().is_empty());
    assert_eq!(w.pending(), 1, "the request stays open");
}

#[test]
fn personal_messages_are_signed_as_text_with_the_personal_intent() {
    let mut w = wc();
    w.connect();
    w.request(
        14,
        "sui:mainnet",
        "sui_signPersonalMessage",
        &format!(
            r#"{{"message":"Hello Sui","address":"{}"}}"#,
            wallet_address()
        ),
    );
    let review = w.f.service.wc_review_request_sui(14, "w", PW).unwrap();
    assert_eq!(text(&review, &["details", "text"]), "Hello Sui");
    w.f.service.wc_approve_request_sui(14, "w", PW).unwrap();
    let answer = w.answer();
    let key = vault().signer().unwrap();
    assert_eq!(
        text(answer.get("result").unwrap(), &["signature"]),
        tx::sign_personal_message(&key, b"Hello Sui").unwrap()
    );
}

#[test]
fn get_accounts_is_a_request_the_wallet_answers() {
    let mut w = wc();
    w.connect();
    w.request(15, "sui:mainnet", "sui_getAccounts", "{}");
    w.f.service.wc_approve_request_sui(15, "w", PW).unwrap();
    let answer = w.answer();
    let account = &answer.get("result").unwrap().as_array().unwrap()[0];
    assert_eq!(text(account, &["address"]), wallet_address());
}

#[test]
fn requests_for_another_network_or_method_never_reach_the_agent() {
    let mut w = wc();
    w.connect();
    w.request(16, "sui:testnet", "sui_signPersonalMessage", "{}");
    assert_eq!(w.pending(), 0);
    let answer = w.answer();
    assert!(answer.get("error").is_some());
    w.request(17, "eip155:1", "personal_sign", r#"["0x00","0x00"]"#);
    assert_eq!(w.pending(), 0);
    assert!(w.answer().get("error").is_some());
    w.request(18, "sui:mainnet", "sui_nope", "{}");
    assert_eq!(w.pending(), 0);
    assert!(w.answer().get("error").is_some());
}

#[test]
fn a_request_naming_another_signer_is_refused() {
    let mut w = wc();
    w.connect();
    w.request(
        19,
        "sui:mainnet",
        "sui_signPersonalMessage",
        &format!(r#"{{"message":"x","address":"{}"}}"#, other_address()),
    );
    let err = w.f.service.wc_approve_request_sui(19, "w", PW).unwrap_err();
    assert!(err.contains("refusing"), "{err}");
    assert!(w.relay.take_published().is_empty());
}

#[test]
fn the_wrong_password_or_wallet_signs_nothing() {
    let mut w = wc();
    w.connect();
    w.request(
        20,
        "sui:mainnet",
        "sui_signPersonalMessage",
        &format!(r#"{{"message":"x","address":"{}"}}"#, wallet_address()),
    );
    assert!(
        w.f.service
            .wc_approve_request_sui(20, "w", "wrong-password")
            .is_err()
    );
    let err =
        w.f.service
            .wc_approve_request_sui(20, "other", PW)
            .unwrap_err();
    assert!(err.contains("belongs to a session"), "{err}");
    assert!(w.relay.take_published().is_empty());
}

#[test]
fn ethereum_approval_tools_refuse_a_sui_request() {
    let mut w = wc();
    w.connect();
    w.request(
        21,
        "sui:mainnet",
        "sui_signPersonalMessage",
        &format!(r#"{{"message":"x","address":"{}"}}"#, wallet_address()),
    );
    let err = w.f.service.wc_approve_request(21, "w", 1, PW).unwrap_err();
    assert!(err.contains("Sui request"), "{err}");
    assert!(w.relay.take_published().is_empty());
}

#[test]
fn malformed_sui_requests_are_rejected_to_the_dapp() {
    let mut w = wc();
    w.connect();
    w.request(
        22,
        "sui:mainnet",
        "sui_signTransaction",
        r#"{"transaction":"AAAA","address":"0x1"}"#,
    );
    assert_eq!(w.pending(), 0);
    assert!(w.answer().get("error").is_some());
}
