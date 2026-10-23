//! WalletConnect v2 (Sign protocol): connect the wallet to any dapp, receive its requests and
//! answer them. Everything here is plain data and logic; the network sits behind `relay::Relay`.

pub mod auth;
pub mod envelope;
pub mod manager;
pub mod namespaces;
pub mod proto;
pub mod relay;
pub mod requests;
#[cfg(test)]
pub mod testing;
pub mod uri;

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
