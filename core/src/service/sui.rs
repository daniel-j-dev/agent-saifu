//! Sui wallets in the wallet service: lifecycle, balances, sending, signing and history.
//!
//! Every operation unlocks the wallet for that single call, exactly like the Ethereum tools.
//! A Sui wallet is bound to one network (mainnet, testnet or devnet) chosen when it is created.

use sui_sdk_types::{Address as SuiAddress, Digest, Transaction, TransactionExpiration};

use super::*;
use crate::eth::explorer::{sanitize_text, sanitize_text_max};
use crate::eth::prices::format_usd;
use crate::sui::coins::{self, SUI_COIN_TYPE, SUI_DECIMALS};
use crate::sui::gql::{Effects, GasSummary, Gql, TransactionInfo};
use crate::sui::network::SuiNetwork;
use crate::sui::tx::{self, Holdings};
use crate::sui::valuation;
use crate::sui::wallet::SuiVault;

pub(super) struct Session<'a> {
    pub gql: Gql<'a>,
    pub network: SuiNetwork,
    pub owner: SuiAddress,
    pub chain: String,
}

pub(super) struct Prepared {
    pub transaction: Transaction,
    pub simulation: Effects,
}

pub(super) struct Submitted {
    pub digest: String,
    pub effects: Effects,
}

/// `1.5` / `-0.25` for signed base-unit amounts.
pub fn format_signed(amount: i128, decimals: u32) -> String {
    let text = coins::format_amount(amount.unsigned_abs(), decimals);
    if amount < 0 { format!("-{text}") } else { text }
}

fn gas_json(g: &GasSummary) -> Value {
    Value::object(vec![
        ("computation_mist", Value::Number(g.computation.to_string())),
        ("storage_mist", Value::Number(g.storage.to_string())),
        ("storage_rebate_mist", Value::Number(g.rebate.to_string())),
        ("net_mist", Value::Number(g.net_cost().to_string())),
        (
            "net_sui",
            Value::String(coins::format_amount(u128::from(g.net_cost()), SUI_DECIMALS)),
        ),
    ])
}

fn network_json(network: SuiNetwork) -> Value {
    Value::object(vec![
        ("name", Value::str(network.id())),
        ("chain", Value::str(network.chain_name())),
        ("graphql_url", Value::str(network.graphql_url())),
        ("explorer_url", Value::str(network.explorer_url())),
        ("currency_symbol", Value::str("SUI")),
    ])
}

fn parse_recipient(to: &str, field: &str) -> Result<SuiAddress, String> {
    let address = coins::parse_address(to).map_err(|e| format!("'{field}': {e}"))?;
    if address == SuiAddress::ZERO {
        return Err(format!(
            "'{field}' must not be the zero address (coins sent there are lost)"
        ));
    }
    Ok(address)
}

fn parse_digest(input: &str) -> Result<Digest, String> {
    let bad = || "a Sui transaction digest is a base58 string of about 44 characters".to_string();
    let digest = input.trim().parse::<Digest>().map_err(|_| bad())?;
    // The SDK pads short inputs with zeros; only the canonical spelling is a real digest.
    if digest.to_string() != input.trim() {
        return Err(bad());
    }
    Ok(digest)
}

impl WalletService {
    pub(super) fn sui_session<'a>(&'a self, vault: &SuiVault) -> Result<Session<'a>, String> {
        let gql = Gql::new(&*self.http, vault.network);
        let chain = gql.verify_network(vault.network)?;
        Ok(Session {
            gql,
            network: vault.network,
            owner: vault.address()?,
            chain,
        })
    }

    pub(super) fn sui_holdings(s: &Session<'_>, coin_type: &str) -> Result<Holdings, String> {
        Ok(Holdings {
            coins: s.gql.coin_objects(&s.owner, coin_type)?,
            address_balance: s.gql.address_balance_of(&s.owner, coin_type)?,
        })
    }

    /// Symbol and decimals; unknown coins have no decimals and are shown in base units.
    pub(super) fn sui_coin_info(s: &Session<'_>, coin_type: &str) -> (String, Option<u32>) {
        if coins::is_sui(coin_type) {
            return ("SUI".to_string(), Some(SUI_DECIMALS));
        }
        match s.gql.coin_metadata(coin_type) {
            Ok(Some(m)) => (m.symbol, Some(m.decimals)),
            _ => (coins::fallback_symbol(coin_type), None),
        }
    }

    /// Decimals are required for sending and swapping: guessing them could move 1000x too much.
    pub(super) fn sui_required_decimals(
        s: &Session<'_>,
        coin_type: &str,
    ) -> Result<(String, u32), String> {
        let (symbol, decimals) = Self::sui_coin_info(s, coin_type);
        match decimals {
            Some(d) => Ok((symbol, d)),
            None => Err(format!(
                "coin {} has no on-chain metadata, so its decimals are unknown and amounts cannot be converted safely",
                sanitize_text(&symbol)
            )),
        }
    }

    // ---- wallet lifecycle -----------------------------------------------------------------------

    pub(super) fn sui_network_arg(network: Option<&str>) -> Result<SuiNetwork, String> {
        match network {
            Some(n) => SuiNetwork::parse(n),
            None => Ok(SuiNetwork::Mainnet),
        }
    }

    pub(super) fn sui_store_new(
        &self,
        name: String,
        password: &str,
        contents: &SuiVault,
        status: &str,
    ) -> Result<Value, String> {
        let address = contents.address()?;
        let record = WalletRecord {
            name: name.clone(),
            blockchain: Blockchain::Sui,
            vault: contents.seal(password, self.iterations)?,
        };
        self.store.create(&record)?;
        Ok(Value::object(vec![
            ("name", Value::String(name)),
            ("blockchain", Value::str(Blockchain::Sui.display_name())),
            ("network", Value::str(contents.network.id())),
            ("address", Value::String(address.to_string())),
            ("status", Value::str(status)),
        ]))
    }

    pub(super) fn sui_address(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_sui(name, password)?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("blockchain", Value::str(Blockchain::Sui.display_name())),
            ("network", Value::str(vault.network.id())),
            ("address", Value::String(vault.address()?.to_string())),
        ]))
    }

    pub(super) fn sui_seed_phrase(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_sui(name, password)?;
        let phrase = vault
            .mnemonic()
            .ok_or("this wallet was imported from a private key and has no seed phrase")?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("seed_phrase", Value::str(phrase.expose())),
            (
                "derivation_path",
                Value::str(crate::crypto::slip10::SUI_DEFAULT_PATH),
            ),
            (
                "warning",
                Value::str("Anyone with this phrase controls the wallet. Never share it."),
            ),
        ]))
    }

    pub(super) fn sui_private_key(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_sui(name, password)?;
        let key = vault.private_key()?;
        let bech32 = vault.suiprivkey()?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("address", Value::String(vault.address()?.to_string())),
            ("private_key", Value::str(bech32.expose())),
            (
                "private_key_hex",
                Value::String(crate::crypto::hex::encode_prefixed(key.expose())),
            ),
            ("scheme", Value::str("ed25519")),
            (
                "warning",
                Value::str(
                    "Anyone with this key controls the wallet. Never share it. private_key is the suiprivkey format that Sui wallets and the Sui CLI import.",
                ),
            ),
        ]))
    }

    pub(super) fn sui_export_payload(
        name: &str,
        vault: &SuiVault,
    ) -> Result<crate::crypto::secret::SecretBytes, String> {
        let key = vault.private_key()?;
        let mut pairs = vec![
            ("format", Value::str("agent-saifu-wallet-export")),
            ("version", Value::int(1)),
            ("name", Value::str(name)),
            ("blockchain", Value::str(Blockchain::Sui.id())),
            ("network", Value::str(vault.network.id())),
            ("address", Value::String(vault.address()?.to_string())),
            ("scheme", Value::str("ed25519")),
            ("privateKey", Value::str(vault.suiprivkey()?.expose())),
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

    pub(super) fn sui_networks(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_sui(name, password)?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("blockchain", Value::str(Blockchain::Sui.display_name())),
            ("networks", Value::Array(vec![network_json(vault.network)])),
            (
                "note",
                Value::str(
                    "A Sui wallet is bound to the one network it was created for, so a testnet key can never sign a mainnet transaction. Create or import another wallet for another network.",
                ),
            ),
        ]))
    }

    // ---- balances -------------------------------------------------------------------------------

    pub fn sui_balances(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_sui(name, password)?;
        let s = self.sui_session(&vault)?;
        let (balances, truncated) = s.gql.balances(&s.owner)?;
        let price_usd = if s.network.is_mainnet() {
            valuation::sui_price_usd(&*self.http)
        } else {
            None
        };
        let mut quotes_left = if s.network.is_mainnet() {
            valuation::MAX_QUOTED_COINS
        } else {
            0
        };
        let mut shown: Vec<_> = balances
            .into_iter()
            .filter(|b| b.total > 0 || coins::is_sui(&b.coin_type))
            .collect();
        shown.sort_by_key(|b| !coins::is_sui(&b.coin_type));
        let mut warnings: Vec<Value> = vec![];
        if !shown.iter().any(|b| coins::is_sui(&b.coin_type)) {
            shown.insert(
                0,
                crate::sui::gql::CoinBalance {
                    coin_type: SUI_COIN_TYPE.to_string(),
                    total: 0,
                    coin_objects: 0,
                    address_balance: 0,
                    symbol: Some("SUI".to_string()),
                    decimals: Some(SUI_DECIMALS),
                },
            );
        }
        let assets: Vec<Value> = shown
            .iter()
            .map(|b| {
                let decimals = b
                    .decimals
                    .or(coins::is_sui(&b.coin_type).then_some(SUI_DECIMALS));
                let symbol = b
                    .symbol
                    .clone()
                    .unwrap_or_else(|| coins::fallback_symbol(&b.coin_type));
                let value = decimals.and_then(|d| {
                    valuation::holding_value_usd(
                        &*self.http,
                        &self.cetus,
                        price_usd,
                        &b.coin_type,
                        b.total,
                        d,
                        &mut quotes_left,
                    )
                });
                if decimals.is_none() {
                    warnings.push(Value::String(format!(
                        "{symbol} has no on-chain metadata; its amount is shown in base units"
                    )));
                }
                Value::object(vec![
                    ("symbol", Value::String(symbol)),
                    (
                        "asset",
                        if coins::is_sui(&b.coin_type) {
                            Value::str("native")
                        } else {
                            Value::str(&b.coin_type)
                        },
                    ),
                    (
                        "balance",
                        Value::String(coins::format_amount(b.total, decimals.unwrap_or(0))),
                    ),
                    ("balance_raw", Value::String(b.total.to_string())),
                    (
                        "decimals",
                        decimals.map_or(Value::Null, |d| num(u64::from(d))),
                    ),
                    (
                        "coin_objects_raw",
                        Value::String(b.coin_objects.to_string()),
                    ),
                    (
                        "address_balance_raw",
                        Value::String(b.address_balance.to_string()),
                    ),
                    (
                        "usdc_value",
                        value.map_or(Value::Null, |v| Value::String(format_usd(v))),
                    ),
                ])
            })
            .collect();
        if truncated {
            warnings.push(Value::str(
                "this address holds a very large number of coin types; only the first ones are listed",
            ));
        }
        let mut pairs = vec![
            ("name", Value::String(record.name)),
            ("address", Value::String(s.owner.to_string())),
            ("network", network_json(s.network)),
            ("assets", Value::Array(assets)),
            (
                "note",
                Value::str(
                    "Every coin type the address holds is listed. usdc_value is indicative USD value: SUI from CoinGecko, USDC at 1, other coins from the Cetus aggregator's quote for selling the whole balance (only the first few coins are valued); it is null on testnet/devnet and when no price exists. A coin's balance is the sum of its coin objects and its address balance.",
                ),
            ),
        ];
        if !warnings.is_empty() {
            pairs.push(("warnings", Value::Array(warnings)));
        }
        Ok(Value::object(pairs))
    }

    // ---- building and sending ---------------------------------------------------------------------

    /// Assembles a transaction that spends `amount` of `coin_type`, dry-runs it to set the gas
    /// budget, and dry-runs the final transaction again. Nothing is signed.
    pub(super) fn sui_prepare(
        s: &Session<'_>,
        sui: &Holdings,
        coin_type: &str,
        amount: u64,
        spend: &Holdings,
        body: tx::Body<'_>,
    ) -> Result<Prepared, String> {
        let epoch = s.gql.epoch()?;
        let env = tx::GasEnv {
            price: epoch.reference_gas_price,
            epoch: epoch.epoch,
            chain: s
                .chain
                .parse::<Digest>()
                .map_err(|_| "the Sui node reported an invalid chain identifier".to_string())?,
            nonce: tx::fresh_nonce()?,
        };
        let spent_sui = if coins::is_sui(coin_type) { amount } else { 0 };
        if spend.total() < u128::from(amount) {
            return Err(tx::insufficient(coin_type, u128::from(amount), spend));
        }
        let sim_budget = tx::max_budget(sui).saturating_sub(spent_sui);
        if sim_budget < env.price.saturating_mul(2000) {
            return Err(
                "not enough SUI to pay for gas (after the SUI being sent, if any)".to_string(),
            );
        }
        let (draft, _) = tx::assemble(
            s.owner, &env, sim_budget, sui, coin_type, amount, spend, body,
        )?;
        let dry = s.gql.simulate(&tx::encode(&draft)?)?;
        if !dry.success {
            return Err(format!(
                "the transaction would fail, so nothing was signed: {}",
                dry.error.as_deref().unwrap_or("unknown reason")
            ));
        }
        let budget = tx::budget_from_simulation(&dry.gas, env.price);
        let (transaction, _) =
            tx::assemble(s.owner, &env, budget, sui, coin_type, amount, spend, body)?;
        let simulation = s.gql.simulate(&tx::encode(&transaction)?)?;
        if !simulation.success {
            return Err(format!(
                "the transaction would fail, so nothing was signed: {}",
                simulation.error.as_deref().unwrap_or("unknown reason")
            ));
        }
        Ok(Prepared {
            transaction,
            simulation,
        })
    }

    /// Signs and submits. Sui executes a given signed transaction at most once, so if the answer
    /// is lost the digest can be looked up to learn what happened.
    pub(super) fn sui_submit(
        s: &Session<'_>,
        vault: &SuiVault,
        transaction: &Transaction,
    ) -> Result<Submitted, String> {
        let digest = tx::digest_of(transaction);
        let signature = {
            let key = vault.signer()?;
            tx::sign_transaction(&key, transaction)?
        };
        match s.gql.execute(&tx::encode(transaction)?, &signature) {
            Ok(effects) => Ok(Submitted { digest, effects }),
            Err(e) => match s.gql.transaction(&digest) {
                Ok(Some(found)) => Ok(Submitted {
                    digest,
                    effects: found.effects,
                }),
                // The node answered with a validation error and has no such transaction.
                Ok(None) if e.starts_with("Sui node error:") => Err(format!(
                    "the Sui network rejected the transaction: {}. It was not found on-chain (digest {digest}); check get_transaction before trying again if in doubt",
                    &e["Sui node error: ".len()..]
                )),
                _ => Err(format!(
                    "the answer to the submission was lost or unclear ({e}); the outcome is unknown. Do not send it again before checking: call get_transaction with transaction_hash {digest}"
                )),
            },
        }
    }

    pub(super) fn sui_changes_json(s: &Session<'_>, effects: &Effects) -> Value {
        let mut items = vec![];
        for (coin_type, amount) in tx::wallet_changes(effects, &s.owner) {
            let (symbol, decimals) = Self::sui_coin_info(s, &coin_type);
            let mut pairs = vec![
                ("symbol", Value::String(symbol)),
                (
                    "asset",
                    if coins::is_sui(&coin_type) {
                        Value::str("native")
                    } else {
                        Value::String(coin_type.clone())
                    },
                ),
                ("change_raw", Value::String(amount.to_string())),
            ];
            if let Some(d) = decimals {
                pairs.push(("change", Value::String(format_signed(amount, d))));
            }
            items.push(Value::object(pairs));
        }
        Value::Array(items)
    }

    pub(super) fn sui_effects_json(s: &Session<'_>, effects: &Effects) -> Value {
        let mut pairs = vec![
            (
                "status",
                Value::str(if effects.success { "success" } else { "failed" }),
            ),
            ("gas", gas_json(&effects.gas)),
            ("wallet_changes", Self::sui_changes_json(s, effects)),
        ];
        if let Some(error) = &effects.error {
            pairs.push(("error", Value::str(error)));
        }
        Value::object(pairs)
    }

    pub(super) fn sui_submission_report(s: &Session<'_>, submitted: &Submitted) -> Value {
        merge(
            Self::sui_effects_json(s, &submitted.effects),
            vec![
                ("transaction_digest", Value::str(&submitted.digest)),
                (
                    "explorer_url",
                    Value::String(s.network.explorer_tx_url(&submitted.digest)),
                ),
            ],
        )
    }

    pub fn sui_send_asset(
        &self,
        name: &str,
        password: &str,
        asset: &str,
        to: &str,
        amount: &str,
    ) -> Result<Value, String> {
        let (record, vault) = self.unlock_sui(name, password)?;
        let recipient = parse_recipient(to, "to")?;
        let coin_type = coins::normalize_coin_type(asset).map_err(|e| format!("'asset': {e}"))?;
        let s = self.sui_session(&vault)?;
        let (symbol, decimals) = Self::sui_required_decimals(&s, &coin_type)?;
        let raw = coins::parse_amount(amount, decimals).map_err(|e| format!("'amount': {e}"))?;
        let sui = Self::sui_holdings(&s, SUI_COIN_TYPE)?;
        let spend = if coins::is_sui(&coin_type) {
            sui.clone()
        } else {
            Self::sui_holdings(&s, &coin_type)?
        };
        let prepared = Self::sui_prepare(
            &s,
            &sui,
            &coin_type,
            raw,
            &spend,
            &tx::transfer_body(recipient),
        )?;
        let submitted = Self::sui_submit(&s, &vault, &prepared.transaction)?;
        Ok(merge(
            Self::sui_submission_report(&s, &submitted),
            vec![
                ("wallet", Value::String(record.name)),
                ("network", Value::str(s.network.id())),
                ("from", Value::String(s.owner.to_string())),
                ("to", Value::String(recipient.to_string())),
                ("asset", Value::String(symbol)),
                (
                    "amount",
                    Value::String(coins::format_amount(u128::from(raw), decimals)),
                ),
            ],
        ))
    }

    // ---- signing arbitrary transactions and messages ----------------------------------------------

    fn sui_check_chain(transaction: &Transaction, s: &Session<'_>) -> Result<(), String> {
        if let TransactionExpiration::ValidDuring { chain, .. } = &transaction.expiration
            && chain.to_string() != s.chain
        {
            return Err("the transaction is valid on a different Sui network than this wallet's; refusing to sign it".to_string());
        }
        Ok(())
    }

    /// Wallet Standard `sui:signTransaction` / `sui:signAndExecuteTransaction`: signs BCS
    /// transaction bytes built elsewhere (for example by a dapp or SDK).
    pub fn sui_sign_transaction(
        &self,
        name: &str,
        password: &str,
        transaction_bytes: &str,
        broadcast: bool,
    ) -> Result<Value, String> {
        let transaction = tx::decode(transaction_bytes)?;
        let (record, vault) = self.unlock_sui(name, password)?;
        let s = self.sui_session(&vault)?;
        tx::check_signable(&transaction, &s.owner)?;
        Self::sui_check_chain(&transaction, &s)?;
        let (summary, mut warnings) = tx::describe(&transaction, &s.owner);
        let encoded = tx::encode(&transaction)?;
        let dry = s.gql.simulate(&encoded);
        let dry_report = match &dry {
            Ok(effects) => {
                if !effects.success {
                    warnings.push(format!(
                        "a dry run of this transaction fails: {}",
                        effects.error.as_deref().unwrap_or("unknown reason")
                    ));
                }
                Self::sui_effects_json(&s, effects)
            }
            Err(e) => {
                warnings.push(format!("the dry run could not be completed: {e}"));
                Value::object(vec![
                    ("status", Value::str("unavailable")),
                    ("error", Value::str(e)),
                ])
            }
        };
        if broadcast && !matches!(&dry, Ok(effects) if effects.success) {
            return Err(format!(
                "not sending: the dry run of this transaction does not succeed ({})",
                match &dry {
                    Ok(effects) => effects
                        .error
                        .clone()
                        .unwrap_or_else(|| "unknown reason".to_string()),
                    Err(e) => e.clone(),
                }
            ));
        }
        let signature = {
            let key = vault.signer()?;
            tx::sign_transaction(&key, &transaction)?
        };
        let mut pairs = vec![
            ("wallet", Value::String(record.name)),
            ("network", Value::str(s.network.id())),
            ("address", Value::String(s.owner.to_string())),
            ("bytes", Value::String(encoded)),
            ("signature", Value::String(signature)),
            (
                "transaction_digest",
                Value::String(tx::digest_of(&transaction)),
            ),
            ("description", summary),
            (
                "warnings",
                Value::Array(warnings.into_iter().map(Value::String).collect()),
            ),
            ("dry_run", dry_report),
        ];
        if broadcast {
            let submitted = Self::sui_submit(&s, &vault, &transaction)?;
            pairs.push(("status", Value::str("submitted")));
            pairs.push(("execution", Self::sui_submission_report(&s, &submitted)));
        } else {
            pairs.push(("status", Value::str("signed")));
            pairs.push((
                "note",
                Value::str("Signed only; nothing was sent. Submit it with a Sui client, or call again with broadcast=true. `bytes` and `signature` are the Wallet Standard sui:signTransaction output."),
            ));
        }
        Ok(Value::object(pairs))
    }

    /// Wallet Standard `sui:signPersonalMessage`.
    pub fn sui_sign_message(
        &self,
        name: &str,
        password: &str,
        message: &str,
        format: Option<&str>,
    ) -> Result<Value, String> {
        let bytes = tx::parse_message(message, format)?;
        let (record, vault) = self.unlock_sui(name, password)?;
        let signature = {
            let key = vault.signer()?;
            tx::sign_personal_message(&key, &bytes)?
        };
        let readable = std::str::from_utf8(&bytes)
            .ok()
            .filter(|t| !t.chars().any(|c| c.is_control() && c != '\n' && c != '\t'));
        Ok(Value::object(vec![
            ("wallet", Value::String(record.name)),
            ("network", Value::str(vault.network.id())),
            ("address", Value::String(vault.address()?.to_string())),
            (
                "bytes",
                Value::String(crate::crypto::base64::encode(&bytes)),
            ),
            ("signature", Value::String(signature)),
            (
                "message_preview",
                match readable {
                    Some(text) => Value::String(sanitize_text_max(text, 500)),
                    None => Value::str("(binary data)"),
                },
            ),
            ("intent", Value::str("PersonalMessage")),
            (
                "note",
                Value::str(
                    "Signed with Sui's personal-message intent, so the signature can never be replayed as a transaction. `bytes` and `signature` are the Wallet Standard sui:signPersonalMessage output.",
                ),
            ),
        ]))
    }

    // ---- estimating and simulating ------------------------------------------------------------------

    /// Dry-runs either a transfer (`asset`, `amount`, `to`) or given `transaction_bytes`.
    fn sui_dry_run(
        &self,
        s: &Session<'_>,
        params: &IntentParams,
    ) -> Result<(Effects, Option<Value>, u64), String> {
        if params.value.is_some() || params.data.is_some() {
            return Err("'value' and 'data' are Ethereum parameters; for Sui give asset + amount + to, or transaction_bytes".to_string());
        }
        if let Some(bytes) = &params.transaction_bytes {
            if params.asset.is_some() || params.amount.is_some() || params.to.is_some() {
                return Err(
                    "use either transaction_bytes, or asset + amount + to, not both".to_string(),
                );
            }
            let transaction = tx::decode(bytes)?;
            tx::check_signable(&transaction, &s.owner)?;
            Self::sui_check_chain(&transaction, s)?;
            let (summary, warnings) = tx::describe(&transaction, &s.owner);
            let effects = s.gql.simulate(&tx::encode(&transaction)?)?;
            let mut pairs = vec![("transaction", summary)];
            if !warnings.is_empty() {
                pairs.push((
                    "warnings",
                    Value::Array(warnings.into_iter().map(Value::String).collect()),
                ));
            }
            return Ok((
                effects,
                Some(Value::object(pairs)),
                transaction.gas_payment.budget,
            ));
        }
        let to = params.to.as_deref().ok_or("'to' is required")?;
        let recipient = parse_recipient(to, "to")?;
        let asset = params
            .asset
            .as_deref()
            .ok_or("'asset' is required (use \"native\" for SUI)")?;
        let amount = params
            .amount
            .as_deref()
            .ok_or("'amount' is required together with 'asset'")?;
        let coin_type = coins::normalize_coin_type(asset).map_err(|e| format!("'asset': {e}"))?;
        let (_, decimals) = Self::sui_required_decimals(s, &coin_type)?;
        let raw = coins::parse_amount(amount, decimals).map_err(|e| format!("'amount': {e}"))?;
        let sui = Self::sui_holdings(s, SUI_COIN_TYPE)?;
        let spend = if coins::is_sui(&coin_type) {
            sui.clone()
        } else {
            Self::sui_holdings(s, &coin_type)?
        };
        let prepared = Self::sui_prepare(
            s,
            &sui,
            &coin_type,
            raw,
            &spend,
            &tx::transfer_body(recipient),
        )?;
        let budget = prepared.transaction.gas_payment.budget;
        Ok((prepared.simulation, None, budget))
    }

    pub fn sui_estimate_gas(
        &self,
        name: &str,
        password: &str,
        params: &IntentParams,
    ) -> Result<Value, String> {
        let (record, vault) = self.unlock_sui(name, password)?;
        let s = self.sui_session(&vault)?;
        let (effects, description, budget) = self.sui_dry_run(&s, params)?;
        let net = effects.gas.net_cost();
        let usd = if s.network.is_mainnet() {
            valuation::sui_price_usd(&*self.http)
                .map(|p| valuation::value_usd(u128::from(net), SUI_DECIMALS, p))
        } else {
            None
        };
        let mut pairs = vec![
            ("wallet", Value::String(record.name)),
            ("network", Value::str(s.network.id())),
            ("will_succeed", Value::Bool(effects.success)),
            ("gas", gas_json(&effects.gas)),
            ("gas_budget_mist", Value::Number(budget.to_string())),
            (
                "fee_usdc",
                usd.map_or(Value::Null, |v| Value::String(format!("{v:.4}"))),
            ),
            (
                "note",
                Value::str(
                    "The fee is computation plus storage minus the storage rebate, from a dry run; the gas budget is the cap the transaction would be given (unused budget is not charged). Sui's gas is paid in SUI.",
                ),
            ),
        ];
        if let Some(d) = description {
            pairs.push(("decoded", d));
        }
        if let Some(error) = &effects.error {
            pairs.push(("error", Value::str(error)));
        }
        Ok(Value::object(pairs))
    }

    pub fn sui_simulate_transaction(
        &self,
        name: &str,
        password: &str,
        params: &IntentParams,
    ) -> Result<Value, String> {
        let (record, vault) = self.unlock_sui(name, password)?;
        let s = self.sui_session(&vault)?;
        let (effects, description, budget) = self.sui_dry_run(&s, params)?;
        let mut pairs = vec![
            ("wallet", Value::String(record.name)),
            ("network", Value::str(s.network.id())),
            ("success", Value::Bool(effects.success)),
            ("gas_budget_mist", Value::Number(budget.to_string())),
            ("result", Self::sui_effects_json(&s, &effects)),
            (
                "note",
                Value::str(
                    "A dry run against the network's current state; nothing was signed or sent. wallet_changes is what this wallet's balances would change by, gas included.",
                ),
            ),
        ];
        if let Some(d) = description {
            pairs.push(("decoded", d));
        }
        Ok(Value::object(pairs))
    }

    // ---- looking things up ------------------------------------------------------------------------

    fn sui_transaction_json(s: &Session<'_>, info: &TransactionInfo) -> Value {
        let mut pairs = vec![
            ("transaction_digest", Value::str(&info.digest)),
            (
                "explorer_url",
                Value::String(s.network.explorer_tx_url(&info.digest)),
            ),
            (
                "status",
                Value::str(if info.effects.success {
                    "success"
                } else {
                    "failed"
                }),
            ),
        ];
        if let Some(sender) = &info.sender {
            pairs.push(("sender", Value::String(sanitize_text_max(sender, 70))));
        }
        if let Some(t) = &info.timestamp {
            pairs.push(("timestamp", Value::String(sanitize_text_max(t, 40))));
        }
        if let Some(c) = info.checkpoint {
            pairs.push(("checkpoint", num(c)));
        }
        if let Some(e) = info.epoch {
            pairs.push(("epoch", num(e)));
        }
        if let Some(error) = &info.effects.error {
            pairs.push(("error", Value::str(error)));
        }
        pairs.push(("gas", gas_json(&info.effects.gas)));
        pairs.push(("wallet_changes", Self::sui_changes_json(s, &info.effects)));
        Value::object(pairs)
    }

    pub fn sui_get_transaction(
        &self,
        name: &str,
        password: &str,
        digest: &str,
    ) -> Result<Value, String> {
        let digest = parse_digest(digest)?.to_string();
        let (_record, vault) = self.unlock_sui(name, password)?;
        let s = self.sui_session(&vault)?;
        match s.gql.transaction(&digest)? {
            Some(info) => Ok(merge(
                Self::sui_transaction_json(&s, &info),
                vec![("network", Value::str(s.network.id()))],
            )),
            None => Err(format!(
                "the Sui network has no transaction {digest} (it may still be pending, or belong to another network)"
            )),
        }
    }

    pub fn sui_history(
        &self,
        name: &str,
        password: &str,
        kind: HistoryKind,
        limit: usize,
    ) -> Result<Value, String> {
        if kind != HistoryKind::Transactions {
            return Err(
                "Sui history lists transactions only; token_transfers is an Ethereum option"
                    .to_string(),
            );
        }
        let (record, vault) = self.unlock_sui(name, password)?;
        let s = self.sui_session(&vault)?;
        let (items, has_more) = s.gql.history(&s.owner, limit)?;
        let list: Vec<Value> = items
            .iter()
            .map(|t| Self::sui_transaction_json(&s, t))
            .collect();
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("address", Value::String(s.owner.to_string())),
            ("network", Value::str(s.network.id())),
            ("type", Value::str("transactions")),
            ("count", num(list.len() as u64)),
            ("has_more", Value::Bool(has_more)),
            ("items", Value::Array(list)),
        ]))
    }
}
