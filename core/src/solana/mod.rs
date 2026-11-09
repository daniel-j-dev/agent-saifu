//! Solana support: wallets, transactions, the JSON-RPC client and swaps.
//!
//! Built to match what Phantom does: Ed25519 keys derived with SLIP-0010 at `m/44'/501'/0'/0'`,
//! base58 addresses and signatures, transactions simulated before they are signed, and
//! `signMessage` refused for bytes that could be a transaction.

pub mod address;
pub mod describe;
pub mod effects;
pub mod network;
pub mod rpc;
pub mod swap;
#[cfg(test)]
pub mod testing;
pub mod tokens;
pub mod tx;
pub mod valuation;
pub mod wallet;
