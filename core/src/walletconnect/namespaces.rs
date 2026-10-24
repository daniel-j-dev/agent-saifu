//! Session proposals: what a dapp asks for, and what this wallet will grant.

use crate::crypto::hex;
use crate::eth::address::{Address, to_checksum};
use crate::eth::explorer::sanitize_text_max;
use crate::json::Value;

/// Methods this wallet can answer. `eth_sign` is deliberately absent: it signs an arbitrary
/// hash and cannot be reviewed, so a dapp that insists on it cannot be connected.
pub const SUPPORTED_METHODS: [&str; 5] = [
    "eth_sendTransaction",
    "eth_signTransaction",
    "personal_sign",
    "eth_signTypedData",
    "eth_signTypedData_v4",
];

/// Methods answered for Sui wallets (Reown's Sui RPC reference).
pub const SUI_SUPPORTED_METHODS: [&str; 4] = [
    "sui_getAccounts",
    "sui_signTransaction",
    "sui_signAndExecuteTransaction",
    "sui_signPersonalMessage",
];

const MAX_CHAINS: usize = 50;
const MAX_ITEMS: usize = 100;
const MAX_TEXT: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DappMetadata {
    pub name: String,
    pub description: String,
    pub url: String,
}

impl DappMetadata {
    pub fn from_json(meta: Option<&Value>) -> DappMetadata {
        let text = |key: &str| {
            meta.and_then(|m| m.get(key))
                .and_then(Value::as_str)
                .map(|s| sanitize_text_max(s, MAX_TEXT))
                .unwrap_or_default()
        };
        DappMetadata {
            name: text("name"),
            description: text("description"),
            url: text("url"),
        }
    }

    pub fn to_value(&self) -> Value {
        Value::object(vec![
            ("name", Value::str(&self.name)),
            ("url", Value::str(&self.url)),
            ("description", Value::str(&self.description)),
        ])
    }
}

/// What one side of a proposal (required or optional) asks for on EVM chains.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Requirements {
    pub chains: Vec<u64>,
    pub methods: Vec<String>,
    pub events: Vec<String>,
}

/// What one side of a proposal asks for on Sui, with chains like `sui:mainnet`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SuiRequirements {
    pub chains: Vec<String>,
    pub methods: Vec<String>,
    pub events: Vec<String>,
}

impl SuiRequirements {
    pub fn is_empty(&self) -> bool {
        self.chains.is_empty() && self.methods.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalData {
    pub proposer_public: [u8; 32],
    pub dapp: DappMetadata,
    pub required: Requirements,
    pub optional: Requirements,
    pub sui_required: SuiRequirements,
    pub sui_optional: SuiRequirements,
    /// Required namespaces other than `eip155` and `sui` (e.g. `solana`), which cannot be provided.
    pub unsupported_required: Vec<String>,
    pub expiry: Option<u64>,
}

pub fn chain_reference(chain_id: u64) -> String {
    format!("eip155:{chain_id}")
}

/// `eip155:<id>` -> id.
pub fn parse_chain_reference(text: &str) -> Result<u64, String> {
    let id = text
        .strip_prefix("eip155:")
        .ok_or_else(|| format!("'{}' is not an EVM chain", sanitize_text_max(text, 40)))?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("invalid chain '{}'", sanitize_text_max(text, 40)));
    }
    id.parse::<u64>()
        .map_err(|_| format!("invalid chain '{}'", sanitize_text_max(text, 40)))
}

/// `sui:<network>` -> the reference after the colon.
pub fn parse_sui_chain(text: &str) -> Result<String, String> {
    let id = text
        .strip_prefix("sui:")
        .ok_or_else(|| format!("'{}' is not a Sui chain", sanitize_text_max(text, 40)))?;
    if id.is_empty() || id.len() > 32 || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(format!("invalid chain '{}'", sanitize_text_max(text, 40)));
    }
    Ok(text.to_string())
}

fn push_unique<T: PartialEq>(list: &mut Vec<T>, item: T) {
    if !list.contains(&item) {
        list.push(item);
    }
}

fn string_list(value: Option<&Value>, what: &str) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(vec![]);
    };
    let items = value
        .as_array()
        .ok_or_else(|| format!("'{what}' must be a list"))?;
    if items.len() > MAX_ITEMS {
        return Err(format!("'{what}' has too many entries"));
    }
    items
        .iter()
        .map(|item| {
            item.as_str()
                .filter(|s| !s.is_empty() && s.len() <= 100)
                .map(str::to_string)
                .ok_or_else(|| format!("'{what}' must hold short strings"))
        })
        .collect()
}

/// Reads a namespace map; entries keyed `eip155` list their chains, entries keyed
/// `eip155:<id>` name one chain in the key itself.
fn parse_namespaces(map: Option<&Value>) -> Result<(Requirements, Vec<String>), String> {
    let mut out = Requirements::default();
    let mut other = vec![];
    let Some(map) = map else {
        return Ok((out, other));
    };
    let Value::Object(pairs) = map else {
        return Err("namespaces must be an object".to_string());
    };
    if pairs.len() > MAX_ITEMS {
        return Err("too many namespaces".to_string());
    }
    for (key, ns) in pairs {
        let namespace = key.split(':').next().unwrap_or("");
        if namespace != "eip155" {
            push_unique(&mut other, sanitize_text_max(namespace, 40));
            continue;
        }
        if key.contains(':') {
            push_unique(&mut out.chains, parse_chain_reference(key)?);
        }
        for chain in string_list(ns.get("chains"), "chains")? {
            push_unique(&mut out.chains, parse_chain_reference(&chain)?);
        }
        for method in string_list(ns.get("methods"), "methods")? {
            push_unique(&mut out.methods, method);
        }
        for event in string_list(ns.get("events"), "events")? {
            push_unique(&mut out.events, event);
        }
    }
    if out.chains.len() > MAX_CHAINS {
        return Err("the proposal names too many chains".to_string());
    }
    Ok((out, other))
}

/// Reads the `sui` entries of a namespace map; keys are `sui` or `sui:<network>`.
fn parse_sui_namespaces(map: Option<&Value>) -> Result<SuiRequirements, String> {
    let mut out = SuiRequirements::default();
    let Some(Value::Object(pairs)) = map else {
        return Ok(out);
    };
    for (key, ns) in pairs {
        if key.split(':').next() != Some("sui") {
            continue;
        }
        if key.contains(':') {
            push_unique(&mut out.chains, parse_sui_chain(key)?);
        }
        for chain in string_list(ns.get("chains"), "chains")? {
            push_unique(&mut out.chains, parse_sui_chain(&chain)?);
        }
        for method in string_list(ns.get("methods"), "methods")? {
            push_unique(&mut out.methods, method);
        }
        for event in string_list(ns.get("events"), "events")? {
            push_unique(&mut out.events, event);
        }
    }
    if out.chains.len() > MAX_CHAINS {
        return Err("the proposal names too many chains".to_string());
    }
    Ok(out)
}

pub fn parse_proposal(params: &Value) -> Result<ProposalData, String> {
    let proposer = params.get("proposer").ok_or("proposal has no proposer")?;
    let key_hex = proposer
        .get("publicKey")
        .and_then(Value::as_str)
        .ok_or("proposal has no proposer public key")?;
    let proposer_public = hex::decode_fixed::<32>(key_hex)
        .map_err(|_| "the proposer public key is invalid".to_string())?;
    let (required, mut unsupported_required) = parse_namespaces(params.get("requiredNamespaces"))?;
    unsupported_required.retain(|n| n != "sui");
    let (optional, _) = parse_namespaces(params.get("optionalNamespaces"))?;
    let sui_required = parse_sui_namespaces(params.get("requiredNamespaces"))?;
    let sui_optional = parse_sui_namespaces(params.get("optionalNamespaces"))?;
    Ok(ProposalData {
        proposer_public,
        dapp: DappMetadata::from_json(proposer.get("metadata")),
        required,
        optional,
        sui_required,
        sui_optional,
        unsupported_required,
        expiry: params.get("expiryTimestamp").and_then(Value::as_u64),
    })
}

/// What the wallet grants for a proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    pub chains: Vec<u64>,
    pub methods: Vec<String>,
    pub events: Vec<String>,
}

fn list_chains(chains: &[u64]) -> String {
    chains
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Checks the chosen chains against the proposal and works out methods and events to grant.
pub fn plan_approval(proposal: &ProposalData, chosen: &[u64]) -> Result<Approval, String> {
    if !proposal.unsupported_required.is_empty() {
        return Err(format!(
            "the dapp requires namespaces this wallet cannot provide ({}); reject the proposal",
            proposal.unsupported_required.join(", ")
        ));
    }
    if !proposal.sui_required.is_empty() {
        return Err(
            "the dapp requires Sui, which an Ethereum wallet cannot provide; approve with a Sui wallet or reject the proposal"
                .to_string(),
        );
    }
    let unsupported: Vec<&str> = proposal
        .required
        .methods
        .iter()
        .map(String::as_str)
        .filter(|m| !SUPPORTED_METHODS.contains(m))
        .collect();
    if !unsupported.is_empty() {
        let shown: Vec<String> = unsupported
            .iter()
            .map(|m| sanitize_text_max(m, 60))
            .collect();
        return Err(format!(
            "the dapp requires methods this wallet does not support ({}); reject the proposal",
            shown.join(", ")
        ));
    }
    if chosen.is_empty() {
        return Err("choose at least one chain".to_string());
    }
    let missing: Vec<u64> = proposal
        .required
        .chains
        .iter()
        .copied()
        .filter(|c| !chosen.contains(c))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "the dapp requires chain(s) {}; include them (chain_id and additional_chain_ids)",
            list_chains(&missing)
        ));
    }
    let not_asked: Vec<u64> = chosen
        .iter()
        .copied()
        .filter(|c| !proposal.required.chains.contains(c) && !proposal.optional.chains.contains(c))
        .collect();
    if !not_asked.is_empty() {
        return Err(format!(
            "the dapp did not ask for chain(s) {}; it asked for {}",
            list_chains(&not_asked),
            list_chains(&all_chains(proposal))
        ));
    }
    let mut methods: Vec<String> = vec![];
    for method in proposal
        .required
        .methods
        .iter()
        .chain(&proposal.optional.methods)
        .filter(|m| SUPPORTED_METHODS.contains(&m.as_str()))
    {
        push_unique(&mut methods, method.clone());
    }
    if methods.is_empty() {
        return Err("none of the methods the dapp asked for are supported".to_string());
    }
    let mut events: Vec<String> = vec![];
    for event in proposal
        .required
        .events
        .iter()
        .chain(&proposal.optional.events)
    {
        if event
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            push_unique(&mut events, event.clone());
        }
    }
    let mut chains = vec![];
    for chain in chosen {
        push_unique(&mut chains, *chain);
    }
    Ok(Approval {
        chains,
        methods,
        events,
    })
}

/// What the wallet grants a dapp on Sui: one chain, the wallet's own network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuiApproval {
    pub chain: String,
    pub methods: Vec<String>,
    pub events: Vec<String>,
}

/// Checks a proposal against a Sui wallet whose network is `chain` (e.g. `sui:mainnet`).
pub fn plan_sui_approval(proposal: &ProposalData, chain: &str) -> Result<SuiApproval, String> {
    if !proposal.unsupported_required.is_empty() {
        return Err(format!(
            "the dapp requires namespaces this wallet cannot provide ({}); reject the proposal",
            proposal.unsupported_required.join(", ")
        ));
    }
    if !proposal.required.chains.is_empty() || !proposal.required.methods.is_empty() {
        return Err(
            "the dapp requires Ethereum (eip155), which a Sui wallet cannot provide; approve with an Ethereum wallet or reject the proposal"
                .to_string(),
        );
    }
    let (req, opt) = (&proposal.sui_required, &proposal.sui_optional);
    let others: Vec<String> = req
        .chains
        .iter()
        .filter(|c| c.as_str() != chain)
        .map(|c| sanitize_text_max(c, 40))
        .collect();
    if !others.is_empty() {
        return Err(format!(
            "the dapp requires {} but this wallet is on {chain}; a Sui wallet is bound to one network",
            others.join(", ")
        ));
    }
    if !req.chains.iter().chain(&opt.chains).any(|c| c == chain) {
        let asked: Vec<String> = req
            .chains
            .iter()
            .chain(&opt.chains)
            .map(|c| sanitize_text_max(c, 40))
            .collect();
        return Err(format!(
            "the dapp did not ask for {chain}; it asked for {}",
            if asked.is_empty() {
                "no Sui chain".to_string()
            } else {
                asked.join(", ")
            }
        ));
    }
    let unsupported: Vec<String> = req
        .methods
        .iter()
        .filter(|m| !SUI_SUPPORTED_METHODS.contains(&m.as_str()))
        .map(|m| sanitize_text_max(m, 60))
        .collect();
    if !unsupported.is_empty() {
        return Err(format!(
            "the dapp requires methods this wallet does not support ({}); reject the proposal",
            unsupported.join(", ")
        ));
    }
    let mut methods: Vec<String> = vec![];
    for method in req.methods.iter().chain(&opt.methods) {
        if SUI_SUPPORTED_METHODS.contains(&method.as_str()) {
            push_unique(&mut methods, method.clone());
        }
    }
    // Reown requires sui_getAccounts for Sui connections.
    push_unique(&mut methods, "sui_getAccounts".to_string());
    let mut events: Vec<String> = vec![];
    for event in req.events.iter().chain(&opt.events) {
        if event
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            push_unique(&mut events, event.clone());
        }
    }
    Ok(SuiApproval {
        chain: chain.to_string(),
        methods,
        events,
    })
}

/// Base64 of the Sui public key with the Ed25519 flag byte in front, as `sui_getAccounts` returns.
pub fn sui_pubkey_text(public_key: &[u8; 32]) -> String {
    let mut flagged = vec![0u8];
    flagged.extend_from_slice(public_key);
    crate::crypto::base64::encode(&flagged)
}

/// The `sui_getAccounts` answer: `[{pubkey, address}]`.
pub fn sui_accounts_value(public_key: &[u8; 32], address: &str) -> Value {
    Value::Array(vec![Value::object(vec![
        ("pubkey", Value::String(sui_pubkey_text(public_key))),
        ("address", Value::str(address)),
    ])])
}

/// The `namespaces` object of `wc_sessionSettle` for a Sui wallet.
pub fn sui_settle_namespaces(approval: &SuiApproval, address: &str) -> Value {
    let strings = |items: Vec<String>| Value::Array(items.into_iter().map(Value::String).collect());
    Value::object(vec![(
        "sui",
        Value::object(vec![
            ("chains", strings(vec![approval.chain.clone()])),
            (
                "accounts",
                strings(vec![format!("{}:{address}", approval.chain)]),
            ),
            ("methods", strings(approval.methods.clone())),
            ("events", strings(approval.events.clone())),
        ]),
    )])
}

pub fn all_chains(proposal: &ProposalData) -> Vec<u64> {
    let mut chains = proposal.required.chains.clone();
    for c in &proposal.optional.chains {
        push_unique(&mut chains, *c);
    }
    chains
}

/// The `namespaces` object of `wc_sessionSettle`.
pub fn settle_namespaces(approval: &Approval, address: &Address) -> Value {
    let checksum = to_checksum(address);
    let strings = |items: Vec<String>| Value::Array(items.into_iter().map(Value::String).collect());
    let chains: Vec<String> = approval
        .chains
        .iter()
        .map(|c| chain_reference(*c))
        .collect();
    let accounts: Vec<String> = chains.iter().map(|c| format!("{c}:{checksum}")).collect();
    Value::object(vec![(
        "eip155",
        Value::object(vec![
            ("chains", strings(chains)),
            ("accounts", strings(accounts)),
            ("methods", strings(approval.methods.clone())),
            ("events", strings(approval.events.clone())),
        ]),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    const PUB: &str = "aa00000000000000000000000000000000000000000000000000000000000001";

    fn proposal(doc: &str) -> Result<ProposalData, String> {
        parse_proposal(&json::parse(doc).unwrap())
    }

    fn sample() -> ProposalData {
        proposal(&format!(
            r#"{{"proposer":{{"publicKey":"{PUB}","metadata":{{"name":"Swap\u0007App","description":"d","url":"https://swap.example","icons":[]}}}},
            "requiredNamespaces":{{"eip155":{{"chains":["eip155:1"],"methods":["eth_sendTransaction","personal_sign"],"events":["chainChanged","accountsChanged"]}}}},
            "optionalNamespaces":{{"eip155":{{"chains":["eip155:8453","eip155:10"],"methods":["eth_signTypedData_v4","eth_sign"],"events":[]}}}},
            "expiryTimestamp":1700000300}}"#
        ))
        .unwrap()
    }

    fn sui_proposal(required: &str, optional: &str) -> ProposalData {
        proposal(&format!(
            r#"{{"proposer":{{"publicKey":"{PUB}","metadata":{{"name":"D","description":"d","url":"https://d.example"}}}},
            "requiredNamespaces":{required},"optionalNamespaces":{optional}}}"#
        ))
        .unwrap()
    }

    const SUI_REQUIRED: &str = r#"{"sui":{"chains":["sui:mainnet"],"methods":["sui_signTransaction","sui_signPersonalMessage"],"events":[]}}"#;

    #[test]
    fn parses_sui_namespaces_and_does_not_call_them_unsupported() {
        let p = sui_proposal(
            SUI_REQUIRED,
            r#"{"sui:testnet":{"methods":["sui_signAndExecuteTransaction"],"events":["x"]}}"#,
        );
        assert!(p.unsupported_required.is_empty());
        assert_eq!(p.sui_required.chains, vec!["sui:mainnet"]);
        assert_eq!(p.sui_optional.chains, vec!["sui:testnet"]);
        assert_eq!(
            p.sui_optional.methods,
            vec!["sui_signAndExecuteTransaction"]
        );
        assert!(p.required.chains.is_empty());
        assert!(proposal(&format!(
            r#"{{"proposer":{{"publicKey":"{PUB}"}},"requiredNamespaces":{{"sui:main net":{{}}}}}}"#
        ))
        .is_err());
    }

    #[test]
    fn plans_a_sui_approval_on_the_wallets_network() {
        let p = sui_proposal(
            SUI_REQUIRED,
            r#"{"sui":{"chains":["sui:testnet"],"methods":["sui_signAndExecuteTransaction","sui_nope"],"events":["a b"]}}"#,
        );
        let a = plan_sui_approval(&p, "sui:mainnet").unwrap();
        assert_eq!(a.chain, "sui:mainnet");
        assert_eq!(
            a.methods,
            vec![
                "sui_signTransaction",
                "sui_signPersonalMessage",
                "sui_signAndExecuteTransaction",
                "sui_getAccounts"
            ]
        );
        assert!(a.events.is_empty());
        let ns = sui_settle_namespaces(&a, "0xabc");
        let sui = ns.get("sui").unwrap();
        assert_eq!(
            sui.get("accounts").unwrap().as_array().unwrap()[0].as_str(),
            Some("sui:mainnet:0xabc")
        );
    }

    #[test]
    fn refuses_mismatched_blockchains_networks_and_methods() {
        let p = sui_proposal(SUI_REQUIRED, "{}");
        let err = plan_sui_approval(&p, "sui:testnet").unwrap_err();
        assert!(err.contains("requires sui:mainnet"), "{err}");
        let err = plan_approval(&p, &[1]).unwrap_err();
        assert!(err.contains("requires Sui"), "{err}");
        let eth = sample();
        let err = plan_sui_approval(&eth, "sui:mainnet").unwrap_err();
        assert!(err.contains("requires Ethereum"), "{err}");
        let p = sui_proposal(
            r#"{"sui":{"chains":["sui:mainnet"],"methods":["sui_sign_everything"],"events":[]}}"#,
            "{}",
        );
        let err = plan_sui_approval(&p, "sui:mainnet").unwrap_err();
        assert!(err.contains("does not support"), "{err}");
        let p = sui_proposal("{}", r#"{"sui":{"chains":["sui:testnet"],"methods":[]}}"#);
        let err = plan_sui_approval(&p, "sui:mainnet").unwrap_err();
        assert!(err.contains("did not ask for sui:mainnet"), "{err}");
    }

    #[test]
    fn sui_accounts_carry_the_flagged_public_key() {
        let v = sui_accounts_value(&[7u8; 32], "0x1");
        let key = v.as_array().unwrap()[0]
            .get("pubkey")
            .unwrap()
            .as_str()
            .unwrap();
        let raw = crate::crypto::base64::decode(key).unwrap();
        assert_eq!(raw.len(), 33);
        assert_eq!(raw[0], 0);
        assert_eq!(&raw[1..], &[7u8; 32]);
    }

    #[test]
    fn parses_a_typical_proposal() {
        let p = sample();
        assert_eq!(hex::encode(&p.proposer_public), PUB);
        assert_eq!(p.dapp.name, "SwapApp");
        assert_eq!(p.dapp.url, "https://swap.example");
        assert_eq!(p.required.chains, vec![1]);
        assert_eq!(p.optional.chains, vec![8453, 10]);
        assert_eq!(
            p.required.methods,
            vec!["eth_sendTransaction", "personal_sign"]
        );
        assert_eq!(p.expiry, Some(1_700_000_300));
        assert!(p.unsupported_required.is_empty());
    }

    #[test]
    fn chain_keyed_namespaces_are_understood() {
        let p = proposal(&format!(
            r#"{{"proposer":{{"publicKey":"{PUB}"}},
            "requiredNamespaces":{{"eip155:137":{{"methods":["personal_sign"],"events":[]}},"eip155:1":{{"methods":["personal_sign"],"events":[]}}}}}}"#
        ))
        .unwrap();
        assert_eq!(p.required.chains, vec![137, 1]);
        assert_eq!(p.required.methods, vec!["personal_sign"]);
        assert_eq!(p.dapp, DappMetadata::default());
    }

    #[test]
    fn other_namespaces_are_flagged_when_required_and_ignored_when_optional() {
        let p = proposal(&format!(
            r#"{{"proposer":{{"publicKey":"{PUB}"}},
            "requiredNamespaces":{{"solana":{{"chains":["solana:5eykt"],"methods":["solana_signMessage"],"events":[]}}}},
            "optionalNamespaces":{{"cosmos":{{"chains":[],"methods":[],"events":[]}}}}}}"#
        ))
        .unwrap();
        assert_eq!(p.unsupported_required, vec!["solana"]);
        assert_eq!(p.optional, Requirements::default());
        assert!(plan_approval(&p, &[1]).unwrap_err().contains("solana"));
    }

    #[test]
    fn malformed_proposals_are_rejected() {
        for bad in [
            r#"{}"#,
            r#"{"proposer":{}}"#,
            r#"{"proposer":{"publicKey":"abcd"}}"#,
            &format!(r#"{{"proposer":{{"publicKey":"{PUB}"}},"requiredNamespaces":[]}}"#),
            &format!(
                r#"{{"proposer":{{"publicKey":"{PUB}"}},"requiredNamespaces":{{"eip155":{{"chains":["eip155:x"]}}}}}}"#
            ),
            &format!(
                r#"{{"proposer":{{"publicKey":"{PUB}"}},"requiredNamespaces":{{"eip155":{{"chains":["solana:1"]}}}}}}"#
            ),
            &format!(
                r#"{{"proposer":{{"publicKey":"{PUB}"}},"requiredNamespaces":{{"eip155":{{"methods":[1]}}}}}}"#
            ),
        ] {
            assert!(proposal(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn approval_grants_only_supported_methods() {
        let a = plan_approval(&sample(), &[1, 8453]).unwrap();
        assert_eq!(a.chains, vec![1, 8453]);
        assert_eq!(
            a.methods,
            vec![
                "eth_sendTransaction",
                "personal_sign",
                "eth_signTypedData_v4"
            ]
        );
        assert_eq!(a.events, vec!["chainChanged", "accountsChanged"]);
    }

    #[test]
    fn approval_must_cover_required_chains_and_nothing_unrequested() {
        let p = sample();
        assert!(
            plan_approval(&p, &[8453])
                .unwrap_err()
                .contains("requires chain(s) 1")
        );
        assert!(
            plan_approval(&p, &[1, 56])
                .unwrap_err()
                .contains("did not ask for chain(s) 56")
        );
        assert!(plan_approval(&p, &[]).is_err());
        assert_eq!(plan_approval(&p, &[1, 1]).unwrap().chains, vec![1]);
    }

    #[test]
    fn required_unsupported_methods_block_approval() {
        let mut p = sample();
        p.required.methods.push("eth_sign".into());
        let err = plan_approval(&p, &[1]).unwrap_err();
        assert!(err.contains("eth_sign") && err.contains("reject"), "{err}");
    }

    #[test]
    fn no_usable_methods_blocks_approval() {
        let mut p = sample();
        p.required.methods.clear();
        p.optional.methods = vec!["eth_sign".into()];
        assert!(
            plan_approval(&p, &[1])
                .unwrap_err()
                .contains("none of the methods")
        );
    }

    #[test]
    fn odd_event_names_are_dropped() {
        let mut p = sample();
        p.optional.events = vec!["bad event!".into(), "ok_event".into()];
        let a = plan_approval(&p, &[1]).unwrap();
        assert_eq!(
            a.events,
            vec!["chainChanged", "accountsChanged", "ok_event"]
        );
    }

    #[test]
    fn settle_namespaces_list_one_account_per_chain() {
        let a = plan_approval(&sample(), &[1, 10]).unwrap();
        let address = [0xabu8; 20];
        let ns = settle_namespaces(&a, &address);
        let eip = ns.get("eip155").unwrap();
        let checksum = to_checksum(&address);
        let accounts = eip.get("accounts").unwrap().as_array().unwrap();
        assert_eq!(
            accounts[0].as_str(),
            Some(format!("eip155:1:{checksum}").as_str())
        );
        assert_eq!(
            accounts[1].as_str(),
            Some(format!("eip155:10:{checksum}").as_str())
        );
        assert_eq!(eip.get("chains").unwrap().as_array().unwrap().len(), 2);
    }

    #[test]
    fn chain_references_round_trip() {
        assert_eq!(chain_reference(137), "eip155:137");
        assert_eq!(parse_chain_reference("eip155:137"), Ok(137));
        for bad in [
            "eip155:",
            "eip155:-1",
            "eip155:abc",
            "solana:1",
            "137",
            "eip155:99999999999999999999999",
        ] {
            assert!(parse_chain_reference(bad).is_err(), "{bad}");
        }
    }
}
