//! Typed views of the wallet's JSON answers. Everything here is pure so it can be unit tested.

use agent_saifu_core::blockchain::Blockchain;
use agent_saifu_core::json::Value;

pub fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn opt_text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn number(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

pub fn strings(value: &Value, key: &str) -> Vec<String> {
    list(value, key)
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct WalletInfo {
    pub name: String,
    pub chain: Blockchain,
}

pub fn parse_wallets(value: &Value) -> Vec<WalletInfo> {
    list(value, "wallets")
        .iter()
        .filter_map(|w| {
            Some(WalletInfo {
                name: text(w, "name"),
                chain: Blockchain::parse(&text(w, "blockchain")).ok()?,
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub symbol: String,
    pub name: String,
    /// `native`, or the token contract / mint / coin type.
    pub asset: String,
    pub balance: String,
    pub usd: Option<f64>,
    pub chain_id: Option<u64>,
    pub network: String,
}

impl Asset {
    pub fn is_native(&self) -> bool {
        self.asset == "native"
    }
}

/// What the wallet showed after its last unlock; held in memory only until the user hides it.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub wallet: String,
    pub address: String,
    pub assets: Vec<Asset>,
    /// Set when the wallet unlocked but the network could not be read.
    pub error: Option<String>,
}

impl Snapshot {
    pub fn total_usd(&self) -> f64 {
        self.assets.iter().filter_map(|a| a.usd).sum()
    }

    /// The held asset with this id on this chain (`None` chain for Sui, Solana and Bitcoin).
    /// Contract addresses compare without regard to letter case.
    pub fn find(&self, chain_id: Option<u64>, asset: &str) -> Option<&Asset> {
        let wanted = if asset.is_empty() { "native" } else { asset };
        self.assets
            .iter()
            .find(|a| a.chain_id == chain_id && a.asset.eq_ignore_ascii_case(wanted))
    }

    /// Distinct EVM networks in the snapshot as `(chain_id, name)`.
    pub fn networks(&self) -> Vec<(u64, String)> {
        let mut out: Vec<(u64, String)> = vec![];
        for a in &self.assets {
            if let Some(id) = a.chain_id
                && !out.iter().any(|(c, _)| *c == id)
            {
                out.push((id, a.network.clone()));
            }
        }
        out
    }
}

fn parse_asset(value: &Value, chain_id: Option<u64>, network: &str) -> Asset {
    let symbol = text(value, "symbol");
    Asset {
        name: opt_text(value, "name").unwrap_or_else(|| symbol.clone()),
        symbol,
        asset: text(value, "asset"),
        balance: text(value, "balance"),
        usd: value
            .get("usdc_value")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok()),
        chain_id: chain_id.or_else(|| number(value, "chain_id")),
        network: network.to_string(),
    }
}

pub fn parse_balances(wallet: &str, value: &Value) -> Snapshot {
    let mut assets = vec![];
    for net in list(value, "networks") {
        let id = number(net, "chain_id");
        let name = text(net, "network");
        assets.extend(
            list(net, "assets")
                .iter()
                .map(|a| parse_asset(a, id, &name)),
        );
    }
    let top_network = value
        .get("network")
        .and_then(|n| n.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assets.extend(
        list(value, "assets")
            .iter()
            .map(|a| parse_asset(a, None, &top_network)),
    );
    Snapshot {
        wallet: wallet.to_string(),
        address: text(value, "address"),
        assets,
        error: opt_text(value, "refresh_error"),
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Dapp {
    pub name: String,
    pub url: String,
    pub description: String,
}

pub fn parse_dapp(value: Option<&Value>) -> Dapp {
    let Some(v) = value else {
        return Dapp::default();
    };
    Dapp {
        name: text(v, "name"),
        url: text(v, "url"),
        description: text(v, "description"),
    }
}

impl Dapp {
    pub fn title(&self) -> String {
        if self.name.trim().is_empty() {
            "Unknown app".to_string()
        } else {
            self.name.clone()
        }
    }
}

/// A request a connected app is waiting on, as listed before any wallet is unlocked.
#[derive(Clone, Debug, PartialEq)]
pub struct WcRequest {
    pub id: String,
    pub session_topic: String,
    pub dapp: Dapp,
    pub method: String,
    pub kind: String,
    pub chain: String,
    /// EVM chain ID; `None` for Sui and Solana requests.
    pub chain_id: Option<u64>,
    pub expires_in: u64,
    pub details: Value,
}

pub fn parse_requests(value: &Value) -> Vec<WcRequest> {
    list(value, "requests")
        .iter()
        .map(|r| {
            let chain_id = number(r, "chain_id");
            WcRequest {
                id: text(r, "request_id"),
                session_topic: text(r, "session_topic"),
                dapp: parse_dapp(r.get("dapp")),
                method: text(r, "method"),
                kind: text(r, "kind"),
                chain: opt_text(r, "chain")
                    .or_else(|| chain_id.map(crate::fmt::chain_label))
                    .unwrap_or_default(),
                chain_id,
                expires_in: number(r, "expires_in_seconds").unwrap_or(0),
                details: r.get("details").cloned().unwrap_or(Value::Null),
            }
        })
        .collect()
}

const ETH_CHAINS: [u64; 9] = [1, 10, 324, 8453, 42161, 59144, 81457, 534352, 7777777];

impl WcRequest {
    /// Short headline for the popup and the activity list.
    pub fn title(&self) -> String {
        match self.kind.as_str() {
            "transaction" => {
                let action = self
                    .details
                    .get("decoded")
                    .map(|d| text(d, "action"))
                    .unwrap_or_default();
                match action.as_str() {
                    "erc20_transfer" => "Token transfer".to_string(),
                    "erc20_approve" => "Token approval".to_string(),
                    "erc20_revoke_approval" => "Revoke approval".to_string(),
                    _ if self.native_amount().is_some() => "Send request".to_string(),
                    _ => "Transaction request".to_string(),
                }
            }
            "message" => "Sign message".to_string(),
            "typed_data" => "Sign typed data".to_string(),
            "accounts" => "Share account".to_string(),
            _ => "Request".to_string(),
        }
    }

    /// Non-zero native amount of an EVM transaction, e.g. `0.05 ETH`.
    pub fn native_amount(&self) -> Option<String> {
        let v = text(&self.details, "value_native");
        let zero = v.is_empty() || v.bytes().all(|b| b == b'0' || b == b'.');
        if zero {
            return None;
        }
        let unit = match self.chain_id {
            Some(id) if ETH_CHAINS.contains(&id) => "ETH",
            _ => "native coin",
        };
        Some(format!("{} {unit}", crate::fmt::trim_amount(&v, 8)))
    }

    pub fn is_transaction(&self) -> bool {
        self.kind == "transaction"
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WcSession {
    pub topic: String,
    pub wallet: String,
    pub dapp: Dapp,
    pub chains: Vec<String>,
    pub methods: Vec<String>,
    pub confirmed: bool,
}

pub fn parse_sessions(value: &Value) -> Vec<WcSession> {
    list(value, "sessions")
        .iter()
        .map(|s| {
            let mut chains = strings(s, "chains");
            chains.extend(
                list(s, "chain_ids")
                    .iter()
                    .filter_map(Value::as_u64)
                    .map(crate::fmt::chain_label),
            );
            WcSession {
                topic: text(s, "session_topic"),
                wallet: text(s, "wallet"),
                dapp: parse_dapp(s.get("dapp")),
                chains,
                methods: strings(s, "methods"),
                confirmed: s
                    .get("confirmed_by_dapp")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct WcProposal {
    pub id: String,
    pub dapp: Dapp,
    pub can_connect: bool,
    pub can_connect_with: Vec<String>,
    pub warnings: Vec<String>,
    /// EVM chains the dapp requires, then the ones it merely allows.
    pub required_chains: Vec<u64>,
    pub optional_chains: Vec<u64>,
    pub expires_in: u64,
}

pub fn parse_proposals(value: &Value) -> Vec<WcProposal> {
    list(value, "proposals")
        .iter()
        .map(parse_proposal)
        .collect()
}

pub fn parse_proposal(p: &Value) -> WcProposal {
    let chains = |key: &str| -> Vec<u64> {
        p.get(key)
            .map(|r| {
                list(r, "chain_ids")
                    .iter()
                    .filter_map(Value::as_u64)
                    .collect()
            })
            .unwrap_or_default()
    };
    WcProposal {
        id: text(p, "proposal_id"),
        dapp: parse_dapp(p.get("dapp")),
        can_connect: p
            .get("can_connect")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        can_connect_with: strings(p, "can_connect_with"),
        warnings: strings(p, "warnings"),
        required_chains: chains("required"),
        optional_chains: chains("optional"),
        expires_in: number(p, "expires_in_seconds").unwrap_or(0),
    }
}

impl WcProposal {
    /// Whether a wallet of this blockchain can satisfy the proposal.
    pub fn works_with(&self, chain: Blockchain) -> bool {
        self.can_connect_with
            .iter()
            .any(|c| c.starts_with(chain.display_name()))
    }

    pub fn all_chains(&self) -> Vec<u64> {
        let mut all = self.required_chains.clone();
        for c in &self.optional_chains {
            if !all.contains(c) {
                all.push(*c);
            }
        }
        all
    }
}

/// `Some(true)` when a simulation or dry run passed, `Some(false)` when it failed.
pub fn verdict(review: &Value) -> Option<bool> {
    review
        .get("simulation_succeeded")
        .or_else(|| review.get("success"))
        .and_then(Value::as_bool)
}

/// Human-readable lines describing what a reviewed request or simulated transaction does.
pub fn outcome_lines(review: &Value) -> Vec<String> {
    let mut lines = strings(review, "expected_outcome");
    if let Some(d) = review.get("description") {
        match d {
            Value::String(s) => lines.push(s.clone()),
            Value::Array(items) => {
                lines.extend(items.iter().filter_map(|i| i.as_str().map(str::to_string)))
            }
            _ => {}
        }
    }
    lines
}

/// Every warning from a review, de-duplicated, whichever level of the answer carries it.
pub fn warnings(review: &Value) -> Vec<String> {
    let mut out = strings(review, "warnings");
    if let Some(sim) = review.get("simulation") {
        out.extend(strings(sim, "warnings"));
    }
    for t in list(review, "transactions") {
        out.extend(strings(t, "warnings"));
    }
    let mut seen: Vec<String> = vec![];
    out.retain(|w| {
        let fresh = !seen.contains(w);
        seen.push(w.clone());
        fresh
    });
    out
}

/// The reason a simulation failed, if it did.
pub fn failure_reason(review: &Value) -> Option<String> {
    let sim = review.get("simulation").unwrap_or(review);
    opt_text(sim, "revert_reason")
        .or_else(|| opt_text(sim, "error"))
        .filter(|s| !s.is_empty())
}

/// The network fee of a review, simulation or estimate, whichever blockchain it is for.
pub fn fee_line(review: &Value) -> Option<String> {
    let sim = review.get("simulation").unwrap_or(review);
    if let Some(fee) = opt_text(sim, "max_fee").or_else(|| opt_text(sim, "expected_fee")) {
        let currency = opt_text(sim, "currency").unwrap_or_default();
        let usd = opt_text(sim, "expected_fee_usdc")
            .filter(|u| u != "0.00")
            .map(|u| format!(" (about ${u})"))
            .unwrap_or_default();
        return Some(format!(
            "{} {currency}{usd}",
            crate::fmt::trim_amount(&fee, 10)
        ));
    }
    let gas = review
        .get("result")
        .and_then(|r| r.get("gas"))
        .and_then(|g| opt_text(g, "net_sui"));
    let named = [("network_fee_sol", "SOL"), ("network_fee_btc", "BTC")]
        .iter()
        .find_map(|(key, unit)| opt_text(review, key).map(|fee| (fee, *unit)));
    match (gas, named) {
        (Some(fee), _) => Some(format!("{} SUI", crate::fmt::trim_amount(&fee, 10))),
        (None, Some((fee, unit))) => Some(format!("{} {unit}", crate::fmt::trim_amount(&fee, 10))),
        _ => None,
    }
}

/// How the wallet's balances would change (Sui and Solana dry runs), e.g. `-2.1 SUI`.
pub fn balance_changes(review: &Value) -> Vec<String> {
    let changes = [Some(review), review.get("result"), review.get("simulation")]
        .into_iter()
        .flatten()
        .find_map(|v| v.get("wallet_changes").and_then(Value::as_array));
    changes
        .map(|list| {
            list.iter()
                .map(|c| {
                    let amount = opt_text(c, "change").unwrap_or_default();
                    let symbol = opt_text(c, "symbol").unwrap_or_default();
                    format!(
                        "Your {symbol} balance changes by {}",
                        crate::fmt::trim_amount(&amount, 9)
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_saifu_core::json::parse;

    fn json(s: &str) -> Value {
        parse(s).unwrap()
    }

    #[test]
    fn parses_wallet_list_and_skips_unknown_chains() {
        let v = json(
            r#"{"wallets":[{"name":"A","blockchain":"Ethereum"},{"name":"B","blockchain":"Bitcoin"},{"name":"C","blockchain":"Dogecoin"}]}"#,
        );
        let w = parse_wallets(&v);
        assert_eq!(w.len(), 2);
        assert_eq!(w[1].chain, Blockchain::Bitcoin);
    }

    #[test]
    fn parses_ethereum_balances_across_networks() {
        let v = json(
            r#"{"address":"0xabc","networks":[{"chain_id":1,"network":"Ethereum Mainnet","assets":[
            {"symbol":"ETH","asset":"native","balance":"1.5","usdc_value":"3000.10","chain_id":1},
            {"symbol":"USDC","asset":"0xA0","balance":"10","usdc_value":null,"chain_id":1}]},
            {"chain_id":8453,"network":"Base","assets":[{"symbol":"ETH","asset":"native","balance":"0","usdc_value":"0.00"}]}]}"#,
        );
        let s = parse_balances("W", &v);
        assert_eq!(s.address, "0xabc");
        assert_eq!(s.assets.len(), 3);
        assert!((s.total_usd() - 3000.10).abs() < 1e-9);
        assert_eq!(s.assets[2].chain_id, Some(8453));
        assert_eq!(
            s.networks(),
            vec![(1, "Ethereum Mainnet".into()), (8453, "Base".into())]
        );
        assert!(s.assets[0].is_native());
        assert_eq!(s.assets[1].usd, None);
    }

    #[test]
    fn finds_a_held_asset_by_chain_and_id() {
        let v = json(
            r#"{"address":"0xabc","networks":[{"chain_id":1,"network":"Ethereum Mainnet","assets":[
            {"symbol":"ETH","asset":"native","balance":"1.5","chain_id":1},
            {"symbol":"USDC","asset":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","balance":"10","chain_id":1}]},
            {"chain_id":8453,"network":"Base","assets":[{"symbol":"ETH","asset":"native","balance":"7"}]}]}"#,
        );
        let s = parse_balances("W", &v);
        assert_eq!(s.find(Some(1), "native").unwrap().balance, "1.5");
        assert_eq!(s.find(Some(8453), "native").unwrap().balance, "7");
        assert_eq!(s.find(Some(1), "").unwrap().balance, "1.5");
        let usdc = s.find(Some(1), "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
        assert_eq!(usdc.unwrap().symbol, "USDC");
        assert!(s.find(Some(10), "native").is_none());
        assert!(s.find(None, "native").is_none());
    }

    #[test]
    fn parses_single_network_balances() {
        let v = json(
            r#"{"address":"So1","network":{"name":"mainnet"},"assets":[{"symbol":"SOL","asset":"native","name":"Solana","balance":"2","usdc_value":"300.00"}]}"#,
        );
        let s = parse_balances("S", &v);
        assert_eq!(s.assets[0].name, "Solana");
        assert_eq!(s.assets[0].network, "mainnet");
        assert_eq!(s.assets[0].chain_id, None);
    }

    #[test]
    fn titles_requests_by_what_they_ask() {
        let v = json(
            r#"{"requests":[
            {"request_id":"7","session_topic":"t","dapp":{"name":"Uniswap","url":"https://u","description":""},"method":"eth_sendTransaction","kind":"transaction","chain_id":1,"expires_in_seconds":120,"details":{"to":"0x1","value_native":"0.05","data_bytes":0}},
            {"request_id":"8","session_topic":"t","dapp":{"name":"","url":"","description":""},"method":"personal_sign","kind":"message","chain_id":137,"expires_in_seconds":10,"details":{"text":"hi"}},
            {"request_id":"9","session_topic":"t","method":"sui_getAccounts","kind":"accounts","chain":"sui:mainnet","expires_in_seconds":10,"details":{}}]}"#,
        );
        let r = parse_requests(&v);
        assert_eq!(r[0].title(), "Send request");
        assert_eq!(r[0].native_amount().as_deref(), Some("0.05 ETH"));
        assert!(r[0].is_transaction());
        assert_eq!(r[1].title(), "Sign message");
        assert_eq!(r[1].dapp.title(), "Unknown app");
        assert_eq!(r[1].chain, "Polygon");
        assert_eq!(r[2].title(), "Share account");
        assert_eq!(r[2].chain, "sui:mainnet");
        assert_eq!(r[2].chain_id, None);
    }

    #[test]
    fn recognises_token_actions_and_zero_values() {
        let v = json(
            r#"{"requests":[{"request_id":"1","session_topic":"t","method":"eth_sendTransaction","kind":"transaction","chain_id":1,"expires_in_seconds":1,"details":{"value_native":"0","decoded":{"action":"erc20_approve"}}}]}"#,
        );
        let r = &parse_requests(&v)[0];
        assert_eq!(r.title(), "Token approval");
        assert_eq!(r.native_amount(), None);
    }

    #[test]
    fn proposals_report_compatible_wallets() {
        let v = json(
            r#"{"proposals":[{"proposal_id":"3","dapp":{"name":"Aave","url":"u","description":"d"},
            "required":{"chain_ids":[1]},"optional":{"chain_ids":[1,10]},
            "can_connect":true,"can_connect_with":["Ethereum","Solana (mainnet)"],"warnings":["not verified"],"expires_in_seconds":300}]}"#,
        );
        let p = &parse_proposals(&v)[0];
        assert!(p.works_with(Blockchain::Ethereum));
        assert!(p.works_with(Blockchain::Solana));
        assert!(!p.works_with(Blockchain::Sui));
        assert_eq!(p.all_chains(), vec![1, 10]);
        assert_eq!(p.warnings, vec!["not verified".to_string()]);
    }

    #[test]
    fn sessions_list_chains_of_every_kind() {
        let v = json(
            r#"{"sessions":[{"session_topic":"abc","wallet":"W","dapp":{"name":"X","url":"","description":""},"chain_ids":[1,10],"methods":["personal_sign"],"confirmed_by_dapp":true},
            {"session_topic":"def","wallet":"S","dapp":{"name":"Y","url":"","description":""},"chains":["solana:abc"],"methods":[],"confirmed_by_dapp":false}]}"#,
        );
        let s = parse_sessions(&v);
        assert_eq!(s[0].chains, vec!["Ethereum", "Optimism"]);
        assert!(s[0].confirmed);
        assert_eq!(s[1].chains, vec!["solana:abc"]);
        assert!(!s[1].confirmed);
    }

    #[test]
    fn reads_review_verdicts_lines_and_fees() {
        let v = json(
            r#"{"simulation_succeeded":false,"expected_outcome":["Sends 1 ETH"],"warnings":["w1"],
            "simulation":{"success":false,"error":"insufficient funds","warnings":["w1","w2"],"max_fee":"0.0000022178","currency":"ETH","expected_fee_usdc":"0.00"}}"#,
        );
        assert_eq!(verdict(&v), Some(false));
        assert_eq!(outcome_lines(&v), vec!["Sends 1 ETH"]);
        assert_eq!(warnings(&v), vec!["w1", "w2"]);
        assert_eq!(failure_reason(&v).as_deref(), Some("insufficient funds"));
        assert_eq!(fee_line(&v).as_deref(), Some("0.0000022178 ETH"));
    }

    #[test]
    fn simulate_answers_use_success_key() {
        let v = json(
            r#"{"success":true,"currency":"ETH","max_fee":"0.001","expected_fee_usdc":"2.10"}"#,
        );
        assert_eq!(verdict(&v), Some(true));
        assert_eq!(fee_line(&v).as_deref(), Some("0.001 ETH (about $2.10)"));
        assert_eq!(failure_reason(&v), None);
        assert_eq!(verdict(&json("{}")), None);
    }

    #[test]
    fn reads_fees_and_changes_of_sui_solana_and_bitcoin_answers() {
        let sui = json(
            r#"{"success":true,"result":{"status":"success","gas":{"net_sui":"0.001988099"},
            "wallet_changes":[{"symbol":"SUI","asset":"native","change":"-2.101988099"}]}}"#,
        );
        assert_eq!(fee_line(&sui).as_deref(), Some("0.001988099 SUI"));
        assert_eq!(
            balance_changes(&sui),
            vec!["Your SUI balance changes by -2.101988099"]
        );
        let sol = json(r#"{"success":true,"network_fee_sol":"0.000005"}"#);
        assert_eq!(fee_line(&sol).as_deref(), Some("0.000005 SOL"));
        let btc = json(r#"{"network_fee_btc":"0.00001410"}"#);
        assert_eq!(fee_line(&btc).as_deref(), Some("0.0000141 BTC"));
        assert!(balance_changes(&btc).is_empty());
        assert_eq!(fee_line(&json("{}")), None);
    }

    #[test]
    fn sui_description_may_be_text() {
        let v = json(r#"{"description":"Transfers 1 SUI"}"#);
        assert_eq!(outcome_lines(&v), vec!["Transfers 1 SUI"]);
    }
}
