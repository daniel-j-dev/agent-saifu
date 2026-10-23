//! Test helper: plays the dapp's side of a WalletConnect session against a `MockRelay`.

use super::envelope;
use super::now_secs;
use super::proto;
use super::relay::mock::{MockRelay, Published};
use crate::crypto::hex;
use crate::crypto::x25519::KeyPair;
use crate::json::{self, Value};

/// The dapp's side of a connection, enough to drive the wallet in tests.
pub struct Dapp {
    pub pairing_key: [u8; 32],
    pub pairing_topic: String,
    pub keys: KeyPair,
    pub session_key: Option<[u8; 32]>,
    pub session_topic: Option<String>,
}

impl Default for Dapp {
    fn default() -> Self {
        Dapp::new()
    }
}

impl Dapp {
    pub fn new() -> Dapp {
        let pairing_key = crate::crypto::random::random_array::<32>().unwrap();
        Dapp {
            pairing_topic: envelope::topic_of(&pairing_key),
            pairing_key,
            keys: KeyPair::generate().unwrap(),
            session_key: None,
            session_topic: None,
        }
    }

    pub fn uri(&self) -> String {
        format!(
            "wc:{}@2?relay-protocol=irn&symKey={}&expiryTimestamp={}",
            self.pairing_topic,
            hex::encode(&self.pairing_key),
            now_secs() + 600
        )
    }

    pub fn proposal(&self, id: u64, required: &str, optional: &str) -> String {
        proto::build_request(
            id,
            proto::METHOD_PROPOSE,
            json::parse(&format!(
                r#"{{"relays":[{{"protocol":"irn"}}],
                "proposer":{{"publicKey":"{}","metadata":{{"name":"Test Dapp","description":"d","url":"https://dapp.example","icons":[]}}}},
                "requiredNamespaces":{required},"optionalNamespaces":{optional}}}"#,
                hex::encode(&self.keys.public)
            ))
            .unwrap(),
        )
    }

    pub fn send_proposal(&self, relay: &MockRelay, id: u64, required: &str, optional: &str) {
        let message = envelope::encode_type0(
            &self.pairing_key,
            self.proposal(id, required, optional).as_bytes(),
        )
        .unwrap();
        relay.push(&self.pairing_topic, message, 1100);
    }

    /// Reads the wallet's proposal answer and derives the session keys like a dapp would.
    pub fn accept(&mut self, published: &[Published]) -> Value {
        let answer = published
            .iter()
            .find(|p| p.topic == self.pairing_topic)
            .expect("answer on the pairing topic");
        let plain = envelope::decode(&self.pairing_key, &answer.message).unwrap();
        let doc = json::parse(std::str::from_utf8(&plain.plaintext).unwrap()).unwrap();
        let responder = hex::decode_fixed::<32>(
            doc.get("result")
                .unwrap()
                .get("responderPublicKey")
                .and_then(Value::as_str)
                .unwrap(),
        )
        .unwrap();
        let key = envelope::derive_session_key(&self.keys, &responder).unwrap();
        self.session_topic = Some(envelope::topic_of(&key));
        self.session_key = Some(key);
        let settle = published
            .iter()
            .find(|p| Some(&p.topic) == self.session_topic.as_ref())
            .expect("settle on the session topic");
        let plain = envelope::decode(&key, &settle.message).unwrap();
        json::parse(std::str::from_utf8(&plain.plaintext).unwrap()).unwrap()
    }

    pub fn push_session_message(&self, relay: &MockRelay, payload: String, tag: u64) {
        let message =
            envelope::encode_type0(&self.session_key.unwrap(), payload.as_bytes()).unwrap();
        relay.push(self.session_topic.as_ref().unwrap(), message, tag);
    }

    pub fn request(&self, relay: &MockRelay, id: u64, chain: &str, method: &str, params: &str) {
        let payload = proto::build_request(
            id,
            proto::METHOD_REQUEST,
            json::parse(&format!(
                r#"{{"request":{{"method":"{method}","params":{params}}},"chainId":"{chain}"}}"#
            ))
            .unwrap(),
        );
        self.push_session_message(relay, payload, 1108);
    }

    pub fn decode_session(&self, message: &str) -> Value {
        let plain = envelope::decode(&self.session_key.unwrap(), message).unwrap();
        json::parse(std::str::from_utf8(&plain.plaintext).unwrap()).unwrap()
    }
}

pub const REQUIRED: &str = r#"{"eip155":{"chains":["eip155:1"],"methods":["eth_sendTransaction","personal_sign"],"events":["chainChanged"]}}"#;
pub const OPTIONAL: &str =
    r#"{"eip155":{"chains":["eip155:10"],"methods":["eth_signTypedData_v4"],"events":[]}}"#;
