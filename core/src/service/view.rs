//! A read-only, password-free view of a wallet for apps that show balances.
//!
//! Unlocking a wallet once reveals its public facts: the address, the network it is bound to
//! (Sui, Solana, Bitcoin) or the networks and tokens it uses (Ethereum). A `PublicView` keeps
//! only those, never a key, so balances can be refreshed again without asking for the password.
//! It is a library API for an app; no MCP tool exposes it, so an agent still needs the password
//! for every read.

use super::*;
use crate::bitcoin::address::Address as BitcoinAddress;
use crate::bitcoin::network::BitcoinNetwork;
use crate::solana::address::Pubkey;
use crate::solana::network::SolanaNetwork;
use crate::sui::network::SuiNetwork;
use sui_sdk_types::Address as SuiAddress;

#[derive(Clone)]
enum Kind {
    Ethereum {
        owner: Address,
        networks: Vec<Network>,
        tokens: Vec<CustomToken>,
    },
    Sui {
        network: SuiNetwork,
        owner: SuiAddress,
    },
    Solana {
        network: SolanaNetwork,
        owner: Pubkey,
    },
    Bitcoin {
        network: BitcoinNetwork,
        address: BitcoinAddress,
    },
}

/// The public facts of one unlocked wallet. Holds no secret.
#[derive(Clone)]
pub struct PublicView {
    wallet: String,
    kind: Kind,
}

impl PublicView {
    pub fn wallet(&self) -> &str {
        &self.wallet
    }

    /// The address as the wallet shows it.
    pub fn address(&self) -> String {
        match &self.kind {
            Kind::Ethereum { owner, .. } => to_checksum(owner),
            Kind::Sui { owner, .. } => owner.to_string(),
            Kind::Solana { owner, .. } => crate::solana::address::to_text(owner),
            Kind::Bitcoin { network, address } => address.to_text(*network),
        }
    }
}

impl WalletService {
    /// Unlocks the wallet once and keeps only its public facts.
    pub fn public_view(&self, name: &str, password: &str) -> Result<PublicView, String> {
        let (wallet, kind) = match self.chain_of(name, password)? {
            Blockchain::Ethereum => {
                let (record, contents) = self.unlock(name, password)?;
                let kind = Kind::Ethereum {
                    owner: contents.address()?,
                    networks: contents.networks.clone(),
                    tokens: contents.tokens.clone(),
                };
                (record.name, kind)
            }
            Blockchain::Sui => {
                let (record, vault) = self.unlock_sui(name, password)?;
                let kind = Kind::Sui {
                    network: vault.network,
                    owner: vault.address()?,
                };
                (record.name, kind)
            }
            Blockchain::Solana => {
                let (record, vault) = self.unlock_solana(name, password)?;
                let kind = Kind::Solana {
                    network: vault.network,
                    owner: vault.public_key()?,
                };
                (record.name, kind)
            }
            Blockchain::Bitcoin => {
                let (record, vault) = self.unlock_bitcoin(name, password)?;
                let kind = Kind::Bitcoin {
                    network: vault.network,
                    address: vault.address_kind()?,
                };
                (record.name, kind)
            }
        };
        Ok(PublicView { wallet, kind })
    }

    /// The same answer as `get_wallet_balances`, read from the network with no password.
    pub fn balances_of_view(&self, view: &PublicView) -> Result<Value, String> {
        let wallet = view.wallet.clone();
        match &view.kind {
            Kind::Ethereum {
                owner,
                networks,
                tokens,
            } => self.balances_of(wallet, owner, networks, tokens, None, &[]),
            Kind::Sui { network, owner } => {
                self.sui_balances_of(wallet, &self.sui_session_for(*network, *owner)?)
            }
            Kind::Solana { network, owner } => {
                self.solana_balances_of(wallet, &self.solana_session_for(*network, *owner)?)
            }
            Kind::Bitcoin { network, address } => {
                self.bitcoin_balances_of(wallet, &self.bitcoin_session_for(*network, address)?)
            }
        }
    }
}
