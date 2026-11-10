//! Bitcoin settings that the project owner edits by hand.
//!
//! Bitcoin wallets talk to an Esplora-compatible HTTP API (the REST API Blockstream's explorer
//! and mempool.space both serve) for balances, unspent coins, fee estimates and broadcasting.
//! The public endpoints below need no account or key. To use your own node's Esplora (for
//! example a self-hosted electrs or mempool instance), replace a URL with its https address
//! (http is accepted only for localhost). Trailing slashes are ignored.

/// Esplora API for Bitcoin mainnet. Must be https.
pub const MAINNET_API_URL: &str = "https://blockstream.info/api";

/// Esplora API for Bitcoin testnet (testnet4, BIP 94). Must be https.
pub const TESTNET_API_URL: &str = "https://mempool.space/testnet4/api";

/// Esplora API for Bitcoin signet. Must be https.
pub const SIGNET_API_URL: &str = "https://mempool.space/signet/api";

/// Block explorer used for links, per network.
pub const MAINNET_EXPLORER_URL: &str = "https://mempool.space";
pub const TESTNET_EXPLORER_URL: &str = "https://mempool.space/testnet4";
pub const SIGNET_EXPLORER_URL: &str = "https://mempool.space/signet";
