//! Solana wallets in the wallet service: lifecycle, balances, sending, signing and history.
//!
//! Every operation unlocks the wallet for that single call, exactly like the Ethereum and Sui
//! tools. A Solana wallet is bound to one cluster (mainnet, devnet or testnet) chosen when it is
//! created, so a devnet key can never sign a mainnet transaction.

use super::*;
use crate::eth::explorer::{sanitize_text, sanitize_text_max};
use crate::eth::prices::format_usd;
use crate::solana::address::{self as saddr, Pubkey};
use crate::solana::describe::{self, Described};
use crate::solana::effects::{self, Effects};
use crate::solana::network::SolanaNetwork;
use crate::solana::rpc::{
    RpcError, SignatureStatus, SolanaRpc, TransactionRecord, parse_signature,
};
use crate::solana::tokens;
use crate::solana::tx::{self, Instruction, Transaction};
use crate::solana::valuation;

/// The most the wallet will bid per compute unit as a priority fee: 250,000 micro-lamports, so
/// a 200,000-unit transaction pays at most 50,000 lamports (0.00005 SOL) on top of the base fee.
pub(super) const MAX_PRIORITY_MICRO_LAMPORTS: u64 = 250_000;
/// Token holdings listed by `get_wallet_balances`; Solana wallets often collect spam airdrops.
const MAX_LISTED_TOKENS: usize = 100;
/// What opening a new token account costs, shown to the agent (the real deposit is on-chain).
const TOKEN_ACCOUNT_DEPOSIT_SOL: &str = "about 0.00204 SOL";
const TOKEN_ACCOUNT_DEPOSIT_LAMPORTS: u64 = 2_039_280;
const MAX_LOGS_SHOWN: usize = 12;

pub(super) struct Session<'a> {
    pub rpc: SolanaRpc<'a>,
    pub network: SolanaNetwork,
    pub owner: Pubkey,
}

/// A transaction ready to sign, with what a dry run says about it.
pub(super) struct Prepared {
    pub transaction: Transaction,
    pub effects: Effects,
    pub fee_lamports: u64,
}

impl Prepared {
    pub(super) fn priority_fee_lamports(&self) -> u64 {
        let message = &self.transaction.message;
        tx::priority_fee_lamports(message, &message.account_keys)
    }
}

pub(super) struct Submitted {
    pub signature: String,
    pub status: Option<SignatureStatus>,
}

fn cluster_name(network: SolanaNetwork) -> &'static str {
    match network {
        SolanaNetwork::Mainnet => "mainnet-beta",
        SolanaNetwork::Devnet => "devnet",
        SolanaNetwork::Testnet => "testnet",
    }
}

/// `host` of an https URL. The path and query of a provider URL may hold a secret key, so only
/// the host is ever shown.
pub fn url_host(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .to_string()
}

pub(super) fn network_json(network: SolanaNetwork) -> Value {
    Value::object(vec![
        ("name", Value::str(network.id())),
        ("cluster", Value::str(cluster_name(network))),
        ("chain", Value::String(network.chain_name())),
        ("rpc_host", Value::String(url_host(network.rpc_url()))),
        ("explorer_url", Value::str(network.explorer_url())),
        ("currency_symbol", Value::str("SOL")),
    ])
}

fn parse_recipient(to: &str, field: &str) -> Result<Pubkey, String> {
    let key = saddr::parse(to).map_err(|e| format!("'{field}': {e}"))?;
    if saddr::is_burn_or_program_address(&key) {
        return Err(format!(
            "'{field}' is the address of a Solana program (or the system program, which is the burn address); funds sent there are lost"
        ));
    }
    Ok(key)
}

fn strings(items: Vec<String>) -> Value {
    Value::Array(items.into_iter().map(Value::String).collect())
}

/// A compute-unit limit with room above what the dry run used. The two compute-budget
/// instructions themselves cost about 150 units each.
pub(super) fn compute_limit_for(units: u64) -> u32 {
    let padded = units
        .saturating_add(300)
        .saturating_add(units / 5)
        .saturating_add(1_000);
    u32::try_from(padded.min(tx::MAX_COMPUTE_UNITS)).unwrap_or(u32::MAX)
}

/// The optional `priority_fee` argument (whole SOL) as lamports.
pub(super) fn parse_priority_option(text: Option<&str>) -> Result<Option<u64>, String> {
    text.map(tokens::parse_priority_fee).transpose()
}

/// The price per compute unit to bid, in micro-lamports. A fee the caller asked for is spread over
/// the transaction's compute-unit limit so the total is what they chose; otherwise the default
/// (`fallback`, for example recent network demand) applies, capped.
pub(super) fn priority_price(
    requested: Option<u64>,
    limit: u32,
    fallback: impl FnOnce() -> u64,
) -> u64 {
    match requested {
        Some(lamports) => tx::price_for_priority_fee(lamports, limit),
        None => fallback().min(MAX_PRIORITY_MICRO_LAMPORTS),
    }
}

pub(super) struct TransferPlan {
    pub body: Vec<Instruction>,
    pub symbol: String,
    pub asset: Value,
    pub raw_amount: u64,
    pub decimals: u32,
    pub recipient: Pubkey,
    /// Lamports the transfer itself moves out of the wallet (SOL sends and account deposits).
    pub lamports_needed: u64,
    pub creates_token_account: bool,
}

impl WalletService {
    pub(super) fn solana_session<'a>(&'a self, vault: &SolanaVault) -> Result<Session<'a>, String> {
        let rpc = SolanaRpc::new(&*self.http, vault.network.rpc_url());
        let genesis = rpc.genesis_hash()?;
        if genesis != vault.network.genesis_hash() {
            return Err(format!(
                "the Solana node at {} does not serve Solana {} (it reported another genesis hash); refusing to use it",
                url_host(vault.network.rpc_url()),
                cluster_name(vault.network)
            ));
        }
        Ok(Session {
            rpc,
            network: vault.network,
            owner: vault.public_key()?,
        })
    }

    /// Decimals and token program of a mint, read from the mint account itself.
    pub(super) fn solana_mint_info(s: &Session<'_>, mint: &Pubkey) -> Result<(u8, Pubkey), String> {
        let account = s
            .rpc
            .account(mint)?
            .ok_or("this token does not exist on the wallet's Solana network")?;
        let is_token_program = account.owner == saddr::id(saddr::TOKEN_PROGRAM)
            || account.owner == saddr::id(saddr::TOKEN_2022_PROGRAM);
        if !is_token_program {
            return Err("this address is not a token mint".to_string());
        }
        let decimals = tokens::mint_decimals(&account.data)
            .ok_or("this token mint is not initialised, so its decimals are unknown")?;
        Ok((decimals, account.owner))
    }

    fn solana_decimals_if_known(s: &Session<'_>, mint: &Pubkey) -> Option<u8> {
        if *mint == saddr::id(saddr::WRAPPED_SOL_MINT) {
            return Some(tx::SOL_DECIMALS as u8);
        }
        Self::solana_mint_info(s, mint).ok().map(|(d, _)| d)
    }

    // ---- wallet lifecycle -----------------------------------------------------------------------

    pub(super) fn solana_network_arg(network: Option<&str>) -> Result<SolanaNetwork, String> {
        match network {
            Some(n) => SolanaNetwork::parse(n),
            None => Ok(SolanaNetwork::Mainnet),
        }
    }

    pub(super) fn solana_store_new(
        &self,
        name: String,
        password: &str,
        contents: &SolanaVault,
        status: &str,
    ) -> Result<Value, String> {
        let address = contents.address()?;
        let record = WalletRecord {
            name: name.clone(),
            blockchain: Blockchain::Solana,
            vault: contents.seal(password, self.iterations)?,
        };
        self.store.create(&record)?;
        Ok(Value::object(vec![
            ("name", Value::String(name)),
            ("blockchain", Value::str(Blockchain::Solana.display_name())),
            ("network", Value::str(contents.network.id())),
            ("address", Value::String(address)),
            ("status", Value::str(status)),
        ]))
    }

    pub(super) fn solana_address(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_solana(name, password)?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("blockchain", Value::str(Blockchain::Solana.display_name())),
            ("network", Value::str(vault.network.id())),
            ("address", Value::String(vault.address()?)),
        ]))
    }

    pub(super) fn solana_seed_phrase(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_solana(name, password)?;
        let phrase = vault
            .mnemonic()
            .ok_or("this wallet was imported from a private key and has no seed phrase")?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("seed_phrase", Value::str(phrase.expose())),
            (
                "derivation_path",
                Value::str(crate::crypto::slip10::SOLANA_DEFAULT_PATH),
            ),
            (
                "warning",
                Value::str("Anyone with this phrase controls the wallet. Never share it."),
            ),
        ]))
    }

    pub(super) fn solana_private_key(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_solana(name, password)?;
        let key = vault.private_key()?;
        let keypair = vault.keypair_base58()?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("address", Value::String(vault.address()?)),
            ("private_key", Value::str(keypair.expose())),
            (
                "private_key_hex",
                Value::String(crate::crypto::hex::encode_prefixed(key.expose())),
            ),
            ("scheme", Value::str("ed25519")),
            (
                "warning",
                Value::str(
                    "Anyone with this key controls the wallet. Never share it. private_key is the base58 keypair format that Phantom and Solflare import; private_key_hex is the 32-byte secret alone.",
                ),
            ),
        ]))
    }

    pub(super) fn solana_export_payload(
        name: &str,
        vault: &SolanaVault,
    ) -> Result<crate::crypto::secret::SecretBytes, String> {
        let key = vault.private_key()?;
        let mut pairs = vec![
            ("format", Value::str("agent-saifu-wallet-export")),
            ("version", Value::int(1)),
            ("name", Value::str(name)),
            ("blockchain", Value::str(Blockchain::Solana.id())),
            ("network", Value::str(vault.network.id())),
            ("address", Value::String(vault.address()?)),
            ("scheme", Value::str("ed25519")),
            ("privateKey", Value::str(vault.keypair_base58()?.expose())),
            (
                "privateKeyHex",
                Value::String(crate::crypto::hex::encode_prefixed(key.expose())),
            ),
        ];
        if let Some(m) = vault.mnemonic() {
            pairs.push(("mnemonic", Value::str(m.expose())));
        }
        let mut doc = Value::object(pairs);
        let text = doc.to_json();
        doc.wipe();
        Ok(crate::crypto::secret::SecretBytes::new(text.into_bytes()))
    }

    pub(super) fn solana_networks(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_solana(name, password)?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("blockchain", Value::str(Blockchain::Solana.display_name())),
            ("networks", Value::Array(vec![network_json(vault.network)])),
            (
                "note",
                Value::str(
                    "A Solana wallet is bound to the one cluster it was created for, so a devnet key can never sign a mainnet transaction. Create or import another wallet for another cluster. Solana has no layer 2 networks to add.",
                ),
            ),
        ]))
    }

    // ---- balances -------------------------------------------------------------------------------

    pub fn solana_balances(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_solana(name, password)?;
        let s = self.solana_session(&vault)?;
        let lamports = s.rpc.balance(&s.owner)?;
        let accounts = s.rpc.token_accounts(&s.owner)?;

        struct Holding {
            mint: Pubkey,
            amount: u128,
            decimals: u8,
            accounts: usize,
        }
        let mut holdings: Vec<Holding> = vec![];
        for account in accounts.iter().filter(|a| a.amount > 0) {
            match holdings.iter_mut().find(|h| h.mint == account.mint) {
                Some(h) => {
                    h.amount += u128::from(account.amount);
                    h.accounts += 1;
                }
                None => holdings.push(Holding {
                    mint: account.mint,
                    amount: u128::from(account.amount),
                    decimals: account.decimals,
                    accounts: 1,
                }),
            }
        }
        let mint_list: Vec<Pubkey> = holdings.iter().map(|h| h.mint).collect();
        let (sol_price, token_prices) = if s.network.is_mainnet() {
            (
                valuation::sol_price_usd(&*self.http),
                valuation::token_prices_usd(&*self.http, &mint_list),
            )
        } else {
            (None, Default::default())
        };
        let mut rows: Vec<(Option<f64>, Value)> = holdings
            .iter()
            .map(|h| {
                let value = token_prices
                    .get(&h.mint)
                    .map(|p| valuation::value_usd(h.amount, u32::from(h.decimals), *p));
                let known = tokens::known_token(&h.mint);
                let mut pairs = vec![
                    ("symbol", Value::String(tokens::token_label(&h.mint))),
                    ("asset", Value::String(saddr::to_text(&h.mint))),
                    (
                        "name",
                        Value::str(known.map_or("unrecognised token", |k| k.name)),
                    ),
                    (
                        "balance",
                        Value::String(tokens::format_amount(h.amount, u32::from(h.decimals))),
                    ),
                    ("balance_raw", Value::String(h.amount.to_string())),
                    ("decimals", num(u64::from(h.decimals))),
                    ("token_accounts", num(h.accounts as u64)),
                    ("recognised", Value::Bool(known.is_some())),
                ];
                pairs.push((
                    "usdc_value",
                    value.map_or(Value::Null, |v| Value::String(format_usd(v))),
                ));
                (value, Value::object(pairs))
            })
            .collect();
        rows.sort_by(|a, b| {
            b.0.unwrap_or(-1.0)
                .partial_cmp(&a.0.unwrap_or(-1.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut warnings: Vec<Value> = vec![];
        if rows.len() > MAX_LISTED_TOKENS {
            rows.truncate(MAX_LISTED_TOKENS);
            warnings.push(Value::str(
                "this address holds a very large number of tokens; only the first ones are listed",
            ));
        }
        if holdings
            .iter()
            .any(|h| tokens::known_token(&h.mint).is_none())
        {
            warnings.push(Value::str(
                "tokens that are not recognised are shown by a shortened address. Solana wallets often receive unsolicited tokens (spam); do not interact with a token you do not know.",
            ));
        }
        let sol_value =
            sol_price.map(|p| valuation::value_usd(u128::from(lamports), tx::SOL_DECIMALS, p));
        let mut assets = vec![Value::object(vec![
            ("symbol", Value::str("SOL")),
            ("asset", Value::str("native")),
            ("name", Value::str("Solana")),
            ("balance", Value::String(tokens::format_sol(lamports))),
            ("balance_raw", Value::String(lamports.to_string())),
            ("decimals", num(u64::from(tx::SOL_DECIMALS))),
            (
                "usdc_value",
                sol_value.map_or(Value::Null, |v| Value::String(format_usd(v))),
            ),
        ])];
        assets.extend(rows.into_iter().map(|(_, v)| v));
        let mut pairs = vec![
            ("name", Value::String(record.name)),
            ("address", Value::String(saddr::to_text(&s.owner))),
            ("network", network_json(s.network)),
            ("assets", Value::Array(assets)),
            (
                "note",
                Value::str(
                    "SOL and every token account the address holds (SPL Token and Token-2022) are listed; a token's balance adds up all of its accounts. usdc_value is indicative USD value from CoinGecko; it is null on devnet/testnet and when no price exists.",
                ),
            ),
        ];
        if !warnings.is_empty() {
            pairs.push(("warnings", Value::Array(warnings)));
        }
        Ok(Value::object(pairs))
    }

    // ---- reading what a transaction would do -----------------------------------------------------

    pub(super) fn solana_changes_json(s: &Session<'_>, effects: &Effects) -> Value {
        let token_rows: Vec<Value> = effects
            .token_changes
            .iter()
            .map(|c| {
                let mut pairs = vec![
                    ("symbol", Value::String(tokens::token_label(&c.mint))),
                    ("asset", Value::String(saddr::to_text(&c.mint))),
                    ("change_raw", Value::String(c.change.to_string())),
                ];
                if let Some(d) = Self::solana_decimals_if_known(s, &c.mint) {
                    pairs.push((
                        "change",
                        Value::String(tokens::format_signed(c.change, u32::from(d))),
                    ));
                }
                Value::object(pairs)
            })
            .collect();
        Value::object(vec![
            (
                "sol",
                Value::object(vec![
                    (
                        "change_lamports",
                        Value::String(effects.sol_change.to_string()),
                    ),
                    (
                        "change",
                        Value::String(tokens::format_signed(effects.sol_change, tx::SOL_DECIMALS)),
                    ),
                ]),
            ),
            ("tokens", Value::Array(token_rows)),
        ])
    }

    pub(super) fn solana_effects_json(s: &Session<'_>, effects: &Effects) -> Value {
        let mut pairs = vec![(
            "status",
            Value::str(if effects.succeeded() {
                "success"
            } else {
                "failed"
            }),
        )];
        if let Some(error) = &effects.error {
            pairs.push(("error", Value::str(error)));
            let start = effects.logs.len().saturating_sub(MAX_LOGS_SHOWN);
            pairs.push(("logs", strings(effects.logs[start..].to_vec())));
        }
        if let Some(units) = effects.units_consumed {
            pairs.push(("compute_units", num(units)));
        }
        if effects.succeeded() {
            pairs.push(("wallet_changes", Self::solana_changes_json(s, effects)));
            if !effects.warnings.is_empty() {
                pairs.push(("warnings", strings(effects.warnings.clone())));
            }
        }
        Value::object(pairs)
    }

    pub(super) fn solana_description_json(described: &Described) -> Value {
        Value::Array(described.instructions.clone())
    }

    // ---- building and sending ---------------------------------------------------------------------

    /// Works out the instructions that send `amount` of `asset` to `to`.
    pub(super) fn solana_plan_transfer(
        s: &Session<'_>,
        asset: &str,
        to: &str,
        amount: &str,
    ) -> Result<TransferPlan, String> {
        let recipient = parse_recipient(to, "to")?;
        if recipient == s.owner {
            return Err("'to' is this wallet's own address; there is nothing to send".to_string());
        }
        if asset.trim().eq_ignore_ascii_case("native") || asset.trim().eq_ignore_ascii_case("sol") {
            let lamports = tokens::parse_amount(amount, tx::SOL_DECIMALS)
                .map_err(|e| format!("'amount': {e}"))?;
            return Ok(TransferPlan {
                body: vec![tx::system_transfer(&s.owner, &recipient, lamports)],
                symbol: "SOL".to_string(),
                asset: Value::str("native"),
                raw_amount: lamports,
                decimals: tx::SOL_DECIMALS,
                recipient,
                lamports_needed: lamports,
                creates_token_account: false,
            });
        }
        let mint = saddr::parse(asset).map_err(|e| {
            format!("'asset': use \"native\" for SOL or a token mint address ({e})")
        })?;
        let (decimals, program) = Self::solana_mint_info(s, &mint)?;
        let raw = tokens::parse_amount(amount, u32::from(decimals))
            .map_err(|e| format!("'amount': {e}"))?;
        let symbol = tokens::token_label(&mint);
        let held = s.rpc.token_accounts_for_mint(&s.owner, &mint)?;
        let held: Vec<_> = held.into_iter().filter(|t| t.program == program).collect();
        let total: u128 = held.iter().map(|t| u128::from(t.amount)).sum();
        let source = held
            .iter()
            .filter(|t| t.amount >= raw)
            .max_by_key(|t| t.amount)
            .ok_or_else(|| {
                if total >= u128::from(raw) {
                    format!(
                        "this wallet's {symbol} is split over several token accounts and none holds {amount} alone; consolidate them first"
                    )
                } else {
                    format!(
                        "insufficient {symbol}: the wallet holds {} but {amount} is needed",
                        tokens::format_amount(total, u32::from(decimals))
                    )
                }
            })?;
        let destination = saddr::associated_token_address(&recipient, &mint, &program)?;
        let existing = s.rpc.account(&destination)?;
        let mut body = vec![];
        let creates = match existing {
            Some(account) if account.owner == program => false,
            Some(_) => {
                return Err("the recipient's token account address is taken by something that is not a token account; cannot send".to_string());
            }
            None => {
                body.push(tx::create_associated_token_account_idempotent(
                    &s.owner,
                    &destination,
                    &recipient,
                    &mint,
                    &program,
                ));
                true
            }
        };
        body.push(tx::token_transfer_checked(
            &program,
            &source.address,
            &mint,
            &destination,
            &s.owner,
            raw,
            decimals,
        ));
        Ok(TransferPlan {
            body,
            symbol,
            asset: Value::String(saddr::to_text(&mint)),
            raw_amount: raw,
            decimals: u32::from(decimals),
            recipient,
            lamports_needed: if creates {
                TOKEN_ACCOUNT_DEPOSIT_LAMPORTS
            } else {
                0
            },
            creates_token_account: creates,
        })
    }

    /// Assembles a transaction from `body`, dry-runs it to set the compute budget, and adds a
    /// priority fee: `priority_fee` lamports in total when the caller chose one (zero means none),
    /// otherwise one in line with what the cluster has recently paid. The final transaction is
    /// simulated again so its effects include the priority fee. Nothing is signed. With
    /// `strict` false a wallet that cannot afford it still gets the fee estimate, with the
    /// shortfall reported as the dry run's error instead of failing the call.
    pub(super) fn solana_prepare(
        s: &Session<'_>,
        body: Vec<Instruction>,
        lamports_needed: u64,
        priority_fee: Option<u64>,
        strict: bool,
    ) -> Result<Prepared, String> {
        let (blockhash, _) = s.rpc.latest_blockhash()?;
        let draft = Transaction::unsigned(tx::compile_legacy(&s.owner, &body, blockhash)?);
        let balance = s.rpc.balance(&s.owner)?;
        let base_fee = tx::fee_estimate(&draft.message, &draft.message.account_keys);
        let needed = lamports_needed
            .saturating_add(base_fee)
            .saturating_add(priority_fee.unwrap_or(0));
        if balance < needed {
            let shortfall = format!(
                "insufficient SOL: the wallet holds {} SOL but needs about {} SOL (including the network fee)",
                tokens::format_sol(balance),
                tokens::format_sol(needed)
            );
            if strict {
                return Err(shortfall);
            }
            return Ok(Prepared {
                transaction: draft,
                effects: Effects {
                    error: Some(shortfall),
                    ..Effects::default()
                },
                fee_lamports: base_fee,
            });
        }
        let effects = effects::simulate_effects(&s.rpc, &draft, &s.owner)?;
        if let Some(error) = &effects.error {
            return Err(format!(
                "the transaction would fail, so nothing was signed: {error}"
            ));
        }
        let limit = compute_limit_for(
            effects
                .units_consumed
                .unwrap_or(tx::DEFAULT_UNITS_PER_INSTRUCTION),
        );
        let price = priority_price(priority_fee, limit, || s.rpc.recent_priority_fee());
        let mut instructions = vec![tx::set_compute_unit_limit(limit)];
        if price > 0 {
            instructions.push(tx::set_compute_unit_price(price));
        }
        instructions.extend(body);
        let message = tx::compile_legacy(&s.owner, &instructions, blockhash)?;
        let fee_lamports = tx::fee_estimate(&message, &message.account_keys);
        let transaction = Transaction::unsigned(message);
        let effects = effects::simulate_effects(&s.rpc, &transaction, &s.owner)?;
        if strict && let Some(error) = &effects.error {
            return Err(format!(
                "the transaction would fail, so nothing was signed: {error}"
            ));
        }
        Ok(Prepared {
            transaction,
            effects,
            fee_lamports,
        })
    }

    fn solana_confirm(&self, s: &Session<'_>, signature: &str) -> Option<SignatureStatus> {
        let (pause, attempts) = self.receipt_wait;
        for attempt in 0..attempts {
            if let Ok(list) = s.rpc.signature_statuses(&[signature.to_string()])
                && let Some(Some(status)) = list.first()
                && (status.error.is_some()
                    || matches!(
                        status.confirmation.as_deref(),
                        Some("confirmed") | Some("finalized")
                    ))
            {
                return Some(status.clone());
            }
            if attempt + 1 < attempts && !pause.is_zero() {
                std::thread::sleep(pause);
            }
        }
        None
    }

    /// Signs and submits. Re-sending the same signed bytes is harmless (same signature), but if the
    /// answer is lost the signature is looked up to learn what happened.
    pub(super) fn solana_submit(
        &self,
        s: &Session<'_>,
        vault: &SolanaVault,
        transaction: &mut Transaction,
    ) -> Result<Submitted, String> {
        let missing = transaction.missing_signers(&s.owner);
        if !missing.is_empty() {
            return Err(
                "not sending: the transaction still needs a signature from someone other than this wallet"
                    .to_string(),
            );
        }
        transaction.sign(vault)?;
        let signature = transaction
            .id()
            .ok_or("the fee payer has not signed this transaction, so it cannot be sent")?;
        match s.rpc.send(&transaction.to_bytes()) {
            Ok(_) => Ok(Submitted {
                status: self.solana_confirm(s, &signature),
                signature,
            }),
            Err(e) => {
                let found = s
                    .rpc
                    .signature_statuses(std::slice::from_ref(&signature))
                    .ok()
                    .and_then(|list| list.into_iter().next().flatten());
                match (found, &e) {
                    (Some(status), _) => Ok(Submitted {
                        signature,
                        status: Some(status),
                    }),
                    (None, RpcError::Node { message, .. }) => Err(format!(
                        "the Solana network rejected the transaction: {}. It was not found on-chain (signature {signature}), so nothing was spent",
                        sanitize_text_max(message, 300)
                    )),
                    (None, _) => Err(format!(
                        "the answer to the submission was lost or unclear ({e}); the outcome is unknown. Do not build a new transaction before checking: call get_transaction with transaction_hash {signature}. (Submitting the same signed transaction again is harmless, but it expires after about a minute.)"
                    )),
                }
            }
        }
    }

    pub(super) fn solana_submission_report(s: &Session<'_>, submitted: &Submitted) -> Value {
        let mut pairs = vec![
            ("transaction_signature", Value::str(&submitted.signature)),
            (
                "explorer_url",
                Value::String(s.network.explorer_tx_url(&submitted.signature)),
            ),
        ];
        match &submitted.status {
            Some(status) => {
                let label = match (&status.error, status.confirmation.as_deref()) {
                    (Some(_), _) => "failed",
                    (None, Some("finalized")) => "finalized",
                    _ => "confirmed",
                };
                pairs.push(("status", Value::str(label)));
                pairs.push(("slot", num(status.slot)));
                if let Some(error) = &status.error {
                    pairs.push(("error", Value::str(error)));
                }
            }
            None => {
                pairs.push(("status", Value::str("submitted_not_yet_confirmed")));
                pairs.push((
                    "note",
                    Value::str(
                        "The network accepted the transaction but it was not confirmed within the wait. Check it with get_transaction before doing anything else.",
                    ),
                ));
            }
        }
        Value::object(pairs)
    }

    pub fn solana_send_asset(
        &self,
        name: &str,
        password: &str,
        asset: &str,
        to: &str,
        amount: &str,
    ) -> Result<Value, String> {
        self.solana_send_asset_with_priority(name, password, asset, to, amount, None)
    }

    /// Like `solana_send_asset`, paying `priority_fee` (whole SOL, in total) as the priority fee
    /// instead of the default that follows recent network demand.
    pub fn solana_send_asset_with_priority(
        &self,
        name: &str,
        password: &str,
        asset: &str,
        to: &str,
        amount: &str,
        priority_fee: Option<&str>,
    ) -> Result<Value, String> {
        let priority_fee = parse_priority_option(priority_fee)?;
        let (record, vault) = self.unlock_solana(name, password)?;
        let s = self.solana_session(&vault)?;
        let plan = Self::solana_plan_transfer(&s, asset, to, amount)?;
        let mut prepared = Self::solana_prepare(
            &s,
            plan.body.clone(),
            plan.lamports_needed,
            priority_fee,
            true,
        )?;
        let submitted = self.solana_submit(&s, &vault, &mut prepared.transaction)?;
        let mut pairs = vec![
            ("wallet", Value::String(record.name)),
            ("network", Value::str(s.network.id())),
            ("from", Value::String(saddr::to_text(&s.owner))),
            ("to", Value::String(saddr::to_text(&plan.recipient))),
            ("asset", Value::String(plan.symbol.clone())),
            ("asset_id", plan.asset.clone()),
            (
                "amount",
                Value::String(tokens::format_amount(
                    u128::from(plan.raw_amount),
                    plan.decimals,
                )),
            ),
            (
                "network_fee_sol",
                Value::String(tokens::format_sol(prepared.fee_lamports)),
            ),
            (
                "priority_fee_sol",
                Value::String(tokens::format_sol(prepared.priority_fee_lamports())),
            ),
        ];
        if plan.creates_token_account {
            pairs.push((
                "note",
                Value::String(format!(
                    "The recipient had no {} account, so one was opened for them; that deposit ({TOKEN_ACCOUNT_DEPOSIT_SOL}) was paid from this wallet.",
                    plan.symbol
                )),
            ));
        }
        Ok(merge(Self::solana_submission_report(&s, &submitted), pairs))
    }

    // ---- signing arbitrary transactions and messages ------------------------------------------------

    /// The wallet must be one of the signers, and the transaction must be valid on this wallet's
    /// cluster (its blockhash is recent), so a transaction built for another cluster is never signed.
    pub(super) fn solana_check_signable(
        s: &Session<'_>,
        transaction: &Transaction,
        accounts: &[Pubkey],
    ) -> Result<(), String> {
        let message = &transaction.message;
        if message.signer_index(&s.owner).is_none() {
            return Err(
                "this wallet is not one of the transaction's signers; refusing to sign it"
                    .to_string(),
            );
        }
        if tx::uses_durable_nonce(message, accounts) {
            return Ok(());
        }
        if !s.rpc.is_blockhash_valid(&message.recent_blockhash)? {
            return Err(
                "the transaction's blockhash is not valid on this wallet's Solana network (it may have expired, or been built for another Solana network); refusing to sign it"
                    .to_string(),
            );
        }
        Ok(())
    }

    /// Reads, checks and dry-runs a transaction built elsewhere. Warnings and the dry run are
    /// returned for reporting; nothing is signed.
    pub(super) fn solana_inspect(
        s: &Session<'_>,
        transaction: &Transaction,
    ) -> (Described, Result<Effects, String>, Vec<String>) {
        let mut warnings = vec![];
        let accounts = match effects::resolve_message_accounts(&s.rpc, &transaction.message) {
            Ok(a) => a,
            Err(e) => {
                warnings.push(format!("address lookup tables could not be read: {e}"));
                transaction.message.account_keys.clone()
            }
        };
        let described = describe::describe(&transaction.message, &s.owner, &accounts);
        warnings.extend(described.warnings.clone());
        let missing = transaction.missing_signers(&s.owner);
        if !missing.is_empty() {
            warnings.push(format!(
                "{} more signature(s) from other accounts are still needed before this can be sent",
                missing.len()
            ));
        }
        if transaction.message.fee_payer() != &s.owner {
            warnings.push(format!(
                "this wallet is not the fee payer; {} pays the network fee",
                saddr::to_text(transaction.message.fee_payer())
            ));
        }
        let result = effects::simulate_effects(&s.rpc, transaction, &s.owner);
        match &result {
            Ok(effects) => {
                if let Some(error) = &effects.error {
                    warnings.push(format!("a dry run of this transaction fails: {error}"));
                }
                warnings.extend(effects.warnings.clone());
            }
            Err(e) => warnings.push(format!("the dry run could not be completed: {e}")),
        }
        (described, result, warnings)
    }

    /// `solana:signTransaction` / `signAndSendTransaction`: signs a serialized transaction built
    /// elsewhere (by a dapp or an SDK).
    pub fn solana_sign_transaction(
        &self,
        name: &str,
        password: &str,
        transaction_text: &str,
        broadcast: bool,
    ) -> Result<Value, String> {
        let mut transaction = Transaction::from_text(transaction_text)?;
        let (record, vault) = self.unlock_solana(name, password)?;
        let s = self.solana_session(&vault)?;
        let accounts = effects::resolve_message_accounts(&s.rpc, &transaction.message)
            .unwrap_or_else(|_| transaction.message.account_keys.clone());
        Self::solana_check_signable(&s, &transaction, &accounts)?;
        let (described, dry, warnings) = Self::solana_inspect(&s, &transaction);
        let dry_report = match &dry {
            Ok(effects) => Self::solana_effects_json(&s, effects),
            Err(e) => Value::object(vec![
                ("status", Value::str("unavailable")),
                ("error", Value::str(e)),
            ]),
        };
        if broadcast {
            match &dry {
                Ok(effects) if effects.succeeded() => {}
                Ok(effects) => {
                    return Err(format!(
                        "not sending: the dry run of this transaction does not succeed ({})",
                        effects
                            .error
                            .clone()
                            .unwrap_or_else(|| "unknown reason".to_string())
                    ));
                }
                Err(e) => {
                    return Err(format!(
                        "not sending: the dry run could not be completed ({e})"
                    ));
                }
            }
        }
        let signature = transaction.sign(&vault)?;
        let mut pairs = vec![
            ("wallet", Value::String(record.name)),
            ("network", Value::str(s.network.id())),
            ("address", Value::String(saddr::to_text(&s.owner))),
            ("transaction", Value::String(transaction.to_base64())),
            (
                "signature",
                Value::String(crate::crypto::base58::encode(&signature)),
            ),
            ("description", Self::solana_description_json(&described)),
            ("warnings", strings(warnings)),
            ("dry_run", dry_report),
        ];
        if broadcast {
            let submitted = self.solana_submit(&s, &vault, &mut transaction)?;
            pairs.push(("status", Value::str("submitted")));
            pairs.push(("execution", Self::solana_submission_report(&s, &submitted)));
        } else {
            pairs.push(("status", Value::str("signed")));
            pairs.push((
                "note",
                Value::str(
                    "Signed only; nothing was sent. `transaction` is the signed transaction in base64 (submit it with any Solana client, soon: its blockhash expires after about a minute), or call again with broadcast=true. Other signers' slots, if any, are unchanged.",
                ),
            ));
        }
        Ok(Value::object(pairs))
    }

    /// `solana:signMessage`: an Ed25519 signature over the raw message bytes.
    pub fn solana_sign_message(
        &self,
        name: &str,
        password: &str,
        message: &str,
        format: Option<&str>,
    ) -> Result<Value, String> {
        let bytes = crate::sui::tx::parse_message(message, format)?;
        Self::solana_check_message_signable(&bytes)?;
        let (record, vault) = self.unlock_solana(name, password)?;
        let signature = vault.sign(&bytes)?;
        let readable = std::str::from_utf8(&bytes)
            .ok()
            .filter(|t| !t.chars().any(|c| c.is_control() && c != '\n' && c != '\t'));
        Ok(Value::object(vec![
            ("wallet", Value::String(record.name)),
            ("network", Value::str(vault.network.id())),
            ("address", Value::String(vault.address()?)),
            (
                "signature",
                Value::String(crate::crypto::base58::encode(&signature)),
            ),
            (
                "signature_base64",
                Value::String(crate::crypto::base64::encode(&signature)),
            ),
            (
                "message_base64",
                Value::String(crate::crypto::base64::encode(&bytes)),
            ),
            (
                "message_preview",
                match readable {
                    Some(text) => Value::String(sanitize_text_max(text, 500)),
                    None => Value::str("(binary data)"),
                },
            ),
            ("standard", Value::str("Ed25519 signMessage")),
            (
                "note",
                Value::str(
                    "Signed the raw message bytes with the wallet's Ed25519 key, as Phantom's signMessage does; nothing is sent. The signature is base58 (as WalletConnect returns it) and base64.",
                ),
            ),
        ]))
    }

    /// A message that is also a valid transaction would hand over a transaction signature, so
    /// message signing refuses it (Phantom does the same).
    pub(super) fn solana_check_message_signable(bytes: &[u8]) -> Result<(), String> {
        if tx::looks_like_transaction(bytes) {
            return Err(
                "refusing: these bytes are a valid Solana transaction, and signing them as a message would sign the transaction. Use sign_contract_transaction if a transaction is what you mean to sign"
                    .to_string(),
            );
        }
        Ok(())
    }

    // ---- estimating and simulating ------------------------------------------------------------------

    /// Dry-runs either a transfer (`asset`, `amount`, `to`) or given `transaction_bytes`.
    fn solana_dry_run(
        &self,
        s: &Session<'_>,
        params: &IntentParams,
    ) -> Result<(Prepared, Option<Value>), String> {
        if params.value.is_some() || params.data.is_some() {
            return Err("'value' and 'data' are Ethereum parameters; for Solana give asset + amount + to, or transaction_bytes".to_string());
        }
        if let Some(bytes) = &params.transaction_bytes {
            if params.asset.is_some() || params.amount.is_some() || params.to.is_some() {
                return Err(
                    "use either transaction_bytes, or asset + amount + to, not both".to_string(),
                );
            }
            if params.priority_fee.is_some() {
                return Err("'priority_fee' applies to transfers the wallet builds; a transaction given as transaction_bytes carries its own compute-budget instructions".to_string());
            }
            let transaction = Transaction::from_text(bytes)?;
            if transaction.message.signer_index(&s.owner).is_none() {
                return Err("this wallet is not one of the transaction's signers".to_string());
            }
            let (described, dry, warnings) = Self::solana_inspect(s, &transaction);
            let effects = dry?;
            let accounts = &transaction.message.account_keys;
            let fee = tx::fee_estimate(&transaction.message, accounts);
            let mut pairs = vec![("instructions", Self::solana_description_json(&described))];
            if !warnings.is_empty() {
                pairs.push(("warnings", strings(warnings)));
            }
            return Ok((
                Prepared {
                    transaction,
                    effects,
                    fee_lamports: fee,
                },
                Some(Value::object(pairs)),
            ));
        }
        let to = params.to.as_deref().ok_or("'to' is required")?;
        let asset = params
            .asset
            .as_deref()
            .ok_or("'asset' is required (use \"native\" for SOL)")?;
        let amount = params
            .amount
            .as_deref()
            .ok_or("'amount' is required together with 'asset'")?;
        let priority_fee = parse_priority_option(params.priority_fee.as_deref())?;
        let plan = Self::solana_plan_transfer(s, asset, to, amount)?;
        let prepared =
            Self::solana_prepare(s, plan.body, plan.lamports_needed, priority_fee, false)?;
        Ok((prepared, None))
    }

    pub fn solana_estimate_gas(
        &self,
        name: &str,
        password: &str,
        params: &IntentParams,
    ) -> Result<Value, String> {
        let (record, vault) = self.unlock_solana(name, password)?;
        let s = self.solana_session(&vault)?;
        let (prepared, description) = self.solana_dry_run(&s, params)?;
        let usd = if s.network.is_mainnet() {
            valuation::sol_price_usd(&*self.http).map(|p| {
                valuation::value_usd(u128::from(prepared.fee_lamports), tx::SOL_DECIMALS, p)
            })
        } else {
            None
        };
        let mut pairs = vec![
            ("wallet", Value::String(record.name)),
            ("network", Value::str(s.network.id())),
            ("will_succeed", Value::Bool(prepared.effects.succeeded())),
            (
                "fee_lamports",
                Value::String(prepared.fee_lamports.to_string()),
            ),
            (
                "fee_sol",
                Value::String(tokens::format_sol(prepared.fee_lamports)),
            ),
            (
                "priority_fee_sol",
                Value::String(tokens::format_sol(prepared.priority_fee_lamports())),
            ),
            (
                "fee_usdc",
                usd.map_or(Value::Null, |v| Value::String(format!("{v:.4}"))),
            ),
            (
                "compute_units",
                prepared.effects.units_consumed.map_or(Value::Null, num),
            ),
            (
                "note",
                Value::str(
                    "Solana's base fee is 5,000 lamports per signature; transactions sent by this wallet add a small priority fee that follows recent network demand (capped at 0.00005 SOL), or the total you choose with priority_fee (up to 0.01 SOL), which is included in fee_sol. Opening a token account for a recipient who has none adds a refundable deposit (about 0.00204 SOL) that is not part of this fee.",
                ),
            ),
        ];
        if let Some(d) = description {
            pairs.push(("decoded", d));
        }
        if let Some(error) = &prepared.effects.error {
            pairs.push(("error", Value::str(error)));
        }
        Ok(Value::object(pairs))
    }

    pub fn solana_simulate_transaction(
        &self,
        name: &str,
        password: &str,
        params: &IntentParams,
    ) -> Result<Value, String> {
        let (record, vault) = self.unlock_solana(name, password)?;
        let s = self.solana_session(&vault)?;
        let (prepared, description) = self.solana_dry_run(&s, params)?;
        let mut pairs = vec![
            ("wallet", Value::String(record.name)),
            ("network", Value::str(s.network.id())),
            ("success", Value::Bool(prepared.effects.succeeded())),
            (
                "fee_lamports",
                Value::String(prepared.fee_lamports.to_string()),
            ),
            (
                "priority_fee_sol",
                Value::String(tokens::format_sol(prepared.priority_fee_lamports())),
            ),
            ("result", Self::solana_effects_json(&s, &prepared.effects)),
            (
                "note",
                Value::str(
                    "A dry run against the network's current state; nothing was signed or sent. wallet_changes is what this wallet's balances would change by, the network fee included.",
                ),
            ),
        ];
        if let Some(d) = description {
            pairs.push(("decoded", d));
        }
        Ok(Value::object(pairs))
    }

    // ---- looking things up ------------------------------------------------------------------------

    /// This wallet's SOL and token changes in a recorded transaction.
    pub(super) fn solana_record_changes(s: &Session<'_>, record: &TransactionRecord) -> Value {
        let sol = record
            .accounts
            .iter()
            .position(|a| *a == s.owner)
            .and_then(|i| {
                Some(
                    i128::from(*record.post_balances.get(i)?)
                        - i128::from(*record.pre_balances.get(i)?),
                )
            });
        let mut mints: Vec<(Pubkey, i128, u8)> = vec![];
        let mut add = |mint: Pubkey, decimals: u8, delta: i128| match mints
            .iter_mut()
            .find(|(m, _, _)| *m == mint)
        {
            Some(entry) => entry.1 += delta,
            None => mints.push((mint, delta, decimals)),
        };
        for b in record
            .pre_tokens
            .iter()
            .filter(|b| b.owner == Some(s.owner))
        {
            add(b.mint, b.decimals, -(b.amount as i128));
        }
        for b in record
            .post_tokens
            .iter()
            .filter(|b| b.owner == Some(s.owner))
        {
            add(b.mint, b.decimals, b.amount as i128);
        }
        let token_rows: Vec<Value> = mints
            .into_iter()
            .filter(|(_, delta, _)| *delta != 0)
            .map(|(mint, delta, decimals)| {
                Value::object(vec![
                    ("symbol", Value::String(tokens::token_label(&mint))),
                    ("asset", Value::String(saddr::to_text(&mint))),
                    ("change_raw", Value::String(delta.to_string())),
                    (
                        "change",
                        Value::String(tokens::format_signed(delta, u32::from(decimals))),
                    ),
                ])
            })
            .collect();
        Value::object(vec![
            (
                "sol",
                match sol {
                    Some(change) => Value::object(vec![
                        ("change_lamports", Value::String(change.to_string())),
                        (
                            "change",
                            Value::String(tokens::format_signed(change, tx::SOL_DECIMALS)),
                        ),
                    ]),
                    None => Value::Null,
                },
            ),
            ("tokens", Value::Array(token_rows)),
        ])
    }

    pub fn solana_get_transaction(
        &self,
        name: &str,
        password: &str,
        signature: &str,
    ) -> Result<Value, String> {
        let signature = parse_signature(signature)?;
        let (_record, vault) = self.unlock_solana(name, password)?;
        let s = self.solana_session(&vault)?;
        let status = s
            .rpc
            .signature_statuses(std::slice::from_ref(&signature))
            .ok()
            .and_then(|list| list.into_iter().next().flatten());
        let Some(record) = s.rpc.transaction(&signature)? else {
            return match status {
                Some(status) => Ok(Value::object(vec![
                    ("transaction_signature", Value::String(signature.clone())),
                    ("network", Value::str(s.network.id())),
                    (
                        "explorer_url",
                        Value::String(s.network.explorer_tx_url(&signature)),
                    ),
                    (
                        "status",
                        Value::str(if status.error.is_some() {
                            "failed"
                        } else {
                            "pending"
                        }),
                    ),
                    ("slot", num(status.slot)),
                    (
                        "confirmation",
                        Value::str(status.confirmation.as_deref().unwrap_or("processed")),
                    ),
                    (
                        "note",
                        Value::str(
                            "The network has seen this transaction but its details are not available yet; ask again in a moment.",
                        ),
                    ),
                ])),
                None => Err(format!(
                    "the Solana network has no transaction {signature} (it may still be pending, expired without being processed, or belong to another Solana network)"
                )),
            };
        };
        let mut pairs = vec![
            ("transaction_signature", Value::String(signature.clone())),
            ("network", Value::str(s.network.id())),
            (
                "explorer_url",
                Value::String(s.network.explorer_tx_url(&signature)),
            ),
            (
                "status",
                Value::str(if record.error.is_some() {
                    "failed"
                } else {
                    "success"
                }),
            ),
            ("slot", num(record.slot)),
            ("fee_lamports", Value::String(record.fee.to_string())),
            ("fee_sol", Value::String(tokens::format_sol(record.fee))),
            ("wallet_changes", Self::solana_record_changes(&s, &record)),
        ];
        if let Some(t) = record.block_time {
            pairs.push(("block_time_unix", Value::int(t)));
        }
        if let Some(error) = &record.error {
            pairs.push(("error", Value::str(error)));
        }
        if let Some(status) = &status {
            if let Some(c) = &status.confirmation {
                pairs.push(("confirmation", Value::str(c)));
            }
            if let Some(n) = status.confirmations {
                pairs.push(("confirmations", num(n)));
            }
        }
        if let Some(payer) = record.accounts.first() {
            pairs.push(("fee_payer", Value::String(saddr::to_text(payer))));
        }
        Ok(Value::object(pairs))
    }

    pub fn solana_history(
        &self,
        name: &str,
        password: &str,
        kind: HistoryKind,
        limit: usize,
    ) -> Result<Value, String> {
        if kind != HistoryKind::Transactions {
            return Err(
                "Solana history lists transactions only; token_transfers is an Ethereum option"
                    .to_string(),
            );
        }
        let limit = limit.clamp(1, explorer::MAX_LIMIT);
        let (record, vault) = self.unlock_solana(name, password)?;
        let s = self.solana_session(&vault)?;
        let items = s.rpc.signatures_for_address(&s.owner, limit)?;
        let list: Vec<Value> = items
            .iter()
            .map(|t| {
                let mut pairs = vec![
                    ("transaction_signature", Value::str(&t.signature)),
                    (
                        "explorer_url",
                        Value::String(s.network.explorer_tx_url(&t.signature)),
                    ),
                    (
                        "status",
                        Value::str(if t.error.is_some() {
                            "failed"
                        } else {
                            "success"
                        }),
                    ),
                    ("slot", num(t.slot)),
                ];
                if let Some(time) = t.block_time {
                    pairs.push(("block_time_unix", Value::int(time)));
                }
                if let Some(error) = &t.error {
                    pairs.push(("error", Value::str(error)));
                }
                if let Some(c) = &t.confirmation {
                    pairs.push(("confirmation", Value::str(c)));
                }
                if let Some(memo) = &t.memo {
                    pairs.push(("memo", Value::str(&sanitize_text(memo))));
                }
                Value::object(pairs)
            })
            .collect();
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("address", Value::String(saddr::to_text(&s.owner))),
            ("network", Value::str(s.network.id())),
            ("type", Value::str("transactions")),
            ("count", num(list.len() as u64)),
            ("items", Value::Array(list)),
            (
                "note",
                Value::str(
                    "Every transaction that touched the address, newest first, from the Solana node. Use get_transaction for one transaction's balance changes. Memos are untrusted text.",
                ),
            ),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_never_include_paths_queries_or_credentials() {
        assert_eq!(
            url_host("https://api.mainnet-beta.solana.com"),
            "api.mainnet-beta.solana.com"
        );
        assert_eq!(
            url_host("https://rpc.example.com/v1/SECRETKEY"),
            "rpc.example.com"
        );
        assert_eq!(
            url_host("https://rpc.example.com?api-key=SECRET"),
            "rpc.example.com"
        );
        assert_eq!(
            url_host("https://user:pw@rpc.example.com:8899/x"),
            "rpc.example.com:8899"
        );
    }

    #[test]
    fn compute_limits_leave_headroom_and_are_capped() {
        assert_eq!(compute_limit_for(150), 150 + 300 + 30 + 1_000);
        assert_eq!(compute_limit_for(100_000), 100_000 + 300 + 20_000 + 1_000);
        assert_eq!(compute_limit_for(5_000_000), 1_400_000);
        assert_eq!(compute_limit_for(u64::MAX), 1_400_000);
    }

    #[test]
    fn recipients_cannot_be_programs_or_the_burn_address() {
        assert!(parse_recipient(&saddr::to_text(&[3u8; 32]), "to").is_ok());
        for bad in [
            saddr::SYSTEM_PROGRAM,
            saddr::TOKEN_PROGRAM,
            "short",
            "0x0000000000000000000000000000000000000001",
        ] {
            assert!(parse_recipient(bad, "to").is_err(), "{bad}");
        }
    }
}
