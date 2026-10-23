//! Supported blockchains. Only Ethereum (and EVM chains via chain IDs) for now.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blockchain {
    Ethereum,
}

impl Blockchain {
    pub const SUPPORTED: [&'static str; 1] = ["ethereum"];

    /// Case-insensitive; accepts the common short name `eth`.
    pub fn parse(input: &str) -> Result<Blockchain, String> {
        match input.trim().to_lowercase().as_str() {
            "ethereum" | "eth" => Ok(Blockchain::Ethereum),
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
        }
    }

    /// Display name, e.g. for wallet listings.
    pub fn display_name(&self) -> &'static str {
        match self {
            Blockchain::Ethereum => "Ethereum",
        }
    }

    /// Whether wallets on this chain use chain IDs (Bitcoin-like chains would not).
    pub fn uses_chain_ids(&self) -> bool {
        match self {
            Blockchain::Ethereum => true,
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
    }

    #[test]
    fn rejects_unsupported() {
        let err = Blockchain::parse("bitcoin").unwrap_err();
        assert!(err.contains("unsupported") && err.contains("ethereum"));
        assert!(Blockchain::parse("").is_err());
    }

    #[test]
    fn names_are_stable() {
        assert_eq!(Blockchain::Ethereum.id(), "ethereum");
        assert_eq!(Blockchain::Ethereum.display_name(), "Ethereum");
        assert_eq!(
            Blockchain::parse(Blockchain::Ethereum.id()),
            Ok(Blockchain::Ethereum)
        );
        assert!(Blockchain::Ethereum.uses_chain_ids());
    }
}
