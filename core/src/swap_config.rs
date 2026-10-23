//! Swap settings that the project owner edits by hand.
//!
//! Swaps go through OpenOcean (https://open-api.de1.exchange). Its public swap API needs no
//! account, API key or environment variable.

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

/// OpenOcean-compatible API root, including the version (`/v4`). Must be https.
/// Every swap request goes to this host, and the calldata it returns is what gets signed.
pub const OPENOCEAN_API_BASE_URL: &str = "https://open-api.de1.exchange/v4";
