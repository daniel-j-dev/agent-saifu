//! Bitcoin wallets in the wallet service: lifecycle, balances, sending, fee estimates, message
//! signing and history.
//!
//! Every operation unlocks the wallet for that single call, exactly like the other chains'
//! tools. A Bitcoin wallet is bound to one network (mainnet, testnet or signet) chosen when it
//! is created, and has exactly one address: native SegWit (P2WPKH).
//!
//! The Esplora server is untrusted. It chooses nothing that is signed without being checked:
//! the recipient and amount come from the caller, the wallet builds and signs the transaction
//! itself, and a lie about a coin's value only produces a signature the network rejects (a
//! SegWit signature commits to the value it spends).

use std::collections::HashSet;

use super::solana::url_host;
use super::*;
use crate::bitcoin::address::Address;
use crate::bitcoin::builder::{self, Plan, PlanError, Utxo};
use crate::bitcoin::effects::{self, Direction};
use crate::bitcoin::journal::{PendingBitcoinTx, journal_key};
use crate::bitcoin::message;
use crate::bitcoin::network::BitcoinNetwork;
use crate::bitcoin::rpc::{self, EsploraClient, RawUtxo, TxInfo};
use crate::bitcoin::tx::{OutPoint, parse_txid, txid_to_hex};
use crate::bitcoin::units::{self, FeeRate, format_btc, format_signed_btc};
use crate::eth::explorer::sanitize_text_max;
use crate::eth::prices::{self, format_usd};

/// Blocks within which the default fee should confirm, Bitcoin Core's default target.
const DEFAULT_CONFIRMATION_TARGET: u32 = 6;
const MAX_CONFIRMATION_TARGET: u64 = 1_008;
/// How many unconfirmed coins are checked for being the wallet's own change.
const MAX_PARENT_CHECKS: usize = 25;
const MAX_HISTORY_PAGES: usize = 4;
const MAX_OUTPUTS_SHOWN: usize = 20;
/// Fees above this share of the amount sent are flagged.
const FEE_WARNING_PERCENT: u64 = 5;
const TYPICAL_VSIZE: u64 = 141;
const ESTIMATE_TARGETS: [u32; 6] = [1, 3, 6, 24, 144, 1008];

pub(super) struct BtcSession<'a> {
    pub api: EsploraClient<'a>,
    pub network: BitcoinNetwork,
    pub address_text: String,
    pub script: Vec<u8>,
}

struct Spendable {
    coins: Vec<Utxo>,
    unconfirmed_own: usize,
    /// Incoming payments from others that are still unconfirmed, which are not spent.
    untrusted: u64,
}

struct Prepared {
    plan: Plan,
    recipient: Address,
    recipient_text: String,
    tip: u64,
    /// The confirmation target the fee came from; `None` for an explicit fee rate.
    target: Option<u32>,
    unconfirmed_own: usize,
}

enum PrepareError {
    /// The request cannot be fulfilled (not enough funds, dust, fee limits).
    Refused(String),
    /// Something went wrong reading the request or the network.
    Failed(String),
}

impl From<String> for PrepareError {
    fn from(message: String) -> PrepareError {
        PrepareError::Failed(message)
    }
}

impl From<PlanError> for PrepareError {
    fn from(error: PlanError) -> PrepareError {
        PrepareError::Refused(error.to_string())
    }
}

impl PrepareError {
    fn message(self) -> String {
        match self {
            PrepareError::Refused(m) | PrepareError::Failed(m) => m,
        }
    }
}

struct Signed {
    txid: String,
    raw: Vec<u8>,
    vsize: u64,
}

fn network_label(network: BitcoinNetwork) -> &'static str {
    match network {
        BitcoinNetwork::Mainnet => "Bitcoin mainnet",
        BitcoinNetwork::Testnet => "Bitcoin testnet (testnet4)",
        BitcoinNetwork::Signet => "Bitcoin signet",
    }
}

pub(super) fn bitcoin_network_json(network: BitcoinNetwork) -> Value {
    Value::object(vec![
        ("name", Value::str(network.id())),
        ("chain", Value::str(network.chain_name())),
        ("description", Value::str(network_label(network))),
        ("api_host", Value::String(url_host(network.api_url()))),
        ("explorer_url", Value::str(network.explorer_url())),
        ("currency_symbol", Value::str("BTC")),
        ("address_type", Value::str("P2WPKH (native SegWit)")),
        ("derivation_path", Value::String(network.derivation_path())),
    ])
}

fn parse_confirmation_target(target: Option<u64>) -> Result<u32, String> {
    match target {
        None => Ok(DEFAULT_CONFIRMATION_TARGET),
        Some(t) if (1..=MAX_CONFIRMATION_TARGET).contains(&t) => Ok(t as u32),
        Some(_) => Err(format!(
            "confirmation_target must be between 1 and {MAX_CONFIRMATION_TARGET} blocks"
        )),
    }
}

fn check_asset(asset: Option<&str>) -> Result<(), String> {
    match asset.map(|a| a.trim().to_lowercase()).as_deref() {
        None | Some("native") | Some("btc") | Some("bitcoin") => Ok(()),
        Some(_) => Err(
            "a Bitcoin wallet holds only bitcoin; use asset \"native\" (there are no tokens)"
                .to_string(),
        ),
    }
}

fn percent_of(part: u64, whole: u64) -> u64 {
    if whole == 0 {
        return 0;
    }
    (u128::from(part) * 100 / u128::from(whole)) as u64
}

fn warnings_json(items: Vec<String>) -> Value {
    Value::Array(items.into_iter().map(Value::String).collect())
}

impl WalletService {
    pub(super) fn bitcoin_session<'a>(
        &'a self,
        vault: &BitcoinVault,
    ) -> Result<BtcSession<'a>, String> {
        let api = EsploraClient::new(&*self.http, vault.network.api_url())?;
        let genesis = api.genesis_hash()?;
        if genesis != vault.network.genesis_hash() {
            return Err(format!(
                "the Bitcoin API at {} does not serve {} (it reported another genesis block); refusing to use it",
                api.host(),
                network_label(vault.network)
            ));
        }
        let address = vault.address_kind()?;
        Ok(BtcSession {
            script: address.script_pubkey(),
            address_text: address.to_text(vault.network),
            network: vault.network,
            api,
        })
    }

    fn btc_usd_price(&self, network: BitcoinNetwork) -> Option<f64> {
        if !network.is_mainnet() {
            return None;
        }
        let body = self.http.get(&prices::native_price_url("bitcoin")).ok()?;
        prices::parse_native_price(&body, "bitcoin")
    }

    fn btc_usd_json(&self, network: BitcoinNetwork, sats: u64) -> Value {
        match self.btc_usd_price(network) {
            Some(price) => Value::String(format_usd(sats as f64 / 1e8 * price)),
            None => Value::Null,
        }
    }

    // ---- wallet lifecycle -----------------------------------------------------------------------

    pub(super) fn bitcoin_network_arg(network: Option<&str>) -> Result<BitcoinNetwork, String> {
        match network {
            Some(n) => BitcoinNetwork::parse(n),
            None => Ok(BitcoinNetwork::Mainnet),
        }
    }

    pub(super) fn bitcoin_store_new(
        &self,
        name: String,
        password: &str,
        contents: &BitcoinVault,
        status: &str,
    ) -> Result<Value, String> {
        let address = contents.address()?;
        let record = WalletRecord {
            name: name.clone(),
            blockchain: Blockchain::Bitcoin,
            vault: contents.seal(password, self.iterations)?,
        };
        self.store.create(&record)?;
        Ok(Value::object(vec![
            ("name", Value::String(name)),
            ("blockchain", Value::str(Blockchain::Bitcoin.display_name())),
            ("network", Value::str(contents.network.id())),
            ("address", Value::String(address)),
            ("address_type", Value::str("P2WPKH (native SegWit)")),
            ("status", Value::str(status)),
        ]))
    }

    pub(super) fn bitcoin_address(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_bitcoin(name, password)?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("blockchain", Value::str(Blockchain::Bitcoin.display_name())),
            ("network", Value::str(vault.network.id())),
            ("address", Value::String(vault.address()?)),
            ("address_type", Value::str("P2WPKH (native SegWit)")),
        ]))
    }

    pub(super) fn bitcoin_seed_phrase(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_bitcoin(name, password)?;
        let phrase = vault
            .mnemonic()
            .ok_or("this wallet was imported from a private key and has no seed phrase")?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("seed_phrase", Value::str(phrase.expose())),
            (
                "derivation_path",
                Value::String(vault.network.derivation_path()),
            ),
            (
                "warning",
                Value::str(
                    "Anyone with this phrase controls the wallet. Never share it. It follows BIP 39 and BIP 84, so wallets such as Sparrow, Electrum (BIP 39 mode), Ledger and Trezor derive the same native SegWit address from it.",
                ),
            ),
        ]))
    }

    pub(super) fn bitcoin_private_key(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_bitcoin(name, password)?;
        let key = vault.private_key()?;
        let wif = vault.wif()?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("address", Value::String(vault.address()?)),
            ("private_key", Value::str(wif.expose())),
            (
                "private_key_hex",
                Value::String(crate::crypto::hex::encode(key.expose())),
            ),
            ("format", Value::str("WIF (compressed)")),
            (
                "warning",
                Value::str(
                    "Anyone with this key controls the wallet. Never share it. private_key is the Wallet Import Format that Bitcoin Core (importdescriptors with wpkh()), Sparrow and Electrum import; private_key_hex is the 32-byte secret alone.",
                ),
            ),
        ]))
    }

    pub(super) fn bitcoin_export_payload(
        name: &str,
        vault: &BitcoinVault,
    ) -> Result<crate::crypto::secret::SecretBytes, String> {
        let key = vault.private_key()?;
        let mut pairs = vec![
            ("format", Value::str("agent-saifu-wallet-export")),
            ("version", Value::int(1)),
            ("name", Value::str(name)),
            ("blockchain", Value::str(Blockchain::Bitcoin.id())),
            ("network", Value::str(vault.network.id())),
            ("address", Value::String(vault.address()?)),
            ("addressType", Value::str("P2WPKH")),
            (
                "derivationPath",
                Value::String(vault.network.derivation_path()),
            ),
            ("privateKey", Value::str(vault.wif()?.expose())),
            (
                "privateKeyHex",
                Value::String(crate::crypto::hex::encode(key.expose())),
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

    pub(super) fn bitcoin_networks(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_bitcoin(name, password)?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("blockchain", Value::str(Blockchain::Bitcoin.display_name())),
            (
                "networks",
                Value::Array(vec![bitcoin_network_json(vault.network)]),
            ),
            (
                "note",
                Value::str(
                    "A Bitcoin wallet is bound to the one network it was created for, so a testnet key can never sign a mainnet transaction, and addresses of other networks are refused. Create or import another wallet for another network.",
                ),
            ),
        ]))
    }

    // ---- balances -------------------------------------------------------------------------------

    pub fn bitcoin_balances(&self, name: &str, password: &str) -> Result<Value, String> {
        let (record, vault) = self.unlock_bitcoin(name, password)?;
        let s = self.bitcoin_session(&vault)?;
        let stats = s.api.address_stats(&s.address_text)?;
        let total = stats.confirmed + stats.pending;
        let to_sats = |value: i128| {
            u64::try_from(value).map_err(|_| {
                "the Bitcoin API reported an impossible (negative) balance".to_string()
            })
        };
        let confirmed = to_sats(stats.confirmed)?;
        let total = to_sats(total)?;
        let asset = Value::object(vec![
            ("symbol", Value::str("BTC")),
            ("asset", Value::str("native")),
            ("name", Value::str("Bitcoin")),
            ("balance", Value::String(format_btc(total))),
            ("balance_raw", Value::String(total.to_string())),
            ("decimals", num(8)),
            ("confirmed_balance", Value::String(format_btc(confirmed))),
            (
                "unconfirmed_change",
                Value::String(format_signed_btc(stats.pending)),
            ),
            ("usdc_value", self.btc_usd_json(s.network, total)),
        ]);
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("address", Value::String(s.address_text.clone())),
            ("network", bitcoin_network_json(s.network)),
            ("assets", Value::Array(vec![asset])),
            (
                "note",
                Value::str(
                    "balance is what the address holds once its unconfirmed transactions are mined (confirmed_balance plus unconfirmed_change). Only confirmed coins, and the wallet's own unconfirmed change, can be spent. usdc_value is indicative USD value from CoinGecko on mainnet; it is null on test networks.",
                ),
            ),
        ]))
    }

    // ---- reading the wallet's coins and fees -----------------------------------------------------

    /// The coins this wallet will spend: confirmed ones, and unconfirmed ones only when they
    /// are change from a transaction funded entirely by this wallet.
    fn btc_spendable(&self, s: &BtcSession<'_>) -> Result<Spendable, String> {
        let raw = s.api.utxos(&s.address_text)?;
        let mut seen: HashSet<(String, u32)> = HashSet::new();
        let mut spendable = Spendable {
            coins: vec![],
            unconfirmed_own: 0,
            untrusted: 0,
        };
        let mut checks_left = MAX_PARENT_CHECKS;
        for coin in raw {
            if coin.value == 0 || !seen.insert((coin.txid.clone(), coin.vout)) {
                continue;
            }
            let outpoint = OutPoint {
                txid: parse_txid(&coin.txid)?,
                vout: coin.vout,
            };
            let usable = Utxo {
                outpoint,
                value: coin.value,
            };
            if coin.status.confirmed {
                spendable.coins.push(usable);
                continue;
            }
            if checks_left > 0 && self.btc_is_own_change(s, &coin, &mut checks_left)? {
                spendable.coins.push(usable);
                spendable.unconfirmed_own += 1;
            } else {
                spendable.untrusted = spendable.untrusted.saturating_add(coin.value);
            }
        }
        Ok(spendable)
    }

    fn btc_is_own_change(
        &self,
        s: &BtcSession<'_>,
        coin: &RawUtxo,
        checks_left: &mut usize,
    ) -> Result<bool, String> {
        *checks_left -= 1;
        let Some(parent) = s.api.transaction(&coin.txid)? else {
            return Ok(false);
        };
        let pays_us = parent
            .outputs
            .get(coin.vout as usize)
            .is_some_and(|o| o.script == s.script && o.value == coin.value);
        Ok(pays_us && effects::funded_entirely_by(&parent, &s.script))
    }

    /// Returns the rate, the confirmation target it came from (none for an explicit rate), and
    /// whether the caller chose the rate.
    fn btc_fee_rate(
        &self,
        s: &BtcSession<'_>,
        fee: &BitcoinFee,
    ) -> Result<(FeeRate, Option<u32>, bool), String> {
        match (&fee.fee_rate, fee.confirmation_target) {
            (Some(_), Some(_)) => {
                Err("give either fee_rate or confirmation_target, not both".to_string())
            }
            (Some(text), None) => {
                let rate = FeeRate::parse(text)?;
                if rate < FeeRate::MIN {
                    return Err("fee_rate must be at least 1 sat/vB, the minimum relay fee of most Bitcoin nodes".to_string());
                }
                if rate > FeeRate::MAX {
                    return Err(
                        "fee_rate is above 1,000 sat/vB; that is almost certainly a mistake"
                            .to_string(),
                    );
                }
                Ok((rate, None, true))
            }
            (None, target) => {
                let target = parse_confirmation_target(target)?;
                let estimates = s.api.fee_estimates()?;
                let (used, rate) = estimates
                    .for_target(target)
                    .ok_or("the Bitcoin API has no fee estimates; give fee_rate")?;
                if rate > FeeRate::MAX {
                    return Err("the Bitcoin API's fee estimate is implausibly high (above 1,000 sat/vB); give fee_rate yourself to proceed".to_string());
                }
                Ok((rate.max(FeeRate::MIN), Some(used), false))
            }
        }
    }

    fn btc_prepare(
        &self,
        s: &BtcSession<'_>,
        to: &str,
        amount: &str,
        fee: &BitcoinFee,
    ) -> Result<Prepared, PrepareError> {
        let recipient = Address::parse(to, s.network).map_err(PrepareError::Failed)?;
        if recipient.script_pubkey() == s.script {
            return Err(PrepareError::Failed(
                "'to' is this wallet's own address; sending to yourself would only cost a fee"
                    .to_string(),
            ));
        }
        let sats = units::parse_btc(amount)?;
        let (rate, target, explicit) = self.btc_fee_rate(s, fee)?;
        let tip = s.api.tip_height()?;
        let spendable = self.btc_spendable(s)?;
        let plan = match builder::plan_transfer(&spendable.coins, &recipient, sats, rate) {
            Ok(plan) => plan,
            Err(PlanError::InsufficientFunds { usable, needed }) if spendable.untrusted > 0 => {
                return Err(PrepareError::Refused(format!(
                    "{} {} BTC of incoming payments are still unconfirmed and cannot be spent until they confirm.",
                    PlanError::InsufficientFunds { usable, needed },
                    format_btc(spendable.untrusted)
                )));
            }
            Err(e) => return Err(e.into()),
        };
        if !explicit && plan.fee > plan.amount {
            return Err(PrepareError::Refused(format!(
                "the network fee ({} BTC at {} sat/vB) is higher than the amount being sent ({} BTC). Give fee_rate to send anyway, or a larger confirmation_target for a cheaper fee.",
                format_btc(plan.fee),
                plan.fee_rate.display(),
                format_btc(plan.amount)
            )));
        }
        Ok(Prepared {
            plan,
            recipient_text: recipient.to_text(s.network),
            recipient,
            tip,
            target,
            unconfirmed_own: spendable.unconfirmed_own,
        })
    }

    fn btc_warnings(prepared: &Prepared) -> Vec<String> {
        let plan = &prepared.plan;
        let mut warnings = vec![];
        let percent = percent_of(plan.fee, plan.amount);
        if percent >= FEE_WARNING_PERCENT {
            warnings.push(format!("the network fee is {percent}% of the amount sent"));
        }
        if plan.change.is_none() {
            let dust = plan.inputs.iter().map(|u| u.value).sum::<u64>() - plan.amount;
            let ordinary = plan.fee_rate.fee_for(plan.vsize);
            if dust > ordinary {
                warnings.push(format!(
                    "the remainder of {} BTC was too small to be worth a change output, so it is added to the fee",
                    format_btc(dust - ordinary)
                ));
            }
        }
        if prepared.unconfirmed_own > 0 {
            warnings.push(
                "this spends unconfirmed change from an earlier transaction of this wallet; it can only confirm after that one does".to_string(),
            );
        }
        warnings
    }

    /// The fields every transfer report shares.
    fn btc_transfer_fields(
        &self,
        s: &BtcSession<'_>,
        prepared: &Prepared,
    ) -> Vec<(&'static str, Value)> {
        let plan = &prepared.plan;
        let mut pairs = vec![
            ("network", Value::str(s.network.id())),
            ("from", Value::String(s.address_text.clone())),
            ("to", Value::String(prepared.recipient_text.clone())),
            ("recipient_type", Value::str(prepared.recipient.type_name())),
            ("asset", Value::str("BTC")),
            ("amount", Value::String(format_btc(plan.amount))),
            ("amount_sats", Value::String(plan.amount.to_string())),
            ("network_fee_btc", Value::String(format_btc(plan.fee))),
            ("network_fee_sats", Value::String(plan.fee.to_string())),
            ("network_fee_usdc", self.btc_usd_json(s.network, plan.fee)),
            (
                "fee_rate_sat_per_vb",
                Value::String(plan.fee_rate.display()),
            ),
            ("estimated_vsize", num(plan.vsize)),
            ("inputs_spent", num(plan.inputs.len() as u64)),
            (
                "change",
                plan.change
                    .map_or(Value::Null, |c| Value::String(format_btc(c))),
            ),
        ];
        if let Some(target) = prepared.target {
            pairs.push(("confirmation_target_blocks", num(u64::from(target))));
        }
        pairs
    }

    // ---- estimating and simulating --------------------------------------------------------------

    fn btc_transfer_params(params: &IntentParams) -> Result<Option<(&str, &str)>, String> {
        if params.value.is_some() || params.data.is_some() || params.transaction_bytes.is_some() {
            return Err("Bitcoin has no smart contracts: 'value', 'data' and 'transaction_bytes' are not used; give 'to' and 'amount'".to_string());
        }
        check_asset(params.asset.as_deref())?;
        match (params.to.as_deref(), params.amount.as_deref()) {
            (Some(to), Some(amount)) => Ok(Some((to, amount))),
            (None, None) => Ok(None),
            _ => Err("give both 'to' and 'amount' to price a payment".to_string()),
        }
    }

    pub fn bitcoin_estimate_gas(
        &self,
        name: &str,
        password: &str,
        params: &IntentParams,
        fee: &BitcoinFee,
    ) -> Result<Value, String> {
        let transfer = Self::btc_transfer_params(params)?;
        let (record, vault) = self.unlock_bitcoin(name, password)?;
        let s = self.bitcoin_session(&vault)?;
        let Some((to, amount)) = transfer else {
            return self.btc_generic_estimate(&s, record.name, fee);
        };
        let prepared = self
            .btc_prepare(&s, to, amount, fee)
            .map_err(PrepareError::message)?;
        let mut pairs = vec![("wallet", Value::String(record.name))];
        pairs.extend(self.btc_transfer_fields(&s, &prepared));
        let warnings = Self::btc_warnings(&prepared);
        if !warnings.is_empty() {
            pairs.push(("warnings", warnings_json(warnings)));
        }
        pairs.push((
            "note",
            Value::str("The fee is charged per virtual byte of the signed transaction (sized for the largest possible signatures, so it is never short of the rate). Nothing was signed or sent."),
        ));
        Ok(Value::object(pairs))
    }

    /// Without a payment: the current rates and what a typical payment costs.
    fn btc_generic_estimate(
        &self,
        s: &BtcSession<'_>,
        wallet: String,
        fee: &BitcoinFee,
    ) -> Result<Value, String> {
        let (rate, target, _) = self.btc_fee_rate(s, fee)?;
        let estimates = s.api.fee_estimates()?;
        let table: Vec<Value> = ESTIMATE_TARGETS
            .iter()
            .filter_map(|t| estimates.for_target(*t).map(|(used, r)| (*t, used, r)))
            .map(|(asked, _, r)| {
                let r = r.max(FeeRate::MIN);
                Value::object(vec![
                    ("confirmation_target_blocks", num(u64::from(asked))),
                    ("fee_rate_sat_per_vb", Value::String(r.display())),
                    (
                        "typical_payment_fee_btc",
                        Value::String(format_btc(r.fee_for(TYPICAL_VSIZE))),
                    ),
                ])
            })
            .collect();
        let typical_fee = rate.fee_for(TYPICAL_VSIZE);
        let mut pairs = vec![
            ("wallet", Value::String(wallet)),
            ("network", Value::str(s.network.id())),
            ("asset", Value::str("BTC")),
            ("fee_rate_sat_per_vb", Value::String(rate.display())),
            ("estimated_vsize", num(TYPICAL_VSIZE)),
            ("network_fee_btc", Value::String(format_btc(typical_fee))),
            ("network_fee_sats", Value::String(typical_fee.to_string())),
            (
                "network_fee_usdc",
                self.btc_usd_json(s.network, typical_fee),
            ),
            ("rates_by_confirmation_target", Value::Array(table)),
            (
                "note",
                Value::str(
                    "No payment was given, so this prices a typical one-input, two-output native SegWit payment (141 virtual bytes). Give 'to' and 'amount' for the exact fee of a payment. Nothing was signed or sent.",
                ),
            ),
        ];
        if let Some(target) = target {
            pairs.push(("confirmation_target_blocks", num(u64::from(target))));
        }
        Ok(Value::object(pairs))
    }

    pub fn bitcoin_simulate_transaction(
        &self,
        name: &str,
        password: &str,
        params: &IntentParams,
        fee: &BitcoinFee,
    ) -> Result<Value, String> {
        let Some((to, amount)) = Self::btc_transfer_params(params)? else {
            return Err("give 'to' and 'amount' to dry-run a payment".to_string());
        };
        let (record, vault) = self.unlock_bitcoin(name, password)?;
        let s = self.bitcoin_session(&vault)?;
        let mut pairs = vec![("wallet", Value::String(record.name))];
        match self.btc_prepare(&s, to, amount, fee) {
            Ok(prepared) => {
                let signed = self.btc_sign(&s, &vault, &prepared)?;
                let balance = prepared.plan.inputs.iter().map(|u| u.value).sum::<u64>();
                pairs.push(("would_succeed", Value::Bool(true)));
                pairs.extend(self.btc_transfer_fields(&s, &prepared));
                pairs.push(("txid", Value::String(signed.txid.clone())));
                pairs.push((
                    "wallet_balance_change",
                    Value::String(format_signed_btc(-i128::from(
                        prepared.plan.amount + prepared.plan.fee,
                    ))),
                ));
                pairs.push(("coins_selected_btc", Value::String(format_btc(balance))));
                pairs.push(("replaceable", Value::Bool(true)));
                pairs.push(("lock_time", num(prepared.tip)));
                pairs.push(("actual_vsize", num(signed.vsize)));
                let warnings = Self::btc_warnings(&prepared);
                if !warnings.is_empty() {
                    pairs.push(("warnings", warnings_json(warnings)));
                }
                pairs.push((
                    "note",
                    Value::str("Bitcoin has no execution to dry-run: the transaction was built, signed in memory and checked (amounts, dust and fee limits, signatures verified) but not sent, and the signed copy was discarded. The coins and fee rate are re-chosen when you send."),
                ));
            }
            Err(PrepareError::Refused(reason)) => {
                pairs.push(("would_succeed", Value::Bool(false)));
                pairs.push(("error", Value::String(reason)));
                pairs.push(("network", Value::str(s.network.id())));
                pairs.push(("note", Value::str("Nothing was signed or sent.")));
            }
            Err(PrepareError::Failed(reason)) => return Err(reason),
        }
        Ok(Value::object(pairs))
    }

    // ---- signing and sending --------------------------------------------------------------------

    /// Builds the transaction for a plan, signs every input and runs the final checks.
    fn btc_sign(
        &self,
        s: &BtcSession<'_>,
        vault: &BitcoinVault,
        prepared: &Prepared,
    ) -> Result<Signed, String> {
        let lock_time = u32::try_from(prepared.tip).map_err(|_| "block height out of range")?;
        let recipient_script = prepared.recipient.script_pubkey();
        let mut transaction =
            builder::assemble(&prepared.plan, &recipient_script, &s.script, lock_time);
        let mut key = vault.private_key_array()?;
        let mut result = Ok(());
        for (index, coin) in prepared.plan.inputs.iter().enumerate() {
            result = transaction.sign_p2wpkh_input(index, coin.value, &key);
            if result.is_err() {
                break;
            }
        }
        crate::crypto::secret::zeroize(&mut key);
        result?;
        builder::verify_signed(&transaction, &prepared.plan, &recipient_script, &s.script)?;
        Ok(Signed {
            txid: transaction.txid_hex(),
            vsize: transaction.vsize(),
            raw: transaction.serialize(),
        })
    }

    /// Settles what an earlier call left unresolved before anything new is sent. Returns a
    /// report when that earlier transaction needed attention, in which case nothing new is sent.
    fn btc_reconcile(&self, s: &BtcSession<'_>) -> Result<Option<Value>, String> {
        let key = journal_key(&s.script);
        let Some(pending) = self.btc_journal.load(&key)? else {
            return Ok(None);
        };
        if pending.network != s.network.id() {
            self.btc_journal.clear(&key);
            return Ok(None);
        }
        let report = |status: &str, message: &str| {
            Value::object(vec![
                ("status", Value::str(status)),
                ("txid", Value::String(pending.txid.clone())),
                ("network", Value::str(s.network.id())),
                (
                    "explorer_url",
                    Value::String(s.network.tx_url(&pending.txid)),
                ),
                ("message", Value::str(message)),
            ])
        };
        let known = s.api.transaction(&pending.txid).map_err(|e| {
            format!(
                "an earlier transaction ({}) has an unknown outcome and could not be checked now ({e}); nothing was sent. Try again shortly.",
                pending.txid
            )
        })?;
        if known.is_some() {
            self.btc_journal.clear(&key);
            return Ok(Some(report(
                "previous_transaction_found",
                "An earlier send whose answer was lost did reach the network: the transaction is known to the node. Nothing new was sent. Check it with get_transaction, then call again if you still want to send another payment.",
            )));
        }
        match s.api.broadcast(&crate::crypto::hex::encode(&pending.raw)) {
            Ok(_) => {
                self.btc_journal.clear(&key);
                Ok(Some(report(
                    "previous_transaction_resent",
                    "An earlier send whose answer was lost had not reached the node, so the identical signed transaction was submitted again (the same inputs, so it can never pay twice). Nothing new was sent. Check it with get_transaction, then call again if you still want to send another payment.",
                )))
            }
            Err(e) if rpc::is_already_known(&e) => {
                self.btc_journal.clear(&key);
                Ok(Some(report(
                    "previous_transaction_found",
                    "An earlier send whose answer was lost did reach the network. Nothing new was sent. Check it with get_transaction, then call again if you still want to send another payment.",
                )))
            }
            Err(e) if rpc::is_definite_rejection(&e) => {
                self.btc_journal.clear(&key);
                Err(format!(
                    "an earlier transaction ({}) was never accepted by the network and is rejected now ({}); it has been discarded and nothing was spent. Send again if you still want to pay.",
                    pending.txid,
                    sanitize_text_max(&e, 200)
                ))
            }
            Err(e) => Err(format!(
                "an earlier transaction ({}) has an unknown outcome and could not be re-sent now ({}); nothing new was sent. Try again shortly.",
                pending.txid,
                sanitize_text_max(&e, 200)
            )),
        }
    }

    /// Records the signed transaction, submits it and works out what happened, so a lost answer
    /// can never lead to a second payment.
    fn btc_broadcast(&self, s: &BtcSession<'_>, signed: &Signed) -> Result<String, String> {
        let key = journal_key(&s.script);
        self.btc_journal.record(
            &key,
            &PendingBitcoinTx {
                network: s.network.id().to_string(),
                txid: signed.txid.clone(),
                raw: signed.raw.clone(),
            },
        )?;
        match s.api.broadcast(&crate::crypto::hex::encode(&signed.raw)) {
            Ok(answer) if answer == signed.txid => {
                self.btc_journal.clear(&key);
                Ok("accepted".to_string())
            }
            Ok(other) => Err(format!(
                "the Bitcoin API answered with a different transaction id ({}) than the one signed ({}); check get_transaction before sending anything else",
                sanitize_text_max(&other, 70),
                signed.txid
            )),
            Err(e) if rpc::is_already_known(&e) => {
                self.btc_journal.clear(&key);
                Ok("already_known".to_string())
            }
            Err(e) if rpc::is_definite_rejection(&e) => {
                self.btc_journal.clear(&key);
                Err(format!(
                    "the Bitcoin node rejected the transaction, so nothing was sent: {}",
                    sanitize_text_max(&e, 300)
                ))
            }
            Err(e) => match s.api.transaction(&signed.txid) {
                Ok(Some(_)) => {
                    self.btc_journal.clear(&key);
                    Ok("accepted".to_string())
                }
                _ => Err(format!(
                    "the answer to the broadcast was lost ({}), so the outcome is unknown. The signed transaction id is {}; do not send again. The next send from this wallet first checks it and re-submits the identical transaction if needed. You can also look it up with get_transaction.",
                    sanitize_text_max(&e, 200),
                    signed.txid
                )),
            },
        }
    }

    pub fn bitcoin_send_asset(
        &self,
        name: &str,
        password: &str,
        asset: &str,
        to: &str,
        amount: &str,
        fee: &BitcoinFee,
    ) -> Result<Value, String> {
        check_asset(Some(asset))?;
        let (record, vault) = self.unlock_bitcoin(name, password)?;
        let s = self.bitcoin_session(&vault)?;
        if let Some(report) = self.btc_reconcile(&s)? {
            return Ok(report);
        }
        let prepared = self
            .btc_prepare(&s, to, amount, fee)
            .map_err(PrepareError::message)?;
        let signed = self.btc_sign(&s, &vault, &prepared)?;
        let outcome = self.btc_broadcast(&s, &signed)?;
        let mut pairs = vec![
            ("wallet", Value::String(record.name)),
            ("status", Value::str("broadcast")),
            ("txid", Value::String(signed.txid.clone())),
        ];
        pairs.extend(self.btc_transfer_fields(&s, &prepared));
        pairs.push((
            "explorer_url",
            Value::String(s.network.tx_url(&signed.txid)),
        ));
        pairs.push(("replaceable", Value::Bool(true)));
        let mut warnings = Self::btc_warnings(&prepared);
        if outcome == "already_known" {
            warnings.push("the node already knew this transaction".to_string());
        }
        if !warnings.is_empty() {
            pairs.push(("warnings", warnings_json(warnings)));
        }
        pairs.push((
            "note",
            Value::str("The transaction is unconfirmed until it is mined (usually within the confirmation target). It signals replaceability (BIP 125). Check progress with get_transaction."),
        ));
        Ok(Value::object(pairs))
    }

    pub fn bitcoin_sign_message(
        &self,
        name: &str,
        password: &str,
        text: &str,
        format: Option<&str>,
    ) -> Result<Value, String> {
        let bytes = crate::sui::tx::parse_message(text, format)?;
        let (record, vault) = self.unlock_bitcoin(name, password)?;
        let mut key = vault.private_key_array()?;
        let signed = message::sign_simple(&key, &bytes);
        crate::crypto::secret::zeroize(&mut key);
        let signed = signed?;
        let readable = std::str::from_utf8(&bytes)
            .ok()
            .filter(|t| !t.chars().any(|c| c.is_control() && c != '\n' && c != '\t'));
        Ok(Value::object(vec![
            ("wallet", Value::String(record.name)),
            ("address", Value::String(vault.address()?)),
            ("signature", Value::String(signed.signature)),
            (
                "signature_without_prefix",
                Value::String(signed.signature_unprefixed),
            ),
            (
                "message_hash",
                Value::String(crate::crypto::hex::encode(&message::message_hash(&bytes))),
            ),
            (
                "message_preview",
                match readable {
                    Some(t) => Value::String(sanitize_text_max(t, 500)),
                    None => Value::str("(binary data)"),
                },
            ),
            ("standard", Value::str("BIP 322 simple (P2WPKH)")),
            (
                "note",
                Value::str(
                    "signature is the base64 witness with BIP 322's `smp` prefix; verifiers written before the prefix existed expect signature_without_prefix. Bitcoin Core's own signmessage supports only legacy (1...) addresses, so it cannot check this signature. Nothing was sent and no network access was needed.",
                ),
            ),
        ]))
    }

    // ---- lookups --------------------------------------------------------------------------------

    fn btc_confirmations(tip: Option<u64>, info: &TxInfo) -> Value {
        match (info.status.block_height, tip) {
            (Some(height), Some(tip)) if tip >= height => num(tip - height + 1),
            (None, _) if !info.status.confirmed => num(0),
            _ => Value::Null,
        }
    }

    fn btc_tx_summary(
        s: &BtcSession<'_>,
        info: &TxInfo,
        tip: Option<u64>,
    ) -> Vec<(&'static str, Value)> {
        let effect = effects::wallet_effect(info, &s.script);
        let mut pairs = vec![
            ("txid", Value::String(info.txid.clone())),
            (
                "status",
                Value::str(if info.status.confirmed {
                    "confirmed"
                } else {
                    "pending"
                }),
            ),
            ("confirmations", Self::btc_confirmations(tip, info)),
            (
                "block_height",
                info.status.block_height.map_or(Value::Null, num),
            ),
            (
                "block_time",
                info.status.block_time.map_or(Value::Null, num),
            ),
            ("direction", Value::str(effect.direction.name())),
        ];
        if effect.direction != Direction::Unrelated {
            let net = if effect.spent_known {
                Value::String(format_signed_btc(effect.net()))
            } else {
                Value::Null
            };
            pairs.push(("wallet_balance_change", net));
        }
        pairs.push((
            "fee_btc",
            info.fee
                .map_or(Value::Null, |f| Value::String(format_btc(f))),
        ));
        pairs
    }

    pub fn bitcoin_get_transaction(
        &self,
        name: &str,
        password: &str,
        transaction_id: &str,
    ) -> Result<Value, String> {
        let txid = txid_to_hex(&parse_txid(transaction_id)?);
        let (record, vault) = self.unlock_bitcoin(name, password)?;
        let s = self.bitcoin_session(&vault)?;
        let Some(info) = s.api.transaction(&txid)? else {
            return Ok(Value::object(vec![
                ("wallet", Value::String(record.name)),
                ("txid", Value::String(txid.clone())),
                ("status", Value::str("not_found")),
                ("network", Value::str(s.network.id())),
                (
                    "note",
                    Value::str(
                        "The Bitcoin API does not know this transaction: it was never broadcast, was dropped from the mempool, or the id is for another network.",
                    ),
                ),
            ]));
        };
        let tip = s.api.tip_height().ok();
        let effect = effects::wallet_effect(&info, &s.script);
        let outputs: Vec<Value> = info
            .outputs
            .iter()
            .enumerate()
            .take(MAX_OUTPUTS_SHOWN)
            .map(|(index, o)| {
                Value::object(vec![
                    ("index", num(index as u64)),
                    ("amount", Value::String(format_btc(o.value))),
                    (
                        "address",
                        o.address
                            .as_deref()
                            .map_or(Value::Null, |a| Value::String(sanitize_text_max(a, 100))),
                    ),
                    ("is_this_wallet", Value::Bool(o.script == s.script)),
                ])
            })
            .collect();
        let mut pairs = vec![("wallet", Value::String(record.name))];
        pairs.extend(Self::btc_tx_summary(&s, &info, tip));
        pairs.push(("network", Value::str(s.network.id())));
        pairs.push(("inputs", num(info.inputs.len() as u64)));
        pairs.push(("outputs", Value::Array(outputs)));
        if info.outputs.len() > MAX_OUTPUTS_SHOWN {
            pairs.push(("outputs_truncated", Value::Bool(true)));
        }
        pairs.push((
            "received_by_wallet",
            Value::String(format_btc(effect.received)),
        ));
        pairs.push((
            "virtual_size",
            info.weight.map_or(Value::Null, |w| num(w.div_ceil(4))),
        ));
        pairs.push(("explorer_url", Value::String(s.network.tx_url(&info.txid))));
        Ok(Value::object(pairs))
    }

    pub fn bitcoin_history(
        &self,
        name: &str,
        password: &str,
        kind: HistoryKind,
        limit: usize,
    ) -> Result<Value, String> {
        if kind == HistoryKind::TokenTransfers {
            return Err("Bitcoin has no tokens; only type \"transactions\" applies".to_string());
        }
        let limit = limit.clamp(1, explorer::MAX_LIMIT);
        let (record, vault) = self.unlock_bitcoin(name, password)?;
        let s = self.bitcoin_session(&vault)?;
        let mut collected: Vec<TxInfo> = vec![];
        let mut seen: HashSet<String> = HashSet::new();
        let mut after: Option<String> = None;
        for _ in 0..MAX_HISTORY_PAGES {
            let page = s
                .api
                .address_transactions(&s.address_text, after.as_deref())?;
            let confirmed_in_page = page.iter().filter(|t| t.status.confirmed).count();
            let last_confirmed = page
                .iter()
                .rev()
                .find(|t| t.status.confirmed)
                .map(|t| t.txid.clone());
            for tx in page {
                if collected.len() < limit && seen.insert(tx.txid.clone()) {
                    collected.push(tx);
                }
            }
            match last_confirmed {
                Some(last) if collected.len() < limit && confirmed_in_page >= rpc::PAGE_SIZE => {
                    after = Some(last);
                }
                _ => break,
            }
        }
        let tip = s.api.tip_height().ok();
        let entries: Vec<Value> = collected
            .iter()
            .map(|info| Value::object(Self::btc_tx_summary(&s, info, tip)))
            .collect();
        Ok(Value::object(vec![
            ("wallet", Value::String(record.name)),
            ("address", Value::String(s.address_text.clone())),
            ("network", Value::str(s.network.id())),
            ("count", num(entries.len() as u64)),
            ("transactions", Value::Array(entries)),
            (
                "note",
                Value::str(
                    "Newest first, unconfirmed transactions before confirmed ones. wallet_balance_change includes the fee for payments the wallet made.",
                ),
            ),
        ]))
    }
}
