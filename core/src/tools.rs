//! MCP tool definitions: names, input schemas and the glue to `WalletService`.

use crate::blockchain::Blockchain;
use crate::eth::address::parse_address;
use crate::eth::explorer::{DEFAULT_LIMIT, HistoryKind};
use crate::eth::networks::Network;
use crate::json::Value;
use crate::metadata;
use crate::service::{BitcoinFee, IntentParams, SwapParams, WalletService};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Integer,
    Boolean,
    TextList,
}

struct Param {
    name: &'static str,
    kind: Kind,
    required: bool,
    description: &'static str,
}

const fn req(name: &'static str, kind: Kind, description: &'static str) -> Param {
    Param {
        name,
        kind,
        required: true,
        description,
    }
}

const fn opt(name: &'static str, kind: Kind, description: &'static str) -> Param {
    Param {
        name,
        kind,
        required: false,
        description,
    }
}

struct Tool {
    name: &'static str,
    description: &'static str,
    params: Vec<Param>,
    handler: fn(&WalletService, &Args) -> Result<Value, String>,
}

/// Typed access to a tool call's arguments.
pub struct Args<'a>(&'a Value);

impl<'a> Args<'a> {
    fn get(&self, key: &str) -> Option<&'a Value> {
        self.0.get(key).filter(|v| !matches!(v, Value::Null))
    }

    pub fn str(&self, key: &str) -> Result<&'a str, String> {
        self.opt_str(key)?
            .ok_or_else(|| format!("missing required argument '{key}'"))
    }

    pub fn opt_str(&self, key: &str) -> Result<Option<&'a str>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.as_str())),
            Some(_) => Err(format!("'{key}' must be a string")),
        }
    }

    pub fn u64(&self, key: &str) -> Result<u64, String> {
        self.opt_u64(key)?
            .ok_or_else(|| format!("missing required argument '{key}'"))
    }

    pub fn opt_u64(&self, key: &str) -> Result<Option<u64>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(v) => v
                .as_u64()
                .map(Some)
                .ok_or_else(|| format!("'{key}' must be a non-negative integer")),
        }
    }

    pub fn bool_or(&self, key: &str, default: bool) -> Result<bool, String> {
        match self.get(key) {
            None => Ok(default),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => Err(format!("'{key}' must be true or false")),
        }
    }

    pub fn str_list(&self, key: &str) -> Result<Vec<String>, String> {
        match self.get(key) {
            None => Ok(vec![]),
            Some(Value::Array(items)) => items
                .iter()
                .map(|i| match i {
                    Value::String(s) => Ok(s.clone()),
                    Value::Number(n) => Ok(n.clone()),
                    Value::Bool(b) => Ok(b.to_string()),
                    _ => Err(format!("'{key}' must be a list of strings")),
                })
                .collect(),
            Some(_) => Err(format!("'{key}' must be a list")),
        }
    }
}

fn wallet() -> Param {
    req("wallet_name", Kind::Text, "Name of the wallet.")
}

fn password() -> Param {
    req(
        "password",
        Kind::Text,
        "The wallet's password. The wallet is unlocked only for this call.",
    )
}

fn chain_id() -> Param {
    req(
        "chain_id",
        Kind::Integer,
        "Chain ID of the network, e.g. 1 (Ethereum), 8453 (Base), 42161 (Arbitrum One). Ethereum wallets only.",
    )
}

/// For tools that work with every blockchain: Ethereum wallets need it, Sui, Solana and Bitcoin
/// wallets must omit it.
fn chain_id_for_eth() -> Param {
    opt(
        "chain_id",
        Kind::Integer,
        "Ethereum wallets: chain ID of the network, e.g. 1 (Ethereum), 8453 (Base), 42161 (Arbitrum One); required. Sui, Solana and Bitcoin wallets: omit (they are bound to the network they were created for).",
    )
}

fn eth_chain_id(a: &Args) -> Result<u64, String> {
    a.opt_u64("chain_id")?.ok_or_else(|| {
        "missing required argument 'chain_id' (required for Ethereum wallets)".to_string()
    })
}

/// Fee choices for Bitcoin payments.
fn fee_params() -> Vec<Param> {
    vec![
        opt(
            "fee_rate",
            Kind::Text,
            "Bitcoin wallets only: the fee rate in satoshis per virtual byte, e.g. \"12.5\" (1 to 1000). Default: the network's estimate for confirmation_target.",
        ),
        opt(
            "confirmation_target",
            Kind::Integer,
            "Bitcoin wallets only: the number of blocks within which the payment should confirm, 1 to 1008. Default 6. Not allowed with fee_rate.",
        ),
    ]
}

fn with_fee_params(mut params: Vec<Param>) -> Vec<Param> {
    params.extend(fee_params());
    params
}

fn fee_from(a: &Args) -> Result<BitcoinFee, String> {
    Ok(BitcoinFee {
        fee_rate: a.opt_str("fee_rate")?.map(str::to_string),
        confirmation_target: a.opt_u64("confirmation_target")?,
    })
}

/// Fee choices exist only for Bitcoin payments; the other chains set their fees themselves.
fn reject_fee_options_for(a: &Args, chain: Blockchain) -> Result<(), String> {
    if a.opt_str("fee_rate")?.is_some() || a.opt_u64("confirmation_target")?.is_some() {
        return Err(format!(
            "'fee_rate' and 'confirmation_target' are for Bitcoin wallets; {} sets its fees itself",
            chain.display_name()
        ));
    }
    Ok(())
}

/// Fee choices for Solana (`priority_fee`) and Sui (`gas_budget`) transfers and swaps.
fn network_fee_params() -> Vec<Param> {
    vec![
        opt(
            "priority_fee",
            Kind::Text,
            "Solana wallets only: the total priority fee to pay on top of the base fee, in whole SOL, e.g. \"0.0001\" (at most 0.01 SOL; \"0\" pays none). It speeds up inclusion when the network is busy. Default: a fee that follows recent network demand (capped at 0.00005 SOL).",
        ),
        opt(
            "gas_budget",
            Kind::Text,
            "Sui wallets only: the gas budget, the most the transaction may spend on gas, in whole SUI, e.g. \"0.05\" (at most 50 SUI). Only what is used is charged. A budget too low for the transaction makes its dry run fail, so nothing is sent. Default: set from a dry run.",
        ),
    ]
}

fn with_network_fee_params(mut params: Vec<Param>) -> Vec<Param> {
    params.extend(network_fee_params());
    params
}

/// `priority_fee` exists only for Solana wallets and `gas_budget` only for Sui wallets.
fn reject_network_fee_options_for(a: &Args, chain: Blockchain) -> Result<(), String> {
    if chain != Blockchain::Solana && a.opt_str("priority_fee")?.is_some() {
        return Err(format!(
            "'priority_fee' is for Solana wallets; {} does not use it",
            chain.display_name()
        ));
    }
    if chain != Blockchain::Sui && a.opt_str("gas_budget")?.is_some() {
        return Err(format!(
            "'gas_budget' is for Sui wallets; {} does not use it",
            chain.display_name()
        ));
    }
    Ok(())
}

/// Sui, Solana and Bitcoin wallets are bound to one network, so a chain ID would be meaningless
/// (or a mistake).
fn reject_chain_id_for(a: &Args, chain: Blockchain) -> Result<(), String> {
    if a.opt_u64("chain_id")?.is_some() {
        return Err(format!(
            "'chain_id' is for Ethereum wallets; a {} wallet is bound to the network it was created for (see list_networks)",
            chain.display_name()
        ));
    }
    Ok(())
}

fn intent_params() -> Vec<Param> {
    vec![
        opt(
            "to",
            Kind::Text,
            "Recipient address, or the contract to call (Ethereum). Required, except for Sui and Solana transaction_bytes.",
        ),
        opt(
            "value",
            Kind::Text,
            "Ethereum only: native currency to send with a contract call, in whole units, e.g. \"0.01\". Not allowed with asset.",
        ),
        opt(
            "data",
            Kind::Text,
            "Ethereum only: hex calldata for a contract call, e.g. \"0xa9059cbb...\". Not allowed with asset.",
        ),
        opt(
            "asset",
            Kind::Text,
            "Instead of value/data: \"native\" or a token to transfer (an ERC-20 contract address on Ethereum; a coin type such as 0x2::sui::SUI on Sui; a token mint address on Solana).",
        ),
        opt(
            "amount",
            Kind::Text,
            "Amount of asset to transfer in whole units, e.g. \"1.5\". Required with asset.",
        ),
        opt(
            "transaction_bytes",
            Kind::Text,
            "Sui: a complete transaction as base64 BCS TransactionData (as built by a dapp or the Sui SDKs). Solana: a complete serialized transaction, base64 (as web3.js or a dapp produces it; its signature slots may be empty). Instead of asset + amount + to.",
        ),
    ]
}

fn network_params(extra: Vec<Param>) -> Vec<Param> {
    let mut p = vec![wallet(), password()];
    p.extend(extra);
    p.extend([
        req("network_name", Kind::Text, "Display name of the network, e.g. \"Base\"."),
        req("chain_id", Kind::Integer, "Chain ID of the network."),
        req("rpc_url", Kind::Text, "Default JSON-RPC URL (https, or http for localhost only)."),
        req("currency_symbol", Kind::Text, "Native currency symbol, e.g. \"ETH\"."),
        req("block_explorer_url", Kind::Text, "Block explorer base URL. Blockscout-compatible explorers also enable transaction history."),
    ]);
    p
}

/// Sui and Solana wallets sign transactions from `transaction_bytes`; Ethereum (and Bitcoin) have
/// no such input.
fn reject_transaction_bytes(a: &Args) -> Result<(), String> {
    if a.opt_str("transaction_bytes")?.is_some() {
        return Err("'transaction_bytes' is for Sui and Solana wallets".to_string());
    }
    Ok(())
}

fn require_swap_chain_ids(a: &Args) -> Result<(), String> {
    for key in ["from_chain_id", "to_chain_id"] {
        if a.opt_u64(key)?.is_none() {
            return Err(format!(
                "missing required argument '{key}' (required for Ethereum wallets)"
            ));
        }
    }
    Ok(())
}

fn intent_from(a: &Args) -> Result<IntentParams, String> {
    Ok(IntentParams {
        to: a.opt_str("to")?.map(str::to_string),
        value: a.opt_str("value")?.map(str::to_string),
        data: a.opt_str("data")?.map(str::to_string),
        asset: a.opt_str("asset")?.map(str::to_string),
        amount: a.opt_str("amount")?.map(str::to_string),
        transaction_bytes: a.opt_str("transaction_bytes")?.map(str::to_string),
        priority_fee: a.opt_str("priority_fee")?.map(str::to_string),
        gas_budget: a.opt_str("gas_budget")?.map(str::to_string),
    })
}

/// Ethereum swap parameters need chain IDs; Sui and Solana swaps leave them out.
fn swap_params(a: &Args, with_min_receive: bool) -> Result<SwapParams, String> {
    Ok(SwapParams {
        from_chain_id: a.opt_u64("from_chain_id")?.unwrap_or(0),
        from_token: a.str("from_token")?.to_string(),
        to_chain_id: a.opt_u64("to_chain_id")?.unwrap_or(0),
        to_token: a.str("to_token")?.to_string(),
        amount: a.str("amount")?.to_string(),
        slippage_bps: a.opt_u64("slippage_bps")?,
        to_address: a.opt_str("to_address")?.map(str::to_string),
        min_receive: if with_min_receive {
            a.opt_str("min_receive")?.map(str::to_string)
        } else {
            None
        },
        priority_fee: a.opt_str("priority_fee")?.map(str::to_string),
        gas_budget: a.opt_str("gas_budget")?.map(str::to_string),
    })
}

fn swap_inputs(with_min_receive: bool) -> Vec<Param> {
    let mut p = vec![
        wallet(),
        password(),
        opt(
            "from_chain_id",
            Kind::Integer,
            "Ethereum wallets (required): chain ID of the network the token you are selling is on; the wallet must have this network. Sui and Solana wallets: omit.",
        ),
        req(
            "from_token",
            Kind::Text,
            "The token to sell: \"native\" for the network's coin (ETH, SUI, SOL), an ERC-20 contract address on Ethereum, a coin type such as 0xdba3...::usdc::USDC on Sui, or a token mint address on Solana.",
        ),
        opt(
            "to_chain_id",
            Kind::Integer,
            "Ethereum wallets (required): chain ID of the network to receive on. Must equal from_chain_id: OpenOcean only swaps within one chain, so cross-chain swaps are not supported. Sui and Solana wallets: omit.",
        ),
        req(
            "to_token",
            Kind::Text,
            "The token to receive: \"native\", an ERC-20 contract address (Ethereum), a coin type (Sui) or a token mint address (Solana).",
        ),
        req(
            "amount",
            Kind::Text,
            "Amount of from_token to sell, in whole units, e.g. \"25.5\".",
        ),
        opt(
            "slippage_bps",
            Kind::Integer,
            "Maximum price slippage in basis points (100 = 1%): 5 to 1000. Default 100 (1%).",
        ),
        opt(
            "to_address",
            Kind::Text,
            "Send the bought tokens to this address on the same chain instead of to the wallet itself (Ethereum and Sui; not supported on Solana).",
        ),
    ];
    if with_min_receive {
        p.push(opt("min_receive", Kind::Text, "Safety floor in whole units of to_token. If the fresh quote's guaranteed minimum is lower, nothing is swapped. Use the minimum_amount from get_swap_quote."));
    }
    p.extend(network_fee_params());
    p
}

const BITCOIN_NO_SWAPS: &str = "swaps are not available for Bitcoin wallets (no aggregator serves native Bitcoin); use send_asset to pay a Bitcoin address";
const BITCOIN_NO_WALLETCONNECT: &str = "WalletConnect is not available for Bitcoin wallets; use send_asset, sign_personal_message and get_wallet_address directly";

fn network_from(a: &Args) -> Result<Network, String> {
    Network::new(
        a.str("network_name")?,
        a.u64("chain_id")?,
        a.str("rpc_url")?,
        a.str("currency_symbol")?,
        a.str("block_explorer_url")?,
    )
}

fn definitions() -> Vec<Tool> {
    vec![
        Tool {
            name: "get_info",
            description: "Returns metadata about agent-saifu: its description and version.",
            params: vec![],
            handler: |_, _| {
                Ok(Value::object(vec![
                    ("description", Value::str(metadata::DESCRIPTION)),
                    ("version", Value::str(metadata::VERSION)),
                ]))
            },
        },
        Tool {
            name: "create_wallet",
            description: "Create a new wallet with a fresh 24-word seed phrase, stored in a password-encrypted vault (PBKDF2-SHA256 + AES-256-GCM, like MetaMask). Works for Ethereum, Sui, Solana and Bitcoin. Several wallets can exist; wallet names are unique. Returns the public address. Use view_seed_phrase to back it up. A Sui wallet (Ed25519, derivation m/44'/784'/0'/0'/0'), a Solana wallet (Ed25519, derivation m/44'/501'/0'/0', the path Phantom uses) and a Bitcoin wallet (secp256k1, one native SegWit bc1q... address at the BIP 84 path m/84'/0'/0'/0/0) are each bound to one network, mainnet by default.",
            params: vec![
                req(
                    "blockchain",
                    Kind::Text,
                    "Blockchain to create the wallet on: \"ethereum\", \"sui\", \"solana\" or \"bitcoin\".",
                ),
                req(
                    "wallet_name",
                    Kind::Text,
                    "Unique name for the wallet (case-insensitive, max 64 characters).",
                ),
                req(
                    "password",
                    Kind::Text,
                    "Password that will unlock this wallet (min 8 characters).",
                ),
                opt(
                    "network",
                    Kind::Text,
                    "Sui, Solana and Bitcoin only: the network the wallet is bound to, \"mainnet\" (default), \"testnet\" or \"devnet\" (Solana: \"devnet\" or \"testnet\" are Solana's test clusters; Bitcoin: \"testnet\" is testnet4 and \"signet\" is also available, there is no devnet). Keep test keys separate from mainnet keys by creating separate wallets.",
                ),
            ],
            handler: |s, a| {
                s.create_wallet_on(
                    a.str("blockchain")?,
                    a.str("wallet_name")?,
                    a.str("password")?,
                    a.opt_str("network")?,
                )
            },
        },
        Tool {
            name: "import_wallet",
            description: "Add an existing wallet from a 24-word seed phrase or a private key (Ethereum, Sui, Solana or Bitcoin). Provide exactly one of seed_phrase or private_key. The secret is stored only inside a password-encrypted vault.",
            params: vec![
                opt(
                    "blockchain",
                    Kind::Text,
                    "Blockchain of the wallet: \"ethereum\" (default), \"sui\", \"solana\" or \"bitcoin\".",
                ),
                req("wallet_name", Kind::Text, "Unique name for the wallet."),
                req(
                    "password",
                    Kind::Text,
                    "Password that will unlock this wallet (min 8 characters).",
                ),
                opt("seed_phrase", Kind::Text, "24-word BIP39 seed phrase."),
                opt(
                    "private_key",
                    Kind::Text,
                    "Ethereum: hex private key (with or without 0x). Sui: a suiprivkey1... string (as shown by Sui wallets and the Sui CLI), or 64 hex characters; Ed25519 only. Solana: the base58 private key that Phantom or Solflare export, a Solana CLI keypair file's byte list ([12,34,...]), or hex of the 32-byte secret. Bitcoin: a compressed WIF key (starting with K or L; c on test networks) or 64 hex characters.",
                ),
                opt(
                    "network",
                    Kind::Text,
                    "Sui, Solana and Bitcoin only: the network the wallet is bound to, \"mainnet\" (default), \"testnet\" or \"devnet\" (Bitcoin: \"mainnet\", \"testnet\" or \"signet\").",
                ),
            ],
            handler: |s, a| {
                s.import_wallet_on(
                    a.opt_str("blockchain")?.unwrap_or("ethereum"),
                    a.str("wallet_name")?,
                    a.str("password")?,
                    a.opt_str("seed_phrase")?,
                    a.opt_str("private_key")?,
                    a.opt_str("network")?,
                )
            },
        },
        Tool {
            name: "list_wallets",
            description: "List every wallet's name and blockchain. No password needed; addresses and balances stay hidden until a wallet is unlocked.",
            params: vec![],
            handler: |s, _| s.list_wallets(),
        },
        Tool {
            name: "rename_wallet",
            description: "Rename a wallet. Requires the wallet's password. The new name must be unique.",
            params: vec![
                wallet(),
                password(),
                req("new_wallet_name", Kind::Text, "The new unique name."),
            ],
            handler: |s, a| {
                s.rename_wallet(
                    a.str("wallet_name")?,
                    a.str("new_wallet_name")?,
                    a.str("password")?,
                )
            },
        },
        Tool {
            name: "delete_wallet",
            description: "Permanently delete a wallet and its keys from this machine. Irreversible: back up the seed phrase first. Requires the password and confirm=true.",
            params: vec![
                wallet(),
                password(),
                req("confirm", Kind::Boolean, "Must be true to proceed."),
            ],
            handler: |s, a| {
                s.delete_wallet(
                    a.str("wallet_name")?,
                    a.str("password")?,
                    a.bool_or("confirm", false)?,
                )
            },
        },
        Tool {
            name: "get_wallet_address",
            description: "Show a wallet's public address (Ethereum, Sui, Solana or Bitcoin; a Bitcoin wallet has one native SegWit address).",
            params: vec![wallet(), password()],
            handler: |s, a| s.get_address(a.str("wallet_name")?, a.str("password")?),
        },
        Tool {
            name: "get_wallet_balances",
            description: "Show a wallet's assets: each asset's symbol, amount held, and USDC value. Ethereum wallets: every network the wallet uses (or one chain), with chain IDs; checks the native coin, well-known tokens, tokens added with add_token, and any token_contracts you list. Sui wallets: every coin type the address holds (coin objects and address balance combined) on the wallet's network. Solana wallets: SOL and every SPL Token / Token-2022 token the address holds on the wallet's network. Bitcoin wallets: BTC, with the confirmed balance and unconfirmed change shown separately.",
            params: vec![
                wallet(),
                password(),
                opt(
                    "chain_id",
                    Kind::Integer,
                    "Ethereum only: just this network. Omit for all networks.",
                ),
                opt(
                    "token_contracts",
                    Kind::TextList,
                    "Ethereum only: extra ERC-20 contract addresses to include once, without saving them (requires chain_id). Use add_token to save one.",
                ),
            ],
            handler: |s, a| {
                let (name, pw) = (a.str("wallet_name")?, a.str("password")?);
                let chain = s.chain_of(name, pw)?;
                if chain != Blockchain::Ethereum {
                    reject_chain_id_for(a, chain)?;
                    if !a.str_list("token_contracts")?.is_empty() {
                        return Err(format!(
                            "token_contracts is an Ethereum option; a {} wallet lists every token it holds",
                            chain.display_name()
                        ));
                    }
                    return match chain {
                        Blockchain::Solana => s.solana_balances(name, pw),
                        Blockchain::Bitcoin => s.bitcoin_balances(name, pw),
                        _ => s.sui_balances(name, pw),
                    };
                }
                let chain = a.opt_u64("chain_id")?;
                let tokens = a.str_list("token_contracts")?;
                if !tokens.is_empty() && chain.is_none() {
                    return Err("token_contracts requires chain_id".to_string());
                }
                let parsed = tokens
                    .iter()
                    .map(|t| parse_address(t).map_err(|e| format!("token_contracts: {e}")))
                    .collect::<Result<Vec<_>, _>>()?;
                s.get_balances(name, pw, chain, &parsed)
            },
        },
        Tool {
            name: "send_asset",
            description: "Send the native coin or a token (an ERC-20 on Ethereum; SUI or any coin type on Sui; SOL or any SPL / Token-2022 token on Solana; BTC on Bitcoin) to another address and broadcast the transaction. Irreversible. Use simulate_transaction first when unsure. Sui and Solana transfers are dry-run before they are signed. Bitcoin: the wallet chooses the coins, pays the change back to its own address, signs a replaceable (BIP 125) native SegWit transaction and checks it against Bitcoin Core's limits (dust, maximum fee) before broadcasting. Returns the transaction hash (Ethereum), digest (Sui), signature (Solana) or transaction id (Bitcoin).",
            params: with_network_fee_params(with_fee_params(vec![
                wallet(),
                password(),
                chain_id_for_eth(),
                req(
                    "asset",
                    Kind::Text,
                    "\"native\" for the network's coin (the only asset on Bitcoin), or the token: an ERC-20 contract address (Ethereum), a coin type such as 0xdba3...::usdc::USDC (Sui) or a token mint address such as EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v (Solana).",
                ),
                req(
                    "to",
                    Kind::Text,
                    "Recipient address (Sui: 0x plus 64 hex characters, written out in full; Solana: base58 wallet address. Sending a Solana token opens the recipient's token account if they have none, paid by this wallet; Bitcoin: an address of the wallet's own network: bc1q... / bc1p... / 1... / 3...; testnet and signet use tb1... / m... / n... / 2...).",
                ),
                req(
                    "amount",
                    Kind::Text,
                    "Amount in whole units, e.g. \"0.25\" (Bitcoin: in BTC, at most 8 decimals).",
                ),
                opt(
                    "gas_limit",
                    Kind::Integer,
                    "Ethereum only: override the automatically estimated gas limit.",
                ),
            ])),
            handler: |s, a| {
                let (name, pw) = (a.str("wallet_name")?, a.str("password")?);
                let chain = s.chain_of(name, pw)?;
                if chain != Blockchain::Bitcoin {
                    reject_fee_options_for(a, chain)?;
                }
                reject_network_fee_options_for(a, chain)?;
                if chain != Blockchain::Ethereum {
                    reject_chain_id_for(a, chain)?;
                    if a.opt_u64("gas_limit")?.is_some() {
                        return Err(format!(
                            "'gas_limit' is an Ethereum option; {} sets its fees itself",
                            chain.display_name()
                        ));
                    }
                    let (asset, to, amount) = (a.str("asset")?, a.str("to")?, a.str("amount")?);
                    return match chain {
                        Blockchain::Solana => s.solana_send_asset_with_priority(
                            name,
                            pw,
                            asset,
                            to,
                            amount,
                            a.opt_str("priority_fee")?,
                        ),
                        Blockchain::Bitcoin => {
                            s.bitcoin_send_asset(name, pw, asset, to, amount, &fee_from(a)?)
                        }
                        _ => s.sui_send_asset_with_budget(
                            name,
                            pw,
                            asset,
                            to,
                            amount,
                            a.opt_str("gas_budget")?,
                        ),
                    };
                }
                s.send_asset(
                    name,
                    pw,
                    eth_chain_id(a)?,
                    a.str("asset")?,
                    a.str("to")?,
                    a.str("amount")?,
                    a.opt_u64("gas_limit")?,
                )
            },
        },
        Tool {
            name: "sign_contract_transaction",
            description: "Sign a smart-contract transaction. Ethereum: calldata to a contract, optionally with value; known token calls (transfer/approve) are decoded with warnings, e.g. unlimited approvals. Sui: a complete transaction (transaction_bytes, base64 BCS TransactionData, e.g. a Move call built by a dapp or the Sui SDKs); it is decoded, dry-run, and refused unless this wallet is its sender. This is the Wallet Standard sui:signTransaction (broadcast=false) / sui:signAndExecuteTransaction (broadcast=true) equivalent. Solana: a complete serialized transaction (transaction_bytes, base64, legacy or version 0, e.g. built by a dapp or web3.js); every instruction is decoded in plain language, the transaction is simulated, and it is refused unless this wallet is one of its signers and its blockhash is valid on the wallet's network (the solana:signTransaction / signAndSendTransaction equivalent). By default it only signs; set broadcast=true to also send it. Bitcoin wallets do not use this tool (no PSBT or contract signing; use send_asset).",
            params: {
                let mut p = vec![wallet(), password(), chain_id_for_eth()];
                p.extend(intent_params());
                p.push(opt(
                    "gas_limit",
                    Kind::Integer,
                    "Ethereum only: override the automatically estimated gas limit.",
                ));
                p.push(opt(
                    "broadcast",
                    Kind::Boolean,
                    "Send the signed transaction. Default false.",
                ));
                p
            },
            handler: |s, a| {
                let (name, pw) = (a.str("wallet_name")?, a.str("password")?);
                let chain = s.chain_of(name, pw)?;
                if chain == Blockchain::Bitcoin {
                    return Err("Bitcoin wallets do not sign externally built transactions (PSBTs) or smart-contract calls; use send_asset to pay, and sign_personal_message to prove ownership of the address".to_string());
                }
                if chain != Blockchain::Ethereum {
                    reject_chain_id_for(a, chain)?;
                    let params = intent_from(a)?;
                    if params.to.is_some()
                        || params.value.is_some()
                        || params.data.is_some()
                        || params.asset.is_some()
                        || params.amount.is_some()
                        || a.opt_u64("gas_limit")?.is_some()
                    {
                        return Err(format!(
                            "for a {} wallet give only transaction_bytes (and broadcast); to, value, data, asset, amount and gas_limit are Ethereum options (use send_asset to build a transfer)",
                            chain.display_name()
                        ));
                    }
                    let bytes = params.transaction_bytes.as_deref().ok_or_else(|| {
                        format!(
                            "missing required argument 'transaction_bytes' (required for {} wallets)",
                            chain.display_name()
                        )
                    })?;
                    let broadcast = a.bool_or("broadcast", false)?;
                    return match chain {
                        Blockchain::Solana => s.solana_sign_transaction(name, pw, bytes, broadcast),
                        _ => s.sui_sign_transaction(name, pw, bytes, broadcast),
                    };
                }
                reject_transaction_bytes(a)?;
                s.sign_contract_transaction(
                    name,
                    pw,
                    eth_chain_id(a)?,
                    &intent_from(a)?,
                    a.opt_u64("gas_limit")?,
                    a.bool_or("broadcast", false)?,
                )
            },
        },
        Tool {
            name: "sign_personal_message",
            description: "Sign a message (the Wallet Standard sui:signPersonalMessage / personal_sign / Solana signMessage equivalent), e.g. to prove ownership of an address to a service. Ethereum: EIP-191 personal_sign. Sui: Sui's personal-message intent, so the signature can never be replayed as a transaction. Solana: a raw Ed25519 signature over the message bytes, as Phantom's signMessage does; bytes that are a valid Solana transaction are refused. Bitcoin: a BIP 322 \"simple\" signature of the wallet's native SegWit address (the message is bound to a virtual transaction that can never be valid on the network). Nothing is sent. Only sign messages you understand.",
            params: vec![
                wallet(),
                password(),
                req("message", Kind::Text, "The message to sign."),
                opt(
                    "message_format",
                    Kind::Text,
                    "How `message` is written: \"text\" (default, UTF-8), \"hex\" or \"base64\".",
                ),
            ],
            handler: |s, a| {
                let (name, pw) = (a.str("wallet_name")?, a.str("password")?);
                let (message, format) = (a.str("message")?, a.opt_str("message_format")?);
                match s.chain_of(name, pw)? {
                    Blockchain::Sui => s.sui_sign_message(name, pw, message, format),
                    Blockchain::Ethereum => s.eth_sign_message(name, pw, message, format),
                    Blockchain::Solana => s.solana_sign_message(name, pw, message, format),
                    Blockchain::Bitcoin => s.bitcoin_sign_message(name, pw, message, format),
                }
            },
        },
        Tool {
            name: "estimate_gas_fee",
            description: "Estimate the fee (in native currency and USDC) for a transaction: a transfer (asset+amount) or, on Ethereum, a contract call (data, value); on Sui and Solana, a transfer or given transaction_bytes. Bitcoin: the fee of a payment (to + amount) in BTC and USDC with the fee rate used, or without a payment the current fee rates by confirmation target and the cost of a typical payment. Nothing is signed or sent.",
            params: {
                let mut p = vec![wallet(), password(), chain_id_for_eth()];
                p.extend(intent_params());
                p.extend(fee_params());
                p.extend(network_fee_params());
                p
            },
            handler: |s, a| {
                let (name, pw) = (a.str("wallet_name")?, a.str("password")?);
                let chain = s.chain_of(name, pw)?;
                if chain != Blockchain::Bitcoin {
                    reject_fee_options_for(a, chain)?;
                }
                reject_network_fee_options_for(a, chain)?;
                if chain != Blockchain::Ethereum {
                    reject_chain_id_for(a, chain)?;
                    let params = intent_from(a)?;
                    return match chain {
                        Blockchain::Solana => s.solana_estimate_gas(name, pw, &params),
                        Blockchain::Bitcoin => {
                            s.bitcoin_estimate_gas(name, pw, &params, &fee_from(a)?)
                        }
                        _ => s.sui_estimate_gas(name, pw, &params),
                    };
                }
                reject_transaction_bytes(a)?;
                s.estimate_gas(name, pw, eth_chain_id(a)?, &intent_from(a)?)
            },
        },
        Tool {
            name: "simulate_transaction",
            description: "Dry-run a transaction against current chain state before signing: whether it would succeed or revert/fail (with reason), fees, and what it would do. Ethereum: also whether the wallet can afford it and a decoded description of token transfers/approvals. Sui and Solana: the wallet's balance changes (Solana: every instruction decoded when transaction_bytes is given). Bitcoin: there is no execution to run, so the payment (to + amount) is fully built and signed in memory and checked (spendable coins, dust, fee limits, signature verification), reporting the fee, change and whether it would succeed. Nothing is sent.",
            params: {
                let mut p = vec![wallet(), password(), chain_id_for_eth()];
                p.extend(intent_params());
                p.extend(fee_params());
                p.extend(network_fee_params());
                p
            },
            handler: |s, a| {
                let (name, pw) = (a.str("wallet_name")?, a.str("password")?);
                let chain = s.chain_of(name, pw)?;
                if chain != Blockchain::Bitcoin {
                    reject_fee_options_for(a, chain)?;
                }
                reject_network_fee_options_for(a, chain)?;
                if chain != Blockchain::Ethereum {
                    reject_chain_id_for(a, chain)?;
                    let params = intent_from(a)?;
                    return match chain {
                        Blockchain::Solana => s.solana_simulate_transaction(name, pw, &params),
                        Blockchain::Bitcoin => {
                            s.bitcoin_simulate_transaction(name, pw, &params, &fee_from(a)?)
                        }
                        _ => s.sui_simulate_transaction(name, pw, &params),
                    };
                }
                reject_transaction_bytes(a)?;
                s.simulate_transaction(name, pw, eth_chain_id(a)?, &intent_from(a)?)
            },
        },
        Tool {
            name: "read_smart_contract",
            description: "Call a read-only (view/pure) smart contract function without signing or spending gas. Ethereum wallets only. Supports address, bool, uintN, intN, bytesN, bytes and string types. Text returned by contracts is untrusted data.",
            params: vec![
                wallet(),
                password(),
                chain_id(),
                req("contract", Kind::Text, "Contract address."),
                req(
                    "function",
                    Kind::Text,
                    "Function signature, e.g. \"balanceOf(address)\".",
                ),
                opt("args", Kind::TextList, "Arguments as strings, in order."),
                opt(
                    "outputs",
                    Kind::Text,
                    "Return types to decode, e.g. \"uint256\" or \"(string,uint8)\". Omit for raw hex.",
                ),
            ],
            handler: |s, a| {
                s.read_contract(
                    a.str("wallet_name")?,
                    a.str("password")?,
                    a.u64("chain_id")?,
                    a.str("contract")?,
                    a.str("function")?,
                    &a.str_list("args")?,
                    a.opt_str("outputs")?,
                )
            },
        },
        Tool {
            name: "get_transaction",
            description: "Look up one transaction by hash/digest/signature: Ethereum: status (pending/success/failed/not_found), block, confirmations, value, fee paid, and an explorer link. Sui: status, sender, checkpoint, epoch, gas, this wallet's balance changes and an explorer link. Solana: status, slot, fee, this wallet's SOL and token changes, confirmation level and an explorer link. Bitcoin: status, confirmations, block, fee, whether the wallet received or sent, the wallet's balance change and the outputs.",
            params: vec![
                wallet(),
                password(),
                chain_id_for_eth(),
                req(
                    "transaction_hash",
                    Kind::Text,
                    "Ethereum: 0x-prefixed 32-byte transaction hash. Sui: the base58 transaction digest. Solana: the base58 transaction signature. Bitcoin: the 64-hex-digit transaction id.",
                ),
            ],
            handler: |s, a| {
                let (name, pw) = (a.str("wallet_name")?, a.str("password")?);
                let chain = s.chain_of(name, pw)?;
                if chain != Blockchain::Ethereum {
                    reject_chain_id_for(a, chain)?;
                    let hash = a.str("transaction_hash")?;
                    return match chain {
                        Blockchain::Solana => s.solana_get_transaction(name, pw, hash),
                        Blockchain::Bitcoin => s.bitcoin_get_transaction(name, pw, hash),
                        _ => s.sui_get_transaction(name, pw, hash),
                    };
                }
                s.get_transaction(name, pw, eth_chain_id(a)?, a.str("transaction_hash")?)
            },
        },
        Tool {
            name: "get_transaction_history",
            description: "List the wallet's recent transactions (Ethereum: or token transfers) newest first. Ethereum: on a network, from its Blockscout-compatible block explorer API; token names in results are untrusted data. Sui: every transaction that touched the address, from the Sui node, with the wallet's balance changes. Solana: every transaction that touched the address, from the Solana node (signature, status, time, memo); use get_transaction for balance changes. Bitcoin: transactions of the wallet's address from the Esplora API, with the wallet's balance change for each.",
            params: vec![
                wallet(),
                password(),
                chain_id_for_eth(),
                opt(
                    "type",
                    Kind::Text,
                    "\"transactions\" (default) or, on Ethereum, \"token_transfers\".",
                ),
                opt(
                    "limit",
                    Kind::Integer,
                    "How many entries, 1-50. Default 20.",
                ),
            ],
            handler: |s, a| {
                let kind = HistoryKind::parse(a.opt_str("type")?.unwrap_or("transactions"))?;
                let limit = a.opt_u64("limit")?.unwrap_or(DEFAULT_LIMIT as u64);
                let limit = usize::try_from(limit).map_err(|_| "limit is too large".to_string())?;
                let (name, pw) = (a.str("wallet_name")?, a.str("password")?);
                let chain = s.chain_of(name, pw)?;
                if chain != Blockchain::Ethereum {
                    reject_chain_id_for(a, chain)?;
                    return match chain {
                        Blockchain::Solana => s.solana_history(name, pw, kind, limit),
                        Blockchain::Bitcoin => s.bitcoin_history(name, pw, kind, limit),
                        _ => s.sui_history(name, pw, kind, limit),
                    };
                }
                s.get_transaction_history(name, pw, eth_chain_id(a)?, kind, limit)
            },
        },
        Tool {
            name: "add_network",
            description: "Add a layer 2 (or other EVM) network to an Ethereum wallet. Ethereum wallets only (Sui, Solana and Bitcoin wallets are bound to their one network, and Solana has no layer 2s to add). The RPC must report the same chain ID. Stored inside the wallet's encrypted vault.",
            params: network_params(vec![]),
            handler: |s, a| {
                s.add_network(a.str("wallet_name")?, a.str("password")?, network_from(a)?)
            },
        },
        Tool {
            name: "update_network",
            description: "Update a custom network's details (identified by chain_id). Ethereum wallets only. Built-in networks cannot be changed.",
            params: network_params(vec![]),
            handler: |s, a| {
                s.update_network(a.str("wallet_name")?, a.str("password")?, network_from(a)?)
            },
        },
        Tool {
            name: "delete_network",
            description: "Delete a custom network from a wallet. Ethereum wallets only. Built-in networks cannot be deleted.",
            params: vec![wallet(), password(), chain_id()],
            handler: |s, a| {
                s.delete_network(
                    a.str("wallet_name")?,
                    a.str("password")?,
                    a.u64("chain_id")?,
                )
            },
        },
        Tool {
            name: "get_swap_quote",
            description: "Preview a swap before doing it, with nothing signed or sent: how much of the new token you would get (expected and guaranteed minimum), the fees, the route, price impact/deviation, and whether more balance is needed. Ethereum wallets: powered by OpenOcean (service fee of 0.7% of the amount sold; estimated network fee; whether an approval is needed); needs no API key; within one chain only, so to_chain_id must equal from_chain_id. Sui wallets (mainnet): powered by the Cetus aggregator (routes over Cetus pools only; the route is verified against Cetus's on-chain packages and the transaction is dry-run); the service fee is taken from the coins produced. Solana wallets (mainnet): powered by Jupiter (keyless, no API key); the wallet assembles the transaction from Jupiter's swap instructions, checks it and simulates it, and the quote is only reported if the simulation shows the wallet spends no more than requested and receives at least minimum_amount. The service fee is taken from the tokens bought.",
            params: swap_inputs(false),
            handler: |s, a| {
                let (name, pw) = (a.str("wallet_name")?, a.str("password")?);
                let params = swap_params(a, false)?;
                let chain = s.chain_of(name, pw)?;
                reject_network_fee_options_for(a, chain)?;
                match chain {
                    Blockchain::Sui => s.sui_swap_quote(name, pw, &params),
                    Blockchain::Solana => s.solana_swap_quote(name, pw, &params),
                    Blockchain::Bitcoin => Err(BITCOIN_NO_SWAPS.to_string()),
                    Blockchain::Ethereum => {
                        require_swap_chain_ids(a)?;
                        s.get_swap_quote(name, pw, &params)
                    }
                }
            },
        },
        Tool {
            name: "swap_tokens",
            description: "Swap one token for another on one chain and broadcast it. Irreversible. Call get_swap_quote first and pass its minimum_amount as min_receive. Ethereum: through OpenOcean; if the sold token needs an approval, exactly the sold amount is approved first (and confirmed) before the swap is sent; cross-chain swaps are not supported. Sui (mainnet): through the Cetus aggregator in one transaction whose contract aborts it on-chain if the result would be below the minimum; it is dry-run before signing. Solana (mainnet): through Jupiter; the transaction is signed only after a simulation shows the wallet spends no more than requested, receives at least the quoted minimum and loses nothing else. Returns the transaction hash/digest/signature.",
            params: swap_inputs(true),
            handler: |s, a| {
                let (name, pw) = (a.str("wallet_name")?, a.str("password")?);
                let params = swap_params(a, true)?;
                let chain = s.chain_of(name, pw)?;
                reject_network_fee_options_for(a, chain)?;
                match chain {
                    Blockchain::Sui => s.sui_swap_tokens(name, pw, &params),
                    Blockchain::Solana => s.solana_swap_tokens(name, pw, &params),
                    Blockchain::Bitcoin => Err(BITCOIN_NO_SWAPS.to_string()),
                    Blockchain::Ethereum => {
                        require_swap_chain_ids(a)?;
                        s.swap_tokens(name, pw, &params)
                    }
                }
            },
        },
        Tool {
            name: "add_token",
            description: "Add an ERC-20 token to an Ethereum wallet on a network, by contract address, so it always appears in get_wallet_balances (even at zero). Ethereum wallets only (Sui and Solana wallets list every coin or token they hold automatically, and a Bitcoin wallet has only BTC). The contract is checked on-chain (decimals and symbol are read from it). Stored inside the wallet's encrypted vault; up to 50 added tokens per network.",
            params: vec![
                wallet(),
                password(),
                chain_id(),
                req(
                    "contract",
                    Kind::Text,
                    "ERC-20 token contract address on that network.",
                ),
            ],
            handler: |s, a| {
                s.add_token(
                    a.str("wallet_name")?,
                    a.str("password")?,
                    a.u64("chain_id")?,
                    a.str("contract")?,
                )
            },
        },
        Tool {
            name: "remove_token",
            description: "Remove a token previously added with add_token from an Ethereum wallet on a network. Ethereum wallets only. This only stops tracking it; no funds move. Built-in well-known tokens cannot be removed.",
            params: vec![
                wallet(),
                password(),
                chain_id(),
                req("contract", Kind::Text, "Token contract address to remove."),
            ],
            handler: |s, a| {
                s.remove_token(
                    a.str("wallet_name")?,
                    a.str("password")?,
                    a.u64("chain_id")?,
                    a.str("contract")?,
                )
            },
        },
        Tool {
            name: "list_networks",
            description: "List the networks a wallet can use with their URLs. Ethereum: built-in and custom networks with chain IDs and RPC/explorer URLs. Sui, Solana and Bitcoin: the one network the wallet is bound to.",
            params: vec![wallet(), password()],
            handler: |s, a| s.view_networks(a.str("wallet_name")?, a.str("password")?),
        },
        Tool {
            name: "view_seed_phrase",
            description: "Reveal a wallet's 24-word seed phrase. Sensitive: anyone who sees it controls the wallet. Unavailable for wallets imported from a private key.",
            params: vec![wallet(), password()],
            handler: |s, a| s.view_seed_phrase(a.str("wallet_name")?, a.str("password")?),
        },
        Tool {
            name: "view_private_key",
            description: "Reveal a wallet's private key (Sui: as a suiprivkey1... string, plus hex; Solana: as the base58 keypair that Phantom and Solflare import, plus the 32-byte secret in hex; Bitcoin: as a compressed WIF key, plus hex). Sensitive: anyone who sees it controls the wallet.",
            params: vec![wallet(), password()],
            handler: |s, a| s.view_private_key(a.str("wallet_name")?, a.str("password")?),
        },
        Tool {
            name: "export_wallet",
            description: "Export a wallet's seed phrase and private key to a ZIP file in the exports folder. The ZIP's content is encrypted with the given key using MetaMask's vault scheme (PBKDF2-SHA256 + AES-256-GCM). Never overwrites existing files.",
            params: vec![
                wallet(),
                req("password", Kind::Text, "The wallet's current password."),
                req(
                    "encryption_key",
                    Kind::Text,
                    "Key that encrypts the export (min 12 characters). Needed to read the export; keep it safe.",
                ),
                opt(
                    "file_name",
                    Kind::Text,
                    "Name for the .zip file (letters, digits, . _ -). Default is generated.",
                ),
            ],
            handler: |s, a| {
                s.export_wallet(
                    a.str("wallet_name")?,
                    a.str("password")?,
                    a.str("encryption_key")?,
                    a.opt_str("file_name")?,
                )
            },
        },
        Tool {
            name: "revoke_token_approval",
            description: "Revoke an ERC-20 allowance previously granted to a smart contract (sets it to zero). Ethereum wallets only (Sui has no token allowances). Does nothing if the allowance is already zero. Broadcasts by default.",
            params: vec![
                wallet(),
                password(),
                chain_id(),
                req("token", Kind::Text, "ERC-20 token contract address."),
                req("spender", Kind::Text, "Address whose allowance to revoke."),
                opt(
                    "broadcast",
                    Kind::Boolean,
                    "Send the transaction. Default true; false only signs.",
                ),
            ],
            handler: |s, a| {
                s.revoke_token_approval(
                    a.str("wallet_name")?,
                    a.str("password")?,
                    a.u64("chain_id")?,
                    a.str("token")?,
                    a.str("spender")?,
                    a.bool_or("broadcast", true)?,
                )
            },
        },
        Tool {
            name: "wc_pair",
            description: "Connect to a dapp with WalletConnect (Ethereum dapps with an Ethereum wallet, Sui dapps with a Sui wallet, and Solana dapps with a Solana wallet; the Sui methods are sui_getAccounts, sui_signTransaction, sui_signAndExecuteTransaction and sui_signPersonalMessage; the Solana methods are solana_getAccounts, solana_requestAccounts, solana_signMessage, solana_signTransaction, solana_signAndSendTransaction and solana_signAllTransactions). Give it the connection URI the dapp shows (starts with \"wc:\"; the text behind its QR code). Waits for the dapp's session proposal and returns what the dapp is asking for. Nothing is connected until you call wc_approve_session. No wallet is unlocked.",
            params: vec![
                req(
                    "uri",
                    Kind::Text,
                    "The WalletConnect v2 pairing URI, e.g. wc:<topic>@2?relay-protocol=irn&symKey=...",
                ),
                opt(
                    "wait_seconds",
                    Kind::Integer,
                    "How long to wait for the dapp's proposal (default 20, max 60).",
                ),
            ],
            handler: |s, a| s.wc_pair(a.str("uri")?, a.opt_u64("wait_seconds")?),
        },
        Tool {
            name: "wc_list_proposals",
            description: "List the dapp connection proposals waiting for an answer (what each dapp asks for, and whether this wallet can satisfy it). Proposals expire after 5 minutes.",
            params: vec![],
            handler: |s, _| s.wc_list_proposals(),
        },
        Tool {
            name: "wc_approve_session",
            description: "Approve a dapp's connection proposal with an Ethereum, a Sui or a Solana wallet (wc_list_proposals shows which the dapp can use). This reveals the wallet's address to the dapp, so it needs the wallet name and the password. Sui and Solana wallets: omit chain_id and additional_chain_ids; the dapp is connected on the wallet's own network (e.g. sui:mainnet, or solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp for Solana mainnet), which the dapp must accept, and the methods it asked for that this wallet supports are granted (Sui: sui_signTransaction, sui_signAndExecuteTransaction, sui_signPersonalMessage and sui_getAccounts; Solana: solana_signMessage, solana_signTransaction, solana_signAndSendTransaction, solana_signAllTransactions, solana_getAccounts and solana_requestAccounts). Ethereum wallets: chain_id plus additional_chain_ids must cover every chain the dapp requires (see wc_list_proposals) and may only include chains the dapp asked for; the wallet must have a network for each. Only the methods this wallet supports are granted (transactions, personal_sign, typed data); eth_sign is never granted.",
            params: vec![
                req(
                    "proposal_id",
                    Kind::Text,
                    "The proposal_id from wc_pair or wc_list_proposals.",
                ),
                wallet(),
                chain_id_for_eth(),
                opt(
                    "additional_chain_ids",
                    Kind::TextList,
                    "Further chain IDs to share with the dapp (required by it or optional). Ethereum wallets only.",
                ),
                password(),
            ],
            handler: |s, a| {
                let extra = a
                    .str_list("additional_chain_ids")?
                    .iter()
                    .map(|c| {
                        c.trim()
                            .parse::<u64>()
                            .map_err(|_| format!("'{c}' is not a valid chain ID"))
                    })
                    .collect::<Result<Vec<u64>, String>>()?;
                let (name, password) = (a.str("wallet_name")?, a.str("password")?);
                let chain = s.chain_of(name, password)?;
                if chain == Blockchain::Bitcoin {
                    return Err(BITCOIN_NO_WALLETCONNECT.to_string());
                }
                if chain != Blockchain::Ethereum {
                    reject_chain_id_for(a, chain)?;
                    if !extra.is_empty() {
                        return Err(format!(
                            "'additional_chain_ids' is for Ethereum wallets; a {} wallet is bound to one network",
                            chain.display_name()
                        ));
                    }
                    let proposal = a.u64("proposal_id")?;
                    return match chain {
                        Blockchain::Solana => s.wc_approve_session_solana(proposal, name, password),
                        _ => s.wc_approve_session_sui(proposal, name, password),
                    };
                }
                s.wc_approve_session(
                    a.u64("proposal_id")?,
                    name,
                    eth_chain_id(a)?,
                    &extra,
                    password,
                )
            },
        },
        Tool {
            name: "wc_reject_session",
            description: "Decline a dapp's connection proposal. Nothing is shared with the dapp.",
            params: vec![req(
                "proposal_id",
                Kind::Text,
                "The proposal_id to decline.",
            )],
            handler: |s, a| s.wc_reject_session(a.u64("proposal_id")?),
        },
        Tool {
            name: "wc_list_sessions",
            description: "List the dapps currently connected (wallet, chains, permitted methods). Addresses are not shown. Sessions last up to 7 days but are kept only while this server runs.",
            params: vec![],
            handler: |s, _| s.wc_list_sessions(),
        },
        Tool {
            name: "wc_disconnect_session",
            description: "Disconnect a dapp. It can no longer send requests and is told the session ended.",
            params: vec![req(
                "session_topic",
                Kind::Text,
                "The session_topic from wc_list_sessions (a unique prefix of at least 8 characters works).",
            )],
            handler: |s, a| s.wc_disconnect_session(a.str("session_topic")?),
        },
        Tool {
            name: "wc_list_requests",
            description: "List the requests connected dapps are waiting on: transactions to send or sign, messages and typed data to sign, and Sui and Solana account queries. Shows the dapp, method, chain and a summary. Use wc_review_request for the full costs and expected outcome. Requests expire after about 5 minutes.",
            params: vec![],
            handler: |s, _| s.wc_list_requests(),
        },
        Tool {
            name: "wc_review_request",
            description: "Review a dapp request before answering it. Unlocks the wallet for this call. For transactions it dry-runs the transaction and reports whether it would succeed, what it sends or approves (token transfers and approvals are decoded), the network fee and whether the wallet can afford it. For messages and typed data it shows exactly what would be signed, with warnings. Signs and sends nothing. Sui and Solana wallets: omit chain_id; the dry run, decoded commands or instructions and balance changes are shown for transaction requests.",
            params: vec![
                req(
                    "request_id",
                    Kind::Text,
                    "The request_id from wc_list_requests.",
                ),
                wallet(),
                chain_id_for_eth(),
                password(),
            ],
            handler: |s, a| {
                let (name, password) = (a.str("wallet_name")?, a.str("password")?);
                let chain = s.chain_of(name, password)?;
                if chain == Blockchain::Bitcoin {
                    return Err(BITCOIN_NO_WALLETCONNECT.to_string());
                }
                if chain != Blockchain::Ethereum {
                    reject_chain_id_for(a, chain)?;
                    let request = a.u64("request_id")?;
                    return match chain {
                        Blockchain::Solana => s.wc_review_request_solana(request, name, password),
                        _ => s.wc_review_request_sui(request, name, password),
                    };
                }
                s.wc_review_request(a.u64("request_id")?, name, eth_chain_id(a)?, password)
            },
        },
        Tool {
            name: "wc_approve_request",
            description: "Approve a dapp request: sign it and send the answer back to the dapp. eth_sendTransaction is signed AND BROADCAST (spends funds); eth_signTransaction only signs; personal_sign and typed data produce a signature. The wallet name must be the wallet the dapp is connected to, and (Ethereum) chain_id must be the request's chain. Review the request first with wc_review_request. A transaction that would fail is not sent. Sui wallets: omit chain_id; sui_signTransaction returns {signature, transactionBytes}, sui_signAndExecuteTransaction signs AND SUBMITS and returns {digest}, sui_signPersonalMessage returns {signature}, and sui_getAccounts returns the wallet's address and public key. The request must be for the wallet's own network and name its address. Solana wallets: omit chain_id; solana_signTransaction returns {signature, transaction}, solana_signAllTransactions returns {transactions}, solana_signAndSendTransaction signs AND SUBMITS and returns {signature}, solana_signMessage returns {signature} (base58; bytes that form a valid transaction are refused), and solana_getAccounts / solana_requestAccounts return the wallet's address. The request must be for the wallet's own Solana network.",
            params: vec![
                req(
                    "request_id",
                    Kind::Text,
                    "The request_id from wc_list_requests.",
                ),
                wallet(),
                chain_id_for_eth(),
                password(),
            ],
            handler: |s, a| {
                let (name, password) = (a.str("wallet_name")?, a.str("password")?);
                let chain = s.chain_of(name, password)?;
                if chain == Blockchain::Bitcoin {
                    return Err(BITCOIN_NO_WALLETCONNECT.to_string());
                }
                if chain != Blockchain::Ethereum {
                    reject_chain_id_for(a, chain)?;
                    let request = a.u64("request_id")?;
                    return match chain {
                        Blockchain::Solana => s.wc_approve_request_solana(request, name, password),
                        _ => s.wc_approve_request_sui(request, name, password),
                    };
                }
                s.wc_approve_request(a.u64("request_id")?, name, eth_chain_id(a)?, password)
            },
        },
        Tool {
            name: "wc_reject_request",
            description: "Reject a dapp request. Nothing is signed; the dapp is told the user rejected it. No password needed.",
            params: vec![
                req("request_id", Kind::Text, "The request_id to reject."),
                opt(
                    "reason",
                    Kind::Text,
                    "Optional short reason shown to the dapp.",
                ),
            ],
            handler: |s, a| s.wc_reject_request(a.u64("request_id")?, a.opt_str("reason")?),
        },
    ]
}

fn kind_schema(p: &Param) -> Value {
    let mut pairs = vec![("description", Value::str(p.description))];
    match p.kind {
        Kind::Text => pairs.insert(0, ("type", Value::str("string"))),
        Kind::Integer => pairs.insert(0, ("type", Value::str("integer"))),
        Kind::Boolean => pairs.insert(0, ("type", Value::str("boolean"))),
        Kind::TextList => {
            pairs.insert(0, ("type", Value::str("array")));
            pairs.push(("items", Value::object(vec![("type", Value::str("string"))])));
        }
    }
    Value::object(pairs)
}

impl Tool {
    fn schema(&self) -> Value {
        let properties = Value::Object(
            self.params
                .iter()
                .map(|p| (p.name.to_string(), kind_schema(p)))
                .collect(),
        );
        let required: Vec<Value> = self
            .params
            .iter()
            .filter(|p| p.required)
            .map(|p| Value::str(p.name))
            .collect();
        Value::object(vec![
            ("type", Value::str("object")),
            ("properties", properties),
            ("required", Value::Array(required)),
            ("additionalProperties", Value::Bool(false)),
        ])
    }

    fn descriptor(&self) -> Value {
        Value::object(vec![
            ("name", Value::str(self.name)),
            ("description", Value::str(self.description)),
            ("inputSchema", self.schema()),
        ])
    }

    /// Rejects unknown arguments and arguments of the wrong type before any handler runs, so
    /// malformed calls are reported without touching a wallet.
    fn check_arguments(&self, args: &Value) -> Result<(), String> {
        let Value::Object(pairs) = args else {
            return Ok(());
        };
        for (key, _) in pairs {
            if !self.params.iter().any(|p| p.name == key) {
                return Err(format!(
                    "unknown argument '{}'",
                    crate::eth::explorer::sanitize_text(key)
                ));
            }
        }
        let typed = Args(args);
        for p in &self.params {
            match p.kind {
                Kind::Text => typed.opt_str(p.name).map(drop)?,
                Kind::Integer => typed.opt_u64(p.name).map(drop)?,
                Kind::Boolean => typed.bool_or(p.name, false).map(drop)?,
                Kind::TextList => typed.str_list(p.name).map(drop)?,
            }
        }
        Ok(())
    }
}

/// Holds the wallet service and exposes every MCP tool.
pub struct Toolbox {
    service: WalletService,
    tools: Vec<Tool>,
}

impl Toolbox {
    pub fn new(service: WalletService) -> Toolbox {
        Toolbox {
            service,
            tools: definitions(),
        }
    }

    /// The wallet service behind the tools, for apps that also use its library-only calls
    /// (such as `WalletService::public_view`).
    pub fn service(&self) -> &WalletService {
        &self.service
    }

    pub fn list(&self) -> Value {
        Value::object(vec![(
            "tools",
            Value::Array(self.tools.iter().map(Tool::descriptor).collect()),
        )])
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|t| t.name).collect()
    }

    /// Runs a tool and returns the MCP tool result; `None` if no such tool exists.
    pub fn call(&self, name: &str, arguments: Option<&Value>) -> Option<Value> {
        let tool = self.tools.iter().find(|t| t.name == name)?;
        let empty = Value::Object(vec![]);
        let args = arguments.unwrap_or(&empty);
        let outcome = match args {
            Value::Object(_) => tool
                .check_arguments(args)
                .and_then(|_| (tool.handler)(&self.service, &Args(args))),
            _ => Err("arguments must be an object".to_string()),
        };
        Some(match outcome {
            Ok(mut value) => {
                let text = value.to_json();
                value.wipe();
                tool_result(text, false)
            }
            Err(message) => tool_result(message, true),
        })
    }
}

fn tool_result(text: String, is_error: bool) -> Value {
    Value::object(vec![
        (
            "content",
            Value::Array(vec![Value::object(vec![
                ("type", Value::str("text")),
                ("text", Value::String(text)),
            ])]),
        ),
        ("isError", Value::Bool(is_error)),
    ])
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use crate::http::mock::RouterMock;
    use crate::service::testing::{Fixture, fixture, fixture_with};

    pub fn toolbox_fixture() -> (Fixture, Toolbox) {
        wrap(fixture())
    }

    pub fn toolbox_with(router: RouterMock) -> (Fixture, Toolbox) {
        wrap(fixture_with(router))
    }

    fn wrap(f: Fixture) -> (Fixture, Toolbox) {
        let http: std::rc::Rc<dyn crate::http::HttpClient> = f.router.clone();
        let store = crate::store::Store::open(&f.dir.0).unwrap();
        let service = WalletService::new(store, http).with_iterations(crate::vault::MIN_ITERATIONS);
        (f, Toolbox::new(service))
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::json;

    fn call(tb: &Toolbox, name: &str, args: &str) -> (bool, Value) {
        let args = json::parse(args).unwrap();
        let result = tb.call(name, Some(&args)).expect("tool exists");
        let is_error = result.get("isError") == Some(&Value::Bool(true));
        let text = result.get("content").and_then(Value::as_array).unwrap()[0]
            .get("text")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let body = json::parse(&text).unwrap_or(Value::String(text));
        (is_error, body)
    }

    #[test]
    fn exposes_every_required_tool() {
        let (_f, tb) = toolbox_fixture();
        let names = tb.names();
        for expected in [
            "get_info",
            "create_wallet",
            "import_wallet",
            "list_wallets",
            "rename_wallet",
            "delete_wallet",
            "get_wallet_address",
            "get_wallet_balances",
            "send_asset",
            "sign_contract_transaction",
            "estimate_gas_fee",
            "simulate_transaction",
            "read_smart_contract",
            "get_transaction",
            "get_transaction_history",
            "add_network",
            "update_network",
            "delete_network",
            "list_networks",
            "add_token",
            "remove_token",
            "get_swap_quote",
            "swap_tokens",
            "view_seed_phrase",
            "view_private_key",
            "export_wallet",
            "revoke_token_approval",
            "wc_pair",
            "wc_list_proposals",
            "wc_approve_session",
            "wc_reject_session",
            "wc_list_sessions",
            "wc_disconnect_session",
            "wc_list_requests",
            "wc_review_request",
            "wc_approve_request",
            "wc_reject_request",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate tool names");
    }

    #[test]
    fn schemas_are_well_formed() {
        let (_f, tb) = toolbox_fixture();
        let list = tb.list();
        for tool in list.get("tools").and_then(Value::as_array).unwrap() {
            let name = tool.get("name").and_then(Value::as_str).unwrap();
            assert!(
                !tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap()
                    .is_empty(),
                "{name}"
            );
            let schema = tool.get("inputSchema").unwrap();
            assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
            let props = match schema.get("properties") {
                Some(Value::Object(p)) => p.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
                _ => panic!("{name}: no properties"),
            };
            for r in schema.get("required").and_then(Value::as_array).unwrap() {
                assert!(
                    props.contains(&r.as_str().unwrap().to_string()),
                    "{name}: required {r:?} not in properties"
                );
            }
            let mut dedup = props.clone();
            dedup.sort();
            dedup.dedup();
            assert_eq!(dedup.len(), props.len(), "{name}: duplicate params");
            if props.iter().any(|p| p == "password") {
                assert!(
                    schema
                        .get("required")
                        .and_then(Value::as_array)
                        .unwrap()
                        .iter()
                        .any(|r| r.as_str() == Some("password")),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn every_tool_requiring_a_wallet_also_requires_its_password() {
        let (_f, tb) = toolbox_fixture();
        let no_password = ["get_info", "list_wallets", "create_wallet", "import_wallet"];
        for tool in &tb.tools {
            let has_wallet = tool.params.iter().any(|p| p.name == "wallet_name");
            let has_pw = tool
                .params
                .iter()
                .any(|p| p.name == "password" && p.required);
            if no_password.contains(&tool.name) {
                continue;
            }
            assert!(
                !has_wallet || has_pw,
                "{} must require a password",
                tool.name
            );
        }
    }

    #[test]
    fn get_info_matches_the_spec_text() {
        let (_f, tb) = toolbox_fixture();
        let (err, body) = call(&tb, "get_info", "{}");
        assert!(!err);
        assert_eq!(
            body.get("description").and_then(Value::as_str),
            Some("All-in-one crypto wallet for Agents. Built from the ground up in Rust.")
        );
        assert_eq!(
            body.get("version").and_then(Value::as_str),
            Some("Version 0.0")
        );
    }

    #[test]
    fn end_to_end_wallet_flow_through_tools() {
        let (_f, tb) = toolbox_fixture();
        let (err, created) = call(
            &tb,
            "create_wallet",
            r#"{"blockchain":"ethereum","wallet_name":"Daniel's wallet","password":"hunter2-hunter2"}"#,
        );
        assert!(!err, "{}", created.to_json());
        let address = created
            .get("address")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();

        let (_, listed) = call(&tb, "list_wallets", "{}");
        assert!(listed.to_json().contains("Daniel's wallet"));
        assert!(!listed.to_json().contains(&address));

        let (err, shown) = call(
            &tb,
            "get_wallet_address",
            r#"{"wallet_name":"Daniel's wallet","password":"hunter2-hunter2"}"#,
        );
        assert!(!err);
        assert_eq!(
            shown.get("address").and_then(Value::as_str),
            Some(address.as_str())
        );

        let (err, seed) = call(
            &tb,
            "view_seed_phrase",
            r#"{"wallet_name":"Daniel's wallet","password":"hunter2-hunter2"}"#,
        );
        assert!(!err);
        assert_eq!(
            seed.get("seed_phrase")
                .and_then(Value::as_str)
                .unwrap()
                .split(' ')
                .count(),
            24
        );

        let (err, renamed) = call(
            &tb,
            "rename_wallet",
            r#"{"wallet_name":"Daniel's wallet","new_wallet_name":"Main","password":"hunter2-hunter2"}"#,
        );
        assert!(!err, "{}", renamed.to_json());
        let (err, _) = call(
            &tb,
            "delete_wallet",
            r#"{"wallet_name":"Main","password":"hunter2-hunter2","confirm":true}"#,
        );
        assert!(!err);
        let (_, listed) = call(&tb, "list_wallets", "{}");
        assert_eq!(
            listed
                .get("wallets")
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn errors_are_reported_as_tool_errors_not_protocol_errors() {
        let (_f, tb) = toolbox_fixture();
        let (err, msg) = call(
            &tb,
            "get_wallet_address",
            r#"{"wallet_name":"nope","password":"whatever-pass"}"#,
        );
        assert!(err);
        assert!(msg.as_str().unwrap().contains("no wallet"));
    }

    #[test]
    fn argument_validation() {
        let (_f, tb) = toolbox_fixture();
        let (err, msg) = call(
            &tb,
            "create_wallet",
            r#"{"blockchain":"ethereum","wallet_name":"w"}"#,
        );
        assert!(
            err && msg
                .as_str()
                .unwrap()
                .contains("missing required argument 'password'")
        );
        let (err, msg) = call(
            &tb,
            "create_wallet",
            r#"{"blockchain":"ethereum","wallet_name":"w","password":"long-enough-pw","extra":1}"#,
        );
        assert!(err && msg.as_str().unwrap().contains("unknown argument"));
        let (err, msg) = call(
            &tb,
            "create_wallet",
            r#"{"blockchain":5,"wallet_name":"w","password":"long-enough-pw"}"#,
        );
        assert!(err && msg.as_str().unwrap().contains("must be a string"));
        let (err, msg) = call(
            &tb,
            "send_asset",
            r#"{"wallet_name":"w","password":"p","chain_id":"abc","asset":"native","to":"0x","amount":"1"}"#,
        );
        assert!(err && msg.as_str().unwrap().contains("non-negative integer"));
        let (err, _) = call(
            &tb,
            "delete_wallet",
            r#"{"wallet_name":"w","password":"p","confirm":"yes"}"#,
        );
        assert!(err);
        let bad_args = tb.call("get_info", Some(&Value::str("x"))).unwrap();
        assert_eq!(bad_args.get("isError"), Some(&Value::Bool(true)));
        assert!(tb.call("nonexistent", None).is_none());
    }

    #[test]
    fn chain_id_accepts_numbers_and_numeric_strings() {
        let (_f, tb) = toolbox_fixture();
        call(
            &tb,
            "create_wallet",
            r#"{"blockchain":"ethereum","wallet_name":"w","password":"long-enough-pw"}"#,
        );
        for chain in ["1", "\"1\""] {
            let (err, body) = call(
                &tb,
                "estimate_gas_fee",
                &format!(
                    r#"{{"wallet_name":"w","password":"long-enough-pw","chain_id":{chain},"to":"0x2222222222222222222222222222222222222222","value":"0.1"}}"#
                ),
            );
            assert!(!err, "{}", body.to_json());
        }
    }

    #[test]
    fn balances_tool_requires_chain_for_extra_tokens() {
        let (_f, tb) = toolbox_fixture();
        call(
            &tb,
            "create_wallet",
            r#"{"blockchain":"ethereum","wallet_name":"w","password":"long-enough-pw"}"#,
        );
        let (err, msg) = call(
            &tb,
            "get_wallet_balances",
            r#"{"wallet_name":"w","password":"long-enough-pw","token_contracts":["0x2222222222222222222222222222222222222222"]}"#,
        );
        assert!(err && msg.as_str().unwrap().contains("requires chain_id"));
        let (err, msg) = call(
            &tb,
            "get_wallet_balances",
            r#"{"wallet_name":"w","password":"long-enough-pw","chain_id":1,"token_contracts":["nope"]}"#,
        );
        assert!(err && msg.as_str().unwrap().contains("token_contracts"));
    }

    #[test]
    fn token_tools_require_wallet_password_chain_and_contract() {
        let (_f, tb) = toolbox_fixture();
        for name in ["add_token", "remove_token"] {
            let tool = tb.tools.iter().find(|t| t.name == name).unwrap();
            for required in ["wallet_name", "password", "chain_id", "contract"] {
                assert!(
                    tool.params.iter().any(|p| p.name == required && p.required),
                    "{name} must require {required}"
                );
            }
            assert_eq!(tool.params.len(), 4, "{name}");
        }
        call(
            &tb,
            "create_wallet",
            r#"{"blockchain":"ethereum","wallet_name":"w","password":"long-enough-pw"}"#,
        );
        let (err, msg) = call(
            &tb,
            "add_token",
            r#"{"wallet_name":"w","password":"long-enough-pw","contract":"0x2222222222222222222222222222222222222222"}"#,
        );
        assert!(err && msg.as_str().unwrap().contains("'chain_id'"));
        let (err, msg) = call(
            &tb,
            "remove_token",
            r#"{"wallet_name":"w","password":"wrong-password","chain_id":1,"contract":"0x2222222222222222222222222222222222222222"}"#,
        );
        assert!(err && msg.as_str().unwrap().contains("incorrect password"));
    }

    #[test]
    fn walletconnect_approvals_require_wallet_chain_and_password() {
        let (_f, tb) = toolbox_fixture();
        let find = |name: &str| tb.tools.iter().find(|t| t.name == name).unwrap();
        for name in [
            "wc_approve_session",
            "wc_approve_request",
            "wc_review_request",
        ] {
            for required in ["wallet_name", "password"] {
                assert!(
                    find(name)
                        .params
                        .iter()
                        .any(|p| p.name == required && p.required),
                    "{name} must require {required}"
                );
            }
            // Optional because Sui wallets omit it; Ethereum wallets are required to give it.
            assert!(
                find(name)
                    .params
                    .iter()
                    .any(|p| p.name == "chain_id" && !p.required),
                "{name} takes an optional chain_id"
            );
        }
        // Looking, connecting and declining never touch a wallet.
        for name in [
            "wc_pair",
            "wc_list_proposals",
            "wc_reject_session",
            "wc_list_sessions",
            "wc_disconnect_session",
            "wc_list_requests",
            "wc_reject_request",
        ] {
            assert!(
                !find(name)
                    .params
                    .iter()
                    .any(|p| p.name == "password" || p.name == "wallet_name"),
                "{name}"
            );
        }
    }

    #[test]
    fn walletconnect_tools_report_a_missing_relay_and_bad_arguments() {
        let (_f, tb) = toolbox_fixture();
        let uri = format!(
            "wc:{}@2?relay-protocol=irn&symKey={}",
            "a".repeat(64),
            "b".repeat(64)
        );
        let (err, msg) = call(&tb, "wc_pair", &format!(r#"{{"uri":"{uri}"}}"#));
        assert!(
            err && msg
                .as_str()
                .unwrap()
                .contains("WalletConnect is not set up"),
            "{msg:?}"
        );
        let (err, msg) = call(&tb, "wc_pair", r#"{"uri":"wc:abc"}"#);
        assert!(
            err && msg.as_str().unwrap().contains("no version"),
            "{msg:?}"
        );
        let (err, msg) = call(&tb, "wc_pair", r#"{}"#);
        assert!(
            err && msg
                .as_str()
                .unwrap()
                .contains("missing required argument 'uri'")
        );
        let (err, msg) = call(
            &tb,
            "wc_approve_session",
            r#"{"proposal_id":"1","wallet_name":"W","chain_id":1,"password":"password-1","additional_chain_ids":["x"]}"#,
        );
        assert!(
            err && msg.as_str().unwrap().contains("not a valid chain ID"),
            "{msg:?}"
        );
        let (err, msg) = call(&tb, "wc_reject_request", r#"{"request_id":"abc"}"#);
        assert!(err && msg.as_str().unwrap().contains("non-negative integer"));
        let (err, _) = call(&tb, "wc_list_requests", r#"{}"#);
        assert!(!err, "listing with nothing connected is not an error");
        let (err, msg) = call(&tb, "wc_list_sessions", "{}");
        assert!(!err);
        assert_eq!(msg.get("count").and_then(Value::as_u64), Some(0));
    }

    #[test]
    fn swap_tools_expose_the_requested_parameters() {
        let (_f, tb) = toolbox_fixture();
        let find = |name: &str| tb.tools.iter().find(|t| t.name == name).unwrap();
        for name in ["get_swap_quote", "swap_tokens"] {
            let tool = find(name);
            for required in [
                "wallet_name",
                "password",
                "from_token",
                "to_token",
                "amount",
            ] {
                assert!(
                    tool.params.iter().any(|p| p.name == required && p.required),
                    "{name} must require {required}"
                );
            }
            // Ethereum wallets need the chain IDs; Sui wallets leave them out.
            for chain in ["from_chain_id", "to_chain_id"] {
                assert!(
                    tool.params.iter().any(|p| p.name == chain && !p.required),
                    "{name} {chain}"
                );
            }
            for optional in ["slippage_bps", "to_address"] {
                assert!(
                    tool.params
                        .iter()
                        .any(|p| p.name == optional && !p.required),
                    "{name} {optional}"
                );
            }
            // no API keys, secrets or provider settings are ever accepted from the caller
            assert!(
                !tool
                    .params
                    .iter()
                    .any(|p| p.name.contains("key") || p.name.contains("secret"))
            );
            // the caller may choose the network fee of their own transaction (`priority_fee`),
            // but never the aggregator's service fee
            assert!(!tool.params.iter().any(|p| {
                (p.name.contains("fee") && p.name != "priority_fee") || p.name.contains("referrer")
            }));
        }
        assert!(
            !find("get_swap_quote")
                .params
                .iter()
                .any(|p| p.name == "min_receive")
        );
        assert!(
            find("swap_tokens")
                .params
                .iter()
                .any(|p| p.name == "min_receive" && !p.required)
        );
    }

    #[test]
    fn swap_tools_explain_unsupported_requests() {
        let (_f, tb) = toolbox_fixture();
        call(
            &tb,
            "create_wallet",
            r#"{"blockchain":"ethereum","wallet_name":"w","password":"long-enough-pw"}"#,
        );
        for tool in ["get_swap_quote", "swap_tokens"] {
            let (err, msg) = call(
                &tb,
                tool,
                r#"{"wallet_name":"w","password":"long-enough-pw","from_chain_id":1,"from_token":"native","to_chain_id":8453,"to_token":"native","amount":"1"}"#,
            );
            assert!(
                err && msg.as_str().unwrap().contains("cross-chain"),
                "{tool}"
            );
        }
        let (err, msg) = call(
            &tb,
            "get_swap_quote",
            r#"{"wallet_name":"w","password":"long-enough-pw","from_chain_id":1,"from_token":"native","to_chain_id":1,"to_token":"native"}"#,
        );
        assert!(err && msg.as_str().unwrap().contains("'amount'"));
        let (err, _) = call(
            &tb,
            "get_swap_quote",
            r#"{"wallet_name":"w","password":"long-enough-pw","from_chain_id":"x","from_token":"native","to_chain_id":1,"to_token":"native","amount":"1"}"#,
        );
        assert!(err);
    }

    #[test]
    fn import_defaults_to_ethereum() {
        let (_f, tb) = toolbox_fixture();
        let key = format!("0x{}01", "00".repeat(31));
        let (err, body) = call(
            &tb,
            "import_wallet",
            &format!(r#"{{"wallet_name":"k","password":"long-enough-pw","private_key":"{key}"}}"#),
        );
        assert!(!err, "{}", body.to_json());
        assert_eq!(
            body.get("address").and_then(Value::as_str),
            Some("0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf")
        );
    }

    #[test]
    fn history_defaults_and_validation() {
        let (_f, tb) = toolbox_fixture();
        call(
            &tb,
            "create_wallet",
            r#"{"blockchain":"ethereum","wallet_name":"w","password":"long-enough-pw"}"#,
        );
        let (err, msg) = call(
            &tb,
            "get_transaction_history",
            r#"{"wallet_name":"w","password":"long-enough-pw","chain_id":1,"type":"bogus"}"#,
        );
        assert!(err && msg.as_str().unwrap().contains("transactions"));
        let (err, _) = call(
            &tb,
            "get_transaction_history",
            r#"{"wallet_name":"w","password":"long-enough-pw","chain_id":1,"limit":500}"#,
        );
        assert!(err);
    }

    #[test]
    fn null_arguments_count_as_missing() {
        let (_f, tb) = toolbox_fixture();
        let (err, msg) = call(
            &tb,
            "create_wallet",
            r#"{"blockchain":"ethereum","wallet_name":"w","password":null}"#,
        );
        assert!(
            err && msg
                .as_str()
                .unwrap()
                .contains("missing required argument 'password'")
        );
    }

    #[test]
    fn list_arguments_accept_numbers_as_strings() {
        let v = json::parse(r#"{"args":["a",1,true]}"#).unwrap();
        assert_eq!(Args(&v).str_list("args").unwrap(), vec!["a", "1", "true"]);
        let bad = json::parse(r#"{"args":[null]}"#).unwrap();
        assert!(Args(&bad).str_list("args").is_err());
        let not_list = json::parse(r#"{"args":"x"}"#).unwrap();
        assert!(Args(&not_list).str_list("args").is_err());
    }
}
