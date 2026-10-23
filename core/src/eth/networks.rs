//! Network definitions (built-in Ethereum mainnet plus user-added layer 2 / EVM networks).

use crate::json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub name: String,
    pub chain_id: u64,
    pub rpc_url: String,
    pub currency_symbol: String,
    pub block_explorer_url: String,
}

pub const MAINNET_CHAIN_ID: u64 = 1;

pub fn ethereum_mainnet() -> Network {
    Network {
        name: "Ethereum Mainnet".to_string(),
        chain_id: MAINNET_CHAIN_ID,
        rpc_url: "https://ethereum-rpc.publicnode.com".to_string(),
        currency_symbol: "ETH".to_string(),
        block_explorer_url: "https://eth.blockscout.com".to_string(),
    }
}

pub fn is_builtin(chain_id: u64) -> bool {
    chain_id == MAINNET_CHAIN_ID
}

fn has_control_or_space(s: &str) -> bool {
    s.chars().any(|c| c.is_control() || c.is_whitespace())
}

pub fn validate_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err("network name must be 1-64 characters without control characters".to_string());
    }
    Ok(name.to_string())
}

pub fn validate_symbol(symbol: &str) -> Result<String, String> {
    let symbol = symbol.trim();
    if symbol.is_empty()
        || symbol.chars().count() > 16
        || !symbol.chars().all(|c| c.is_alphanumeric())
    {
        return Err("currency symbol must be 1-16 letters or digits".to_string());
    }
    Ok(symbol.to_string())
}

pub fn validate_chain_id(chain_id: u64) -> Result<u64, String> {
    if chain_id == 0 || chain_id > (u64::MAX - 35) / 2 {
        return Err("chain ID is out of range".to_string());
    }
    Ok(chain_id)
}

fn split_url(url: &str) -> Result<(&str, &str), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("URL must start with https://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    Ok((scheme, authority))
}

fn host_of(authority: &str) -> &str {
    if let Some(stripped) = authority.strip_prefix('[') {
        return stripped.split(']').next().unwrap_or("");
    }
    authority.split(':').next().unwrap_or("")
}

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "::1" || host.starts_with("127.") && host.split('.').count() == 4
}

/// Literal private / link-local IPv4 hosts are refused (SSRF protection); loopback is allowed.
fn is_private_ipv4(host: &str) -> bool {
    let parts: Vec<u8> = host.split('.').filter_map(|p| p.parse().ok()).collect();
    if parts.len() != 4 || host.split('.').count() != 4 {
        return false;
    }
    matches!(
        (parts[0], parts[1]),
        (10, _) | (172, 16..=31) | (192, 168) | (169, 254) | (0, _)
    )
}

/// Accepts `https://` URLs, and `http://` only for loopback hosts.
pub fn validate_url(url: &str, what: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() || url.len() > 2048 || has_control_or_space(url) {
        return Err(format!(
            "{what} must be a URL without spaces (max 2048 characters)"
        ));
    }
    let (scheme, authority) = split_url(url).map_err(|e| format!("{what}: {e}"))?;
    if authority.contains('@') {
        return Err(format!("{what} must not contain credentials"));
    }
    let host = host_of(authority);
    if host.is_empty() {
        return Err(format!("{what} has no host"));
    }
    match scheme {
        "https" => {}
        "http" if is_loopback(host) => {}
        "http" => {
            return Err(format!(
                "{what} must use https (http is only allowed for localhost)"
            ));
        }
        _ => return Err(format!("{what} must start with https://")),
    }
    if is_private_ipv4(host) {
        return Err(format!(
            "{what} must not point to a private network address"
        ));
    }
    Ok(url.to_string())
}

impl Network {
    pub fn new(
        name: &str,
        chain_id: u64,
        rpc_url: &str,
        currency_symbol: &str,
        block_explorer_url: &str,
    ) -> Result<Network, String> {
        Ok(Network {
            name: validate_name(name)?,
            chain_id: validate_chain_id(chain_id)?,
            rpc_url: validate_url(rpc_url, "RPC URL")?,
            currency_symbol: validate_symbol(currency_symbol)?,
            block_explorer_url: validate_url(block_explorer_url, "block explorer URL")?
                .trim_end_matches('/')
                .to_string(),
        })
    }

    pub fn to_json(&self) -> Value {
        Value::object(vec![
            ("name", Value::str(&self.name)),
            ("chainId", Value::Number(self.chain_id.to_string())),
            ("rpcUrl", Value::str(&self.rpc_url)),
            ("currencySymbol", Value::str(&self.currency_symbol)),
            ("blockExplorerUrl", Value::str(&self.block_explorer_url)),
        ])
    }

    pub fn from_json(v: &Value) -> Result<Network, String> {
        let text = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("network is missing '{k}'"))
        };
        let chain_id = v
            .get("chainId")
            .and_then(Value::as_u64)
            .ok_or("network is missing 'chainId'")?;
        Network::new(
            text("name")?,
            chain_id,
            text("rpcUrl")?,
            text("currencySymbol")?,
            text("blockExplorerUrl")?,
        )
    }
}

/// The networks a wallet can use: built-in ones plus its own custom list.
pub fn all_networks(custom: &[Network]) -> Vec<Network> {
    let mut all = vec![ethereum_mainnet()];
    all.extend(custom.iter().cloned());
    all
}

pub fn find_network(custom: &[Network], chain_id: u64) -> Option<Network> {
    all_networks(custom)
        .into_iter()
        .find(|n| n.chain_id == chain_id)
}

fn name_taken(custom: &[Network], name: &str, except_chain: Option<u64>) -> bool {
    all_networks(custom)
        .iter()
        .any(|n| Some(n.chain_id) != except_chain && n.name.to_lowercase() == name.to_lowercase())
}

pub fn add_network(custom: &mut Vec<Network>, network: Network) -> Result<(), String> {
    if find_network(custom, network.chain_id).is_some() {
        return Err(format!(
            "a network with chain ID {} already exists",
            network.chain_id
        ));
    }
    if name_taken(custom, &network.name, None) {
        return Err(format!("a network named '{}' already exists", network.name));
    }
    custom.push(network);
    Ok(())
}

pub fn delete_network(custom: &mut Vec<Network>, chain_id: u64) -> Result<Network, String> {
    if is_builtin(chain_id) {
        return Err("built-in networks cannot be deleted".to_string());
    }
    let pos = custom
        .iter()
        .position(|n| n.chain_id == chain_id)
        .ok_or_else(|| format!("no custom network with chain ID {chain_id}"))?;
    Ok(custom.remove(pos))
}

pub fn update_network(custom: &mut [Network], network: Network) -> Result<(), String> {
    if is_builtin(network.chain_id) {
        return Err("built-in networks cannot be modified".to_string());
    }
    if name_taken(custom, &network.name, Some(network.chain_id)) {
        return Err(format!("a network named '{}' already exists", network.name));
    }
    let slot = custom
        .iter_mut()
        .find(|n| n.chain_id == network.chain_id)
        .ok_or_else(|| format!("no custom network with chain ID {}", network.chain_id))?;
    *slot = network;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Network {
        Network::new(
            "Base",
            8453,
            "https://mainnet.base.org",
            "ETH",
            "https://basescan.org/",
        )
        .unwrap()
    }

    #[test]
    fn builds_and_normalizes() {
        let n = base();
        assert_eq!(n.block_explorer_url, "https://basescan.org");
        assert_eq!(
            Network::new("  Base  ", 8453, "https://x.io", "ETH", "https://y.io")
                .unwrap()
                .name,
            "Base"
        );
    }

    #[test]
    fn url_rules() {
        assert!(validate_url("https://rpc.example.com/v1/key", "u").is_ok());
        assert!(validate_url("http://localhost:8545", "u").is_ok());
        assert!(validate_url("http://127.0.0.1:8545", "u").is_ok());
        assert!(validate_url("http://[::1]:8545", "u").is_ok());
        for bad in [
            "",
            "ftp://x.io",
            "http://example.com",
            "rpc.example.com",
            "https://",
            "https://user:pw@x.io",
            "https://x.io/a b",
            "https://169.254.169.254/latest",
            "https://10.0.0.5",
            "https://192.168.1.1",
            "https://172.16.0.1",
            "https://0.0.0.0",
            "javascript:alert(1)",
        ] {
            assert!(validate_url(bad, "u").is_err(), "{bad}");
        }
        assert!(validate_url(&format!("https://x.io/{}", "a".repeat(2100)), "u").is_err());
    }

    #[test]
    fn field_rules() {
        assert!(Network::new("", 1, "https://x.io", "ETH", "https://y.io").is_err());
        assert!(Network::new(&"a".repeat(65), 5, "https://x.io", "ETH", "https://y.io").is_err());
        assert!(Network::new("n", 0, "https://x.io", "ETH", "https://y.io").is_err());
        assert!(Network::new("n", u64::MAX, "https://x.io", "ETH", "https://y.io").is_err());
        assert!(Network::new("n", 5, "https://x.io", "", "https://y.io").is_err());
        assert!(Network::new("n", 5, "https://x.io", "E T", "https://y.io").is_err());
        assert!(Network::new("n", 5, "https://x.io", "ETH", "javascript:x").is_err());
        assert!(Network::new("n\u{0}", 5, "https://x.io", "ETH", "https://y.io").is_err());
    }

    #[test]
    fn json_round_trip() {
        let n = base();
        assert_eq!(Network::from_json(&n.to_json()).unwrap(), n);
        assert!(Network::from_json(&Value::Object(vec![])).is_err());
    }

    #[test]
    fn mainnet_is_always_present() {
        assert_eq!(all_networks(&[]).len(), 1);
        assert_eq!(find_network(&[], 1).unwrap().currency_symbol, "ETH");
        assert!(find_network(&[], 8453).is_none());
    }

    #[test]
    fn add_update_delete_lifecycle() {
        let mut custom = vec![];
        add_network(&mut custom, base()).unwrap();
        assert_eq!(find_network(&custom, 8453).unwrap().name, "Base");
        let mut changed = base();
        changed.name = "Base Mainnet".to_string();
        changed.rpc_url = "https://other.base.org".to_string();
        update_network(&mut custom, changed.clone()).unwrap();
        assert_eq!(find_network(&custom, 8453).unwrap(), changed);
        assert_eq!(delete_network(&mut custom, 8453).unwrap(), changed);
        assert!(custom.is_empty());
    }

    #[test]
    fn duplicates_and_builtins_are_protected() {
        let mut custom = vec![];
        add_network(&mut custom, base()).unwrap();
        assert!(add_network(&mut custom, base()).is_err());
        let other_chain_same_name =
            Network::new("base", 9, "https://x.io", "ETH", "https://y.io").unwrap();
        assert!(add_network(&mut custom, other_chain_same_name).is_err());
        let mainnet_clash = Network::new("Mine", 1, "https://x.io", "ETH", "https://y.io").unwrap();
        assert!(add_network(&mut custom, mainnet_clash.clone()).is_err());
        assert!(update_network(&mut custom, mainnet_clash).is_err());
        assert!(delete_network(&mut custom, 1).is_err());
        assert!(delete_network(&mut custom, 999).is_err());
        let ghost = Network::new("Ghost", 999, "https://x.io", "ETH", "https://y.io").unwrap();
        assert!(update_network(&mut custom, ghost).is_err());
        let mainnet_name =
            Network::new("ethereum mainnet", 7, "https://x.io", "ETH", "https://y.io").unwrap();
        assert!(add_network(&mut custom, mainnet_name).is_err());
    }

    #[test]
    fn update_may_keep_own_name() {
        let mut custom = vec![];
        add_network(&mut custom, base()).unwrap();
        let mut same_name = base();
        same_name.currency_symbol = "BASE".to_string();
        assert!(update_network(&mut custom, same_name).is_ok());
    }
}
