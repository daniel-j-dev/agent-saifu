//! WalletConnect operations of the wallet service.
//!
//! Connecting a dapp and answering its requests both unlock the wallet for that one action
//! only. Every approval names the wallet, the chain and the password, and the wallet and chain
//! must match what the dapp's session and request are for.

use std::time::Duration;

use crate::crypto::hex;
use crate::eth::address::{Address, to_checksum};
use crate::eth::explorer::sanitize_text_max;
use crate::eth::networks::Network;
use crate::eth::ops::{self, TokenInfo, TxIntent, num};
use crate::eth::sign;
use crate::eth::u256::U256;
use crate::json::Value;
use crate::service::WalletService;
use crate::walletconnect::manager::{PendingRequest, Proposal, Session};
use crate::walletconnect::namespaces::{self, DappMetadata, ProposalData, Requirements};
use crate::walletconnect::now_secs;
use crate::walletconnect::proto;
use crate::walletconnect::requests::{self, Action, TxRequest};

pub const DEFAULT_PAIR_WAIT_SECS: u64 = 20;
pub const MAX_PAIR_WAIT_SECS: u64 = 60;
const DAPP_NOT_VERIFIED: &str = "the dapp's name and URL are reported by the dapp itself and are not verified; only connect to sites you trust";

fn id_string(id: u64) -> Value {
    Value::String(id.to_string())
}

fn strings(items: &[String]) -> Value {
    Value::Array(items.iter().map(|s| Value::str(s)).collect())
}

fn chain_list(chains: &[u64]) -> Value {
    Value::Array(chains.iter().map(|c| num(*c)).collect())
}

fn seconds_left(deadline: u64) -> Value {
    num(deadline.saturating_sub(now_secs()))
}

fn requirements_value(r: &Requirements) -> Value {
    Value::object(vec![
        ("chain_ids", chain_list(&r.chains)),
        ("methods", strings(&r.methods)),
        ("events", strings(&r.events)),
    ])
}

/// What the agent needs to decide whether to connect; no address is involved.
pub fn proposal_value(proposal: &Proposal) -> Value {
    let data: &ProposalData = &proposal.data;
    let problem = namespaces::plan_approval(data, &namespaces::all_chains(data)).err();
    let mut warnings = vec![DAPP_NOT_VERIFIED.to_string()];
    if let Some(p) = &problem {
        warnings.push(p.clone());
    }
    Value::object(vec![
        ("proposal_id", id_string(proposal.id)),
        ("dapp", data.dapp.to_value()),
        ("required", requirements_value(&data.required)),
        ("optional", requirements_value(&data.optional)),
        ("can_connect", Value::Bool(problem.is_none())),
        ("expires_in_seconds", seconds_left(proposal.expires_at)),
        ("warnings", strings(&warnings)),
    ])
}

pub fn session_value(session: &Session) -> Value {
    Value::object(vec![
        ("session_topic", Value::str(&session.topic)),
        ("wallet", Value::str(&session.wallet)),
        ("dapp", session.dapp.to_value()),
        ("chain_ids", chain_list(&session.chains)),
        ("methods", strings(&session.methods)),
        ("confirmed_by_dapp", Value::Bool(session.acknowledged)),
        ("expires_in_seconds", seconds_left(session.expires_at)),
    ])
}

fn selector_of(data: &[u8]) -> Option<String> {
    (data.len() >= 4).then(|| hex::encode_prefixed(&data[..4]))
}

fn transaction_details(tx: &TxRequest) -> Value {
    let mut pairs = vec![
        (
            "to",
            tx.to
                .map(|a| Value::String(to_checksum(&a)))
                .unwrap_or(Value::Null),
        ),
        (
            "value_native",
            Value::String(tx.value.format_units(ops::NATIVE_DECIMALS)),
        ),
        ("data_bytes", num(tx.data.len() as u64)),
    ];
    if let Some(selector) = selector_of(&tx.data) {
        pairs.push(("function_selector", Value::String(selector)));
    }
    if let Some(to) = tx.to
        && let Some((decoded, warnings)) = ops::describe_intent(&TxIntent {
            to,
            value: tx.value,
            data: tx.data.clone(),
        })
    {
        pairs.push(("decoded", decoded));
        pairs.push(("warnings", strings(&warnings)));
    }
    Value::object(pairs)
}

fn action_details(action: &Action) -> (&'static str, Value) {
    match action {
        Action::SendTransaction(tx) | Action::SignTransaction(tx) => {
            ("transaction", transaction_details(tx))
        }
        Action::PersonalSign { message, .. } => ("message", requests::describe_message(message)),
        Action::SignTypedData { data, .. } => ("typed_data", requests::describe_typed_data(data)),
    }
}

/// A pending request as the agent first sees it. The signer's address is left out because
/// addresses stay hidden until the wallet is unlocked.
pub fn request_value(request: &PendingRequest, dapp: &DappMetadata) -> Value {
    let (kind, details) = action_details(&request.action);
    Value::object(vec![
        ("request_id", id_string(request.id)),
        ("session_topic", Value::str(&request.session_topic)),
        ("dapp", dapp.to_value()),
        ("method", Value::str(&request.method)),
        ("kind", Value::str(kind)),
        ("chain_id", num(request.chain_id)),
        ("expires_in_seconds", seconds_left(request.expires_at)),
        ("details", details),
    ])
}

fn amount_text(raw: &str, token: Option<&TokenInfo>) -> String {
    match (U256::from_dec_str(raw), token) {
        (Ok(v), Some(t)) => format!("{} {}", v.format_units(t.decimals), t.symbol),
        _ => format!("{raw} units of the token"),
    }
}

/// Plain-language lines describing what the transaction does to the wallet.
pub fn outcome_lines(
    network: &Network,
    intent: &TxIntent,
    decoded: Option<&Value>,
    token: Option<&TokenInfo>,
) -> Vec<String> {
    let mut lines = vec![];
    if !intent.value.is_zero() {
        lines.push(format!(
            "Sends {} {} from your wallet to {}",
            intent.value.format_units(ops::NATIVE_DECIMALS),
            network.currency_symbol,
            to_checksum(&intent.to)
        ));
    }
    let text = |key: &str| {
        decoded
            .and_then(|d| d.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let action = text("action");
    let symbol = token.map(|t| t.symbol.as_str()).unwrap_or("tokens");
    match action.as_str() {
        "erc20_transfer" => lines.push(format!(
            "Transfers {} to {}",
            amount_text(&text("amount_raw"), token),
            text("recipient")
        )),
        "erc20_approve" => {
            let unlimited = decoded
                .and_then(|d| d.get("unlimited"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let amount = if unlimited {
                format!("UNLIMITED {symbol}")
            } else {
                amount_text(&text("amount_raw"), token)
            };
            lines.push(format!(
                "Lets {} spend up to {amount} from your wallet",
                text("spender")
            ));
        }
        "erc20_revoke_approval" => lines.push(format!(
            "Removes {}'s permission to spend your {symbol}",
            text("spender")
        )),
        "set_approval_for_all" => {
            let approved = decoded
                .and_then(|d| d.get("approved"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            lines.push(if approved {
                format!(
                    "Lets {} move ALL of your tokens in the collection at {}",
                    text("operator"),
                    text("token_contract")
                )
            } else {
                format!(
                    "Removes {}'s permission over your tokens in the collection at {}",
                    text("operator"),
                    text("token_contract")
                )
            });
        }
        _ if !intent.data.is_empty() => lines.push(format!(
            "Calls the contract at {}{}. What it does with your assets cannot be decoded here, so check the dapp and the contract address",
            to_checksum(&intent.to),
            selector_of(&intent.data)
                .map(|s| format!(" (function {s})"))
                .unwrap_or_default()
        )),
        _ if intent.value.is_zero() => lines.push(format!(
            "Sends an empty transaction to {}",
            to_checksum(&intent.to)
        )),
        _ => {}
    }
    lines
}

fn names_match(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

struct OpenRequest {
    record_name: String,
    contents: crate::eth::wallet::VaultContents,
    network: Network,
    owner: Address,
    request: PendingRequest,
    dapp: DappMetadata,
}

fn token_for(rpc: &crate::eth::rpc::RpcClient<'_>, intent: &TxIntent) -> Option<TokenInfo> {
    let (decoded, _) = ops::describe_intent(intent)?;
    decoded.get("token_contract")?;
    ops::token_info(rpc, &intent.to).ok()
}

fn intent_of(tx: &TxRequest) -> Result<TxIntent, String> {
    let to = tx.to.ok_or(
        "this transaction creates a contract, which this wallet does not sign for dapps; reject it",
    )?;
    Ok(TxIntent {
        to,
        value: tx.value,
        data: tx.data.clone(),
    })
}

impl WalletService {
    fn wc_clone_request(
        &self,
        request_id: u64,
    ) -> Result<(PendingRequest, SessionSummary), String> {
        let mut wc = self.wc.borrow_mut();
        wc.poll();
        let request = wc.request(request_id)?;
        let session = wc
            .sessions()
            .iter()
            .find(|s| s.topic == request.session_topic)
            .ok_or("the session for this request has ended")?;
        let copy = PendingRequest {
            id: request.id,
            session_topic: request.session_topic.clone(),
            method: request.method.clone(),
            chain_id: request.chain_id,
            action: request.action.clone(),
            received_at: request.received_at,
            expires_at: request.expires_at,
        };
        Ok((copy, clone_session_summary(session)))
    }

    /// Checks wallet and chain against the session and request, then unlocks for one action.
    fn wc_open_request(
        &self,
        request_id: u64,
        wallet: &str,
        chain_id: u64,
        password: &str,
    ) -> Result<OpenRequest, String> {
        let (request, session) = self.wc_clone_request(request_id)?;
        if chain_id != request.chain_id {
            return Err(format!(
                "this request is for chain ID {}, not {chain_id}",
                request.chain_id
            ));
        }
        if !names_match(wallet, &session.wallet) {
            return Err(format!(
                "this request belongs to a session connected with wallet '{}', not '{}'",
                session.wallet,
                sanitize_text_max(wallet, 60)
            ));
        }
        let (record, contents) = self.unlock(wallet, password)?;
        let network = Self::network_of(&contents, chain_id)?;
        let owner = contents.address()?;
        if request.action.signer() != &owner {
            return Err(format!(
                "the dapp asked {} to sign, but this wallet's address is {}; refusing",
                to_checksum(request.action.signer()),
                to_checksum(&owner)
            ));
        }
        Ok(OpenRequest {
            record_name: record.name,
            contents,
            network,
            owner,
            request,
            dapp: session.dapp,
        })
    }

    // ---- connecting ---------------------------------------------------------

    pub fn wc_pair(&self, uri: &str, wait_secs: Option<u64>) -> Result<Value, String> {
        let wait = wait_secs
            .unwrap_or(DEFAULT_PAIR_WAIT_SECS)
            .min(MAX_PAIR_WAIT_SECS);
        let mut wc = self.wc.borrow_mut();
        let outcome = wc.pair(uri, Duration::from_secs(wait))?;
        let proposals: Vec<Value> = outcome
            .proposal_ids
            .iter()
            .filter_map(|id| wc.proposal(*id))
            .map(proposal_value)
            .collect();
        let received = !proposals.is_empty();
        Ok(Value::object(vec![
            ("pairing_topic", Value::String(outcome.pairing_topic)),
            (
                "status",
                Value::str(if received {
                    "proposal_received"
                } else {
                    "waiting_for_proposal"
                }),
            ),
            ("proposals", Value::Array(proposals)),
            (
                "next_step",
                Value::str(if received {
                    "Check the dapp and what it asks for, then call wc_approve_session (proposal_id, wallet_name, chain_id, password) or wc_reject_session (proposal_id)."
                } else {
                    "No proposal has arrived yet. Make sure the dapp is still showing its connection URI, then call wc_list_proposals."
                }),
            ),
        ]))
    }

    pub fn wc_list_proposals(&self) -> Result<Value, String> {
        let mut wc = self.wc.borrow_mut();
        wc.poll();
        let items: Vec<Value> = wc.proposals().iter().map(proposal_value).collect();
        Ok(Value::object(vec![
            ("count", num(items.len() as u64)),
            ("proposals", Value::Array(items)),
        ]))
    }

    pub fn wc_approve_session(
        &self,
        proposal_id: u64,
        wallet: &str,
        chain_id: u64,
        additional_chain_ids: &[u64],
        password: &str,
    ) -> Result<Value, String> {
        let data = {
            let mut wc = self.wc.borrow_mut();
            wc.poll();
            wc.proposal(proposal_id)
                .ok_or("no such proposal (it may have expired); ask the dapp to connect again")?
                .data
                .clone()
        };
        let mut chosen = vec![chain_id];
        chosen.extend_from_slice(additional_chain_ids);
        let approval = namespaces::plan_approval(&data, &chosen)?;
        let (record, contents) = self.unlock(wallet, password)?;
        for chain in &approval.chains {
            Self::network_of(&contents, *chain)?;
        }
        let address = contents.address()?;
        let approved =
            self.wc
                .borrow_mut()
                .approve_session(proposal_id, &record.name, &address, &approval)?;
        let note = if approved.acknowledged {
            "The dapp confirmed the session."
        } else {
            "The session was sent to the dapp but it has not confirmed yet; check wc_list_sessions."
        };
        Ok(Value::object(vec![
            ("session_topic", Value::String(approved.topic)),
            ("wallet", Value::String(record.name)),
            ("dapp", data.dapp.to_value()),
            ("chain_ids", chain_list(&approval.chains)),
            ("methods", strings(&approval.methods)),
            ("confirmed_by_dapp", Value::Bool(approved.acknowledged)),
            ("note", Value::str(note)),
        ]))
    }

    pub fn wc_reject_session(&self, proposal_id: u64) -> Result<Value, String> {
        let mut wc = self.wc.borrow_mut();
        wc.poll();
        let notified = wc.reject_session(proposal_id)?;
        Ok(Value::object(vec![
            ("proposal_id", id_string(proposal_id)),
            ("status", Value::str("rejected")),
            ("dapp_notified", Value::Bool(notified)),
        ]))
    }

    pub fn wc_list_sessions(&self) -> Result<Value, String> {
        let mut wc = self.wc.borrow_mut();
        wc.poll();
        let items: Vec<Value> = wc.sessions().iter().map(session_value).collect();
        Ok(Value::object(vec![
            ("count", num(items.len() as u64)),
            ("sessions", Value::Array(items)),
            (
                "note",
                Value::str(
                    "Sessions live only while this server is running; reconnect the dapp after a restart.",
                ),
            ),
        ]))
    }

    pub fn wc_disconnect_session(&self, session_topic: &str) -> Result<Value, String> {
        let mut wc = self.wc.borrow_mut();
        wc.poll();
        let topic = wc.find_session(session_topic)?.topic.clone();
        let notified = wc.disconnect(&topic)?;
        Ok(Value::object(vec![
            ("session_topic", Value::String(topic)),
            ("status", Value::str("disconnected")),
            ("dapp_notified", Value::Bool(notified)),
        ]))
    }

    // ---- requests -----------------------------------------------------------

    pub fn wc_list_requests(&self) -> Result<Value, String> {
        let mut wc = self.wc.borrow_mut();
        wc.poll();
        let items: Vec<Value> = wc
            .requests()
            .iter()
            .filter_map(|r| {
                let dapp = wc
                    .sessions()
                    .iter()
                    .find(|s| s.topic == r.session_topic)
                    .map(|s| s.dapp.clone())?;
                Some(request_value(r, &dapp))
            })
            .collect();
        Ok(Value::object(vec![
            ("count", num(items.len() as u64)),
            ("requests", Value::Array(items)),
        ]))
    }

    /// Everything about a request: what it does, what it costs and whether it would succeed.
    pub fn wc_review_request(
        &self,
        request_id: u64,
        wallet: &str,
        chain_id: u64,
        password: &str,
    ) -> Result<Value, String> {
        let open = self.wc_open_request(request_id, wallet, chain_id, password)?;
        let (kind, details) = action_details(&open.request.action);
        let mut pairs = vec![
            ("request_id", id_string(request_id)),
            ("wallet", Value::str(&open.record_name)),
            ("from", Value::String(to_checksum(&open.owner))),
            ("dapp", open.dapp.to_value()),
            ("method", Value::str(&open.request.method)),
            ("kind", Value::str(kind)),
            ("chain_id", num(chain_id)),
            ("network", Value::str(&open.network.name)),
            ("expires_in_seconds", seconds_left(open.request.expires_at)),
        ];
        match &open.request.action {
            Action::SendTransaction(tx) | Action::SignTransaction(tx) => {
                self.review_transaction(&open, tx, &mut pairs)?;
            }
            Action::SignTypedData { data, .. } => {
                check_domain_chain(data, chain_id)?;
                pairs.push(("details", details));
                pairs.push((
                    "note",
                    Value::str(
                        "Approving signs this data off-chain; nothing is sent to the network.",
                    ),
                ));
            }
            Action::PersonalSign { .. } => {
                pairs.push(("details", details));
                pairs.push((
                    "note",
                    Value::str(
                        "Approving signs this message off-chain; nothing is sent to the network.",
                    ),
                ));
            }
        }
        Ok(Value::object(pairs))
    }

    fn review_transaction(
        &self,
        open: &OpenRequest,
        tx: &TxRequest,
        pairs: &mut Vec<(&'static str, Value)>,
    ) -> Result<(), String> {
        check_tx_chain(tx, open.network.chain_id)?;
        let intent = intent_of(tx)?;
        let rpc = ops::connect(&*self.http, &open.network)?;
        let simulation =
            ops::simulate_report(&*self.http, &rpc, &open.network, &open.owner, &intent)?;
        let token = token_for(&rpc, &intent);
        let decoded = simulation.get("decoded").cloned();
        let mut lines = outcome_lines(&open.network, &intent, decoded.as_ref(), token.as_ref());
        if let Some(max_fee) = simulation.get("max_fee").and_then(Value::as_str) {
            let usd = simulation
                .get("expected_fee_usdc")
                .and_then(Value::as_str)
                .map(|u| format!(" (about {u} USDC expected)"))
                .unwrap_or_default();
            lines.push(format!(
                "Network fee: at most {max_fee} {}{usd}",
                open.network.currency_symbol
            ));
        }
        let will_succeed = simulation.get("success").and_then(Value::as_bool) == Some(true);
        pairs.push((
            "will_broadcast",
            Value::Bool(matches!(open.request.action, Action::SendTransaction(_))),
        ));
        pairs.push(("simulation_succeeded", Value::Bool(will_succeed)));
        pairs.push(("expected_outcome", strings(&lines)));
        if let Some(t) = &token {
            pairs.push((
                "token",
                Value::object(vec![
                    ("symbol", Value::str(&t.symbol)),
                    ("decimals", num(u64::from(t.decimals))),
                    ("contract", Value::String(to_checksum(&t.address))),
                ]),
            ));
        }
        if !tx.ignored_fields.is_empty() {
            pairs.push((
                "note",
                Value::String(format!(
                    "The dapp's {} setting(s) are ignored: this wallet picks the gas limit, fees and nonce itself.",
                    tx.ignored_fields.join(", ")
                )),
            ));
        }
        pairs.push(("simulation", simulation));
        Ok(())
    }

    /// Signs (and for `eth_sendTransaction` broadcasts), then answers the dapp.
    pub fn wc_approve_request(
        &self,
        request_id: u64,
        wallet: &str,
        chain_id: u64,
        password: &str,
    ) -> Result<Value, String> {
        let open = self.wc_open_request(request_id, wallet, chain_id, password)?;
        let key = open.contents.private_key_array()?;
        let (answer, mut report) = match &open.request.action {
            Action::SendTransaction(tx) | Action::SignTransaction(tx) => {
                check_tx_chain(tx, chain_id)?;
                let broadcast = matches!(open.request.action, Action::SendTransaction(_));
                let intent = intent_of(tx)?;
                let rpc = ops::connect_for_send(&*self.http, &open.network, &self.journal)?;
                let prepared = ops::prepare_tx(&rpc, &open.network, &open.owner, &intent, None)?;
                let sent =
                    ops::sign_and_maybe_send(&rpc, &open.network, &prepared, &key.0, broadcast)?;
                let field = if broadcast {
                    "transaction_hash"
                } else {
                    "raw_transaction"
                };
                let answer = sent
                    .get(field)
                    .and_then(Value::as_str)
                    .ok_or("the signed transaction is missing")?
                    .to_string();
                (answer, sent)
            }
            Action::PersonalSign { message, .. } => {
                let signature = sign::sign_personal_message(&key.0, message)?;
                let hex_sig = hex::encode_prefixed(&signature);
                let report = Value::object(vec![("signature", Value::str(&hex_sig))]);
                (hex_sig, report)
            }
            Action::SignTypedData { data, .. } => {
                check_domain_chain(data, chain_id)?;
                let digest = data.signing_hash()?;
                let signature = sign::sign_digest(&key.0, &digest)?;
                let hex_sig = hex::encode_prefixed(&signature);
                let report = Value::object(vec![
                    ("signature", Value::str(&hex_sig)),
                    (
                        "typed_data_hash",
                        Value::String(hex::encode_prefixed(&digest)),
                    ),
                ]);
                (hex_sig, report)
            }
        };
        let notified = self
            .wc
            .borrow_mut()
            .answer_request(request_id, Ok(Value::String(answer)))?;
        if let Value::Object(pairs) = &mut report {
            pairs.insert(0, ("status".into(), Value::str("approved")));
            pairs.insert(1, ("request_id".into(), id_string(request_id)));
            pairs.insert(2, ("method".into(), Value::str(&open.request.method)));
            pairs.insert(3, ("wallet".into(), Value::str(&open.record_name)));
            pairs.insert(4, ("dapp_notified".into(), Value::Bool(notified)));
            if !notified {
                pairs.push((
                    "note".into(),
                    Value::str(
                        "The dapp could not be reached, so it has not received the answer; the request is closed here.",
                    ),
                ));
            }
        }
        Ok(report)
    }

    pub fn wc_reject_request(
        &self,
        request_id: u64,
        reason: Option<&str>,
    ) -> Result<Value, String> {
        let mut wc = self.wc.borrow_mut();
        wc.poll();
        wc.request(request_id)?;
        let message = reason
            .map(|r| sanitize_text_max(r, 100))
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| "User rejected.".to_string());
        let notified = wc.answer_request(request_id, Err((proto::USER_REJECTED, message)))?;
        Ok(Value::object(vec![
            ("request_id", id_string(request_id)),
            ("status", Value::str("rejected")),
            ("dapp_notified", Value::Bool(notified)),
        ]))
    }
}

struct SessionSummary {
    wallet: String,
    dapp: DappMetadata,
}

fn clone_session_summary(session: &Session) -> SessionSummary {
    SessionSummary {
        wallet: session.wallet.clone(),
        dapp: session.dapp.clone(),
    }
}

fn check_tx_chain(tx: &TxRequest, chain_id: u64) -> Result<(), String> {
    match tx.chain_id {
        Some(c) if c != chain_id => Err(format!(
            "the transaction is for chain ID {c} but the request is for chain ID {chain_id}; refusing"
        )),
        _ => Ok(()),
    }
}

fn check_domain_chain(
    data: &crate::eth::typed_data::TypedData,
    chain_id: u64,
) -> Result<(), String> {
    match data.domain_chain_id()? {
        Some(c) if c != chain_id => Err(format!(
            "the signature is bound to chain ID {c} but the request is for chain ID {chain_id}; refusing"
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::keccak::keccak256;
    use crate::crypto::secp256k1;
    use crate::eth::address::parse_address;
    use crate::http::mock::RouterMock;
    use crate::json;
    use crate::service::testing::{Fixture, PW, fixture_custom, mainnet_router};
    use crate::walletconnect::manager::WcManager;
    use crate::walletconnect::relay::mock::MockRelay;
    use crate::walletconnect::testing::{Dapp, OPTIONAL, REQUIRED};
    use std::rc::Rc;

    const PROPOSAL_ID: u64 = 1_700_000_000_000_000_001;
    const TX_HASH: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

    struct Env {
        f: Fixture,
        relay: Rc<MockRelay>,
        dapp: Dapp,
        address: Address,
    }

    fn router() -> RouterMock {
        mainnet_router().rpc("eth_sendRawTransaction", &format!("\"{TX_HASH}\""))
    }

    fn env() -> Env {
        let relay = Rc::new(MockRelay::default());
        let manager = WcManager::new(relay.clone()).with_settle_wait(Duration::ZERO);
        let f = fixture_custom(router(), |s| s.with_walletconnect(manager));
        f.service.create_wallet("ethereum", "Main", PW).unwrap();
        let address = parse_address(
            f.service
                .get_address("Main", PW)
                .unwrap()
                .get("address")
                .and_then(Value::as_str)
                .unwrap(),
        )
        .unwrap();
        Env {
            f,
            relay,
            dapp: Dapp::new(),
            address,
        }
    }

    impl Env {
        fn propose(&self, required: &str, optional: &str) -> Value {
            self.dapp
                .send_proposal(&self.relay, PROPOSAL_ID, required, optional);
            self.f.service.wc_pair(&self.dapp.uri(), Some(0)).unwrap()
        }

        /// Connects the dapp on chain 1 and returns the session topic.
        fn connect(&mut self) -> String {
            self.connect_asking(REQUIRED, OPTIONAL)
        }

        fn connect_asking(&mut self, required: &str, optional: &str) -> String {
            self.propose(required, optional);
            let out = self
                .f
                .service
                .wc_approve_session(PROPOSAL_ID, "Main", 1, &[], PW)
                .unwrap();
            let settle = self.dapp.accept(&self.relay.take_published());
            let id = settle.get("id").cloned().unwrap();
            self.dapp.push_session_message(
                &self.relay,
                proto::build_result(&id, Value::Bool(true)),
                1103,
            );
            out.get("session_topic")
                .and_then(Value::as_str)
                .unwrap()
                .to_string()
        }

        fn request(&self, id: u64, chain: &str, method: &str, params: &str) {
            self.dapp.request(&self.relay, id, chain, method, params);
        }

        fn answer(&self) -> Value {
            let published = self.relay.take_published();
            assert_eq!(published.len(), 1, "expected exactly one answer");
            self.dapp.decode_session(&published[0].message)
        }

        fn me(&self) -> String {
            to_checksum(&self.address)
        }
    }

    fn recovered_signer(digest: &[u8; 32], signature_hex: &str) -> Address {
        let sig = hex::decode(signature_hex).unwrap();
        let rs: [u8; 64] = sig[..64].try_into().unwrap();
        let public = secp256k1::recover_public_key(digest, &rs, sig[64] - 27).unwrap();
        keccak256(&public[1..])[12..].try_into().unwrap()
    }

    fn err_of(result: Result<Value, String>) -> String {
        result.expect_err("expected an error")
    }

    // ---- connecting -------------------------------------------------------------

    #[test]
    fn pairing_shows_what_the_dapp_asks_for_without_any_address() {
        let env = env();
        let out = env.propose(REQUIRED, OPTIONAL);
        assert_eq!(
            out.get("status").and_then(Value::as_str),
            Some("proposal_received")
        );
        let proposal = &out.get("proposals").unwrap().as_array().unwrap()[0];
        assert_eq!(
            proposal.get("proposal_id").and_then(Value::as_str),
            Some("1700000000000000001")
        );
        assert_eq!(
            proposal.get("can_connect").and_then(Value::as_bool),
            Some(true)
        );
        let dapp = proposal.get("dapp").unwrap();
        assert_eq!(dapp.get("name").and_then(Value::as_str), Some("Test Dapp"));
        let required = proposal.get("required").unwrap();
        assert_eq!(
            required.get("chain_ids").unwrap().as_array().unwrap().len(),
            1
        );
        assert!(
            !out.to_json()
                .to_lowercase()
                .contains(&env.me().to_lowercase())
        );
    }

    #[test]
    fn pairing_without_a_proposal_says_so() {
        let env = env();
        let out = env.f.service.wc_pair(&env.dapp.uri(), Some(0)).unwrap();
        assert_eq!(
            out.get("status").and_then(Value::as_str),
            Some("waiting_for_proposal")
        );
    }

    #[test]
    fn without_a_relay_the_tools_explain_how_to_set_one_up() {
        let f = crate::service::testing::fixture();
        let err = err_of(f.service.wc_pair(&Dapp::new().uri(), Some(0)));
        assert!(err.contains("not set up"), "{err}");
    }

    #[test]
    fn approval_needs_the_right_password_wallet_and_chains() {
        let env = env();
        env.propose(REQUIRED, OPTIONAL);
        let s = &env.f.service;
        assert_eq!(
            err_of(s.wc_approve_session(PROPOSAL_ID, "Main", 1, &[], "wrong-password")),
            "incorrect password"
        );
        assert!(
            err_of(s.wc_approve_session(PROPOSAL_ID, "Nobody", 1, &[], PW)).contains("no wallet")
        );
        assert!(
            err_of(s.wc_approve_session(PROPOSAL_ID, "Main", 10, &[], PW))
                .contains("requires chain(s) 1")
        );
        assert!(
            err_of(s.wc_approve_session(PROPOSAL_ID, "Main", 1, &[56], PW))
                .contains("did not ask for chain(s) 56")
        );
        // Chain 10 is asked for but the wallet has no network for it.
        assert!(
            err_of(s.wc_approve_session(PROPOSAL_ID, "Main", 1, &[10], PW))
                .contains("no network with chain ID 10")
        );
        assert!(err_of(s.wc_approve_session(999, "Main", 1, &[], PW)).contains("no such proposal"));
        assert!(
            env.relay.take_published().is_empty(),
            "nothing may be sent before approval succeeds"
        );
        assert_eq!(
            s.wc_list_proposals()
                .unwrap()
                .get("count")
                .and_then(Value::as_u64),
            Some(1)
        );
    }

    #[test]
    fn approval_shares_the_address_only_with_the_dapp() {
        let mut env = env();
        let topic = env.connect();
        let sessions = env.f.service.wc_list_sessions().unwrap();
        assert_eq!(sessions.get("count").and_then(Value::as_u64), Some(1));
        let session = &sessions.get("sessions").unwrap().as_array().unwrap()[0];
        assert_eq!(
            session.get("session_topic").and_then(Value::as_str),
            Some(topic.as_str())
        );
        assert_eq!(session.get("wallet").and_then(Value::as_str), Some("Main"));
        assert_eq!(
            session.get("confirmed_by_dapp").and_then(Value::as_bool),
            Some(true)
        );
        assert!(
            !sessions
                .to_json()
                .to_lowercase()
                .contains(&env.me().to_lowercase())
        );
    }

    #[test]
    fn the_settled_session_carries_the_wallets_account() {
        let mut env = env();
        env.propose(REQUIRED, OPTIONAL);
        env.f
            .service
            .wc_approve_session(PROPOSAL_ID, "main", 1, &[], PW)
            .unwrap();
        let settle = env.dapp.accept(&env.relay.take_published());
        let accounts = settle
            .get("params")
            .and_then(|p| p.get("namespaces"))
            .and_then(|n| n.get("eip155"))
            .and_then(|e| e.get("accounts"))
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(
            accounts[0].as_str(),
            Some(format!("eip155:1:{}", env.me()).as_str())
        );
    }

    #[test]
    fn a_dapp_needing_unsupported_methods_cannot_be_connected() {
        let env = env();
        let required = r#"{"eip155":{"chains":["eip155:1"],"methods":["eth_sign"],"events":[]}}"#;
        let out = env.propose(required, "{}");
        let proposal = &out.get("proposals").unwrap().as_array().unwrap()[0];
        assert_eq!(
            proposal.get("can_connect").and_then(Value::as_bool),
            Some(false)
        );
        assert!(proposal.to_json().contains("eth_sign"));
        let err = err_of(
            env.f
                .service
                .wc_approve_session(PROPOSAL_ID, "Main", 1, &[], PW),
        );
        assert!(err.contains("does not support"), "{err}");
    }

    #[test]
    fn rejecting_a_proposal_and_disconnecting_work() {
        let mut env = env();
        env.propose(REQUIRED, OPTIONAL);
        let out = env.f.service.wc_reject_session(PROPOSAL_ID).unwrap();
        assert_eq!(
            out.get("dapp_notified").and_then(Value::as_bool),
            Some(true)
        );
        env.relay.take_published();
        env.dapp = Dapp::new();
        let topic = env.connect();
        let out = env.f.service.wc_disconnect_session(&topic[..12]).unwrap();
        assert_eq!(
            out.get("status").and_then(Value::as_str),
            Some("disconnected")
        );
        assert_eq!(
            env.f
                .service
                .wc_list_sessions()
                .unwrap()
                .get("count")
                .and_then(Value::as_u64),
            Some(0)
        );
    }

    // ---- signing messages -----------------------------------------------------------

    #[test]
    fn personal_sign_is_reviewed_then_signed_for_the_wallet() {
        let mut env = env();
        env.connect();
        let params = format!(r#"["0x48656c6c6f","{}"]"#, env.me());
        env.request(900, "eip155:1", "personal_sign", &params);
        let s = &env.f.service;
        let listed = s.wc_list_requests().unwrap();
        let request = &listed.get("requests").unwrap().as_array().unwrap()[0];
        assert_eq!(request.get("kind").and_then(Value::as_str), Some("message"));
        assert_eq!(
            request
                .get("details")
                .and_then(|d| d.get("text"))
                .and_then(Value::as_str),
            Some("Hello")
        );
        assert!(
            !listed
                .to_json()
                .to_lowercase()
                .contains(&env.me().to_lowercase())
        );

        let review = s.wc_review_request(900, "Main", 1, PW).unwrap();
        assert_eq!(
            review.get("from").and_then(Value::as_str),
            Some(env.me().as_str())
        );
        assert!(
            review
                .get("note")
                .and_then(Value::as_str)
                .unwrap()
                .contains("off-chain")
        );

        let out = s.wc_approve_request(900, "Main", 1, PW).unwrap();
        assert_eq!(out.get("status").and_then(Value::as_str), Some("approved"));
        assert_eq!(
            out.get("dapp_notified").and_then(Value::as_bool),
            Some(true)
        );
        let signature = out
            .get("signature")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        assert_eq!(
            recovered_signer(&sign::personal_message_hash(b"Hello"), &signature),
            env.address
        );
        let answer = env.answer();
        assert_eq!(
            answer.get("result").and_then(Value::as_str),
            Some(signature.as_str())
        );
        assert_eq!(
            s.wc_list_requests()
                .unwrap()
                .get("count")
                .and_then(Value::as_u64),
            Some(0)
        );
    }

    #[test]
    fn approval_requires_matching_wallet_chain_and_password() {
        let mut env = env();
        env.connect();
        env.f
            .service
            .create_wallet("ethereum", "Other", PW)
            .unwrap();
        let params = format!(r#"["0x48656c6c6f","{}"]"#, env.me());
        env.request(901, "eip155:1", "personal_sign", &params);
        let s = &env.f.service;
        assert!(
            err_of(s.wc_approve_request(901, "Other", 1, PW))
                .contains("belongs to a session connected with wallet 'Main'")
        );
        assert!(
            err_of(s.wc_approve_request(901, "Main", 10, PW)).contains("is for chain ID 1, not 10")
        );
        assert_eq!(
            err_of(s.wc_approve_request(901, "Main", 1, "bad-password")),
            "incorrect password"
        );
        assert!(err_of(s.wc_review_request(901, "Other", 1, PW)).contains("belongs to a session"));
        assert!(err_of(s.wc_approve_request(12345, "Main", 1, PW)).contains("no such request"));
        assert!(env.relay.take_published().is_empty());
        assert_eq!(
            s.wc_list_requests()
                .unwrap()
                .get("count")
                .and_then(Value::as_u64),
            Some(1)
        );
    }

    #[test]
    fn a_request_for_another_address_is_refused() {
        let mut env = env();
        env.connect();
        env.request(
            902,
            "eip155:1",
            "personal_sign",
            r#"["0x00","0x1111111111111111111111111111111111111111"]"#,
        );
        let err = err_of(env.f.service.wc_approve_request(902, "Main", 1, PW));
        assert!(err.contains("refusing"), "{err}");
        assert!(env.relay.take_published().is_empty());
    }

    const TYPED: &str = r#"{"types":{"EIP712Domain":[{"name":"name","type":"string"},{"name":"chainId","type":"uint256"}],"Permit":[{"name":"spender","type":"address"},{"name":"value","type":"uint256"}]},"primaryType":"Permit","domain":{"name":"Token","chainId":CHAIN},"message":{"spender":"0x2222222222222222222222222222222222222222","value":"1000"}}"#;

    fn typed_params(env: &Env, chain: u64) -> String {
        let doc = TYPED.replace("CHAIN", &chain.to_string());
        format!(r#"["{}",{}]"#, env.me(), Value::String(doc).to_json())
    }

    #[test]
    fn typed_data_is_shown_with_warnings_and_signed() {
        let mut env = env();
        env.connect();
        let params = typed_params(&env, 1);
        env.request(910, "eip155:1", "eth_signTypedData_v4", &params);
        let s = &env.f.service;
        let review = s.wc_review_request(910, "Main", 1, PW).unwrap();
        let details = review.get("details").unwrap();
        assert_eq!(
            details.get("primary_type").and_then(Value::as_str),
            Some("Permit")
        );
        assert!(
            details.get("warnings").unwrap().as_array().unwrap()[0]
                .as_str()
                .unwrap()
                .contains("permit")
        );
        let out = s.wc_approve_request(910, "Main", 1, PW).unwrap();
        let digest =
            hex::decode_fixed::<32>(out.get("typed_data_hash").and_then(Value::as_str).unwrap())
                .unwrap();
        let signature = out.get("signature").and_then(Value::as_str).unwrap();
        assert_eq!(recovered_signer(&digest, signature), env.address);
        assert_eq!(
            env.answer().get("result").and_then(Value::as_str),
            Some(signature)
        );
    }

    #[test]
    fn hyperliquid_terms_can_be_reviewed_and_signed() {
        let mut env = env();
        env.connect();
        let doc = r#"{"types":{"Hyperliquid:AcceptTerms":[{"name":"hyperliquidChain","type":"string"},{"name":"nonce","type":"uint64"}]},"primaryType":"Hyperliquid:AcceptTerms","domain":{"name":"HyperliquidSignTransaction","chainId":1},"message":{"hyperliquidChain":"Mainnet","nonce":123456789}}"#;
        let params = format!(r#"["{}",{}]"#, env.me(), Value::str(doc).to_json());
        env.request(912, "eip155:1", "eth_signTypedData_v4", &params);
        let s = &env.f.service;
        let review = s.wc_review_request(912, "Main", 1, PW).unwrap();
        assert_eq!(
            review
                .get("details")
                .unwrap()
                .get("primary_type")
                .and_then(Value::as_str),
            Some("Hyperliquid:AcceptTerms")
        );
        assert!(env.relay.take_published().is_empty());
        let out = s.wc_approve_request(912, "Main", 1, PW).unwrap();
        let digest =
            hex::decode_fixed::<32>(out.get("typed_data_hash").and_then(Value::as_str).unwrap())
                .unwrap();
        let signature = out.get("signature").and_then(Value::as_str).unwrap();
        assert_eq!(recovered_signer(&digest, signature), env.address);
        assert_eq!(
            env.answer().get("result").and_then(Value::as_str),
            Some(signature)
        );
    }

    #[test]
    fn typed_data_bound_to_another_chain_is_refused() {
        let mut env = env();
        env.connect();
        let params = typed_params(&env, 137);
        env.request(911, "eip155:1", "eth_signTypedData_v4", &params);
        let s = &env.f.service;
        assert!(err_of(s.wc_review_request(911, "Main", 1, PW)).contains("bound to chain ID 137"));
        assert!(err_of(s.wc_approve_request(911, "Main", 1, PW)).contains("bound to chain ID 137"));
        assert!(env.relay.take_published().is_empty());
    }

    #[test]
    fn rejecting_a_request_tells_the_dapp() {
        let mut env = env();
        env.connect();
        env.request(
            920,
            "eip155:1",
            "personal_sign",
            r#"["0x00","0x1111111111111111111111111111111111111111"]"#,
        );
        let out = env
            .f
            .service
            .wc_reject_request(920, Some("not today\n"))
            .unwrap();
        assert_eq!(out.get("status").and_then(Value::as_str), Some("rejected"));
        let answer = env.answer();
        let error = answer.get("error").unwrap();
        assert_eq!(error.get("code").and_then(Value::as_u64), Some(5000));
        assert_eq!(
            error.get("message").and_then(Value::as_str),
            Some("not today")
        );
        assert!(err_of(env.f.service.wc_reject_request(920, None)).contains("no such request"));
    }

    // ---- transactions ---------------------------------------------------------------

    fn tx_params(env: &Env, extra: &str) -> String {
        format!(
            r#"[{{"from":"{}","to":"0x2222222222222222222222222222222222222222","value":"0x2386f26fc10000"{extra}}}]"#,
            env.me()
        )
    }

    #[test]
    fn a_transaction_is_reviewed_with_costs_and_outcome() {
        let mut env = env();
        env.connect();
        let params = tx_params(&env, r#","gas":"0x1","maxFeePerGas":"0x1","nonce":"0x9""#);
        env.request(930, "eip155:1", "eth_sendTransaction", &params);
        let review = env.f.service.wc_review_request(930, "Main", 1, PW).unwrap();
        assert_eq!(
            review.get("simulation_succeeded").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            review.get("will_broadcast").and_then(Value::as_bool),
            Some(true)
        );
        let lines: Vec<&str> = review
            .get("expected_outcome")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            lines[0].starts_with("Sends 0.01 ETH from your wallet to 0x2222"),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("Network fee: at most")),
            "{lines:?}"
        );
        let note = review.get("note").and_then(Value::as_str).unwrap();
        assert!(note.contains("gas, maxFeePerGas, nonce"), "{note}");
        let simulation = review.get("simulation").unwrap();
        assert!(simulation.get("max_fee").is_some());
        assert_eq!(
            simulation.get("sufficient_funds").and_then(Value::as_bool),
            Some(true)
        );
        // Reviewing signs and sends nothing.
        assert!(
            !env.f
                .router
                .sent_methods()
                .contains(&"eth_sendRawTransaction".to_string())
        );
        assert!(env.relay.take_published().is_empty());
    }

    #[test]
    fn approving_a_transaction_broadcasts_it_and_answers_with_the_hash() {
        let mut env = env();
        env.connect();
        env.request(931, "eip155:1", "eth_sendTransaction", &tx_params(&env, ""));
        let out = env
            .f
            .service
            .wc_approve_request(931, "Main", 1, PW)
            .unwrap();
        assert_eq!(out.get("status").and_then(Value::as_str), Some("approved"));
        assert_eq!(
            out.get("transaction_hash").and_then(Value::as_str),
            Some(TX_HASH)
        );
        assert_eq!(out.get("broadcast").and_then(Value::as_bool), Some(true));
        assert!(
            env.f
                .router
                .sent_methods()
                .contains(&"eth_sendRawTransaction".to_string())
        );
        assert_eq!(
            env.answer().get("result").and_then(Value::as_str),
            Some(TX_HASH)
        );
        // A second approval of the same request cannot send it again.
        assert!(
            err_of(env.f.service.wc_approve_request(931, "Main", 1, PW))
                .contains("no such request")
        );
    }

    #[test]
    fn sign_transaction_only_signs() {
        let mut env = env();
        let required =
            r#"{"eip155":{"chains":["eip155:1"],"methods":["eth_signTransaction"],"events":[]}}"#;
        env.connect_asking(required, "{}");
        let before = env.f.router.sent_methods().len();
        env.request(932, "eip155:1", "eth_signTransaction", &tx_params(&env, ""));
        let out = env
            .f
            .service
            .wc_approve_request(932, "Main", 1, PW)
            .unwrap();
        assert_eq!(out.get("broadcast").and_then(Value::as_bool), Some(false));
        let raw = out.get("raw_transaction").and_then(Value::as_str).unwrap();
        assert!(raw.starts_with("0x02"), "{raw}");
        assert!(
            !env.f.router.sent_methods()[before..].contains(&"eth_sendRawTransaction".to_string())
        );
        assert_eq!(
            env.answer().get("result").and_then(Value::as_str),
            Some(raw)
        );
    }

    #[test]
    fn transactions_for_the_wrong_chain_or_signer_are_refused() {
        let mut env = env();
        env.connect();
        let wrong_chain = tx_params(&env, r#","chainId":"0x89""#);
        env.request(933, "eip155:1", "eth_sendTransaction", &wrong_chain);
        let s = &env.f.service;
        assert!(err_of(s.wc_review_request(933, "Main", 1, PW)).contains("chain ID 137"));
        assert!(err_of(s.wc_approve_request(933, "Main", 1, PW)).contains("chain ID 137"));
        env.request(
            934,
            "eip155:1",
            "eth_sendTransaction",
            r#"[{"from":"0x1111111111111111111111111111111111111111","to":"0x2222222222222222222222222222222222222222"}]"#,
        );
        assert!(err_of(s.wc_approve_request(934, "Main", 1, PW)).contains("refusing"));
        assert!(
            !env.f
                .router
                .sent_methods()
                .contains(&"eth_sendRawTransaction".to_string())
        );
    }

    #[test]
    fn contract_creation_is_refused() {
        let mut env = env();
        env.connect();
        env.request(
            935,
            "eip155:1",
            "eth_sendTransaction",
            &format!(r#"[{{"from":"{}","data":"0x6000"}}]"#, env.me()),
        );
        assert!(
            err_of(env.f.service.wc_approve_request(935, "Main", 1, PW))
                .contains("creates a contract")
        );
    }

    #[test]
    fn a_transaction_that_would_fail_is_not_sent() {
        let relay = Rc::new(MockRelay::default());
        let manager = WcManager::new(relay.clone()).with_settle_wait(Duration::ZERO);
        let failing = router().rpc_error("eth_estimateGas", 3, "execution reverted", None);
        let f = fixture_custom(failing, |s| s.with_walletconnect(manager));
        f.service.create_wallet("ethereum", "Main", PW).unwrap();
        let address = f.service.get_address("Main", PW).unwrap();
        let me = address
            .get("address")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let mut env = Env {
            f,
            relay,
            dapp: Dapp::new(),
            address: parse_address(&me).unwrap(),
        };
        env.connect();
        env.request(936, "eip155:1", "eth_sendTransaction", &tx_params(&env, ""));
        let err = err_of(env.f.service.wc_approve_request(936, "Main", 1, PW));
        assert!(err.contains("would fail"), "{err}");
        assert!(
            !env.f
                .router
                .sent_methods()
                .contains(&"eth_sendRawTransaction".to_string())
        );
        // The request stays open so it can still be rejected.
        assert_eq!(
            env.f
                .service
                .wc_list_requests()
                .unwrap()
                .get("count")
                .and_then(Value::as_u64),
            Some(1)
        );
    }

    #[test]
    fn token_approvals_are_decoded_in_requests_and_reviews() {
        let mut env = env();
        env.connect();
        let spender = "2222222222222222222222222222222222222222";
        let data = format!("0x095ea7b3{:0>64}{}", spender, "f".repeat(64));
        let params = format!(
            r#"[{{"from":"{}","to":"0x3333333333333333333333333333333333333333","data":"{data}"}}]"#,
            env.me()
        );
        env.request(940, "eip155:1", "eth_sendTransaction", &params);
        let listed = env.f.service.wc_list_requests().unwrap();
        let details = listed.get("requests").unwrap().as_array().unwrap()[0]
            .get("details")
            .unwrap();
        assert_eq!(
            details
                .get("decoded")
                .and_then(|d| d.get("action"))
                .and_then(Value::as_str),
            Some("erc20_approve")
        );
        assert_eq!(
            details.get("function_selector").and_then(Value::as_str),
            Some("0x095ea7b3")
        );
        assert!(details.to_json().contains("unlimited"));
        let review = env.f.service.wc_review_request(940, "Main", 1, PW).unwrap();
        let lines = review.get("expected_outcome").unwrap().to_json();
        assert!(
            lines.contains("Lets 0x2222222222222222222222222222222222222222 spend up to"),
            "{lines}"
        );
        assert!(review.to_json().contains("unlimited token allowance"));
    }

    // ---- outcome descriptions -------------------------------------------------------

    fn network() -> Network {
        crate::eth::networks::ethereum_mainnet()
    }

    fn intent(value: u64, data: Vec<u8>) -> TxIntent {
        TxIntent {
            to: [0x44; 20],
            value: U256::from_u64(value),
            data,
        }
    }

    fn token() -> TokenInfo {
        TokenInfo {
            address: [0x44; 20],
            symbol: "USDC".into(),
            decimals: 6,
        }
    }

    #[test]
    fn outcome_lines_describe_native_transfers_and_plain_calls() {
        let lines = outcome_lines(
            &network(),
            &intent(1_500_000_000_000_000_000, vec![]),
            None,
            None,
        );
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("Sends 1.5 ETH from your wallet to 0x4444"));
        let call = outcome_lines(
            &network(),
            &intent(0, vec![0xde, 0xad, 0xbe, 0xef, 1]),
            None,
            None,
        );
        assert!(call[0].contains("(function 0xdeadbeef)") && call[0].contains("cannot be decoded"));
        let empty = outcome_lines(&network(), &intent(0, vec![]), None, None);
        assert!(empty[0].starts_with("Sends an empty transaction"));
    }

    #[test]
    fn outcome_lines_use_token_details_when_known() {
        let transfer = json::parse(
            r#"{"action":"erc20_transfer","recipient":"0xabc","amount_raw":"2500000"}"#,
        )
        .unwrap();
        let line = &outcome_lines(
            &network(),
            &intent(0, vec![1; 4]),
            Some(&transfer),
            Some(&token()),
        )[0];
        assert_eq!(line, "Transfers 2.5 USDC to 0xabc");
        let unknown = &outcome_lines(&network(), &intent(0, vec![1; 4]), Some(&transfer), None)[0];
        assert!(unknown.contains("2500000 units of the token"));
        let approve = json::parse(
            r#"{"action":"erc20_approve","spender":"0xdef","amount_raw":"1","unlimited":true}"#,
        )
        .unwrap();
        let line = &outcome_lines(
            &network(),
            &intent(0, vec![1; 4]),
            Some(&approve),
            Some(&token()),
        )[0];
        assert_eq!(
            line,
            "Lets 0xdef spend up to UNLIMITED USDC from your wallet"
        );
        let limited = json::parse(r#"{"action":"erc20_approve","spender":"0xdef","amount_raw":"3000000","unlimited":false}"#).unwrap();
        assert!(
            outcome_lines(
                &network(),
                &intent(0, vec![1; 4]),
                Some(&limited),
                Some(&token())
            )[0]
            .contains("up to 3 USDC")
        );
        let revoke =
            json::parse(r#"{"action":"erc20_revoke_approval","spender":"0xdef","amount_raw":"0"}"#)
                .unwrap();
        assert!(
            outcome_lines(
                &network(),
                &intent(0, vec![1; 4]),
                Some(&revoke),
                Some(&token())
            )[0]
            .starts_with("Removes 0xdef's permission")
        );
    }

    #[test]
    fn outcome_lines_flag_collection_wide_approvals() {
        let all = json::parse(r#"{"action":"set_approval_for_all","operator":"0xaaa","token_contract":"0xbbb","approved":true}"#).unwrap();
        assert!(
            outcome_lines(&network(), &intent(0, vec![1; 4]), Some(&all), None)[0]
                .contains("ALL of your tokens")
        );
        let none = json::parse(r#"{"action":"set_approval_for_all","operator":"0xaaa","token_contract":"0xbbb","approved":false}"#).unwrap();
        assert!(
            outcome_lines(&network(), &intent(0, vec![1; 4]), Some(&none), None)[0]
                .starts_with("Removes 0xaaa")
        );
    }
}
