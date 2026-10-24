//! Swap settings for Sui that the project owner edits by hand.
//!
//! Sui swaps go through the Cetus aggregator (https://api-sui.cetus.zone). Its public API needs
//! no account, API key or environment variable.

/// >>> PUT THE SUI WALLET ADDRESS THAT SHOULD RECEIVE YOUR SWAP FEE BETWEEN THE QUOTES BELOW <<<
///
/// This is the Cetus aggregator's "overlay fee" receiver: the aggregator's on-chain contract
/// sends this share of the coins the swap produces straight to this address on every swap.
/// Use a normal Sui address (`0x` followed by 64 hex characters). It must be a Sui address, not an
/// Ethereum one: the Ethereum fee address in `swap_config.rs` cannot receive Sui coins.
///
/// While this is left empty (`""`) NO fee is charged and swaps still work.
/// An address that is not valid makes the server refuse to start (with a clear message on
/// stderr) instead of sending fees somewhere unexpected.
pub const SUI_SWAP_FEE_RECIPIENT: &str =
    "0x7744b1acf734218f054a556b56cfd6d7234f2798cccc621f5620a3173e0cd5a2";

/// Fee in basis points of the amount the swap produces. 70 bps = 0.7%. The aggregator's contract
/// accepts 1 to 1000 bps (0.01% to 10%).
pub const SUI_SWAP_FEE_BPS: u32 = 70;

/// Cetus aggregator API root (the `router_v3` service). Must be https.
/// The routes it returns are checked against Cetus's on-chain packages before anything is signed.
pub const CETUS_AGGREGATOR_URL: &str = "https://api-sui.cetus.zone/router_v3";
