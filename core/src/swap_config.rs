//! Swap settings that the project owner edits by hand.
//!
//! Ethereum swaps go through OpenOcean (https://open-api.de1.exchange) and Solana swaps through
//! Jupiter (https://api.jup.ag). Neither needs an account, API key or environment variable.

/// >>> PUT THE WALLET ADDRESS THAT SHOULD RECEIVE YOUR SWAP FEE BETWEEN THE QUOTES BELOW <<<
///
/// This is OpenOcean's `referrer` parameter: the address that earns the fee on every swap.
/// Use a normal Ethereum-style address (`0x` + 40 hex characters). The same address
/// receives the fee on every chain.
///
/// While this is left empty (`""`) NO fee is charged and swaps still work.
/// An address that is not valid makes the server refuse to start (with a clear message on
/// stderr) instead of sending fees somewhere unexpected.
pub const SWAP_FEE_RECIPIENT: &str = "0xf8564095b5b0EdFc8f334b82BF04a57B27dcA4CD";

/// Fee in basis points of the amount sold (OpenOcean's `referrerFee`, which it takes as a
/// percentage). 70 bps = 0.7%. OpenOcean accepts 1 to 500 bps (0.01% to 5%).
/// By OpenOcean's published terms it keeps 20% of the fee by default.
pub const SWAP_FEE_BPS: u32 = 70;

/// >>> PUT THE SOLANA WALLET ADDRESS THAT SHOULD RECEIVE YOUR SOLANA SWAP FEE BETWEEN THE QUOTES BELOW <<<
///
/// Solana swaps go through Jupiter (see `JUPITER_API_BASE_URL`), whose "platform fee" pays this
/// address a share of what the buyer receives. It must be a Solana address (base58, 32 to 44
/// characters); the Ethereum address above cannot receive Solana tokens. The fee arrives in the
/// token that was bought, in this address's associated token account for it. The wallet opens
/// that account in the swap transaction when it does not exist yet (a one-off deposit of about
/// 0.00204 SOL per token, paid by the swapping wallet), so nothing needs to be set up first.
///
/// While this is left empty (`""`) NO fee is charged and Solana swaps still work.
/// An address that is not valid makes the server refuse to start (with a clear message on
/// stderr) instead of sending fees somewhere unexpected.
pub const SOLANA_SWAP_FEE_RECIPIENT: &str = "7sdBFvFVfUMYiVMHhzJVWDgPn48vfpMPCiRm6kiTEC51";

/// Fee in basis points of the amount bought on Solana (Jupiter's `platformFeeBps`).
/// 70 bps = 0.7%. Accepted range is 1 to 500 bps (0.01% to 5%). Jupiter takes no cut of it
/// on the `/build` endpoint this wallet uses. It is only charged when
/// `SOLANA_SWAP_FEE_RECIPIENT` is set.
pub const SOLANA_SWAP_FEE_BPS: u32 = 70;

/// OpenOcean-compatible API root, including the version (`/v4`). Must be https.
/// Every Ethereum swap request goes to this host, and the calldata it returns is what gets
/// signed.
pub const OPENOCEAN_API_BASE_URL: &str = "https://open-api.de1.exchange/v4";

/// Jupiter Swap API root for Solana swaps (`/build` returns the swap instructions the wallet
/// assembles, checks, simulates and signs itself). Must be https. It is used without an API
/// key ("keyless" access, about one request every two seconds), so no account or secret is
/// needed.
pub const JUPITER_API_BASE_URL: &str = "https://api.jup.ag/swap/v2";
