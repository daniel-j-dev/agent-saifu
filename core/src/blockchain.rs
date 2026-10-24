//! Supported blockchains: Ethereum (and EVM chains via chain IDs) and Sui.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blockchain {
    Ethereum,
    Sui,
}

impl Blockchain {
    pub const SUPPORTED: [&'static str; 2] = ["ethereum", "sui"];

    /// Case-insensitive; accepts the common short name `eth`.
    pub fn parse(input: &str) -> Result<Blockchain, String> {
        match input.trim().to_lowercase().as_str() {
            "ethereum" | "eth" => Ok(Blockchain::Ethereum),
            "sui" => Ok(Blockchain::Sui),
            other => Err(format!(
                "unsupported blockchain '{}'; supported: {}",
                crate::eth::explorer::sanitize_text(other),
                Self::SUPPORTED.join(", ")
            )),
        }
    }

    /// Stable identifier used in storage.
    pub fn id(&self) -> &'static str {
        match self {
            Blockchain::Ethereum => "ethereum",
            Blockchain::Sui => "sui",
        }
    }

    /// Display name, e.g. for wallet listings.
    pub fn display_name(&self) -> &'static str {
        match self {
            Blockchain::Ethereum => "Ethereum",
            Blockchain::Sui => "Sui",
        }
    }

    /// Whether wallets on this chain use chain IDs. Sui wallets are bound to one named network
    /// (mainnet, testnet or devnet) instead.
    pub fn uses_chain_ids(&self) -> bool {
        match self {
            Blockchain::Ethereum => true,
            Blockchain::Sui => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_case_insensitively() {
        for s in ["ethereum", "Ethereum", " ETH ", "eth"] {
            assert_eq!(Blockchain::parse(s), Ok(Blockchain::Ethereum), "{s}");
        }
        for s in ["sui", "SUI", " Sui "] {
            assert_eq!(Blockchain::parse(s), Ok(Blockchain::Sui), "{s}");
        }
    }

    #[test]
    fn rejects_unsupported() {
        let err = Blockchain::parse("bitcoin").unwrap_err();
        assert!(err.contains("unsupported") && err.contains("ethereum") && err.contains("sui"));
        assert!(Blockchain::parse("").is_err());
    }

    #[test]
    fn names_are_stable() {
        assert_eq!(Blockchain::Ethereum.id(), "ethereum");
        assert_eq!(Blockchain::Ethereum.display_name(), "Ethereum");
        assert_eq!(Blockchain::Sui.id(), "sui");
        assert_eq!(Blockchain::Sui.display_name(), "Sui");
        for chain in [Blockchain::Ethereum, Blockchain::Sui] {
            assert_eq!(Blockchain::parse(chain.id()), Ok(chain));
        }
        assert!(Blockchain::Ethereum.uses_chain_ids());
        assert!(!Blockchain::Sui.uses_chain_ids());
    }
}
