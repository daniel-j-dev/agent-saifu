//! WalletConnect settings that the project owner edits by hand.
//!
//! WalletConnect's relay needs a free project ID. Create one at https://dashboard.walletconnect.com/
//! (formerly WalletConnect Cloud), then either put it below or set the
//! `WALLETCONNECT_PROJECT_ID` environment variable (the variable wins). Without a project ID
//! the rest of the wallet works as usual and the `wc_*` tools explain what is missing.

/// >>> PUT YOUR WALLETCONNECT PROJECT ID BETWEEN THE QUOTES BELOW (or use the environment variable) <<<
pub const WALLETCONNECT_PROJECT_ID: &str = "79ee3a229d664718cdf1479b39d3197c";

/// Environment variable that overrides `WALLETCONNECT_PROJECT_ID`.
pub const PROJECT_ID_ENV: &str = "WALLETCONNECT_PROJECT_ID";

/// The WalletConnect relay. Must be `wss://`; every dapp message passes through it end-to-end
/// encrypted, so the relay cannot read requests or tamper with them undetected.
pub const WALLETCONNECT_RELAY_URL: &str = "wss://relay.walletconnect.org";
