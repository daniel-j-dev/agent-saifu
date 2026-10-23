//! Pairing URIs (EIP-1328): `wc:<topic>@2?symKey=<hex>&relay-protocol=irn&expiryTimestamp=<secs>`.

use crate::crypto::hex;

pub const MAX_URI_LEN: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingUri {
    /// 64 lowercase hex characters.
    pub topic: String,
    pub sym_key: [u8; 32],
    pub expiry: Option<u64>,
}

/// Decodes `%XX` escapes (query values are only ever hex or short words, so this is enough).
fn percent_decode(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let pair = s.get(i + 1..i + 3).ok_or("invalid percent escape in URI")?;
            let byte = u8::from_str_radix(pair, 16).map_err(|_| "invalid percent escape in URI")?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "URI is not valid UTF-8".to_string())
}

fn query_value(query: &str, key: &str) -> Result<Option<String>, String> {
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if percent_decode(k)? == key {
            return percent_decode(v).map(Some);
        }
    }
    Ok(None)
}

pub fn parse(input: &str, now: u64) -> Result<PairingUri, String> {
    let uri = input.trim();
    if uri.len() > MAX_URI_LEN {
        return Err("the WalletConnect URI is too long".to_string());
    }
    let rest = uri
        .strip_prefix("wc:")
        .ok_or("a WalletConnect URI starts with 'wc:'")?;
    let (head, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (topic, version) = head.split_once('@').ok_or("the URI has no version")?;
    if version.contains('/') {
        return Err(
            "this is a session link, not a pairing URI; ask the dapp for a new connection URI"
                .to_string(),
        );
    }
    if version != "2" {
        return Err(format!(
            "unsupported WalletConnect version '{version}'; only version 2 is supported"
        ));
    }
    if topic.len() != 64 || !topic.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("the URI topic must be 64 hex characters".to_string());
    }
    let relay = query_value(query, "relay-protocol")?.unwrap_or_default();
    if relay != "irn" {
        return Err(format!("unsupported relay protocol '{relay}'"));
    }
    let sym_key = query_value(query, "symKey")?.ok_or("the URI has no symKey")?;
    let sym_key = hex::decode_fixed::<32>(&sym_key)
        .map_err(|_| "symKey must be 64 hex characters".to_string())?;
    let expiry = match query_value(query, "expiryTimestamp")? {
        Some(text) => Some(
            text.parse::<u64>()
                .map_err(|_| "expiryTimestamp is not a number".to_string())?,
        ),
        None => None,
    };
    if let Some(expiry) = expiry
        && expiry <= now
    {
        return Err("this connection URI has expired; ask the dapp for a new one".to_string());
    }
    Ok(PairingUri {
        topic: topic.to_ascii_lowercase(),
        sym_key,
        expiry,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOPIC: &str = "7f6e504bfad60b485450578e05678ed3e8e8c4751d3c6160be17160d63ec90f9";
    const KEY: &str = "587d5484ce2a2a6ee3ba1962fdd7e8588e06200c46823bd18fbd67def96ad303";

    fn uri(query: &str) -> String {
        format!("wc:{TOPIC}@2?{query}")
    }

    #[test]
    fn parses_the_documented_example() {
        let u = parse(
            &uri(&format!(
                "symKey={KEY}&methods=[wc_sessionPropose],[wc_authRequest,wc_authBatchRequest]&relay-protocol=irn&expiryTimestamp=1705667684"
            )),
            1_705_667_000,
        )
        .unwrap();
        assert_eq!(u.topic, TOPIC);
        assert_eq!(hex::encode(&u.sym_key), KEY);
        assert_eq!(u.expiry, Some(1_705_667_684));
    }

    #[test]
    fn expiry_is_optional_and_enforced() {
        let q = format!("relay-protocol=irn&symKey={KEY}");
        assert_eq!(parse(&uri(&q), 5).unwrap().expiry, None);
        let q = format!("relay-protocol=irn&symKey={KEY}&expiryTimestamp=100");
        assert!(parse(&uri(&q), 100).unwrap_err().contains("expired"));
        assert!(parse(&uri(&q), 99).is_ok());
    }

    #[test]
    fn percent_escapes_and_whitespace_are_handled() {
        let q = format!("relay-protocol=ir%6E&symKey={KEY}");
        assert!(parse(&format!("  {}\n", uri(&q)), 0).is_ok());
        assert!(parse(&uri(&format!("relay-protocol=irn&symKey=%zz{KEY}")), 0).is_err());
        assert!(
            parse(
                &uri(&format!("relay-protocol=irn&symKey={}%", &KEY[..62])),
                0
            )
            .is_err()
        );
    }

    #[test]
    fn upper_case_topic_is_normalised() {
        let u = parse(
            &format!(
                "wc:{}@2?relay-protocol=irn&symKey={KEY}",
                TOPIC.to_uppercase()
            ),
            0,
        )
        .unwrap();
        assert_eq!(u.topic, TOPIC);
    }

    #[test]
    fn rejects_bad_uris() {
        let good = format!("relay-protocol=irn&symKey={KEY}");
        assert!(parse("https://example.com", 0).is_err());
        assert!(parse(&format!("wc:{TOPIC}?{good}"), 0).is_err());
        assert!(
            parse(&format!("wc:{TOPIC}@1?{good}"), 0)
                .unwrap_err()
                .contains("version")
        );
        assert!(
            parse(&format!("wc:{TOPIC}@2/wc?requestId=1&sessionTopic=x"), 0)
                .unwrap_err()
                .contains("session link")
        );
        assert!(parse(&format!("wc:abcd@2?{good}"), 0).is_err());
        assert!(parse(&format!("wc:{}zz@2?{good}", &TOPIC[..62]), 0).is_err());
        assert!(parse(&uri(&format!("relay-protocol=waku&symKey={KEY}")), 0).is_err());
        assert!(parse(&uri(&format!("symKey={KEY}")), 0).is_err());
        assert!(
            parse(&uri("relay-protocol=irn"), 0)
                .unwrap_err()
                .contains("symKey")
        );
        assert!(parse(&uri("relay-protocol=irn&symKey=abcd"), 0).is_err());
        assert!(
            parse(
                &uri(&format!(
                    "relay-protocol=irn&symKey={KEY}&expiryTimestamp=soon"
                )),
                0
            )
            .is_err()
        );
        assert!(
            parse(&"wc:".repeat(2000), 0)
                .unwrap_err()
                .contains("too long")
        );
    }
}
