//! Bitcoin networks. Like a Sui or Solana wallet, a Bitcoin wallet is bound to exactly one
//! network, so a testnet key can never sign a mainnet transaction and an address is only ever
//! accepted for the network the wallet is on.

use crate::bitcoin_config;
use crate::eth::explorer::sanitize_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitcoinNetwork {
    Mainnet,
    /// Testnet4 (BIP 94), the test network Bitcoin Core now uses.
    Testnet,
    Signet,
}

impl BitcoinNetwork {
    pub const SUPPORTED: [&'static str; 3] = ["mainnet", "testnet", "signet"];

    /// Case-insensitive; accepts `bitcoin` and `main` for mainnet and `testnet4` for testnet.
    pub fn parse(input: &str) -> Result<BitcoinNetwork, String> {
        match input.trim().to_lowercase().as_str() {
            "mainnet" | "main" | "bitcoin" => Ok(BitcoinNetwork::Mainnet),
            "testnet" | "testnet4" => Ok(BitcoinNetwork::Testnet),
            "signet" => Ok(BitcoinNetwork::Signet),
            "testnet3" => Err(
                "Bitcoin testnet3 is retired; use \"testnet\" (testnet4) or \"signet\"".to_string(),
            ),
            other => Err(format!(
                "unsupported Bitcoin network '{}'; supported: {}",
                sanitize_text(other),
                Self::SUPPORTED.join(", ")
            )),
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            BitcoinNetwork::Mainnet => "mainnet",
            BitcoinNetwork::Testnet => "testnet",
            BitcoinNetwork::Signet => "signet",
        }
    }

    /// Bitcoin Core's name for the chain.
    pub fn chain_name(&self) -> &'static str {
        match self {
            BitcoinNetwork::Mainnet => "main",
            BitcoinNetwork::Testnet => "testnet4",
            BitcoinNetwork::Signet => "signet",
        }
    }

    pub fn is_mainnet(&self) -> bool {
        *self == BitcoinNetwork::Mainnet
    }

    /// Human-readable part of SegWit addresses (BIP 173).
    pub fn hrp(&self) -> &'static str {
        match self {
            BitcoinNetwork::Mainnet => "bc",
            BitcoinNetwork::Testnet | BitcoinNetwork::Signet => "tb",
        }
    }

    pub fn p2pkh_version(&self) -> u8 {
        if self.is_mainnet() { 0x00 } else { 0x6f }
    }

    pub fn p2sh_version(&self) -> u8 {
        if self.is_mainnet() { 0x05 } else { 0xc4 }
    }

    /// Version byte of WIF private keys.
    pub fn wif_version(&self) -> u8 {
        if self.is_mainnet() { 0x80 } else { 0xef }
    }

    /// SLIP-44 coin type in derivation paths: 0 for Bitcoin, 1 for every test network.
    pub fn coin_type(&self) -> u32 {
        if self.is_mainnet() { 0 } else { 1 }
    }

    /// BIP 84 path of the wallet's one address: `m/84'/<coin>'/0'/0/0`.
    pub fn derivation_path(&self) -> String {
        format!("m/84'/{}'/0'/0/0", self.coin_type())
    }

    /// Hash of block 0, which the API must report: it is what tells the networks apart.
    pub fn genesis_hash(&self) -> &'static str {
        match self {
            BitcoinNetwork::Mainnet => {
                "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
            }
            BitcoinNetwork::Testnet => {
                "00000000da84f2bafbbc53dee25a72ae507ff4914b867c565be350b0da8bf043"
            }
            BitcoinNetwork::Signet => {
                "00000008819873e925422c1ff0f99f7cc9bbb232af63a077a480a3633bee1ef6"
            }
        }
    }

    pub fn api_url(&self) -> &'static str {
        match self {
            BitcoinNetwork::Mainnet => bitcoin_config::MAINNET_API_URL,
            BitcoinNetwork::Testnet => bitcoin_config::TESTNET_API_URL,
            BitcoinNetwork::Signet => bitcoin_config::SIGNET_API_URL,
        }
    }

    pub fn explorer_url(&self) -> &'static str {
        match self {
            BitcoinNetwork::Mainnet => bitcoin_config::MAINNET_EXPLORER_URL,
            BitcoinNetwork::Testnet => bitcoin_config::TESTNET_EXPLORER_URL,
            BitcoinNetwork::Signet => bitcoin_config::SIGNET_EXPLORER_URL,
        }
    }

    pub fn tx_url(&self, txid: &str) -> String {
        format!("{}/tx/{txid}", self.explorer_url())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_case_insensitively() {
        for s in ["mainnet", " Mainnet ", "BITCOIN", "main"] {
            assert_eq!(BitcoinNetwork::parse(s), Ok(BitcoinNetwork::Mainnet), "{s}");
        }
        for s in ["testnet", "TESTNET4"] {
            assert_eq!(BitcoinNetwork::parse(s), Ok(BitcoinNetwork::Testnet), "{s}");
        }
        assert_eq!(BitcoinNetwork::parse("Signet"), Ok(BitcoinNetwork::Signet));
    }

    #[test]
    fn refuses_unknown_and_retired_networks() {
        assert!(
            BitcoinNetwork::parse("regtest")
                .unwrap_err()
                .contains("supported")
        );
        assert!(BitcoinNetwork::parse("").is_err());
        assert!(
            BitcoinNetwork::parse("testnet3")
                .unwrap_err()
                .contains("retired")
        );
        assert!(BitcoinNetwork::parse("devnet").is_err());
    }

    #[test]
    fn parameters_follow_the_chain_parameters() {
        let main = BitcoinNetwork::Mainnet;
        assert_eq!(
            (
                main.hrp(),
                main.p2pkh_version(),
                main.p2sh_version(),
                main.wif_version()
            ),
            ("bc", 0x00, 0x05, 0x80)
        );
        for test in [BitcoinNetwork::Testnet, BitcoinNetwork::Signet] {
            assert_eq!(
                (
                    test.hrp(),
                    test.p2pkh_version(),
                    test.p2sh_version(),
                    test.wif_version()
                ),
                ("tb", 0x6f, 0xc4, 0xef)
            );
        }
        assert_eq!(main.derivation_path(), "m/84'/0'/0'/0/0");
        assert_eq!(BitcoinNetwork::Testnet.derivation_path(), "m/84'/1'/0'/0/0");
        assert_eq!(BitcoinNetwork::Signet.derivation_path(), "m/84'/1'/0'/0/0");
    }

    #[test]
    fn ids_round_trip_and_genesis_hashes_differ() {
        let all = [
            BitcoinNetwork::Mainnet,
            BitcoinNetwork::Testnet,
            BitcoinNetwork::Signet,
        ];
        for n in all {
            assert_eq!(BitcoinNetwork::parse(n.id()), Ok(n));
            assert_eq!(n.genesis_hash().len(), 64);
        }
        assert_ne!(all[0].genesis_hash(), all[1].genesis_hash());
        assert_ne!(all[1].genesis_hash(), all[2].genesis_hash());
    }

    #[test]
    fn explorer_links_point_at_the_network() {
        assert_eq!(
            BitcoinNetwork::Mainnet.tx_url("ab"),
            "https://mempool.space/tx/ab"
        );
        assert!(BitcoinNetwork::Testnet.tx_url("ab").contains("testnet4"));
        assert!(BitcoinNetwork::Signet.tx_url("ab").contains("signet"));
    }
}
