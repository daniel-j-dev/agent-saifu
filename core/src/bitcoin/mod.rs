//! Bitcoin support: wallets, transactions, the Esplora client and message signing.
//!
//! Built to follow Bitcoin Core's wallet behaviour where it matters for safety: native SegWit
//! (P2WPKH) addresses at the BIP 84 path, BIP 143 signatures that are deterministic (RFC 6979),
//! low-S, and verified before use, replaceable (BIP 125) transactions with an anti-fee-sniping
//! lock time, the dust and maximum-fee limits, and a hard binding of each wallet to one network.

pub mod address;
pub mod builder;
pub mod effects;
pub mod journal;
pub mod message;
pub mod network;
pub mod rpc;
#[cfg(test)]
pub mod testing;
pub mod tx;
pub mod units;
pub mod wallet;
