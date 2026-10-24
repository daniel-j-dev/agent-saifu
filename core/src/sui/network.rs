//! Sui networks. A Sui wallet is bound to exactly one network, so a testnet key can never sign
//! a mainnet transaction (Sui's security guidance: keep testnet and mainnet keys separate).

use crate::eth::explorer::sanitize_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuiNetwork {
    Mainnet,
    Testnet,
    Devnet,
}

impl SuiNetwork {
    pub const SUPPORTED: [&'static str; 3] = ["mainnet", "testnet", "devnet"];

    /// Case-insensitive; accepts the Wallet Standard chain identifiers (`sui:mainnet`).
    pub fn parse(input: &str) -> Result<SuiNetwork, String> {
        let lower = input.trim().to_lowercase();
        match lower.strip_prefix("sui:").unwrap_or(&lower) {
            "mainnet" => Ok(SuiNetwork::Mainnet),
            "testnet" => Ok(SuiNetwork::Testnet),
            "devnet" => Ok(SuiNetwork::Devnet),
            other => Err(format!(
                "unsupported Sui network '{}'; supported: {}",
                sanitize_text(other),
                Self::SUPPORTED.join(", ")
            )),
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            SuiNetwork::Mainnet => "mainnet",
            SuiNetwork::Testnet => "testnet",
            SuiNetwork::Devnet => "devnet",
        }
    }

    /// The Wallet Standard chain identifier.
    pub fn chain_name(&self) -> &'static str {
        match self {
            SuiNetwork::Mainnet => "sui:mainnet",
            SuiNetwork::Testnet => "sui:testnet",
            SuiNetwork::Devnet => "sui:devnet",
        }
    }

    pub fn graphql_url(&self) -> &'static str {
        match self {
            SuiNetwork::Mainnet => "https://graphql.mainnet.sui.io/graphql",
            SuiNetwork::Testnet => "https://graphql.testnet.sui.io/graphql",
            SuiNetwork::Devnet => "https://graphql.devnet.sui.io/graphql",
        }
    }

    pub fn explorer_url(&self) -> &'static str {
        match self {
            SuiNetwork::Mainnet => "https://suiscan.xyz/mainnet",
            SuiNetwork::Testnet => "https://suiscan.xyz/testnet",
            SuiNetwork::Devnet => "https://suiscan.xyz/devnet",
        }
    }

    pub fn explorer_tx_url(&self, digest: &str) -> String {
        format!("{}/tx/{digest}", self.explorer_url())
    }

    /// Chain identifier (genesis checkpoint digest) the endpoint must report. Devnet is reset
    /// regularly, so it has none.
    pub fn expected_chain_identifier(&self) -> Option<&'static str> {
        match self {
            SuiNetwork::Mainnet => Some("4btiuiMPvEENsttpZC7CZ53DruC3MAgfznDbASZ7DR6S"),
            SuiNetwork::Testnet => Some("69WiPg3DAQiwdxfncX6wYQ2siKwAe6L9BZthQea3JNMD"),
            SuiNetwork::Devnet => None,
        }
    }

    /// Real value only exists on mainnet.
    pub fn is_mainnet(&self) -> bool {
        *self == SuiNetwork::Mainnet
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_case_insensitively_and_chain_ids() {
        assert_eq!(SuiNetwork::parse("mainnet"), Ok(SuiNetwork::Mainnet));
        assert_eq!(SuiNetwork::parse(" TestNet "), Ok(SuiNetwork::Testnet));
        assert_eq!(SuiNetwork::parse("sui:devnet"), Ok(SuiNetwork::Devnet));
    }

    #[test]
    fn rejects_unknown_networks_without_echoing_control_characters() {
        let err = SuiNetwork::parse("main\u{7}net2").unwrap_err();
        assert!(err.contains("unsupported") && err.contains("mainnet, testnet, devnet"));
        assert!(!err.contains('\u{7}'));
        assert!(SuiNetwork::parse("").is_err());
        assert!(SuiNetwork::parse("sui:").is_err());
    }

    #[test]
    fn identifiers_round_trip() {
        for n in [SuiNetwork::Mainnet, SuiNetwork::Testnet, SuiNetwork::Devnet] {
            assert_eq!(SuiNetwork::parse(n.id()), Ok(n));
            assert_eq!(SuiNetwork::parse(n.chain_name()), Ok(n));
            assert!(n.graphql_url().starts_with("https://"));
            assert!(n.explorer_tx_url("abc").ends_with("/tx/abc"));
        }
    }

    #[test]
    fn only_mainnet_and_testnet_have_fixed_chain_identifiers() {
        assert!(SuiNetwork::Mainnet.expected_chain_identifier().is_some());
        assert!(SuiNetwork::Testnet.expected_chain_identifier().is_some());
        assert!(SuiNetwork::Devnet.expected_chain_identifier().is_none());
        assert!(SuiNetwork::Mainnet.is_mainnet());
        assert!(!SuiNetwork::Testnet.is_mainnet());
    }
}
