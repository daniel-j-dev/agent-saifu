//! Solana settings that the project owner edits by hand.
//!
//! Solana wallets talk to a Solana JSON-RPC node. The public endpoints below need no account or
//! key, but Solana Foundation's public nodes are rate limited and may refuse some cloud networks.
//! For heavier use, put the https URL of a node from your own provider (Helius, QuickNode,
//! Triton, ...) in place of one of them; a provider's URL often contains a secret API key, so
//! keep it out of version control.

/// JSON-RPC endpoint for Solana mainnet-beta. Must be https.
pub const MAINNET_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

/// JSON-RPC endpoint for Solana devnet. Must be https.
pub const DEVNET_RPC_URL: &str = "https://api.devnet.solana.com";

/// JSON-RPC endpoint for Solana testnet. Must be https.
pub const TESTNET_RPC_URL: &str = "https://api.testnet.solana.com";
