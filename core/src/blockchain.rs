//! Supported blockchains: Ethereum (and EVM chains via chain IDs), Sui, Solana and Bitcoin.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blockchain {
    Ethereum,
    Sui,
    Solana,
    Bitcoin,
}

impl Blockchain {
    pub const SUPPORTED: [&'static str; 4] = ["ethereum", "sui", "solana", "bitcoin"];

    /// Case-insensitive; accepts the common short names `eth`, `sol` and `btc`.
    pub fn parse(input: &str) -> Result<Blockchain, String> {
        match input.trim().to_lowercase().as_str() {
            "ethereum" | "eth" => Ok(Blockchain::Ethereum),
            "sui" => Ok(Blockchain::Sui),
            "solana" | "sol" => Ok(Blockchain::Solana),
            "bitcoin" | "btc" => Ok(Blockchain::Bitcoin),
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
            Blockchain::Solana => "solana",
            Blockchain::Bitcoin => "bitcoin",
        }
    }

    /// Display name, e.g. for wallet listings.
    pub fn display_name(&self) -> &'static str {
        match self {
            Blockchain::Ethereum => "Ethereum",
            Blockchain::Sui => "Sui",
            Blockchain::Solana => "Solana",
            Blockchain::Bitcoin => "Bitcoin",
        }
    }

    /// Whether wallets on this chain use chain IDs. Sui, Solana and Bitcoin wallets are bound to
    /// one named network (mainnet, testnet, devnet or signet) instead.
    pub fn uses_chain_ids(&self) -> bool {
        match self {
            Blockchain::Ethereum => true,
            Blockchain::Sui | Blockchain::Solana | Blockchain::Bitcoin => false,
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
        for s in ["solana", "Solana", " SOL ", "sol"] {
            assert_eq!(Blockchain::parse(s), Ok(Blockchain::Solana), "{s}");
        }
        for s in ["bitcoin", "Bitcoin", " BTC ", "btc"] {
            assert_eq!(Blockchain::parse(s), Ok(Blockchain::Bitcoin), "{s}");
        }
    }

    #[test]
    fn rejects_unsupported() {
        let err = Blockchain::parse("dogecoin").unwrap_err();
        assert!(
            err.contains("unsupported")
                && err.contains("ethereum")
                && err.contains("sui")
                && err.contains("solana")
                && err.contains("bitcoin")
        );
        assert!(Blockchain::parse("").is_err());
    }

    #[test]
    fn names_are_stable() {
        assert_eq!(Blockchain::Ethereum.id(), "ethereum");
        assert_eq!(Blockchain::Ethereum.display_name(), "Ethereum");
        assert_eq!(Blockchain::Sui.id(), "sui");
        assert_eq!(Blockchain::Sui.display_name(), "Sui");
        assert_eq!(Blockchain::Solana.id(), "solana");
        assert_eq!(Blockchain::Solana.display_name(), "Solana");
        assert_eq!(Blockchain::Bitcoin.id(), "bitcoin");
        assert_eq!(Blockchain::Bitcoin.display_name(), "Bitcoin");
        for chain in [
            Blockchain::Ethereum,
            Blockchain::Sui,
            Blockchain::Solana,
            Blockchain::Bitcoin,
        ] {
            assert_eq!(Blockchain::parse(chain.id()), Ok(chain));
        }
        assert!(Blockchain::Ethereum.uses_chain_ids());
        assert!(!Blockchain::Sui.uses_chain_ids());
        assert!(!Blockchain::Solana.uses_chain_ids());
        assert!(!Blockchain::Bitcoin.uses_chain_ids());
    }
}
