//! WalletConnect session state: pairings, proposals, sessions and pending requests.
//!
//! The manager owns the symmetric keys of its pairings and sessions and talks to the relay only
//! through the `Relay` trait. It never sees wallet secrets: signing happens in the wallet
//! service, which then hands the answer back here to be encrypted and sent to the dapp.

use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::envelope;
use super::namespaces::{self, Approval, DappMetadata, ProposalData};
use super::now_secs;
use super::proto::{self, Message, RpcError};
use super::relay::{Inbound, Relay};
use super::requests::{self, Action};
use super::uri;
use crate::crypto::secret::zeroize;
use crate::crypto::x25519::KeyPair;
use crate::crypto::{hex, secret};
use crate::eth::address::Address;
use crate::eth::explorer::sanitize_text_max;
use crate::json::Value;

pub const WALLET_NAME: &str = "agent-saifu";
pub const WALLET_DESCRIPTION: &str = "All-in-one crypto wallet for Agents";
pub const SESSION_LIFETIME_SECS: u64 = 7 * 24 * 60 * 60;
pub const PROPOSAL_LIFETIME_SECS: u64 = 300;
pub const REQUEST_LIFETIME_SECS: u64 = 300;
pub const MAX_PROPOSALS: usize = 20;
pub const MAX_SESSIONS: usize = 50;
pub const MAX_REQUESTS: usize = 50;
const SEEN_LIMIT: usize = 2048;
const SETTLE_WAIT: Duration = Duration::from_secs(10);
const STEP: Duration = Duration::from_millis(250);

/// A 32-byte key that is wiped when dropped.
#[derive(Clone)]
struct SymKey([u8; 32]);

impl Drop for SymKey {
    fn drop(&mut self) {
        zeroize(&mut self.0);
    }
}

struct Pairing {
    topic: String,
    key: SymKey,
    expires_at: u64,
}

pub struct Proposal {
    pub id: u64,
    pairing_topic: String,
    pub data: ProposalData,
    pub received_at: u64,
    pub expires_at: u64,
}

pub struct Session {
    pub topic: String,
    key: SymKey,
    pub wallet: String,
    pub chains: Vec<u64>,
    /// The Sui chain (`sui:mainnet`) of a Sui session; such a session has no EVM chains.
    pub sui_chain: Option<String>,
    pub methods: Vec<String>,
    pub dapp: DappMetadata,
    pub created_at: u64,
    pub expires_at: u64,
    pub acknowledged: bool,
    settle_error: Option<String>,
    settle_id: u64,
}

pub struct PendingRequest {
    pub id: u64,
    pub session_topic: String,
    pub method: String,
    /// The EVM chain ID, or 0 for a Sui request.
    pub chain_id: u64,
    pub sui_chain: Option<String>,
    pub action: Action,
    pub received_at: u64,
    pub expires_at: u64,
}

/// What a session grants, already in the form `wc_sessionSettle` carries it.
pub struct SessionTerms {
    pub namespaces: Value,
    /// `sessionProperties`, a string-to-string map (Sui puts `sui_getAccounts` here).
    pub properties: Option<Value>,
    pub chains: Vec<u64>,
    pub sui_chain: Option<String>,
    pub methods: Vec<String>,
}

pub struct PairOutcome {
    pub pairing_topic: String,
    pub proposal_ids: Vec<u64>,
}

pub struct ApprovedSession {
    pub topic: String,
    pub acknowledged: bool,
}

pub struct WcManager {
    relay: Rc<dyn Relay>,
    pairings: Vec<Pairing>,
    proposals: Vec<Proposal>,
    sessions: Vec<Session>,
    requests: Vec<PendingRequest>,
    seen: VecDeque<(String, String)>,
    settle_wait: Duration,
}

fn controller_metadata() -> Value {
    Value::object(vec![
        ("name", Value::str(WALLET_NAME)),
        ("description", Value::str(WALLET_DESCRIPTION)),
        (
            "url",
            Value::str("https://github.com/daniel-j-dev/agent-saifu"),
        ),
        ("icons", Value::Array(vec![])),
    ])
}

fn id_value(id: u64) -> Value {
    Value::Number(id.to_string())
}

impl WcManager {
    pub fn new(relay: Rc<dyn Relay>) -> WcManager {
        WcManager {
            relay,
            pairings: vec![],
            proposals: vec![],
            sessions: vec![],
            requests: vec![],
            seen: VecDeque::new(),
            settle_wait: SETTLE_WAIT,
        }
    }

    /// How long approving a session waits for the dapp to acknowledge it.
    pub fn with_settle_wait(mut self, wait: Duration) -> WcManager {
        self.settle_wait = wait;
        self
    }

    // ---- publishing -------------------------------------------------------

    fn send(
        &self,
        topic: &str,
        key: &[u8; 32],
        payload: &str,
        delivery: proto::Delivery,
    ) -> Result<(), String> {
        let message = envelope::encode_type0(key, payload.as_bytes())?;
        self.relay
            .publish(topic, &message, delivery.ttl, delivery.tag, delivery.prompt)
    }

    fn send_on_session(
        &self,
        topic: &str,
        payload: &str,
        delivery: proto::Delivery,
    ) -> Result<(), String> {
        let session = self
            .sessions
            .iter()
            .find(|s| s.topic == topic)
            .ok_or("unknown session")?;
        self.send(topic, &session.key.0, payload, delivery)
    }

    fn reply_ok(&self, topic: &str, id: &Value, method: &str) {
        if let Some(delivery) = proto::response_delivery(method) {
            let _ =
                self.send_on_session(topic, &proto::build_result(id, Value::Bool(true)), delivery);
        }
    }

    fn reply_error(&self, topic: &str, id: &Value, method: &str, code: i64, message: &str) {
        if let Some(delivery) = proto::response_delivery(method) {
            let _ = self.send_on_session(topic, &proto::build_error(id, code, message), delivery);
        }
    }

    // ---- receiving --------------------------------------------------------

    /// Handles every message that is already waiting.
    fn drain_ready(&mut self) {
        while let Some(inbound) = self.relay.recv(Duration::ZERO) {
            self.process(inbound);
        }
    }

    /// Handles messages for up to `wait`, stopping early once `done` holds.
    fn pump_until(&mut self, wait: Duration, done: impl Fn(&WcManager) -> bool) {
        self.drain_ready();
        let deadline = Instant::now() + wait;
        while !done(self) {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            if let Some(inbound) = self.relay.recv((deadline - now).min(STEP)) {
                self.process(inbound);
            }
        }
        self.purge_expired();
    }

    /// Picks up whatever arrived since the last call and drops what has expired.
    pub fn poll(&mut self) {
        self.drain_ready();
        self.purge_expired();
    }

    fn already_seen(&mut self, topic: &str, id: &Value) -> bool {
        let key = (topic.to_string(), id.to_json());
        if self.seen.contains(&key) {
            return true;
        }
        if self.seen.len() >= SEEN_LIMIT {
            self.seen.pop_front();
        }
        self.seen.push_back(key);
        false
    }

    fn process(&mut self, inbound: Inbound) {
        let key = if let Some(p) = self.pairings.iter().find(|p| p.topic == inbound.topic) {
            p.key.clone()
        } else if let Some(s) = self.sessions.iter().find(|s| s.topic == inbound.topic) {
            s.key.clone()
        } else {
            return;
        };
        let Ok(decoded) = envelope::decode(&key.0, &inbound.message) else {
            return;
        };
        let Ok(mut text) = String::from_utf8(decoded.plaintext) else {
            return;
        };
        let parsed = proto::parse_message(&text);
        secret::zeroize_string(&mut text);
        let Ok(message) = parsed else { return };
        let id = match &message {
            Message::Request { id, .. } | Message::Response { id, .. } => id.clone(),
        };
        if self.already_seen(&inbound.topic, &id) {
            return;
        }
        let is_pairing = self.pairings.iter().any(|p| p.topic == inbound.topic);
        match message {
            Message::Request { id, method, params } if is_pairing => {
                self.on_pairing_request(&inbound.topic, &id, &method, &params)
            }
            Message::Request { id, method, params } => {
                self.on_session_request(&inbound.topic, &id, &method, &params)
            }
            Message::Response { id, outcome } => self.on_response(&inbound.topic, &id, outcome),
        }
    }

    fn on_pairing_request(&mut self, topic: &str, id: &Value, method: &str, params: &Value) {
        match method {
            proto::METHOD_PROPOSE => self.on_proposal(topic, id, params),
            "wc_pairingPing" | "wc_pairingDelete" => {
                let (ttl, tag) = if method == "wc_pairingPing" {
                    (30, 1003)
                } else {
                    (86_400, 1001)
                };
                if let Some(p) = self.pairings.iter().find(|p| p.topic == topic) {
                    let delivery = proto::Delivery {
                        ttl,
                        tag,
                        prompt: false,
                    };
                    let _ = self.send(
                        topic,
                        &p.key.0,
                        &proto::build_result(id, Value::Bool(true)),
                        delivery,
                    );
                }
            }
            _ => {}
        }
    }

    fn on_proposal(&mut self, pairing_topic: &str, id: &Value, params: &Value) {
        let Some(proposal_id) = id.as_u64() else {
            return;
        };
        let now = now_secs();
        let Ok(data) = namespaces::parse_proposal(params) else {
            return;
        };
        if self.proposals.len() >= MAX_PROPOSALS
            || self.proposals.iter().any(|p| p.id == proposal_id)
        {
            return;
        }
        let expires_at = data
            .expiry
            .unwrap_or(now + PROPOSAL_LIFETIME_SECS)
            .min(now + PROPOSAL_LIFETIME_SECS);
        if expires_at <= now {
            return;
        }
        self.proposals.push(Proposal {
            id: proposal_id,
            pairing_topic: pairing_topic.to_string(),
            data,
            received_at: now,
            expires_at,
        });
    }

    fn on_response(&mut self, topic: &str, id: &Value, outcome: Result<Value, RpcError>) {
        let Some(id) = id.as_u64() else { return };
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|s| s.topic == topic && s.settle_id == id)
        else {
            return;
        };
        match outcome {
            Ok(_) => session.acknowledged = true,
            Err(e) => session.settle_error = Some(sanitize_text_max(&e.message, 120)),
        }
    }

    fn on_session_request(&mut self, topic: &str, id: &Value, method: &str, params: &Value) {
        let Some(session) = self.sessions.iter().find(|s| s.topic == topic) else {
            return;
        };
        let (session_chains, session_sui, session_methods) = (
            session.chains.clone(),
            session.sui_chain.clone(),
            session.methods.clone(),
        );
        let now = now_secs();
        match method {
            proto::METHOD_REQUEST => {}
            proto::METHOD_PING | proto::METHOD_EVENT | proto::METHOD_EXTEND => {
                self.reply_ok(topic, id, method);
                return;
            }
            proto::METHOD_DELETE => {
                self.remove_session(topic);
                return;
            }
            proto::METHOD_UPDATE => {
                self.reply_error(
                    topic,
                    id,
                    method,
                    proto::UNAUTHORIZED_METHOD,
                    "Update not allowed",
                );
                return;
            }
            _ => return,
        }
        let respond_error = |this: &Self, code: i64, message: &str| {
            this.reply_error(topic, id, proto::METHOD_REQUEST, code, message);
        };
        let Some(request_id) = id.as_u64() else {
            return;
        };
        let inner = params.get("request");
        let (Some(request_method), Some(chain_text)) = (
            inner.and_then(|r| r.get("method")).and_then(Value::as_str),
            params.get("chainId").and_then(Value::as_str),
        ) else {
            return respond_error(self, proto::INVALID_REQUEST, "Malformed request");
        };
        let (chain_id, sui_chain) = if let Some(sui) = &session_sui {
            if chain_text != sui {
                return respond_error(self, proto::UNAUTHORIZED_CHAIN, "Unauthorized chain");
            }
            (0, Some(sui.clone()))
        } else {
            let Ok(chain_id) = namespaces::parse_chain_reference(chain_text) else {
                return respond_error(self, proto::UNAUTHORIZED_CHAIN, "Unauthorized chain");
            };
            if !session_chains.contains(&chain_id) {
                return respond_error(self, proto::UNAUTHORIZED_CHAIN, "Unauthorized chain");
            }
            (chain_id, None)
        };
        if !session_methods.iter().any(|m| m == request_method) {
            return respond_error(self, proto::UNAUTHORIZED_METHOD, "Unauthorized method");
        }
        if self.requests.len() >= MAX_REQUESTS {
            return respond_error(self, proto::USER_REJECTED, "Too many pending requests");
        }
        let expires_at = inner
            .and_then(|r| r.get("expiry"))
            .and_then(Value::as_u64)
            .map(|e| e.min(now + 7 * 86_400))
            .unwrap_or(now + REQUEST_LIFETIME_SECS);
        if expires_at <= now {
            return respond_error(self, proto::REQUEST_EXPIRED, "Request expired");
        }
        let request_params = inner.and_then(|r| r.get("params")).unwrap_or(&Value::Null);
        match requests::parse_request(request_method, request_params) {
            Ok(action) => self.requests.push(PendingRequest {
                id: request_id,
                session_topic: topic.to_string(),
                method: request_method.to_string(),
                chain_id,
                sui_chain,
                action,
                received_at: now,
                expires_at,
            }),
            Err(e) => respond_error(self, proto::INVALID_REQUEST, &sanitize_text_max(&e, 120)),
        }
    }

    fn purge_expired(&mut self) {
        let now = now_secs();
        self.proposals.retain(|p| p.expires_at > now);
        self.pairings.retain(|p| p.expires_at > now);
        let expired: Vec<(String, u64)> = self
            .requests
            .iter()
            .filter(|r| r.expires_at <= now)
            .map(|r| (r.session_topic.clone(), r.id))
            .collect();
        for (topic, id) in expired {
            self.requests
                .retain(|r| !(r.session_topic == topic && r.id == id));
            self.reply_error(
                &topic,
                &id_value(id),
                proto::METHOD_REQUEST,
                proto::REQUEST_EXPIRED,
                "Request expired",
            );
        }
        let ended: Vec<String> = self
            .sessions
            .iter()
            .filter(|s| s.expires_at <= now)
            .map(|s| s.topic.clone())
            .collect();
        for topic in ended {
            self.remove_session(&topic);
        }
    }

    fn remove_session(&mut self, topic: &str) {
        self.sessions.retain(|s| s.topic != topic);
        self.requests.retain(|r| r.session_topic != topic);
        self.relay.unsubscribe(topic);
    }

    // ---- pairing and proposals ---------------------------------------------

    /// Subscribes to the pairing topic and waits up to `wait` for the dapp's proposal.
    pub fn pair(&mut self, uri_text: &str, wait: Duration) -> Result<PairOutcome, String> {
        let parsed = uri::parse(uri_text, now_secs())?;
        if self.pairings.iter().all(|p| p.topic != parsed.topic) {
            if self.pairings.len() >= 20 {
                return Err("too many open pairings; wait for them to expire".to_string());
            }
            self.relay.subscribe(&parsed.topic)?;
            self.pairings.push(Pairing {
                topic: parsed.topic.clone(),
                key: SymKey(parsed.sym_key),
                expires_at: parsed
                    .expiry
                    .unwrap_or(now_secs() + PROPOSAL_LIFETIME_SECS)
                    .max(now_secs() + PROPOSAL_LIFETIME_SECS),
            });
        }
        let topic = parsed.topic.clone();
        self.pump_until(wait, |m| {
            m.proposals.iter().any(|p| p.pairing_topic == topic)
        });
        Ok(PairOutcome {
            proposal_ids: self
                .proposals
                .iter()
                .filter(|p| p.pairing_topic == topic)
                .map(|p| p.id)
                .collect(),
            pairing_topic: topic,
        })
    }

    pub fn proposals(&self) -> &[Proposal] {
        &self.proposals
    }

    pub fn proposal(&self, id: u64) -> Option<&Proposal> {
        self.proposals.iter().find(|p| p.id == id)
    }

    /// Accepts a proposal: derives the session key, answers on the pairing topic and settles
    /// the session. `address` is only used to build the account list and is not kept.
    pub fn approve_session(
        &mut self,
        proposal_id: u64,
        wallet: &str,
        address: &Address,
        approval: &Approval,
    ) -> Result<ApprovedSession, String> {
        self.approve_session_with(
            proposal_id,
            wallet,
            SessionTerms {
                namespaces: namespaces::settle_namespaces(approval, address),
                properties: None,
                chains: approval.chains.clone(),
                sui_chain: None,
                methods: approval.methods.clone(),
            },
        )
    }

    /// Like `approve_session`, for terms the caller has already worked out (Ethereum or Sui).
    pub fn approve_session_with(
        &mut self,
        proposal_id: u64,
        wallet: &str,
        terms: SessionTerms,
    ) -> Result<ApprovedSession, String> {
        if self.sessions.len() >= MAX_SESSIONS {
            return Err("too many open sessions; disconnect one first".to_string());
        }
        let position = self
            .proposals
            .iter()
            .position(|p| p.id == proposal_id)
            .ok_or("no such proposal (it may have expired); ask the dapp to connect again")?;
        let pairing_key = {
            let proposal = &self.proposals[position];
            self.pairings
                .iter()
                .find(|p| p.topic == proposal.pairing_topic)
                .map(|p| p.key.clone())
                .ok_or("the pairing for this proposal has expired")?
        };
        let proposal = &self.proposals[position];
        let own = KeyPair::generate()?;
        let session_key = envelope::derive_session_key(&own, &proposal.data.proposer_public)?;
        let topic = envelope::topic_of(&session_key);
        let now = now_secs();
        let settle_id = proto::new_message_id()?;
        let mut settle_params = vec![
            (
                "relay",
                Value::object(vec![("protocol", Value::str("irn"))]),
            ),
            ("namespaces", terms.namespaces.clone()),
        ];
        if let Some(properties) = &terms.properties {
            settle_params.push(("sessionProperties", properties.clone()));
        }
        settle_params.extend(vec![
            (
                "controller",
                Value::object(vec![
                    ("publicKey", Value::String(hex::encode(&own.public))),
                    ("metadata", controller_metadata()),
                ]),
            ),
            (
                "expiry",
                Value::Number((now + SESSION_LIFETIME_SECS).to_string()),
            ),
        ]);
        let settle = proto::build_request(
            settle_id,
            proto::METHOD_SETTLE,
            Value::object(settle_params),
        );
        let answer = proto::build_result(
            &id_value(proposal.id),
            Value::object(vec![
                (
                    "relay",
                    Value::object(vec![("protocol", Value::str("irn"))]),
                ),
                (
                    "responderPublicKey",
                    Value::String(hex::encode(&own.public)),
                ),
            ]),
        );
        let dapp = proposal.data.dapp.clone();
        let pairing_topic = proposal.pairing_topic.clone();

        self.relay.subscribe(&topic)?;
        let answered = self
            .send(
                &pairing_topic,
                &pairing_key.0,
                &answer,
                proto::response_delivery(proto::METHOD_PROPOSE).expect("known method"),
            )
            .and_then(|_| {
                self.send(
                    &topic,
                    &session_key,
                    &settle,
                    proto::request_delivery(proto::METHOD_SETTLE).expect("known method"),
                )
            });
        if let Err(e) = answered {
            self.relay.unsubscribe(&topic);
            return Err(format!("could not reach the dapp: {e}"));
        }
        self.proposals.remove(position);
        self.sessions.push(Session {
            topic: topic.clone(),
            key: SymKey(session_key),
            wallet: wallet.to_string(),
            chains: terms.chains.clone(),
            sui_chain: terms.sui_chain.clone(),
            methods: terms.methods.clone(),
            dapp,
            created_at: now,
            expires_at: now + SESSION_LIFETIME_SECS,
            acknowledged: false,
            settle_error: None,
            settle_id,
        });
        self.pump_until(self.settle_wait, |m| {
            m.sessions
                .iter()
                .find(|s| s.topic == topic)
                .is_none_or(|s| s.acknowledged || s.settle_error.is_some())
        });
        if let Some(session) = self.sessions.iter().find(|s| s.topic == topic)
            && let Some(error) = session.settle_error.clone()
        {
            self.remove_session(&topic);
            return Err(format!("the dapp refused the session: {error}"));
        }
        let acknowledged = self
            .sessions
            .iter()
            .find(|s| s.topic == topic)
            .is_some_and(|s| s.acknowledged);
        Ok(ApprovedSession {
            topic,
            acknowledged,
        })
    }

    /// Tells the dapp the proposal was declined. Returns whether the dapp could be notified.
    pub fn reject_session(&mut self, proposal_id: u64) -> Result<bool, String> {
        let position = self
            .proposals
            .iter()
            .position(|p| p.id == proposal_id)
            .ok_or("no such proposal (it may have expired)")?;
        let proposal = self.proposals.remove(position);
        let Some(key) = self
            .pairings
            .iter()
            .find(|p| p.topic == proposal.pairing_topic)
            .map(|p| p.key.clone())
        else {
            return Ok(false);
        };
        let payload = proto::build_error(
            &id_value(proposal.id),
            proto::USER_REJECTED,
            "User rejected.",
        );
        Ok(self
            .send(
                &proposal.pairing_topic,
                &key.0,
                &payload,
                proto::PROPOSE_REJECT,
            )
            .is_ok())
    }

    // ---- sessions ---------------------------------------------------------

    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    /// Finds a session by full topic or by a unique prefix of at least 8 characters.
    pub fn find_session(&self, id: &str) -> Result<&Session, String> {
        let id = id.trim().to_lowercase();
        if id.len() < 8 {
            return Err("session_topic must be at least 8 characters".to_string());
        }
        let mut matches = self.sessions.iter().filter(|s| s.topic.starts_with(&id));
        match (matches.next(), matches.next()) {
            (Some(s), None) => Ok(s),
            (Some(_), Some(_)) => Err("that prefix matches more than one session".to_string()),
            _ => Err("no such session".to_string()),
        }
    }

    /// Ends a session. Returns whether the dapp could be notified.
    pub fn disconnect(&mut self, topic: &str) -> Result<bool, String> {
        let topic = self.find_session(topic)?.topic.clone();
        let id = proto::new_message_id()?;
        let payload = proto::build_request(
            id,
            proto::METHOD_DELETE,
            Value::object(vec![
                ("code", Value::int(proto::USER_DISCONNECTED)),
                ("message", Value::str("User disconnected.")),
            ]),
        );
        let notified = self
            .send_on_session(
                &topic,
                &payload,
                proto::request_delivery(proto::METHOD_DELETE).expect("known method"),
            )
            .is_ok();
        self.remove_session(&topic);
        Ok(notified)
    }

    // ---- requests ---------------------------------------------------------

    pub fn requests(&self) -> &[PendingRequest] {
        &self.requests
    }

    pub fn request(&self, id: u64) -> Result<&PendingRequest, String> {
        let mut matches = self.requests.iter().filter(|r| r.id == id);
        match (matches.next(), matches.next()) {
            (Some(r), None) => Ok(r),
            (Some(_), Some(_)) => Err("that request id is ambiguous".to_string()),
            _ => Err("no such request (it may have expired)".to_string()),
        }
    }

    /// Answers a request and forgets it. The request is removed even if the dapp cannot be
    /// reached, so a transaction that was already sent can never be approved twice.
    /// Returns whether the dapp could be notified.
    pub fn answer_request(
        &mut self,
        id: u64,
        outcome: Result<Value, (i64, String)>,
    ) -> Result<bool, String> {
        let position = self
            .requests
            .iter()
            .position(|r| r.id == id)
            .ok_or("no such request (it may have expired)")?;
        let request = self.requests.remove(position);
        let payload = match outcome {
            Ok(result) => proto::build_result(&id_value(id), result),
            Err((code, message)) => proto::build_error(&id_value(id), code, &message),
        };
        Ok(self
            .send_on_session(
                &request.session_topic,
                &payload,
                proto::response_delivery(proto::METHOD_REQUEST).expect("known method"),
            )
            .is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eth::address::to_checksum;
    use crate::json;
    use crate::walletconnect::relay::mock::MockRelay;
    use crate::walletconnect::testing::{Dapp, OPTIONAL, REQUIRED};

    const ADDRESS: Address = [0x11; 20];

    fn setup() -> (Rc<MockRelay>, WcManager, Dapp) {
        let relay = Rc::new(MockRelay::default());
        let manager = WcManager::new(relay.clone()).with_settle_wait(Duration::ZERO);
        (relay, manager, Dapp::new())
    }

    /// Pairs, approves the proposal for chains 1 and 10, then plays the dapp: it derives the
    /// session keys from the wallet's answer and acknowledges the settlement.
    fn connect(relay: &MockRelay, manager: &mut WcManager, dapp: &mut Dapp) -> String {
        dapp.send_proposal(relay, 1_700_000_000_000_000_001, REQUIRED, OPTIONAL);
        let out = manager.pair(&dapp.uri(), Duration::ZERO).unwrap();
        assert_eq!(out.proposal_ids, vec![1_700_000_000_000_000_001]);
        let approval = namespaces::plan_approval(&manager.proposals()[0].data, &[1, 10]).unwrap();
        let approved = manager
            .approve_session(1_700_000_000_000_000_001, "Main", &ADDRESS, &approval)
            .unwrap();
        assert!(!approved.acknowledged);
        let settle = dapp.accept(&relay.take_published());
        let settle_id = settle.get("id").cloned().unwrap();
        dapp.push_session_message(
            relay,
            proto::build_result(&settle_id, Value::Bool(true)),
            1103,
        );
        manager.poll();
        approved.topic
    }

    #[test]
    fn pairing_receives_a_proposal() {
        let (relay, mut manager, dapp) = setup();
        dapp.send_proposal(&relay, 1_700_000_000_000_000_001, REQUIRED, OPTIONAL);
        let out = manager.pair(&dapp.uri(), Duration::ZERO).unwrap();
        assert_eq!(out.pairing_topic, dapp.pairing_topic);
        assert_eq!(out.proposal_ids, vec![1_700_000_000_000_000_001]);
        assert_eq!(*relay.subscribed.borrow(), vec![dapp.pairing_topic.clone()]);
        let p = &manager.proposals()[0];
        assert_eq!(p.data.dapp.name, "Test Dapp");
        assert_eq!(p.data.required.chains, vec![1]);
        assert!(p.expires_at > now_secs());
    }

    #[test]
    fn pairing_twice_does_not_subscribe_twice() {
        let (relay, mut manager, dapp) = setup();
        manager.pair(&dapp.uri(), Duration::ZERO).unwrap();
        manager.pair(&dapp.uri(), Duration::ZERO).unwrap();
        assert_eq!(relay.subscribed.borrow().len(), 1);
    }

    #[test]
    fn bad_uris_and_relay_failures_surface() {
        let (_relay, mut manager, _dapp) = setup();
        assert!(manager.pair("https://example.com", Duration::ZERO).is_err());
        let unavailable = Rc::new(crate::walletconnect::relay::UnavailableRelay(
            "no project ID".into(),
        ));
        let mut manager = WcManager::new(unavailable);
        let dapp = Dapp::new();
        assert_eq!(
            manager.pair(&dapp.uri(), Duration::ZERO).err().unwrap(),
            "no project ID"
        );
    }

    #[test]
    fn messages_for_unknown_topics_or_with_bad_keys_are_ignored() {
        let (relay, mut manager, dapp) = setup();
        manager.pair(&dapp.uri(), Duration::ZERO).unwrap();
        relay.push("unknown-topic", "AAAA".into(), 1);
        let wrong = envelope::encode_type0(&[1u8; 32], dapp.proposal(5, REQUIRED, "{}").as_bytes())
            .unwrap();
        relay.push(&dapp.pairing_topic, wrong, 1100);
        relay.push(&dapp.pairing_topic, "not base64!".into(), 1100);
        let junk = envelope::encode_type0(&dapp.pairing_key, b"not json").unwrap();
        relay.push(&dapp.pairing_topic, junk, 1100);
        manager.poll();
        assert!(manager.proposals().is_empty());
    }

    #[test]
    fn duplicate_deliveries_are_processed_once() {
        let (relay, mut manager, dapp) = setup();
        dapp.send_proposal(&relay, 1_700_000_000_000_000_042, REQUIRED, OPTIONAL);
        let again = relay.inbound.borrow()[0].clone();
        relay.inbound.borrow_mut().push_back(again);
        manager.pair(&dapp.uri(), Duration::ZERO).unwrap();
        manager.poll();
        assert_eq!(manager.proposals().len(), 1);
    }

    #[test]
    fn approving_a_session_answers_and_settles() {
        let (relay, mut manager, mut dapp) = setup();
        let topic = connect(&relay, &mut manager, &mut dapp);
        assert_eq!(Some(&topic), dapp.session_topic.as_ref());
        assert!(manager.proposals().is_empty());
        let session = &manager.sessions()[0];
        assert!(session.acknowledged);
        assert_eq!(session.wallet, "Main");
        assert_eq!(session.chains, vec![1, 10]);
        assert!(relay.subscribed.borrow().contains(&topic));
    }

    #[test]
    fn settle_message_carries_accounts_namespaces_and_controller() {
        let (relay, mut manager, mut dapp) = setup();
        dapp.send_proposal(&relay, 1_700_000_000_000_000_001, REQUIRED, OPTIONAL);
        manager.pair(&dapp.uri(), Duration::ZERO).unwrap();
        let approval = namespaces::plan_approval(&manager.proposals()[0].data, &[1, 10]).unwrap();
        manager
            .approve_session(1_700_000_000_000_000_001, "Main", &ADDRESS, &approval)
            .unwrap();
        let published = relay.take_published();
        assert_eq!(published.len(), 2);
        assert_eq!((published[0].tag, published[0].ttl), (1101, 300));
        assert_eq!((published[1].tag, published[1].ttl), (1102, 300));
        let settle = dapp.accept(&published);
        assert_eq!(
            settle.get("method").and_then(Value::as_str),
            Some("wc_sessionSettle")
        );
        let params = settle.get("params").unwrap();
        let eip = params.get("namespaces").unwrap().get("eip155").unwrap();
        let accounts = eip.get("accounts").unwrap().as_array().unwrap();
        assert_eq!(
            accounts[0].as_str(),
            Some(format!("eip155:1:{}", to_checksum(&ADDRESS)).as_str())
        );
        assert_eq!(accounts.len(), 2);
        let controller = params.get("controller").unwrap();
        assert_eq!(
            controller
                .get("publicKey")
                .and_then(Value::as_str)
                .map(str::len),
            Some(64)
        );
        assert!(params.get("expiry").and_then(Value::as_u64).unwrap() > now_secs());
    }

    #[test]
    fn session_keys_are_not_leaked_in_the_session_view() {
        let (relay, mut manager, mut dapp) = setup();
        connect(&relay, &mut manager, &mut dapp);
        let session = &manager.sessions()[0];
        assert_eq!(session.dapp.url, "https://dapp.example");
        assert!(
            session
                .methods
                .contains(&"eth_signTypedData_v4".to_string())
        );
    }

    #[test]
    fn dapp_refusing_the_settlement_removes_the_session() {
        let (relay, mut manager, mut dapp) = setup();
        dapp.send_proposal(&relay, 1_700_000_000_000_000_001, REQUIRED, OPTIONAL);
        manager.pair(&dapp.uri(), Duration::ZERO).unwrap();
        let approval = namespaces::plan_approval(&manager.proposals()[0].data, &[1]).unwrap();
        // Approve, but queue a refusal for the settlement first: the id is only known after
        // publishing, so deliver it through a second approval round.
        let approved = manager
            .approve_session(1_700_000_000_000_000_001, "Main", &ADDRESS, &approval)
            .unwrap();
        let published = relay.take_published();
        let settle = dapp.accept(&published);
        let settle_id = settle.get("id").cloned().unwrap();
        dapp.push_session_message(
            &relay,
            proto::build_error(&settle_id, proto::SESSION_SETTLEMENT_FAILED, "nope"),
            1103,
        );
        manager.poll();
        assert_eq!(approved.topic, dapp.session_topic.clone().unwrap());
        assert_eq!(manager.sessions()[0].settle_error.as_deref(), Some("nope"));
    }

    #[test]
    fn approval_errors_leave_the_proposal_usable() {
        let (relay, mut manager, dapp) = setup();
        dapp.send_proposal(&relay, 1_700_000_000_000_000_001, REQUIRED, OPTIONAL);
        manager.pair(&dapp.uri(), Duration::ZERO).unwrap();
        let approval = namespaces::plan_approval(&manager.proposals()[0].data, &[1]).unwrap();
        assert!(
            manager
                .approve_session(7, "Main", &ADDRESS, &approval)
                .is_err()
        );
        relay.fail_publish.set(true);
        let err = manager
            .approve_session(1_700_000_000_000_000_001, "Main", &ADDRESS, &approval)
            .err()
            .unwrap();
        assert!(err.contains("could not reach the dapp"), "{err}");
        assert_eq!(manager.proposals().len(), 1);
        assert!(manager.sessions().is_empty());
        assert_eq!(relay.unsubscribed.borrow().len(), 1);
    }

    #[test]
    fn rejecting_a_proposal_notifies_the_dapp() {
        let (relay, mut manager, mut dapp) = setup();
        dapp.send_proposal(&relay, 1_700_000_000_000_000_001, REQUIRED, OPTIONAL);
        manager.pair(&dapp.uri(), Duration::ZERO).unwrap();
        assert!(manager.reject_session(1_700_000_000_000_000_001).unwrap());
        let published = relay.take_published();
        assert_eq!(published[0].tag, 1120);
        let plain = envelope::decode(&dapp.pairing_key, &published[0].message).unwrap();
        let doc = json::parse(std::str::from_utf8(&plain.plaintext).unwrap()).unwrap();
        assert_eq!(
            doc.get("error")
                .and_then(|e| e.get("code"))
                .and_then(Value::as_u64),
            Some(5000)
        );
        assert!(manager.proposals().is_empty());
        assert!(manager.reject_session(1_700_000_000_000_000_001).is_err());
        dapp.session_key = None;
    }

    #[test]
    fn session_requests_become_pending_requests() {
        let (relay, mut manager, mut dapp) = setup();
        connect(&relay, &mut manager, &mut dapp);
        let params = r#"["0x48656c6c6f","0x1111111111111111111111111111111111111111"]"#;
        dapp.request(&relay, 900, "eip155:1", "personal_sign", params);
        manager.poll();
        let request = manager.request(900).unwrap();
        assert_eq!(request.method, "personal_sign");
        assert_eq!(request.chain_id, 1);
        assert!(matches!(request.action, Action::PersonalSign { .. }));
        assert!(request.expires_at > now_secs());
        assert_eq!(manager.requests().len(), 1);
    }

    #[test]
    fn requests_outside_the_session_are_refused_immediately() {
        let (relay, mut manager, mut dapp) = setup();
        connect(&relay, &mut manager, &mut dapp);
        let params = r#"["0x00","0x1111111111111111111111111111111111111111"]"#;
        dapp.request(&relay, 901, "eip155:56", "personal_sign", params);
        dapp.request(&relay, 902, "eip155:1", "eth_sign", params);
        dapp.request(&relay, 903, "eip155:1", "personal_sign", r#"["0x00"]"#);
        manager.poll();
        assert!(manager.requests().is_empty());
        let codes: Vec<u64> = relay
            .take_published()
            .iter()
            .map(|p| {
                let doc = dapp.decode_session(&p.message);
                assert_eq!(p.tag, 1109);
                doc.get("error")
                    .and_then(|e| e.get("code"))
                    .and_then(Value::as_u64)
                    .unwrap()
            })
            .collect();
        assert_eq!(codes, vec![3005, 3001, 1001]);
    }

    #[test]
    fn malformed_session_requests_get_an_error() {
        let (relay, mut manager, mut dapp) = setup();
        connect(&relay, &mut manager, &mut dapp);
        let payload = proto::build_request(
            950,
            proto::METHOD_REQUEST,
            json::parse(r#"{"nothing":1}"#).unwrap(),
        );
        dapp.push_session_message(&relay, payload, 1108);
        manager.poll();
        let published = relay.take_published();
        assert_eq!(published.len(), 1);
        assert!(manager.requests().is_empty());
    }

    #[test]
    fn answering_a_request_sends_the_result_and_forgets_it() {
        let (relay, mut manager, mut dapp) = setup();
        connect(&relay, &mut manager, &mut dapp);
        dapp.request(
            &relay,
            910,
            "eip155:1",
            "personal_sign",
            r#"["0x00","0x1111111111111111111111111111111111111111"]"#,
        );
        manager.poll();
        assert!(
            manager
                .answer_request(910, Ok(Value::str("0xsig")))
                .unwrap()
        );
        assert!(manager.requests().is_empty());
        let published = relay.take_published();
        let doc = dapp.decode_session(&published[0].message);
        assert_eq!(doc.get("id").and_then(Value::as_u64), Some(910));
        assert_eq!(doc.get("result").and_then(Value::as_str), Some("0xsig"));
        assert_eq!(
            (published[0].tag, published[0].ttl, published[0].prompt),
            (1109, 300, false)
        );
        assert!(manager.answer_request(910, Ok(Value::Null)).is_err());
    }

    #[test]
    fn a_request_is_removed_even_when_the_relay_is_down() {
        let (relay, mut manager, mut dapp) = setup();
        connect(&relay, &mut manager, &mut dapp);
        dapp.request(
            &relay,
            911,
            "eip155:1",
            "personal_sign",
            r#"["0x00","0x1111111111111111111111111111111111111111"]"#,
        );
        manager.poll();
        relay.fail_publish.set(true);
        assert!(!manager.answer_request(911, Ok(Value::Null)).unwrap());
        assert!(manager.requests().is_empty());
    }

    #[test]
    fn rejecting_sends_a_user_rejected_error() {
        let (relay, mut manager, mut dapp) = setup();
        connect(&relay, &mut manager, &mut dapp);
        dapp.request(
            &relay,
            912,
            "eip155:1",
            "personal_sign",
            r#"["0x00","0x1111111111111111111111111111111111111111"]"#,
        );
        manager.poll();
        manager
            .answer_request(912, Err((proto::USER_REJECTED, "User rejected.".into())))
            .unwrap();
        let doc = dapp.decode_session(&relay.take_published()[0].message);
        assert_eq!(
            doc.get("error")
                .and_then(|e| e.get("code"))
                .and_then(Value::as_u64),
            Some(5000)
        );
    }

    #[test]
    fn pings_events_and_updates_are_answered() {
        let (relay, mut manager, mut dapp) = setup();
        connect(&relay, &mut manager, &mut dapp);
        for (id, method, tag) in [
            (1u64, proto::METHOD_PING, 1115u64),
            (2, proto::METHOD_EVENT, 1111),
            (3, proto::METHOD_EXTEND, 1107),
            (4, proto::METHOD_UPDATE, 1105),
        ] {
            dapp.push_session_message(
                &relay,
                proto::build_request(id, method, Value::object(vec![])),
                0,
            );
            manager.poll();
            let published = relay.take_published();
            assert_eq!(published.len(), 1, "{method}");
            assert_eq!(published[0].tag, tag, "{method}");
            let doc = dapp.decode_session(&published[0].message);
            assert_eq!(doc.get("error").is_some(), method == proto::METHOD_UPDATE);
        }
    }

    #[test]
    fn the_dapp_can_end_the_session() {
        let (relay, mut manager, mut dapp) = setup();
        let topic = connect(&relay, &mut manager, &mut dapp);
        dapp.request(
            &relay,
            913,
            "eip155:1",
            "personal_sign",
            r#"["0x00","0x1111111111111111111111111111111111111111"]"#,
        );
        manager.poll();
        dapp.push_session_message(
            &relay,
            proto::build_request(
                5,
                proto::METHOD_DELETE,
                json::parse(r#"{"code":6000,"message":"bye"}"#).unwrap(),
            ),
            1112,
        );
        manager.poll();
        assert!(manager.sessions().is_empty());
        assert!(manager.requests().is_empty());
        assert!(relay.unsubscribed.borrow().contains(&topic));
    }

    #[test]
    fn disconnecting_notifies_the_dapp_and_cleans_up() {
        let (relay, mut manager, mut dapp) = setup();
        let topic = connect(&relay, &mut manager, &mut dapp);
        assert!(manager.disconnect(&topic[..10]).unwrap());
        let published = relay.take_published();
        assert_eq!((published[0].tag, published[0].ttl), (1112, 86_400));
        let doc = dapp.decode_session(&published[0].message);
        assert_eq!(
            doc.get("method").and_then(Value::as_str),
            Some("wc_sessionDelete")
        );
        assert!(manager.sessions().is_empty());
        assert!(relay.unsubscribed.borrow().contains(&topic));
        assert!(manager.disconnect(&topic).is_err());
    }

    #[test]
    fn session_lookup_by_prefix_is_strict() {
        let (relay, mut manager, mut dapp) = setup();
        let topic = connect(&relay, &mut manager, &mut dapp);
        assert_eq!(manager.find_session(&topic).unwrap().topic, topic);
        assert_eq!(
            manager
                .find_session(&topic[..8].to_uppercase())
                .unwrap()
                .topic,
            topic
        );
        assert!(manager.find_session(&topic[..7]).is_err());
        assert!(manager.find_session("ffffffffffff").is_err());
    }

    #[test]
    fn expired_requests_get_an_expiry_error() {
        let (relay, mut manager, mut dapp) = setup();
        connect(&relay, &mut manager, &mut dapp);
        dapp.request(
            &relay,
            914,
            "eip155:1",
            "personal_sign",
            r#"["0x00","0x1111111111111111111111111111111111111111"]"#,
        );
        manager.poll();
        manager.requests[0].expires_at = now_secs() - 1;
        manager.poll();
        assert!(manager.requests().is_empty());
        let doc = dapp.decode_session(&relay.take_published()[0].message);
        assert_eq!(
            doc.get("error")
                .and_then(|e| e.get("code"))
                .and_then(Value::as_u64),
            Some(8000)
        );
    }

    #[test]
    fn expired_proposals_and_sessions_are_dropped() {
        let (relay, mut manager, dapp) = setup();
        dapp.send_proposal(&relay, 1_700_000_000_000_000_009, REQUIRED, OPTIONAL);
        manager.pair(&dapp.uri(), Duration::ZERO).unwrap();
        manager.proposals[0].expires_at = now_secs() - 1;
        manager.poll();
        assert!(manager.proposals().is_empty());
        let mut manager = WcManager::new(relay.clone()).with_settle_wait(Duration::ZERO);
        let mut dapp = Dapp::new();
        let topic = connect(&relay, &mut manager, &mut dapp);
        manager.sessions[0].expires_at = now_secs() - 1;
        manager.poll();
        assert!(manager.sessions().is_empty());
        assert!(relay.unsubscribed.borrow().contains(&topic));
    }

    #[test]
    fn an_already_expired_proposal_is_never_listed() {
        let (relay, mut manager, dapp) = setup();
        let mut p = json::parse(&dapp.proposal(5, REQUIRED, "{}")).unwrap();
        if let Value::Object(pairs) = &mut p
            && let Some((_, Value::Object(params))) = pairs.iter_mut().find(|(k, _)| k == "params")
        {
            params.push(("expiryTimestamp".into(), Value::int(now_secs() as i64 - 5)));
        }
        let message = envelope::encode_type0(&dapp.pairing_key, p.to_json().as_bytes()).unwrap();
        relay.push(&dapp.pairing_topic, message, 1100);
        manager.pair(&dapp.uri(), Duration::ZERO).unwrap();
        assert!(manager.proposals().is_empty());
    }

    #[test]
    fn request_limits_are_enforced() {
        let (relay, mut manager, mut dapp) = setup();
        connect(&relay, &mut manager, &mut dapp);
        for i in 0..(MAX_REQUESTS as u64 + 1) {
            dapp.request(
                &relay,
                2000 + i,
                "eip155:1",
                "personal_sign",
                r#"["0x00","0x1111111111111111111111111111111111111111"]"#,
            );
        }
        manager.poll();
        assert_eq!(manager.requests().len(), MAX_REQUESTS);
        assert_eq!(relay.take_published().len(), 1);
    }

    #[test]
    fn pairing_pings_are_answered() {
        let (relay, mut manager, dapp) = setup();
        manager.pair(&dapp.uri(), Duration::ZERO).unwrap();
        let ping = proto::build_request(77, "wc_pairingPing", Value::object(vec![]));
        relay.push(
            &dapp.pairing_topic,
            envelope::encode_type0(&dapp.pairing_key, ping.as_bytes()).unwrap(),
            1002,
        );
        manager.poll();
        let published = relay.take_published();
        assert_eq!((published[0].tag, published[0].ttl), (1003, 30));
    }
}
