//! Relay client authentication: an EdDSA JWT whose issuer is the `did:key` of a client key.

use crate::crypto::{base58, base64, ed25519};
use crate::json::Value;

/// Multicodec prefix for an Ed25519 public key.
const ED25519_MULTICODEC: [u8; 2] = [0xed, 0x01];
pub const TOKEN_LIFETIME_SECS: u64 = 24 * 60 * 60;

pub fn did_key(public_key: &[u8; 32]) -> String {
    let mut data = ED25519_MULTICODEC.to_vec();
    data.extend_from_slice(public_key);
    format!("did:key:z{}", base58::encode(&data))
}

/// `subject` is a random per-connection nonce (hex); `audience` is the relay URL.
pub fn build_jwt(seed: &[u8; 32], subject: &str, audience: &str, now: u64) -> String {
    let header = Value::object(vec![
        ("alg", Value::str("EdDSA")),
        ("typ", Value::str("JWT")),
    ]);
    let claims = Value::object(vec![
        ("iss", Value::String(did_key(&ed25519::public_key(seed)))),
        ("sub", Value::str(subject)),
        ("aud", Value::str(audience)),
        ("iat", Value::Number(now.to_string())),
        (
            "exp",
            Value::Number((now + TOKEN_LIFETIME_SECS).to_string()),
        ),
        ("act", Value::str("client_auth")),
    ]);
    let signing_input = format!(
        "{}.{}",
        base64::encode_url(header.to_json().as_bytes()),
        base64::encode_url(claims.to_json().as_bytes())
    );
    let signature = ed25519::sign(seed, signing_input.as_bytes());
    format!("{signing_input}.{}", base64::encode_url(&signature))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    fn decode_segment(segment: &str) -> Vec<u8> {
        let mut padded = segment.replace('-', "+").replace('_', "/");
        while !padded.len().is_multiple_of(4) {
            padded.push('=');
        }
        base64::decode(&padded).unwrap()
    }

    #[test]
    fn did_key_uses_the_ed25519_multicodec_prefix() {
        for key in [[0u8; 32], [0xffu8; 32], [0x5au8; 32]] {
            let did = did_key(&key);
            assert!(did.starts_with("did:key:z6Mk"), "{did}");
            let encoded = did.len() - "did:key:z".len();
            assert!((46..=48).contains(&encoded), "{did}");
        }
    }

    #[test]
    fn jwt_has_three_segments_and_the_expected_claims() {
        let seed = [42u8; 32];
        let jwt = build_jwt(&seed, "abc123", "wss://relay.example.org", 1_700_000_000);
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header = json::parse(std::str::from_utf8(&decode_segment(parts[0])).unwrap()).unwrap();
        assert_eq!(header.get("alg").and_then(Value::as_str), Some("EdDSA"));
        assert_eq!(header.get("typ").and_then(Value::as_str), Some("JWT"));
        let claims = json::parse(std::str::from_utf8(&decode_segment(parts[1])).unwrap()).unwrap();
        assert_eq!(claims.get("sub").and_then(Value::as_str), Some("abc123"));
        assert_eq!(
            claims.get("aud").and_then(Value::as_str),
            Some("wss://relay.example.org")
        );
        assert_eq!(
            claims.get("iat").and_then(Value::as_u64),
            Some(1_700_000_000)
        );
        assert_eq!(
            claims.get("exp").and_then(Value::as_u64),
            Some(1_700_000_000 + TOKEN_LIFETIME_SECS)
        );
        assert_eq!(
            claims.get("iss").and_then(Value::as_str),
            Some(did_key(&ed25519::public_key(&seed)).as_str())
        );
        assert_eq!(decode_segment(parts[2]).len(), 64);
        assert!(!jwt.contains('=') && !jwt.contains('+') && !jwt.contains('/'));
    }

    #[test]
    fn signature_covers_header_and_claims() {
        let seed = [1u8; 32];
        let jwt = build_jwt(&seed, "n", "wss://r", 10);
        let (input, sig) = jwt.rsplit_once('.').unwrap();
        assert_eq!(
            decode_segment(sig),
            ed25519::sign(&seed, input.as_bytes()).to_vec()
        );
    }
}
