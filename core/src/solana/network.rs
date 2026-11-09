//! Solana clusters. Like a Sui wallet, a Solana wallet is bound to exactly one cluster, so a
//! devnet key can never sign a mainnet transaction.

use crate::eth::explorer::sanitize_text;
use crate::solana_config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolanaNetwork {
    Mainnet,
    Devnet,
    Testnet,
}

/// How many leading characters of the genesis hash make up a CAIP-2 chain reference.
const CHAIN_REFERENCE_CHARS: usize = 32;

impl SolanaNetwork {
    pub const SUPPORTED: [&'static str; 3] = ["mainnet", "devnet", "testnet"];

    /// Case-insensitive; accepts Solana's own name `mainnet-beta` and WalletConnect chain
    /// identifiers (`solana:<reference>`).
    pub fn parse(input: &str) -> Result<SolanaNetwork, String> {
        let text = input.trim();
        if text.to_lowercase().starts_with("solana:") {
            return Self::from_chain(text).ok_or_else(|| {
                format!(
                    "unsupported Solana chain '{}'; supported: {}",
                    sanitize_text(text),
                    Self::SUPPORTED.join(", ")
                )
            });
        }
        match text.to_lowercase().as_str() {
            "mainnet" | "mainnet-beta" => Ok(SolanaNetwork::Mainnet),
            "devnet" => Ok(SolanaNetwork::Devnet),
            "testnet" => Ok(SolanaNetwork::Testnet),
            other => Err(format!(
                "unsupported Solana network '{}'; supported: {}",
                sanitize_text(other),
                Self::SUPPORTED.join(", ")
            )),
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            SolanaNetwork::Mainnet => "mainnet",
            SolanaNetwork::Devnet => "devnet",
            SolanaNetwork::Testnet => "testnet",
        }
    }

    pub fn rpc_url(&self) -> &'static str {
        match self {
            SolanaNetwork::Mainnet => solana_config::MAINNET_RPC_URL,
            SolanaNetwork::Devnet => solana_config::DEVNET_RPC_URL,
            SolanaNetwork::Testnet => solana_config::TESTNET_RPC_URL,
        }
    }

    /// The hash of the cluster's first block, which the RPC endpoint must report. It is what
    /// tells the clusters apart (Solana has no chain IDs).
    pub fn genesis_hash(&self) -> &'static str {
        match self {
            SolanaNetwork::Mainnet => "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d",
            SolanaNetwork::Devnet => "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG",
            SolanaNetwork::Testnet => "4uhcVJyU9pJkvQyS88uRDiswHXSCkY3zQawwpjk2NsNY",
        }
    }

    /// CAIP-2 reference: the first 32 characters of the genesis hash.
    pub fn chain_reference(&self) -> &'static str {
        &self.genesis_hash()[..CHAIN_REFERENCE_CHARS]
    }

    /// The WalletConnect / CAIP-2 chain identifier, e.g. `solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp`.
    pub fn chain_name(&self) -> String {
        format!("solana:{}", self.chain_reference())
    }

    /// Older identifiers some dapps still use for the same cluster (Web3Modal's early Solana
    /// support named mainnet and devnet this way).
    fn legacy_references(&self) -> &'static [&'static str] {
        match self {
            SolanaNetwork::Mainnet => &["4sGjMW1sUnHzSxGspuhpqLDx6wiyjNtZ"],
            SolanaNetwork::Devnet => &["8E9rvCKLFQia2Y35HXjjpWzj8weVo44K"],
            SolanaNetwork::Testnet => &[],
        }
    }

    /// Whether a WalletConnect chain identifier names this cluster (current or legacy form).
    pub fn matches_chain(&self, chain: &str) -> bool {
        let Some(reference) = chain.strip_prefix("solana:") else {
            return false;
        };
        reference == self.chain_reference() || self.legacy_references().contains(&reference)
    }

    pub fn from_chain(chain: &str) -> Option<SolanaNetwork> {
        [
            SolanaNetwork::Mainnet,
            SolanaNetwork::Devnet,
            SolanaNetwork::Testnet,
        ]
        .into_iter()
        .find(|n| n.matches_chain(chain))
    }

    pub fn explorer_url(&self) -> &'static str {
        "https://explorer.solana.com"
    }

    fn cluster_query(&self) -> &'static str {
        match self {
            SolanaNetwork::Mainnet => "",
            SolanaNetwork::Devnet => "?cluster=devnet",
            SolanaNetwork::Testnet => "?cluster=testnet",
        }
    }

    pub fn explorer_tx_url(&self, signature: &str) -> String {
        format!(
            "{}/tx/{signature}{}",
            self.explorer_url(),
            self.cluster_query()
        )
    }

    pub fn explorer_address_url(&self, address: &str) -> String {
        format!(
            "{}/address/{address}{}",
            self.explorer_url(),
            self.cluster_query()
        )
    }

    /// Real value only exists on mainnet.
    pub fn is_mainnet(&self) -> bool {
        *self == SolanaNetwork::Mainnet
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [SolanaNetwork; 3] = [
        SolanaNetwork::Mainnet,
        SolanaNetwork::Devnet,
        SolanaNetwork::Testnet,
    ];

    #[test]
    fn parses_names_case_insensitively_and_chain_ids() {
        assert_eq!(SolanaNetwork::parse("mainnet"), Ok(SolanaNetwork::Mainnet));
        assert_eq!(
            SolanaNetwork::parse(" Mainnet-Beta "),
            Ok(SolanaNetwork::Mainnet)
        );
        assert_eq!(SolanaNetwork::parse("DEVNET"), Ok(SolanaNetwork::Devnet));
        assert_eq!(SolanaNetwork::parse("testnet"), Ok(SolanaNetwork::Testnet));
        for n in ALL {
            assert_eq!(SolanaNetwork::parse(n.id()), Ok(n));
            assert_eq!(SolanaNetwork::parse(&n.chain_name()), Ok(n));
        }
        assert!(SolanaNetwork::parse("localnet").is_err());
        assert!(SolanaNetwork::parse("solana:unknown").is_err());
        assert!(SolanaNetwork::parse("").is_err());
    }

    #[test]
    fn chain_names_follow_caip_2() {
        assert_eq!(
            SolanaNetwork::Mainnet.chain_name(),
            "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
        );
        assert_eq!(
            SolanaNetwork::Devnet.chain_name(),
            "solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1"
        );
        assert_eq!(
            SolanaNetwork::Testnet.chain_name(),
            "solana:4uhcVJyU9pJkvQyS88uRDiswHXSCkY3z"
        );
    }

    #[test]
    fn legacy_chain_identifiers_name_the_same_cluster() {
        assert!(SolanaNetwork::Mainnet.matches_chain("solana:4sGjMW1sUnHzSxGspuhpqLDx6wiyjNtZ"));
        assert!(SolanaNetwork::Devnet.matches_chain("solana:8E9rvCKLFQia2Y35HXjjpWzj8weVo44K"));
        assert!(!SolanaNetwork::Devnet.matches_chain("solana:4sGjMW1sUnHzSxGspuhpqLDx6wiyjNtZ"));
        assert!(!SolanaNetwork::Mainnet.matches_chain("eip155:1"));
        assert!(!SolanaNetwork::Mainnet.matches_chain("solana"));
        assert_eq!(
            SolanaNetwork::from_chain("solana:4sGjMW1sUnHzSxGspuhpqLDx6wiyjNtZ"),
            Some(SolanaNetwork::Mainnet)
        );
    }

    #[test]
    fn clusters_are_distinct_and_endpoints_are_https() {
        for (i, a) in ALL.iter().enumerate() {
            assert!(a.rpc_url().starts_with("https://"));
            assert_eq!(a.genesis_hash().len(), 44);
            for b in &ALL[i + 1..] {
                assert_ne!(a.genesis_hash(), b.genesis_hash());
                assert_ne!(a.chain_reference(), b.chain_reference());
            }
        }
        assert!(SolanaNetwork::Mainnet.is_mainnet());
        assert!(!SolanaNetwork::Devnet.is_mainnet());
    }

    #[test]
    fn explorer_links_name_the_cluster() {
        assert_eq!(
            SolanaNetwork::Mainnet.explorer_tx_url("SIG"),
            "https://explorer.solana.com/tx/SIG"
        );
        assert_eq!(
            SolanaNetwork::Devnet.explorer_tx_url("SIG"),
            "https://explorer.solana.com/tx/SIG?cluster=devnet"
        );
        assert_eq!(
            SolanaNetwork::Testnet.explorer_address_url("ADDR"),
            "https://explorer.solana.com/address/ADDR?cluster=testnet"
        );
    }
}
