//! Sui wallet support: Ed25519 keys through the official Sui Rust SDK (`sui-sdk-types`,
//! `sui-crypto`, `sui-transaction-builder`), reading and submitting through Sui's GraphQL API,
//! and swaps through the Cetus aggregator.

pub mod cetus;
pub mod coins;
pub mod gql;
pub mod network;
pub mod sdk_json;
#[cfg(test)]
pub mod testing;
pub mod tx;
pub mod valuation;
pub mod wallet;
