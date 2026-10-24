//! Wallet service: every operation unlocks the wallet for that single action only.
//! The decrypted vault lives in a local variable and is wiped when the call returns,
//! so after any operation every wallet is locked again.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::blockchain::Blockchain;
use crate::crypto::{hex, sha2::sha256};
use crate::eth::abi;
use crate::eth::address::{Address, parse_address, to_checksum};
use crate::eth::explorer::{self, HistoryKind};
use crate::eth::journal::Journal;
use crate::eth::networks::{self, Network};
use crate::eth::ops::{self, TxIntent, num};
use crate::eth::rpc::{CallRequest, RpcClient};
use crate::eth::swap::{self, ChainState, Expectation, Quote, Side, SwapRequest, SwapSettings};
use crate::eth::tokens::{self, CustomToken};
use crate::eth::u256::U256;
use crate::eth::wallet::{VaultContents, validate_export_key, validate_password};
use crate::http::HttpClient;
use crate::json::Value;
use crate::store::{Store, WalletRecord, validate_wallet_name, write_new_private_file};
use crate::sui::cetus::CetusSettings;
use crate::sui::wallet::SuiVault;
use crate::vault;
use crate::walletconnect::manager::WcManager;
use crate::walletconnect::relay::UnavailableRelay;
use crate::zip;

mod sui;
mod sui_swap;
#[cfg(test)]
mod sui_tests;

pub const MAX_FAILED_ATTEMPTS: u32 = 5;
pub const LOCKOUT: Duration = Duration::from_secs(60);
pub const EXPORT_ENTRY_NAME: &str = "agent-saifu-wallet-export.json";

/// Slows down password guessing: after repeated failures a wallet is locked out for a while.
#[derive(Default)]
pub struct AttemptLimiter {
    entries: HashMap<String, (u32, Instant)>,
}

impl AttemptLimiter {
    fn key(name: &str) -> String {
        name.trim().to_lowercase()
    }

    pub fn check(&self, name: &str, now: Instant) -> Result<(), String> {
        if let Some((failures, last)) = self.entries.get(&Self::key(name)) {
            let elapsed = now.saturating_duration_since(*last);
            if *failures >= MAX_FAILED_ATTEMPTS && elapsed < LOCKOUT {
                let wait = (LOCKOUT - elapsed).as_secs() + 1;
                return Err(format!(
                    "too many failed password attempts; try again in {wait} seconds"
                ));
            }
        }
        Ok(())
    }

    pub fn record_failure(&mut self, name: &str, now: Instant) {
        let entry = self.entries.entry(Self::key(name)).or_insert((0, now));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = now;
    }

    pub fn record_success(&mut self, name: &str) {
        self.entries.remove(&Self::key(name));
    }
}

/// Parameters shared by gas estimation and simulation: either a token/native transfer
/// (`asset` + `amount` + `to`) or a raw call (`to` + optional `value` and `data`). For Sui wallets
/// a raw call is given as `transaction_bytes` instead.
#[derive(Debug, Default, Clone)]
pub struct IntentParams {
    pub to: Option<String>,
    pub value: Option<String>,
    pub data: Option<String>,
    pub asset: Option<String>,
    pub amount: Option<String>,
    pub transaction_bytes: Option<String>,
}

pub struct WalletService {
    store: Store,
    pub(crate) journal: Journal,
    pub(crate) http: Rc<dyn HttpClient>,
    pub(crate) wc: RefCell<WcManager>,
    limiter: RefCell<AttemptLimiter>,
    iterations: u32,
    swap: SwapSettings,
    cetus: CetusSettings,
    /// Pause between receipt checks and how many checks to make when waiting on an approval.
    receipt_wait: (Duration, u32),
}

/// Input shared by `get_swap_quote` and `swap_tokens`.
#[derive(Debug, Clone, Default)]
pub struct SwapParams {
    pub from_chain_id: u64,
    pub from_token: String,
    pub to_chain_id: u64,
    pub to_token: String,
    pub amount: String,
    pub slippage_bps: Option<u64>,
    /// Receive the bought tokens at another address on the same chain.
    pub to_address: Option<String>,
    pub min_receive: Option<String>,
}

struct PreparedSwap {
    owner: Address,
    receiver: Address,
    from: Side,
    to: Side,
    request: SwapRequest,
    expect: Expectation,
}

fn merge(base: Value, extra: Vec<(&str, Value)>) -> Value {
    match base {
        Value::Object(mut pairs) => {
            pairs.extend(extra.into_iter().map(|(k, v)| (k.to_string(), v)));
            Value::Object(pairs)
        }
        other => other,
    }
}

fn parse_hash(input: &str) -> Result<[u8; 32], String> {
    hex::decode_fixed::<32>(input.trim())
        .map_err(|_| "transaction hash must be 0x followed by 64 hex characters".to_string())
}

pub fn parse_call_data(input: &str) -> Result<Vec<u8>, String> {
    let data = hex::decode(input.trim()).map_err(|e| format!("data must be hex: {e}"))?;
    if data.len() > 128 * 1024 {
        return Err("call data is too large".to_string());
    }
    Ok(data)
}

/// Export file names stay inside the exports directory and cannot hide or traverse.
pub fn validate_export_file_name(name: &str) -> Result<String, String> {
    let ok = !name.is_empty()
        && name.len() <= 100
        && name.ends_with(".zip")
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
    if !ok {
        return Err("file name must be 1-100 characters of letters, digits, '.', '_' or '-', end with .zip and not start with '.'".to_string());
    }
    Ok(name.to_string())
}

fn default_export_name(wallet_name: &str) -> String {
    let stem = &hex::encode(&sha256(wallet_name.to_lowercase().as_bytes()))[..8];
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("agent-saifu-export-{stem}-{ts}.zip")
}

pub fn export_payload(
    name: &str,
    chain: Blockchain,
    contents: &VaultContents,
) -> Result<crate::crypto::secret::SecretBytes, String> {
    let address = contents.address()?;
    let key = contents.private_key()?;
    let mut pairs = vec![
        ("format", Value::str("agent-saifu-wallet-export")),
        ("version", Value::int(1)),
        ("name", Value::str(name)),
        ("blockchain", Value::str(chain.id())),
        ("address", Value::String(to_checksum(&address))),
        (
            "privateKey",
            Value::String(hex::encode_prefixed(key.expose())),
        ),
    ];
    if let Some(m) = contents.mnemonic() {
        pairs.push(("mnemonic", Value::str(m.expose())));
    }
    let mut doc = Value::object(pairs);
    let text = doc.to_json();
    doc.wipe();
    Ok(crate::crypto::secret::SecretBytes::new(text.into_bytes()))
}

impl WalletService {
    pub fn new(store: Store, http: Rc<dyn HttpClient>) -> WalletService {
        WalletService {
            journal: Journal::new(store.pending_dir()),
            store,
            http,
            wc: RefCell::new(WcManager::new(Rc::new(UnavailableRelay(
                "WalletConnect is not set up".to_string(),
            )))),
            limiter: RefCell::new(AttemptLimiter::default()),
            iterations: vault::DEFAULT_ITERATIONS,
            swap: SwapSettings::default(),
            cetus: CetusSettings::default(),
            receipt_wait: (Duration::from_secs(3), 60),
        }
    }

    pub fn with_swap_settings(mut self, settings: SwapSettings) -> WalletService {
        self.swap = settings;
        self
    }

    /// Connects the WalletConnect tools to a relay.
    pub fn with_walletconnect(mut self, manager: WcManager) -> WalletService {
        self.wc = RefCell::new(manager);
        self
    }

    pub fn with_receipt_wait(mut self, pause: Duration, attempts: u32) -> WalletService {
        self.receipt_wait = (pause, attempts);
        self
    }

    /// Lower PBKDF2 cost; only meant for tests.
    pub fn with_iterations(mut self, iterations: u32) -> WalletService {
        self.iterations = iterations;
        self
    }

    /// Loads the wallet file and applies the empty-password check and the lockout, so a wrong
    /// guess is rate limited no matter which blockchain the wallet is on.
    fn open_record(&self, name: &str, password: &str) -> Result<(WalletRecord, Instant), String> {
        if password.is_empty() {
            return Err("password is required".to_string());
        }
        let now = Instant::now();
        self.limiter.borrow().check(name, now)?;
        Ok((self.store.load(name)?, now))
    }

    fn unseal_with<T>(
        &self,
        name: &str,
        now: Instant,
        unsealed: Result<T, String>,
    ) -> Result<T, String> {
        match unsealed {
            Ok(contents) => {
                self.limiter.borrow_mut().record_success(name);
                Ok(contents)
            }
            Err(e) if e == vault::WRONG_PASSWORD => {
                self.limiter.borrow_mut().record_failure(name, now);
                Err("incorrect password".to_string())
            }
            Err(e) => Err(e),
        }
    }

    fn wrong_chain(record: &WalletRecord, needed: Blockchain) -> String {
        format!(
            "wallet '{}' is a {} wallet, but this operation needs a {} wallet",
            record.name,
            record.blockchain.display_name(),
            needed.display_name()
        )
    }

    /// Unlocks an Ethereum wallet for one action. Sui wallets are refused before any decryption.
    pub(crate) fn unlock(
        &self,
        name: &str,
        password: &str,
    ) -> Result<(WalletRecord, VaultContents), String> {
        let (record, now) = self.open_record(name, password)?;
        if record.blockchain != Blockchain::Ethereum {
            return Err(Self::wrong_chain(&record, Blockchain::Ethereum));
        }
        let contents =
            self.unseal_with(name, now, VaultContents::unseal(&record.vault, password))?;
        Ok((record, contents))
    }

    /// Unlocks a Sui wallet for one action.
    pub(crate) fn unlock_sui(
        &self,
        name: &str,
        password: &str,
    ) -> Result<(WalletRecord, SuiVault), String> {
        let (record, now) = self.open_record(name, password)?;
        if record.blockchain != Blockchain::Sui {
            return Err(Self::wrong_chain(&record, Blockchain::Sui));
        }
        let contents = self.unseal_with(name, now, SuiVault::unseal(&record.vault, password))?;
        Ok((record, contents))
    }

    /// Checks the password of a wallet on any blockchain (rename, delete).
    fn authenticate(&self, name: &str, password: &str) -> Result<WalletRecord, String> {
        let (record, now) = self.open_record(name, password)?;
        match record.blockchain {
            Blockchain::Ethereum => {
                self.unseal_with(name, now, VaultContents::unseal(&record.vault, password))
                    .map(|_| ())?;
            }
            Blockchain::Sui => {
                self.unseal_with(name, now, SuiVault::unseal(&record.vault, password))
                    .map(|_| ())?;
            }
        }
        Ok(record)
    }

    /// Which blockchain a wallet is on; tools shared by several blockchains dispatch on it.
    /// The file reveals only the chain, never keys or addresses.
    pub(crate) fn chain_of(&self, name: &str, password: &str) -> Result<Blockchain, String> {
        if password.is_empty() {
            return Err("password is required".to_string());
        }
        Ok(self.store.load(name)?.blockchain)
    }

    fn seal_and_save(
        &self,
        mut record: WalletRecord,
        contents: &VaultContents,
        password: &str,
    ) -> Result<(), String> {
        record.vault = contents.seal(password, self.iterations)?;
        self.store.save(&record)
    }

    pub(crate) fn network_of(contents: &VaultContents, chain_id: u64) -> Result<Network, String> {
        networks::find_network(&contents.networks, chain_id).ok_or_else(|| {
            format!(
                "this wallet has no network with chain ID {chain_id}; add it with add_network first"
            )
        })
    }

    // ---- wallet lifecycle -------------------------------------------------

    pub fn create_wallet(
        &self,
        blockchain: &str,
        name: &str,
        password: &str,
    ) -> Result<Value, String> {
        self.create_wallet_on(blockchain, name, password, None)
    }

    /// `network` (mainnet, testnet, devnet) applies to Sui wallets only; the default is mainnet.
    pub fn create_wallet_on(
        &self,
        blockchain: &str,
        name: &str,
        password: &str,
        network: Option<&str>,
    ) -> Result<Value, String> {
        let chain = Blockchain::parse(blockchain)?;
        let name = validate_wallet_name(name)?;
        validate_password(password)?;
        let sui_network = Self::wallet_network(chain, network)?;
        if self.store.exists(&name) {
            return Err(format!("a wallet named '{name}' already exists"));
        }
        match chain {
            Blockchain::Ethereum => {
                let contents = VaultContents::create_new()?;
                self.store_new_wallet(chain, name, password, &contents, "created")
            }
            Blockchain::Sui => {
                let contents = SuiVault::create_new(sui_network)?;
                self.sui_store_new(name, password, &contents, "created")
            }
        }
    }

    pub fn import_wallet(
        &self,
        blockchain: &str,
        name: &str,
        password: &str,
        seed_phrase: Option<&str>,
        private_key: Option<&str>,
    ) -> Result<Value, String> {
        self.import_wallet_on(blockchain, name, password, seed_phrase, private_key, None)
    }

    pub fn import_wallet_on(
        &self,
        blockchain: &str,
        name: &str,
        password: &str,
        seed_phrase: Option<&str>,
        private_key: Option<&str>,
        network: Option<&str>,
    ) -> Result<Value, String> {
        let chain = Blockchain::parse(blockchain)?;
        let name = validate_wallet_name(name)?;
        validate_password(password)?;
        let sui_network = Self::wallet_network(chain, network)?;
        if seed_phrase.is_some() == private_key.is_some() {
            return Err("provide exactly one of seed_phrase or private_key".to_string());
        }
        match chain {
            Blockchain::Ethereum => {
                if private_key.is_some_and(|k| k.trim().starts_with("suiprivkey")) {
                    return Err(
                        "this looks like a Sui private key; set blockchain to \"sui\"".to_string(),
                    );
                }
                let contents = match (seed_phrase, private_key) {
                    (Some(s), None) => VaultContents::from_mnemonic(s)?,
                    (None, Some(k)) => VaultContents::from_private_key(k)?,
                    _ => unreachable!("exactly one secret was checked above"),
                };
                if self.store.exists(&name) {
                    return Err(format!("a wallet named '{name}' already exists"));
                }
                self.store_new_wallet(chain, name, password, &contents, "imported")
            }
            Blockchain::Sui => {
                let contents = match (seed_phrase, private_key) {
                    (Some(s), None) => SuiVault::from_mnemonic(s, sui_network)?,
                    (None, Some(k)) => SuiVault::from_private_key(k, sui_network)?,
                    _ => unreachable!("exactly one secret was checked above"),
                };
                if self.store.exists(&name) {
                    return Err(format!("a wallet named '{name}' already exists"));
                }
                self.sui_store_new(name, password, &contents, "imported")
            }
        }
    }

    /// The Sui network a new wallet is bound to; Ethereum wallets take no network.
    fn wallet_network(
        chain: Blockchain,
        network: Option<&str>,
    ) -> Result<crate::sui::network::SuiNetwork, String> {
        match (chain, network) {
            (Blockchain::Sui, n) => Self::sui_network_arg(n),
            (Blockchain::Ethereum, None) => Ok(crate::sui::network::SuiNetwork::Mainnet),
            (Blockchain::Ethereum, Some(_)) => Err(
                "'network' applies to Sui wallets only; Ethereum networks are chosen per call with chain_id and managed with add_network".to_string(),
            ),
        }
    }

    fn store_new_wallet(
        &self,
        chain: Blockchain,
        name: String,
        password: &str,
        contents: &VaultContents,
        status: &str,
    ) -> Result<Value, String> {
        let address = contents.address()?;
        let record = WalletRecord {
            name: name.clone(),
            blockchain: chain,
            vault: contents.seal(password, self.iterations)?,
        };
        self.store.create(&record)?;
        Ok(Value::object(vec![
            ("name", Value::String(name)),
            ("blockchain", Value::str(chain.display_name())),
            ("address", Value::String(to_checksum(&address))),
            ("status", Value::str(status)),
        ]))
    }

    pub fn list_wallets(&self) -> Result<Value, String> {
        let wallets = self.store.list()?;
        Ok(Value::object(vec![(
            "wallets",
            Value::Array(
                wallets
                    .into_iter()
                    .map(|(name, chain)| {
                        Value::object(vec![
                            ("name", Value::String(name)),
                            ("blockchain", Value::str(chain.display_name())),
                        ])
                    })
                    .collect(),
            ),
        )]))
    }

    pub fn rename_wallet(
        &self,
        name: &str,
        new_name: &str,
        password: &str,
    ) -> Result<Value, String> {
        let record = self.authenticate(name, password)?;
        let renamed = self.store.rename(&record.name, new_name)?;
        Ok(Value::object(vec![
            ("old_name", Value::String(record.name)),
            ("new_name", Value::String(renamed.name)),
        ]))
    }

    pub fn delete_wallet(
        &self,
        name: &str,
        password: &str,
        confirm: bool,
    ) -> Result<Value, String> {
        if !confirm {
            return Err("deleting a wallet is irreversible and destroys its keys; set confirm to true after backing up the seed phrase".to_string());
        }
        let record = self.authenticate(name, password)?;
        self.store.delete(&record.name)?;
        Ok(Value::object(vec![("deleted", Value::String(record.name))]))
    }

    pub fn get_address(&self, name: &str, password: &str) -> Result<Value, String> {
        if self.chain_of(name, password)? == Blockchain::Sui {
            return self.sui_address(name, password);
        }
        let (record, contents) = self.unlock(name, password)?;
        let address = contents.address()?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("blockchain", Value::str(record.blockchain.display_name())),
            ("address", Value::String(to_checksum(&address))),
        ]))
    }

    // ---- secrets ------------------------------------------------------------

    pub fn view_seed_phrase(&self, name: &str, password: &str) -> Result<Value, String> {
        if self.chain_of(name, password)? == Blockchain::Sui {
            return self.sui_seed_phrase(name, password);
        }
        let (record, contents) = self.unlock(name, password)?;
        let phrase = contents
            .mnemonic()
            .ok_or("this wallet was imported from a private key and has no seed phrase")?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("seed_phrase", Value::str(phrase.expose())),
            (
                "warning",
                Value::str("Anyone with this phrase controls the wallet. Never share it."),
            ),
        ]))
    }

    pub fn view_private_key(&self, name: &str, password: &str) -> Result<Value, String> {
        if self.chain_of(name, password)? == Blockchain::Sui {
            return self.sui_private_key(name, password);
        }
        let (record, contents) = self.unlock(name, password)?;
        let key = contents.private_key()?;
        let address = contents.address()?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("address", Value::String(to_checksum(&address))),
            (
                "private_key",
                Value::String(hex::encode_prefixed(key.expose())),
            ),
            (
                "warning",
                Value::str("Anyone with this key controls the wallet. Never share it."),
            ),
        ]))
    }

    /// EIP-191 `personal_sign` with an Ethereum wallet.
    pub fn eth_sign_message(
        &self,
        name: &str,
        password: &str,
        message: &str,
        format: Option<&str>,
    ) -> Result<Value, String> {
        let bytes = crate::sui::tx::parse_message(message, format)?;
        let (record, contents) = self.unlock(name, password)?;
        let key = contents.private_key_array()?;
        let signature = crate::eth::sign::sign_personal_message(&key.0, &bytes)?;
        let readable = std::str::from_utf8(&bytes)
            .ok()
            .filter(|t| !t.chars().any(|c| c.is_control() && c != '\n' && c != '\t'));
        Ok(Value::object(vec![
            ("wallet", Value::String(record.name)),
            ("address", Value::String(to_checksum(&contents.address()?))),
            ("signature", Value::String(hex::encode_prefixed(&signature))),
            (
                "message_hash",
                Value::String(hex::encode_prefixed(
                    &crate::eth::sign::personal_message_hash(&bytes),
                )),
            ),
            (
                "message_preview",
                match readable {
                    Some(text) => Value::String(crate::eth::explorer::sanitize_text_max(text, 500)),
                    None => Value::str("(binary data)"),
                },
            ),
            ("standard", Value::str("EIP-191 personal_sign")),
        ]))
    }

    pub fn export_wallet(
        &self,
        name: &str,
        password: &str,
        encryption_key: &str,
        file_name: Option<&str>,
    ) -> Result<Value, String> {
        validate_export_key(encryption_key)?;
        let file_name = match file_name {
            Some(f) => validate_export_file_name(f)?,
            None => default_export_name(name),
        };
        let (wallet_name, payload, has_seed_phrase) = match self.chain_of(name, password)? {
            Blockchain::Sui => {
                let (record, vault) = self.unlock_sui(name, password)?;
                let payload = Self::sui_export_payload(&record.name, &vault)?;
                (record.name, payload, vault.mnemonic().is_some())
            }
            Blockchain::Ethereum => {
                let (record, contents) = self.unlock(name, password)?;
                let payload = export_payload(&record.name, record.blockchain, &contents)?;
                (record.name, payload, contents.mnemonic().is_some())
            }
        };
        let sealed = vault::encrypt(encryption_key, payload.expose(), vault::DEFAULT_ITERATIONS)?;
        let sealed_json = sealed.to_json().to_json();
        let archive = zip::build_zip(&[(EXPORT_ENTRY_NAME, sealed_json.as_bytes())])?;
        let path = self.store.exports_dir()?.join(&file_name);
        write_new_private_file(&path, &archive).map_err(|e| {
            format!("cannot write export (existing files are never overwritten): {e}")
        })?;
        Ok(Value::object(vec![
            ("name", Value::String(wallet_name)),
            ("file", Value::String(path.to_string_lossy().into_owned())),
            ("entry", Value::str(EXPORT_ENTRY_NAME)),
            (
                "contains",
                Value::str(if has_seed_phrase {
                    "seed phrase and private key"
                } else {
                    "private key"
                }),
            ),
            (
                "encryption",
                Value::String(format!(
                    "PBKDF2-HMAC-SHA256 ({} iterations) + AES-256-GCM, MetaMask vault format",
                    vault::DEFAULT_ITERATIONS
                )),
            ),
        ]))
    }

    // ---- networks -----------------------------------------------------------

    pub fn view_networks(&self, name: &str, password: &str) -> Result<Value, String> {
        if self.chain_of(name, password)? == Blockchain::Sui {
            return self.sui_networks(name, password);
        }
        let (record, contents) = self.unlock(name, password)?;
        let list = networks::all_networks(&contents.networks)
            .iter()
            .map(|n| {
                merge(
                    n.to_json(),
                    vec![("builtin", Value::Bool(networks::is_builtin(n.chain_id)))],
                )
            })
            .collect();
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("networks", Value::Array(list)),
        ]))
    }

    fn verify_rpc_chain(&self, network: &Network) -> Result<(), String> {
        ops::connect(&*self.http, network).map(|_| ())
    }

    pub fn add_network(
        &self,
        name: &str,
        password: &str,
        network: Network,
    ) -> Result<Value, String> {
        let (record, mut contents) = self.unlock(name, password)?;
        self.verify_rpc_chain(&network)?;
        networks::add_network(&mut contents.networks, network.clone())?;
        self.seal_and_save(record, &contents, password)?;
        Ok(Value::object(vec![("added", network.to_json())]))
    }

    pub fn update_network(
        &self,
        name: &str,
        password: &str,
        network: Network,
    ) -> Result<Value, String> {
        let (record, mut contents) = self.unlock(name, password)?;
        self.verify_rpc_chain(&network)?;
        networks::update_network(&mut contents.networks, network.clone())?;
        self.seal_and_save(record, &contents, password)?;
        Ok(Value::object(vec![("updated", network.to_json())]))
    }

    pub fn delete_network(
        &self,
        name: &str,
        password: &str,
        chain_id: u64,
    ) -> Result<Value, String> {
        let (record, mut contents) = self.unlock(name, password)?;
        let removed = networks::delete_network(&mut contents.networks, chain_id)?;
        let dropped = tokens::remove_tokens_of_chain(&mut contents.tokens, chain_id);
        self.seal_and_save(record, &contents, password)?;
        Ok(Value::object(vec![
            ("deleted", removed.to_json()),
            ("removed_tokens", num(dropped as u64)),
        ]))
    }

    // ---- tokens -------------------------------------------------------------

    /// Adds an ERC-20 token to the wallet on one network, after checking it on-chain.
    pub fn add_token(
        &self,
        name: &str,
        password: &str,
        chain_id: u64,
        contract: &str,
    ) -> Result<Value, String> {
        let address = parse_address(contract).map_err(|e| format!("'contract': {e}"))?;
        let (record, mut contents) = self.unlock(name, password)?;
        let network = Self::network_of(&contents, chain_id)?;
        if let Some(reason) = tokens::add_conflict(&contents.tokens, chain_id, &address) {
            return Err(reason);
        }
        let rpc = ops::connect(&*self.http, &network)?;
        let info = ops::token_info(&rpc, &address)?;
        let token = CustomToken {
            chain_id,
            address,
            symbol: info.symbol,
            decimals: info.decimals,
        };
        tokens::add_custom_token(&mut contents.tokens, token.clone())?;
        self.seal_and_save(record, &contents, password)?;
        Ok(Value::object(vec![
            ("added", token.to_json()),
            ("network", Value::String(network.name)),
        ]))
    }

    /// Removes a token added with `add_token`. Built-in tokens cannot be removed.
    pub fn remove_token(
        &self,
        name: &str,
        password: &str,
        chain_id: u64,
        contract: &str,
    ) -> Result<Value, String> {
        let address = parse_address(contract).map_err(|e| format!("'contract': {e}"))?;
        let (record, mut contents) = self.unlock(name, password)?;
        Self::network_of(&contents, chain_id)?;
        let removed = tokens::remove_custom_token(&mut contents.tokens, chain_id, &address)?;
        self.seal_and_save(record, &contents, password)?;
        Ok(Value::object(vec![("removed", removed.to_json())]))
    }

    // ---- chain operations ---------------------------------------------------

    pub fn get_balances(
        &self,
        name: &str,
        password: &str,
        chain_id: Option<u64>,
        token_contracts: &[Address],
    ) -> Result<Value, String> {
        let (record, contents) = self.unlock(name, password)?;
        let owner = contents.address()?;
        let selected: Vec<Network> = match chain_id {
            Some(id) => vec![Self::network_of(&contents, id)?],
            None => networks::all_networks(&contents.networks),
        };
        let reports = selected
            .iter()
            .map(|n| {
                match ops::network_balances(
                    &*self.http,
                    n,
                    &owner,
                    &contents.tokens,
                    token_contracts,
                ) {
                    Ok(v) => v,
                    Err(e) => Value::object(vec![
                        ("chain_id", num(n.chain_id)),
                        ("network", Value::str(&n.name)),
                        ("error", Value::String(e)),
                    ]),
                }
            })
            .collect();
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("address", Value::String(to_checksum(&owner))),
            ("networks", Value::Array(reports)),
            (
                "note",
                Value::str(
                    "usdc_value is the USD market value (USDC ~ 1 USD); it is null when no mainnet price exists, e.g. testnets or unlisted tokens. Only the native coin, well-known tokens, tokens you added with add_token and any token_contracts you pass are checked.",
                ),
            ),
        ]))
    }

    fn resolve_intent(
        &self,
        rpc: &crate::eth::rpc::RpcClient<'_>,
        network: &Network,
        p: &IntentParams,
    ) -> Result<TxIntent, String> {
        let to = p.to.as_deref().ok_or("'to' is required")?;
        let to = parse_address(to).map_err(|e| format!("'to': {e}"))?;
        if let Some(asset) = &p.asset {
            if p.value.is_some() || p.data.is_some() {
                return Err("use either asset+amount, or value/data, not both".to_string());
            }
            let amount = p
                .amount
                .as_deref()
                .ok_or("'amount' is required together with 'asset'")?;
            return ops::transfer_intent(rpc, network, asset, &to, amount).map(|t| t.0);
        }
        if p.amount.is_some() {
            return Err("'amount' requires 'asset'".to_string());
        }
        let value = match &p.value {
            Some(v) => {
                U256::parse_units(v, ops::NATIVE_DECIMALS).map_err(|e| format!("'value': {e}"))?
            }
            None => U256::ZERO,
        };
        let data = match &p.data {
            Some(d) => parse_call_data(d)?,
            None => vec![],
        };
        Ok(TxIntent { to, value, data })
    }

    pub fn send_asset(
        &self,
        name: &str,
        password: &str,
        chain_id: u64,
        asset: &str,
        to: &str,
        amount: &str,
        gas_limit: Option<u64>,
    ) -> Result<Value, String> {
        let (record, contents) = self.unlock(name, password)?;
        let network = Self::network_of(&contents, chain_id)?;
        let owner = contents.address()?;
        let to = parse_address(to).map_err(|e| format!("'to': {e}"))?;
        let rpc = ops::connect_for_send(&*self.http, &network, &self.journal)?;
        let (intent, symbol, raw, decimals) =
            ops::transfer_intent(&rpc, &network, asset, &to, amount)?;
        let prepared = ops::prepare_tx(&rpc, &network, &owner, &intent, gas_limit)?;
        let key = contents.private_key_array()?;
        let sent = ops::sign_and_maybe_send(&rpc, &network, &prepared, &key.0, true)?;
        Ok(merge(
            sent,
            vec![
                ("wallet", Value::String(record.name)),
                ("from", Value::String(to_checksum(&owner))),
                ("to", Value::String(to_checksum(&to))),
                ("asset", Value::String(symbol)),
                ("amount", Value::String(raw.format_units(decimals))),
            ],
        ))
    }

    pub fn sign_contract_transaction(
        &self,
        name: &str,
        password: &str,
        chain_id: u64,
        params: &IntentParams,
        gas_limit: Option<u64>,
        broadcast: bool,
    ) -> Result<Value, String> {
        let (record, contents) = self.unlock(name, password)?;
        let network = Self::network_of(&contents, chain_id)?;
        let owner = contents.address()?;
        let rpc = ops::connect_for_send(&*self.http, &network, &self.journal)?;
        let intent = self.resolve_intent(&rpc, &network, params)?;
        let prepared = ops::prepare_tx(&rpc, &network, &owner, &intent, gas_limit)?;
        let key = contents.private_key_array()?;
        let out = ops::sign_and_maybe_send(&rpc, &network, &prepared, &key.0, broadcast)?;
        let mut extra = vec![
            ("wallet", Value::String(record.name)),
            ("from", Value::String(to_checksum(&owner))),
            ("to", Value::String(to_checksum(&intent.to))),
            (
                "value",
                Value::String(intent.value.format_units(ops::NATIVE_DECIMALS)),
            ),
        ];
        if let Some((desc, warnings)) = ops::describe_intent(&intent) {
            extra.push(("decoded", desc));
            extra.push((
                "warnings",
                Value::Array(warnings.into_iter().map(Value::String).collect()),
            ));
        }
        Ok(merge(out, extra))
    }

    pub fn estimate_gas(
        &self,
        name: &str,
        password: &str,
        chain_id: u64,
        params: &IntentParams,
    ) -> Result<Value, String> {
        let (_record, contents) = self.unlock(name, password)?;
        let network = Self::network_of(&contents, chain_id)?;
        let owner = contents.address()?;
        let rpc = ops::connect(&*self.http, &network)?;
        let intent = self.resolve_intent(&rpc, &network, params)?;
        ops::estimate_report(&*self.http, &rpc, &network, &owner, &intent)
    }

    pub fn simulate_transaction(
        &self,
        name: &str,
        password: &str,
        chain_id: u64,
        params: &IntentParams,
    ) -> Result<Value, String> {
        let (_record, contents) = self.unlock(name, password)?;
        let network = Self::network_of(&contents, chain_id)?;
        let owner = contents.address()?;
        let rpc = ops::connect(&*self.http, &network)?;
        let intent = self.resolve_intent(&rpc, &network, params)?;
        ops::simulate_report(&*self.http, &rpc, &network, &owner, &intent)
    }

    pub fn read_contract(
        &self,
        name: &str,
        password: &str,
        chain_id: u64,
        contract: &str,
        function: &str,
        args: &[String],
        outputs: Option<&str>,
    ) -> Result<Value, String> {
        let (_record, contents) = self.unlock(name, password)?;
        let network = Self::network_of(&contents, chain_id)?;
        let owner = contents.address()?;
        let contract_addr = parse_address(contract).map_err(|e| format!("'contract': {e}"))?;
        let rpc = ops::connect(&*self.http, &network)?;
        let result = ops::read_contract(&rpc, &owner, &contract_addr, function, args, outputs)?;
        Ok(merge(
            result,
            vec![
                ("chain_id", num(chain_id)),
                ("contract", Value::String(to_checksum(&contract_addr))),
                ("function", Value::str(function)),
            ],
        ))
    }

    pub fn get_transaction(
        &self,
        name: &str,
        password: &str,
        chain_id: u64,
        tx_hash: &str,
    ) -> Result<Value, String> {
        let hash = parse_hash(tx_hash)?;
        let (_record, contents) = self.unlock(name, password)?;
        let network = Self::network_of(&contents, chain_id)?;
        let rpc = ops::connect(&*self.http, &network)?;
        ops::transaction_report(&rpc, &network, &hash)
    }

    pub fn get_transaction_history(
        &self,
        name: &str,
        password: &str,
        chain_id: u64,
        kind: HistoryKind,
        limit: usize,
    ) -> Result<Value, String> {
        let (record, contents) = self.unlock(name, password)?;
        let network = Self::network_of(&contents, chain_id)?;
        let owner = contents.address()?;
        let (items, has_more) = explorer::fetch_history(
            &*self.http,
            &network.block_explorer_url,
            &owner,
            kind,
            &network.currency_symbol,
            limit,
        )?;
        Ok(Value::object(vec![
            ("name", Value::String(record.name)),
            ("address", Value::String(to_checksum(&owner))),
            ("chain_id", num(chain_id)),
            ("network", Value::String(network.name)),
            (
                "type",
                Value::str(if kind == HistoryKind::Transactions {
                    "transactions"
                } else {
                    "token_transfers"
                }),
            ),
            ("count", num(items.len() as u64)),
            ("has_more", Value::Bool(has_more)),
            ("items", Value::Array(items)),
        ]))
    }

    pub fn revoke_token_approval(
        &self,
        name: &str,
        password: &str,
        chain_id: u64,
        token: &str,
        spender: &str,
        broadcast: bool,
    ) -> Result<Value, String> {
        let (record, contents) = self.unlock(name, password)?;
        let network = Self::network_of(&contents, chain_id)?;
        let owner = contents.address()?;
        let token = parse_address(token).map_err(|e| format!("'token': {e}"))?;
        let spender = parse_address(spender).map_err(|e| format!("'spender': {e}"))?;
        let rpc = ops::connect_for_send(&*self.http, &network, &self.journal)?;
        let Some((intent, info, allowance)) = ops::revoke_intent(&rpc, &owner, &token, &spender)?
        else {
            return Ok(Value::object(vec![
                ("wallet", Value::String(record.name)),
                ("status", Value::str("nothing_to_revoke")),
                (
                    "detail",
                    Value::str("the spender's allowance is already zero"),
                ),
            ]));
        };
        let prepared = ops::prepare_tx(&rpc, &network, &owner, &intent, None)?;
        let key = contents.private_key_array()?;
        let out = ops::sign_and_maybe_send(&rpc, &network, &prepared, &key.0, broadcast)?;
        Ok(merge(
            out,
            vec![
                ("wallet", Value::String(record.name)),
                ("token", Value::String(info.symbol)),
                ("token_contract", Value::String(to_checksum(&token))),
                ("spender", Value::String(to_checksum(&spender))),
                (
                    "previous_allowance_raw",
                    Value::String(allowance.to_dec_string()),
                ),
            ],
        ))
    }
}

// ---- swaps (OpenOcean) ----------------------------------------------------------

fn without_key(value: Value, key: &str) -> Value {
    match value {
        Value::Object(mut pairs) => {
            pairs.retain(|(k, _)| k != key);
            Value::Object(pairs)
        }
        other => other,
    }
}

impl WalletService {
    fn resolve_side(
        &self,
        rpc: &RpcClient<'_>,
        network: &Network,
        token: &str,
    ) -> Result<Side, String> {
        let native = || Side {
            network: network.clone(),
            token: swap::native_token_address(network.chain_id),
            native: true,
            symbol: network.currency_symbol.clone(),
            decimals: ops::NATIVE_DECIMALS,
        };
        if ops::is_native(token) {
            return Ok(native());
        }
        let address = parse_address(token).map_err(|e| {
            format!(
                "token '{}': {e}",
                crate::eth::explorer::sanitize_text(token)
            )
        })?;
        if swap::is_native_address(network.chain_id, &address) {
            return Ok(native());
        }
        let info = ops::token_info(rpc, &address)?;
        Ok(Side {
            network: network.clone(),
            token: address,
            native: false,
            symbol: info.symbol,
            decimals: info.decimals,
        })
    }

    fn prepare_swap(
        &self,
        rpc: &RpcClient<'_>,
        network: &Network,
        owner: Address,
        p: &SwapParams,
    ) -> Result<PreparedSwap, String> {
        let slippage_bps = match p.slippage_bps {
            None => None,
            Some(s)
                if (u64::from(swap::MIN_SLIPPAGE_BPS)..=u64::from(swap::MAX_SLIPPAGE_BPS))
                    .contains(&s) =>
            {
                Some(s as u32)
            }
            Some(_) => {
                return Err(format!(
                    "slippage_bps must be between {} and {} (0.05% to 10%)",
                    swap::MIN_SLIPPAGE_BPS,
                    swap::MAX_SLIPPAGE_BPS
                ));
            }
        };
        let receiver = match &p.to_address {
            Some(a) => {
                let addr = parse_address(a).map_err(|e| format!("'to_address': {e}"))?;
                if addr.iter().all(|b| *b == 0) {
                    return Err(
                        "'to_address' must not be the zero address (tokens would be burned)"
                            .to_string(),
                    );
                }
                addr
            }
            None => owner,
        };
        let from = self.resolve_side(rpc, network, &p.from_token)?;
        let to = self.resolve_side(rpc, network, &p.to_token)?;
        if from.token == to.token {
            return Err("from_token and to_token are the same token".to_string());
        }
        let amount =
            U256::parse_units(&p.amount, from.decimals).map_err(|e| format!("'amount': {e}"))?;
        if amount.is_zero() {
            return Err("amount must be greater than zero".to_string());
        }
        let min_receive = match &p.min_receive {
            Some(m) => {
                Some(U256::parse_units(m, to.decimals).map_err(|e| format!("'min_receive': {e}"))?)
            }
            None => None,
        };
        let gas_price = rpc
            .gas_price()
            .map_err(|e| format!("cannot read the gas price: {e}"))?;
        Ok(PreparedSwap {
            owner,
            receiver,
            request: SwapRequest {
                chain_id: network.chain_id,
                in_token: from.token,
                out_token: to.token,
                amount,
                gas_price,
                // The API insists on a slippage value, so the default is always sent.
                slippage_bps: Some(slippage_bps.unwrap_or(swap::DEFAULT_SLIPPAGE_BPS)),
                sender: owner,
                receiver,
            },
            expect: Expectation {
                chain_id: network.chain_id,
                in_token: from.token,
                in_native: from.native,
                in_decimals: from.decimals,
                out_token: to.token,
                out_native: to.native,
                out_decimals: to.decimals,
                requested: amount,
                sender: owner,
                receiver,
                slippage_bps: slippage_bps.unwrap_or(swap::DEFAULT_SLIPPAGE_BPS),
                min_receive,
            },
            from,
            to,
        })
    }

    /// Validates the chain choice and returns the network to swap on.
    fn swap_network(contents: &VaultContents, p: &SwapParams) -> Result<Network, String> {
        if p.from_chain_id != p.to_chain_id {
            return Err(format!(
                "cross-chain swaps are not supported: OpenOcean's API only swaps within one chain (from chain {} to chain {} was requested). Swap on the origin chain into the token you want instead",
                p.from_chain_id, p.to_chain_id
            ));
        }
        if !swap::is_supported_chain(p.from_chain_id) {
            return Err(format!(
                "OpenOcean does not support chain ID {}",
                p.from_chain_id
            ));
        }
        Self::network_of(contents, p.from_chain_id)
    }

    fn read_state(&self, rpc: &RpcClient<'_>, prep: &PreparedSwap) -> Result<ChainState, String> {
        if prep.from.native {
            let balance = rpc
                .balance(&prep.owner)
                .map_err(|e| format!("cannot read balance: {e}"))?;
            return Ok(ChainState {
                balance,
                allowance: None,
            });
        }
        let balance = ops::token_balance(rpc, &prep.from.token, &prep.owner)
            .map_err(|e| format!("cannot read the {} balance: {e}", prep.from.symbol))?;
        let router = swap::router_for_chain(prep.request.chain_id);
        let data = rpc
            .eth_call(&CallRequest {
                to: Some(prep.from.token),
                data: abi::erc20_allowance(&prep.owner, &router),
                ..Default::default()
            })
            .map_err(|e| format!("cannot read the allowance: {e}"))?;
        Ok(ChainState {
            balance,
            allowance: Some(abi::decode_uint256(&data)?),
        })
    }

    fn fetch_quote(&self, prep: &PreparedSwap) -> Result<Quote, String> {
        let url = swap::swap_url(&self.swap.base_url, &prep.request, self.swap.fee.as_ref());
        swap::parse_swap(&swap::fetch(&*self.http, &url)?)
    }

    fn require_balance(prep: &PreparedSwap, state: &ChainState) -> Result<(), String> {
        if state.balance < prep.request.amount {
            return Err(format!(
                "insufficient {} balance: the wallet holds {} but the swap needs {}",
                prep.from.symbol,
                state.balance.format_units(prep.from.decimals),
                prep.request.amount.format_units(prep.from.decimals)
            ));
        }
        Ok(())
    }

    /// Returns what a swap would give: expected and minimum amounts and the fees.
    pub fn get_swap_quote(
        &self,
        name: &str,
        password: &str,
        p: &SwapParams,
    ) -> Result<Value, String> {
        let (record, contents) = self.unlock(name, password)?;
        let network = Self::swap_network(&contents, p)?;
        let rpc = ops::connect(&*self.http, &network)?;
        let prep = self.prepare_swap(&rpc, &network, contents.address()?, p)?;
        let state = self.read_state(&rpc, &prep)?;
        let quote = self.fetch_quote(&prep)?;
        let report = swap::quote_report(
            &prep.from,
            &prep.to,
            &quote,
            self.swap.fee.as_ref(),
            &prep.request.amount,
            &prep.receiver,
            &prep.request.gas_price,
            &state,
        );
        Ok(merge(report, vec![("wallet", Value::String(record.name))]))
    }

    fn wait_for_success(&self, rpc: &RpcClient<'_>, hash: &[u8; 32]) -> Result<(), String> {
        let (pause, attempts) = self.receipt_wait;
        for attempt in 0..attempts {
            if let Ok(Some(receipt)) = rpc.transaction_receipt(hash) {
                return match receipt.get("status").and_then(Value::as_str) {
                    Some("0x1") => Ok(()),
                    _ => Err(
                        "the approval transaction failed on-chain; the swap was not sent"
                            .to_string(),
                    ),
                };
            }
            if attempt + 1 < attempts {
                std::thread::sleep(pause);
            }
        }
        Err(format!(
            "the approval {} was not confirmed in time; the swap was NOT sent. Check it with get_transaction, then try again",
            hex::encode_prefixed(hash)
        ))
    }

    /// Approves exactly `amount` (never unlimited); resets a non-zero allowance first,
    /// because some tokens (e.g. USDT) refuse to change one non-zero allowance to another.
    fn approve_exact(
        &self,
        rpc: &RpcClient<'_>,
        prep: &PreparedSwap,
        key: &[u8; 32],
        current: &U256,
    ) -> Result<Vec<Value>, String> {
        let spender = swap::router_for_chain(prep.request.chain_id);
        let mut steps = vec![];
        if !current.is_zero() {
            steps.push(("reset_allowance", U256::ZERO));
        }
        steps.push(("approve", prep.request.amount));
        let mut done = vec![];
        for (purpose, value) in steps {
            let intent = TxIntent {
                to: prep.from.token,
                value: U256::ZERO,
                data: abi::erc20_approve(&spender, &value),
            };
            let network = &prep.from.network;
            let prepared = ops::prepare_tx(rpc, network, &prep.owner, &intent, None)
                .map_err(|e| format!("approval failed: {e}"))?;
            let sent = ops::sign_and_maybe_send(rpc, network, &prepared, key, true)
                .map_err(|e| format!("approval failed: {e}"))?;
            let hash_text = sent
                .get("transaction_hash")
                .and_then(Value::as_str)
                .ok_or("approval returned no transaction hash")?
                .to_string();
            self.wait_for_success(rpc, &parse_hash(&hash_text)?)?;
            done.push(Value::object(vec![
                ("purpose", Value::str(purpose)),
                ("spender", Value::String(to_checksum(&spender))),
                ("amount_raw", Value::String(value.to_dec_string())),
                ("transaction_hash", Value::String(hash_text)),
            ]));
        }
        Ok(done)
    }

    /// Swaps tokens on one chain through OpenOcean and broadcasts the transaction.
    pub fn swap_tokens(&self, name: &str, password: &str, p: &SwapParams) -> Result<Value, String> {
        let (record, contents) = self.unlock(name, password)?;
        let network = Self::swap_network(&contents, p)?;
        let rpc = ops::connect_for_send(&*self.http, &network, &self.journal)?;
        let prep = self.prepare_swap(&rpc, &network, contents.address()?, p)?;
        let key = contents.private_key_array()?;

        let mut state = self.read_state(&rpc, &prep)?;
        Self::require_balance(&prep, &state)?;
        let mut quote = self.fetch_quote(&prep)?;
        swap::check_quote(&quote, &prep.expect)?;

        let mut approvals = vec![];
        if let Some(allowance) = state.allowance
            && allowance < prep.request.amount
        {
            approvals = self.approve_exact(&rpc, &prep, &key.0, &allowance)?;
            // Price the swap again now that the approval is in place.
            quote = self.fetch_quote(&prep)?;
            state = self.read_state(&rpc, &prep)?;
            Self::require_balance(&prep, &state)?;
        }
        let tx = swap::check_quote(&quote, &prep.expect)?.clone();

        let intent = TxIntent {
            to: tx.to,
            value: tx.value,
            data: tx.data,
        };
        let mut prepared = ops::prepare_tx(&rpc, &network, &prep.owner, &intent, None)?;
        // OpenOcean advises 1.25x to 2.5x of the estimate for aggregator swaps.
        let roomy = prepared.gas_estimate.saturating_mul(3) / 2;
        prepared.tx.gas_limit = prepared.tx.gas_limit.max(roomy);
        let sent = ops::sign_and_maybe_send(&rpc, &network, &prepared, &key.0, true)?;

        let report = without_key(
            swap::quote_report(
                &prep.from,
                &prep.to,
                &quote,
                self.swap.fee.as_ref(),
                &prep.request.amount,
                &prep.receiver,
                &prep.request.gas_price,
                &state,
            ),
            "note",
        );
        Ok(merge(
            report,
            vec![
                ("wallet", Value::String(record.name)),
                ("status", Value::str("submitted")),
                ("transaction", sent),
                ("approvals", Value::Array(approvals)),
                (
                    "next_step",
                    Value::str(
                        "Confirm the swap with get_transaction (chain_id, transaction_hash).",
                    ),
                ),
            ],
        ))
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use crate::http::mock::RouterMock;
    use crate::store::testutil::TempDir;

    pub fn word(v: u64) -> String {
        format!("\"0x{}\"", hex::encode(&U256::from_u64(v).to_be_bytes()))
    }

    /// A router that behaves like a healthy Ethereum mainnet node.
    pub fn mainnet_router() -> RouterMock {
        RouterMock::new()
            .rpc("eth_chainId", "\"0x1\"")
            .rpc("eth_getBalance", "\"0xde0b6b3a7640000\"")
            .rpc("eth_getTransactionCount", "\"0x5\"")
            .rpc(
                "eth_getBlockByNumber",
                r#"{"baseFeePerGas":"0x3b9aca00","timestamp":"0x65"}"#,
            )
            .rpc("eth_maxPriorityFeePerGas", "\"0x3b9aca00\"")
            .rpc("eth_estimateGas", "\"0x5208\"")
            .rpc("eth_blockNumber", "\"0x70\"")
            .rpc("eth_call", &word(0))
    }

    pub struct Fixture {
        pub dir: TempDir,
        pub router: Rc<RouterMock>,
        pub service: WalletService,
    }

    pub fn fixture_with(router: RouterMock) -> Fixture {
        fixture_custom(router, |s| s)
    }

    /// Like `fixture_with`, letting the test configure the service (e.g. swap settings).
    pub fn fixture_custom(
        router: RouterMock,
        configure: impl FnOnce(WalletService) -> WalletService,
    ) -> Fixture {
        let dir = TempDir::new();
        let router = Rc::new(router);
        let http: Rc<dyn HttpClient> = router.clone();
        let service = configure(
            WalletService::new(Store::open(&dir.0).unwrap(), http)
                .with_iterations(vault::MIN_ITERATIONS)
                .with_receipt_wait(Duration::ZERO, 3),
        );
        Fixture {
            dir,
            router,
            service,
        }
    }

    pub fn fixture() -> Fixture {
        fixture_with(mainnet_router())
    }

    pub const PW: &str = "correct-horse-battery";
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::http::mock::RouterMock;

    fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
        v.get(key)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("missing {key} in {}", v.to_json()))
    }

    fn abandon_art() -> String {
        format!("{}art", "abandon ".repeat(23))
    }

    fn create(f: &Fixture, name: &str) -> Value {
        f.service.create_wallet("ethereum", name, PW).unwrap()
    }

    #[test]
    fn limiter_locks_after_repeated_failures_and_recovers() {
        let mut l = AttemptLimiter::default();
        let t0 = Instant::now();
        for _ in 0..MAX_FAILED_ATTEMPTS - 1 {
            l.record_failure("W", t0);
            assert!(l.check("w", t0).is_ok());
        }
        l.record_failure("w", t0);
        assert!(l.check("W", t0).unwrap_err().contains("try again"));
        assert!(l.check("other", t0).is_ok());
        assert!(l.check("w", t0 + LOCKOUT).is_ok());
        l.record_success("w");
        assert!(l.check("w", t0).is_ok());
    }

    #[test]
    fn create_returns_address_but_no_secrets() {
        let f = fixture();
        let out = create(&f, "Daniel's wallet");
        assert_eq!(str_field(&out, "name"), "Daniel's wallet");
        assert_eq!(str_field(&out, "blockchain"), "Ethereum");
        assert_eq!(str_field(&out, "address").len(), 42);
        let text = out.to_json();
        assert!(!text.contains("seed") && !text.contains("private") && !text.contains(PW));
    }

    #[test]
    fn can_create_multiple_wallets_and_names_are_unique() {
        let f = fixture();
        let a = create(&f, "one");
        let b = create(&f, "two");
        assert_ne!(str_field(&a, "address"), str_field(&b, "address"));
        assert!(
            f.service
                .create_wallet("ethereum", "ONE", PW)
                .unwrap_err()
                .contains("already exists")
        );
        let listed = f.service.list_wallets().unwrap();
        assert_eq!(
            listed
                .get("wallets")
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn create_validates_inputs() {
        let f = fixture();
        assert!(
            f.service
                .create_wallet("bitcoin", "w", PW)
                .unwrap_err()
                .contains("unsupported")
        );
        assert!(f.service.create_wallet("ethereum", "  ", PW).is_err());
        assert!(
            f.service
                .create_wallet("ethereum", "w", "short")
                .unwrap_err()
                .contains("at least 8")
        );
        assert!(
            f.service
                .list_wallets()
                .unwrap()
                .get("wallets")
                .and_then(Value::as_array)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn list_needs_no_password_and_reveals_only_name_and_chain() {
        let f = fixture();
        let addr = create(&f, "w");
        let text = f.service.list_wallets().unwrap().to_json();
        assert!(text.contains("\"name\":\"w\"") && text.contains("Ethereum"));
        assert!(!text.contains(str_field(&addr, "address")));
        assert!(!text.contains("0x"));
        let on_disk: String = std::fs::read_dir(f.dir.0.join("wallets"))
            .unwrap()
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect();
        assert!(
            !on_disk
                .to_lowercase()
                .contains(&str_field(&addr, "address")[2..].to_lowercase())
        );
    }

    #[test]
    fn import_seed_phrase_and_private_key() {
        let f = fixture();
        let a = f
            .service
            .import_wallet("ethereum", "seeded", PW, Some(&abandon_art()), None)
            .unwrap();
        let again = f.service.get_address("seeded", PW).unwrap();
        assert_eq!(str_field(&a, "address"), str_field(&again, "address"));
        let key = format!("0x{}01", "00".repeat(31));
        let k = f
            .service
            .import_wallet("ethereum", "keyed", PW, None, Some(&key))
            .unwrap();
        assert_eq!(
            str_field(&k, "address"),
            "0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf"
        );
    }

    #[test]
    fn import_requires_exactly_one_secret_and_valid_input() {
        let f = fixture();
        assert!(
            f.service
                .import_wallet("ethereum", "w", PW, None, None)
                .is_err()
        );
        assert!(
            f.service
                .import_wallet("ethereum", "w", PW, Some("x"), Some("y"))
                .is_err()
        );
        assert!(
            f.service
                .import_wallet("ethereum", "w", PW, Some("abandon abandon"), None)
                .is_err()
        );
        assert!(
            f.service
                .import_wallet("ethereum", "w", PW, None, Some("0x1234"))
                .is_err()
        );
        assert!(
            f.service
                .import_wallet("ethereum", "w", "short", Some(&abandon_art()), None)
                .is_err()
        );
        f.service
            .import_wallet("ethereum", "w", PW, Some(&abandon_art()), None)
            .unwrap();
        assert!(
            f.service
                .import_wallet("ethereum", "W", PW, Some(&abandon_art()), None)
                .unwrap_err()
                .contains("already exists")
        );
    }

    #[test]
    fn wrong_password_is_rejected_and_rate_limited() {
        let f = fixture();
        create(&f, "w");
        for _ in 0..MAX_FAILED_ATTEMPTS {
            assert_eq!(
                f.service.get_address("w", "wrong-password").unwrap_err(),
                "incorrect password"
            );
        }
        let locked = f.service.get_address("w", PW).unwrap_err();
        assert!(locked.contains("too many failed"), "{locked}");
        // other wallets are unaffected
        create(&f, "other");
        assert!(f.service.get_address("other", PW).is_ok());
    }

    #[test]
    fn successful_unlock_resets_failures() {
        let f = fixture();
        create(&f, "w");
        for _ in 0..MAX_FAILED_ATTEMPTS - 1 {
            let _ = f.service.get_address("w", "wrong-password");
        }
        assert!(f.service.get_address("w", PW).is_ok());
        for _ in 0..MAX_FAILED_ATTEMPTS - 1 {
            let _ = f.service.get_address("w", "wrong-password");
        }
        assert!(f.service.get_address("w", PW).is_ok());
    }

    #[test]
    fn unlocking_one_wallet_does_not_unlock_another() {
        let f = fixture();
        create(&f, "a");
        f.service
            .create_wallet("ethereum", "b", "different-password")
            .unwrap();
        assert!(f.service.get_address("a", PW).is_ok());
        assert_eq!(
            f.service.get_address("b", PW).unwrap_err(),
            "incorrect password"
        );
        assert!(f.service.get_address("b", "different-password").is_ok());
    }

    #[test]
    fn empty_password_and_unknown_wallet() {
        let f = fixture();
        create(&f, "w");
        assert!(
            f.service
                .get_address("w", "")
                .unwrap_err()
                .contains("required")
        );
        assert!(
            f.service
                .get_address("nope", PW)
                .unwrap_err()
                .contains("no wallet")
        );
    }

    #[test]
    fn rename_requires_password_and_keeps_keys() {
        let f = fixture();
        let before = create(&f, "old");
        assert!(
            f.service
                .rename_wallet("old", "new", "wrong-password")
                .is_err()
        );
        let out = f.service.rename_wallet("old", "New Name", PW).unwrap();
        assert_eq!(str_field(&out, "new_name"), "New Name");
        assert!(f.service.get_address("old", PW).is_err());
        let after = f.service.get_address("new name", PW).unwrap();
        assert_eq!(str_field(&before, "address"), str_field(&after, "address"));
        create(&f, "taken");
        assert!(
            f.service
                .rename_wallet("New Name", "TAKEN", PW)
                .unwrap_err()
                .contains("already exists")
        );
    }

    #[test]
    fn delete_requires_confirm_and_password() {
        let f = fixture();
        create(&f, "w");
        assert!(
            f.service
                .delete_wallet("w", PW, false)
                .unwrap_err()
                .contains("irreversible")
        );
        assert!(
            f.service
                .delete_wallet("w", "wrong-password", true)
                .is_err()
        );
        assert!(f.service.get_address("w", PW).is_ok());
        let out = f.service.delete_wallet("w", PW, true).unwrap();
        assert_eq!(str_field(&out, "deleted"), "w");
        assert!(f.service.get_address("w", PW).is_err());
        assert!(
            f.service
                .list_wallets()
                .unwrap()
                .get("wallets")
                .and_then(Value::as_array)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn secrets_are_only_shown_by_the_dedicated_tools() {
        let f = fixture();
        f.service
            .import_wallet("ethereum", "w", PW, Some(&abandon_art()), None)
            .unwrap();
        let seed = f.service.view_seed_phrase("w", PW).unwrap();
        assert_eq!(str_field(&seed, "seed_phrase"), abandon_art());
        assert!(f.service.view_seed_phrase("w", "wrong-password").is_err());
        let key = f.service.view_private_key("w", PW).unwrap();
        assert!(
            str_field(&key, "private_key").starts_with("0x")
                && str_field(&key, "private_key").len() == 66
        );
        let key_only = format!("0x{}01", "00".repeat(31));
        f.service
            .import_wallet("ethereum", "k", PW, None, Some(&key_only))
            .unwrap();
        assert!(
            f.service
                .view_seed_phrase("k", PW)
                .unwrap_err()
                .contains("no seed phrase")
        );
        assert_eq!(
            str_field(&f.service.view_private_key("k", PW).unwrap(), "private_key"),
            key_only
        );
    }

    #[test]
    fn export_creates_decryptable_zip_and_never_overwrites() {
        let f = fixture();
        f.service
            .import_wallet("ethereum", "w", PW, Some(&abandon_art()), None)
            .unwrap();
        let key = "export-encryption-key";
        let out = f
            .service
            .export_wallet("w", PW, key, Some("backup.zip"))
            .unwrap();
        let path = std::path::PathBuf::from(str_field(&out, "file"));
        assert!(path.starts_with(f.dir.0.join("exports")));
        let zip = std::fs::read(&path).unwrap();
        assert_eq!(&zip[..4], b"PK\x03\x04");
        let text = String::from_utf8_lossy(&zip).into_owned();
        assert!(!text.contains("abandon") && !text.contains("privateKey"));
        let size = u32::from_le_bytes(zip[18..22].try_into().unwrap()) as usize;
        let data_start = 30 + EXPORT_ENTRY_NAME.len();
        let vault_json =
            crate::json::parse(std::str::from_utf8(&zip[data_start..data_start + size]).unwrap())
                .unwrap();
        let sealed = crate::vault::EncryptedVault::from_json(&vault_json).unwrap();
        assert_eq!(sealed.iterations, crate::vault::DEFAULT_ITERATIONS);
        let plain = crate::vault::decrypt(key, &sealed).unwrap();
        let doc = crate::json::parse(std::str::from_utf8(plain.expose()).unwrap()).unwrap();
        assert_eq!(str_field(&doc, "mnemonic"), abandon_art());
        assert_eq!(str_field(&doc, "name"), "w");
        assert!(str_field(&doc, "privateKey").starts_with("0x"));
        assert!(crate::vault::decrypt("wrong-export-key", &sealed).is_err());
        assert!(
            f.service
                .export_wallet("w", PW, key, Some("backup.zip"))
                .is_err()
        );
    }

    #[test]
    fn export_input_validation() {
        let f = fixture();
        create(&f, "w");
        assert!(
            f.service
                .export_wallet("w", PW, "short", None)
                .unwrap_err()
                .contains("at least 12")
        );
        assert!(
            f.service
                .export_wallet("w", "wrong-password", "long-enough-key-1", None)
                .is_err()
        );
        for bad in [
            "../x.zip",
            "a/b.zip",
            "x.txt",
            ".hidden.zip",
            "",
            "sp ace.zip",
        ] {
            assert!(
                f.service
                    .export_wallet("w", PW, "long-enough-key-1", Some(bad))
                    .is_err(),
                "{bad}"
            );
        }
        let default = f
            .service
            .export_wallet("w", PW, "long-enough-key-1", None)
            .unwrap();
        assert!(str_field(&default, "file").ends_with(".zip"));
    }

    #[test]
    fn export_file_name_rules() {
        assert!(validate_export_file_name("ok-1_2.zip").is_ok());
        assert!(validate_export_file_name(&format!("{}.zip", "a".repeat(100))).is_err());
        assert!(validate_export_file_name("..zip").is_err());
    }

    #[test]
    fn networks_can_be_added_updated_and_deleted() {
        let router = mainnet_router();
        let f = fixture_with(router);
        create(&f, "w");
        let base = Network::new(
            "Base",
            1,
            "https://mainnet.base.org",
            "ETH",
            "https://basescan.org",
        )
        .unwrap();
        // the mock node reports chain 1, so a network claiming another chain is rejected
        let wrong = Network {
            chain_id: 8453,
            ..base.clone()
        };
        assert!(
            f.service
                .add_network("w", PW, wrong)
                .unwrap_err()
                .contains("reports chain ID")
        );
        assert!(
            f.service
                .add_network("w", PW, base)
                .unwrap_err()
                .contains("already exists")
        );
    }

    fn l2_fixture() -> Fixture {
        // One router answers as chain 8453 so the L2 can be added and used.
        let router = RouterMock::new()
            .rpc("eth_chainId", "\"0x2105\"")
            .rpc("eth_getBalance", "\"0xde0b6b3a7640000\"")
            .rpc("eth_getTransactionCount", "\"0x2\"")
            .rpc(
                "eth_getBlockByNumber",
                r#"{"baseFeePerGas":"0x3b9aca00","timestamp":"0x65"}"#,
            )
            .rpc("eth_maxPriorityFeePerGas", "\"0x3b9aca00\"")
            .rpc("eth_estimateGas", "\"0x5208\"")
            .rpc("eth_blockNumber", "\"0x70\"")
            .rpc("eth_call", &word(0))
            .rpc(
                "eth_sendRawTransaction",
                &format!("\"0x{}\"", "ab".repeat(32)),
            );
        fixture_with(router)
    }

    fn base_network() -> Network {
        Network::new(
            "Base",
            8453,
            "https://mainnet.base.org",
            "ETH",
            "https://base.blockscout.com",
        )
        .unwrap()
    }

    #[test]
    fn network_lifecycle_persists_in_the_vault() {
        let f = l2_fixture();
        create(&f, "w");
        f.service.add_network("w", PW, base_network()).unwrap();
        let listed = f.service.view_networks("w", PW).unwrap();
        let nets = listed.get("networks").and_then(Value::as_array).unwrap();
        assert_eq!(nets.len(), 2);
        assert_eq!(nets[0].get("builtin"), Some(&Value::Bool(true)));
        assert_eq!(nets[1].get("builtin"), Some(&Value::Bool(false)));

        assert!(
            f.service
                .add_network("w", PW, base_network())
                .unwrap_err()
                .contains("already exists")
        );
        let mut changed = base_network();
        changed.name = "Base Mainnet".to_string();
        f.service.update_network("w", PW, changed).unwrap();
        let after = f.service.view_networks("w", PW).unwrap().to_json();
        assert!(after.contains("Base Mainnet") && !after.contains("\"Base\""));

        assert!(
            f.service
                .delete_network("w", PW, 1)
                .unwrap_err()
                .contains("built-in")
        );
        assert!(
            f.service
                .delete_network("w", "wrong-password", 8453)
                .is_err()
        );
        f.service.delete_network("w", PW, 8453).unwrap();
        assert_eq!(
            f.service
                .view_networks("w", PW)
                .unwrap()
                .get("networks")
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            1
        );
        assert!(f.service.delete_network("w", PW, 8453).is_err());
        assert!(f.service.update_network("w", PW, base_network()).is_err());
    }

    #[test]
    fn networks_are_per_wallet() {
        let f = l2_fixture();
        create(&f, "a");
        create(&f, "b");
        f.service.add_network("a", PW, base_network()).unwrap();
        assert_eq!(
            f.service
                .view_networks("b", PW)
                .unwrap()
                .get("networks")
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            1
        );
        assert!(
            f.service
                .estimate_gas(
                    "b",
                    PW,
                    8453,
                    &IntentParams {
                        to: Some(to_checksum(&[2; 20])),
                        ..Default::default()
                    }
                )
                .unwrap_err()
                .contains("add_network")
        );
    }

    fn token_router(chain_hex: &str) -> RouterMock {
        RouterMock::new()
            .rpc("eth_chainId", &format!("\"{chain_hex}\""))
            .rpc("eth_getBalance", "\"0x0\"")
            .rpc("eth_call:313ce567", &word(6))
            .rpc(
                "eth_call:95d89b41",
                &format!(
                    "\"0x{}\"",
                    hex::encode(&crate::eth::abi::encode_values(&[
                        crate::eth::abi::AbiValue::Str("MYTKN".into())
                    ]))
                ),
            )
            .rpc("eth_call:70a08231", &word(2_500_000))
    }

    fn token_addr() -> String {
        to_checksum(&[0xaa; 20])
    }

    #[test]
    fn add_token_reads_the_contract_and_persists_it() {
        let f = fixture_with(token_router("0x1"));
        create(&f, "w");
        let out = f.service.add_token("w", PW, 1, &token_addr()).unwrap();
        let added = out.get("added").unwrap();
        assert_eq!(str_field(added, "symbol"), "MYTKN");
        assert_eq!(str_field(added, "address"), token_addr());
        assert_eq!(added.get("decimals"), Some(&Value::Number("6".into())));
        assert_eq!(added.get("chainId"), Some(&Value::Number("1".into())));

        // it shows up in balances without being passed again
        let bal = f.service.get_balances("w", PW, Some(1), &[]).unwrap();
        let assets = bal.get("networks").and_then(Value::as_array).unwrap()[0]
            .get("assets")
            .and_then(Value::as_array)
            .unwrap()
            .clone();
        let mine = assets
            .iter()
            .find(|a| a.get("symbol").and_then(Value::as_str) == Some("MYTKN"))
            .expect("added token is listed");
        assert_eq!(str_field(mine, "balance"), "2.5");
        assert_eq!(str_field(mine, "asset"), token_addr());
    }

    #[test]
    fn added_tokens_survive_vault_rewrites_and_stay_per_wallet() {
        let f = fixture_with(token_router("0x1"));
        create(&f, "a");
        create(&f, "b");
        f.service.add_token("a", PW, 1, &token_addr()).unwrap();
        // a second add and a rename both re-seal the vault; the first token must remain
        f.service
            .add_token("a", PW, 1, &to_checksum(&[0xbb; 20]))
            .unwrap();
        f.service.rename_wallet("a", "a2", PW).unwrap();
        let a = f
            .service
            .get_balances("a2", PW, Some(1), &[])
            .unwrap()
            .to_json();
        assert!(a.contains(&token_addr()) && a.contains(&to_checksum(&[0xbb; 20])));
        // another wallet never sees them
        let b = f
            .service
            .get_balances("b", PW, Some(1), &[])
            .unwrap()
            .to_json();
        assert!(!b.contains(&token_addr()));
        assert!(
            f.service
                .remove_token("b", PW, 1, &token_addr())
                .unwrap_err()
                .contains("not been added")
        );
    }

    #[test]
    fn add_token_validation() {
        let f = fixture_with(token_router("0x1"));
        create(&f, "w");
        assert!(
            f.service
                .add_token("w", PW, 1, "nope")
                .unwrap_err()
                .contains("'contract'")
        );
        assert!(
            f.service
                .add_token("w", PW, 1, &to_checksum(&[0; 20]))
                .unwrap_err()
                .contains("zero address")
        );
        assert!(
            f.service
                .add_token("w", PW, 999, &token_addr())
                .unwrap_err()
                .contains("add_network")
        );
        assert!(
            f.service
                .add_token("w", "wrong-password", 1, &token_addr())
                .is_err()
        );
        f.service.add_token("w", PW, 1, &token_addr()).unwrap();
        assert!(
            f.service
                .add_token("w", PW, 1, &token_addr())
                .unwrap_err()
                .contains("already added")
        );
        let usdc = to_checksum(&crate::eth::tokens::known_tokens(1)[0].address);
        assert!(
            f.service
                .add_token("w", PW, 1, &usdc)
                .unwrap_err()
                .contains("by default")
        );
    }

    #[test]
    fn add_token_rejects_non_tokens_and_wrong_chain_rpc() {
        let not_a_token = RouterMock::new().rpc("eth_chainId", "\"0x1\"").rpc_error(
            "eth_call",
            3,
            "execution reverted",
            None,
        );
        let f = fixture_with(not_a_token);
        create(&f, "w");
        assert!(
            f.service
                .add_token("w", PW, 1, &token_addr())
                .unwrap_err()
                .contains("not an ERC-20")
        );
        let wrong_chain = fixture_with(token_router("0x2105"));
        create(&wrong_chain, "w");
        assert!(
            wrong_chain
                .service
                .add_token("w", PW, 1, &token_addr())
                .unwrap_err()
                .contains("reports chain ID")
        );
        // nothing was stored by the failed attempts
        let bal = f
            .service
            .get_balances("w", PW, Some(1), &[])
            .unwrap()
            .to_json();
        assert!(!bal.contains("MYTKN"));
    }

    #[test]
    fn remove_token_stops_tracking_it() {
        let f = fixture_with(token_router("0x1"));
        create(&f, "w");
        f.service.add_token("w", PW, 1, &token_addr()).unwrap();
        assert!(
            f.service
                .remove_token("w", "wrong-password", 1, &token_addr())
                .is_err()
        );
        assert!(
            f.service
                .get_balances("w", PW, Some(1), &[])
                .unwrap()
                .to_json()
                .contains("MYTKN")
        );
        let out = f.service.remove_token("w", PW, 1, &token_addr()).unwrap();
        assert_eq!(
            str_field(out.get("removed").unwrap(), "address"),
            token_addr()
        );
        assert!(
            f.service
                .remove_token("w", PW, 1, &token_addr())
                .unwrap_err()
                .contains("not been added")
        );
        let usdc = to_checksum(&crate::eth::tokens::known_tokens(1)[0].address);
        assert!(
            f.service
                .remove_token("w", PW, 1, &usdc)
                .unwrap_err()
                .contains("built-in")
        );
        assert!(
            f.service
                .remove_token("w", PW, 999, &token_addr())
                .unwrap_err()
                .contains("add_network")
        );
    }

    #[test]
    fn removing_a_network_also_removes_its_tokens() {
        let router = RouterMock::new()
            .rpc("eth_chainId", "\"0x2105\"")
            .rpc("eth_call:313ce567", &word(18))
            .rpc("eth_call:95d89b41", &word(0));
        let f = fixture_with(router);
        create(&f, "w");
        f.service.add_network("w", PW, base_network()).unwrap();
        f.service.add_token("w", PW, 8453, &token_addr()).unwrap();
        let out = f.service.delete_network("w", PW, 8453).unwrap();
        assert_eq!(out.get("removed_tokens"), Some(&Value::Number("1".into())));
        // adding the network again does not resurrect the token
        f.service.add_network("w", PW, base_network()).unwrap();
        assert!(
            f.service
                .remove_token("w", PW, 8453, &token_addr())
                .unwrap_err()
                .contains("not been added")
        );
    }

    #[test]
    fn tokens_live_only_inside_the_encrypted_vault() {
        let f = fixture_with(token_router("0x1"));
        create(&f, "w");
        f.service.add_token("w", PW, 1, &token_addr()).unwrap();
        let on_disk: String = std::fs::read_dir(f.dir.0.join("wallets"))
            .unwrap()
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect();
        assert!(!on_disk.contains("MYTKN") && !on_disk.to_lowercase().contains("aaaaaaaaaaaaaaaa"));
    }

    #[test]
    fn adding_a_network_does_not_change_keys() {
        let f = l2_fixture();
        let before = create(&f, "w");
        f.service.add_network("w", PW, base_network()).unwrap();
        let after = f.service.get_address("w", PW).unwrap();
        assert_eq!(str_field(&before, "address"), str_field(&after, "address"));
    }

    #[test]
    fn balances_cover_all_networks_and_isolate_failures() {
        let f = l2_fixture();
        create(&f, "w");
        f.service.add_network("w", PW, base_network()).unwrap();
        let out = f.service.get_balances("w", PW, None, &[]).unwrap();
        let nets = out.get("networks").and_then(Value::as_array).unwrap();
        assert_eq!(nets.len(), 2);
        // the mock answers chain 8453, so mainnet (chain 1) reports an error while Base succeeds
        assert!(nets[0].get("error").is_some());
        assert!(nets[1].get("assets").is_some());
        let only = f.service.get_balances("w", PW, Some(8453), &[]).unwrap();
        assert_eq!(
            only.get("networks")
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            1
        );
        assert!(f.service.get_balances("w", PW, Some(999), &[]).is_err());
    }

    #[test]
    fn send_native_asset_broadcasts() {
        let f = l2_fixture();
        create(&f, "w");
        f.service.add_network("w", PW, base_network()).unwrap();
        let to = to_checksum(&[0x22; 20]);
        let out = f
            .service
            .send_asset("w", PW, 8453, "native", &to, "0.1", None)
            .unwrap();
        assert_eq!(out.get("broadcast"), Some(&Value::Bool(true)));
        assert_eq!(
            str_field(&out, "transaction_hash"),
            format!("0x{}", "ab".repeat(32))
        );
        assert_eq!(str_field(&out, "amount"), "0.1");
        assert_eq!(str_field(&out, "asset"), "ETH");
        assert_eq!(str_field(&out, "to"), to);
        assert!(
            f.router
                .sent_methods()
                .contains(&"eth_sendRawTransaction".to_string())
        );
        assert!(!out.to_json().contains("raw_transaction"));
    }

    #[test]
    fn send_validates_before_touching_the_network() {
        let f = l2_fixture();
        create(&f, "w");
        f.service.add_network("w", PW, base_network()).unwrap();
        let before = f.router.sent_methods().len();
        assert!(
            f.service
                .send_asset("w", PW, 8453, "native", "0xnope", "1", None)
                .is_err()
        );
        assert!(
            f.service
                .send_asset("w", PW, 8453, "native", &to_checksum(&[0; 20]), "1", None)
                .is_err()
        );
        assert!(
            f.service
                .send_asset(
                    "w",
                    "wrong-password",
                    8453,
                    "native",
                    &to_checksum(&[2; 20]),
                    "1",
                    None
                )
                .is_err()
        );
        assert!(
            f.service
                .send_asset(
                    "w",
                    PW,
                    8453,
                    "native",
                    &to_checksum(&[2; 20]),
                    "1000",
                    None
                )
                .unwrap_err()
                .contains("insufficient funds")
        );
        assert!(!f.router.sent_methods()[before..].contains(&"eth_sendRawTransaction".to_string()));
    }

    #[test]
    fn sign_contract_transaction_signs_without_broadcasting_by_default() {
        let f = l2_fixture();
        create(&f, "w");
        f.service.add_network("w", PW, base_network()).unwrap();
        let params = IntentParams {
            to: Some(to_checksum(&[0xaa; 20])),
            data: Some("0xa9059cbb".to_string()),
            ..Default::default()
        };
        let out = f
            .service
            .sign_contract_transaction("w", PW, 8453, &params, None, false)
            .unwrap();
        assert_eq!(out.get("broadcast"), Some(&Value::Bool(false)));
        assert!(str_field(&out, "raw_transaction").starts_with("0x02"));
        assert!(
            !f.router
                .sent_methods()
                .contains(&"eth_sendRawTransaction".to_string())
        );
        let sent = f
            .service
            .sign_contract_transaction("w", PW, 8453, &params, Some(100_000), true)
            .unwrap();
        assert_eq!(sent.get("broadcast"), Some(&Value::Bool(true)));
        assert_eq!(sent.get("gas_limit"), Some(&num(100_000)));
    }

    #[test]
    fn intent_parameter_rules() {
        let f = l2_fixture();
        create(&f, "w");
        f.service.add_network("w", PW, base_network()).unwrap();
        let to = to_checksum(&[0x22; 20]);
        let bad = [
            IntentParams::default(),
            IntentParams {
                to: Some("nope".into()),
                ..Default::default()
            },
            IntentParams {
                to: Some(to.clone()),
                asset: Some("native".into()),
                ..Default::default()
            },
            IntentParams {
                to: Some(to.clone()),
                asset: Some("native".into()),
                amount: Some("1".into()),
                data: Some("0x".into()),
                ..Default::default()
            },
            IntentParams {
                to: Some(to.clone()),
                amount: Some("1".into()),
                ..Default::default()
            },
            IntentParams {
                to: Some(to.clone()),
                value: Some("x".into()),
                ..Default::default()
            },
            IntentParams {
                to: Some(to.clone()),
                data: Some("0xzz".into()),
                ..Default::default()
            },
        ];
        for p in bad {
            assert!(f.service.estimate_gas("w", PW, 8453, &p).is_err(), "{p:?}");
        }
        let good = IntentParams {
            to: Some(to),
            asset: Some("native".into()),
            amount: Some("0.01".into()),
            ..Default::default()
        };
        assert!(f.service.estimate_gas("w", PW, 8453, &good).is_ok());
    }

    #[test]
    fn estimate_and_simulate_work_without_sending() {
        let f = l2_fixture();
        create(&f, "w");
        f.service.add_network("w", PW, base_network()).unwrap();
        let p = IntentParams {
            to: Some(to_checksum(&[0x22; 20])),
            value: Some("0.1".into()),
            ..Default::default()
        };
        let est = f.service.estimate_gas("w", PW, 8453, &p).unwrap();
        assert_eq!(est.get("gas_estimate"), Some(&num(21_000)));
        let sim = f.service.simulate_transaction("w", PW, 8453, &p).unwrap();
        assert_eq!(sim.get("success"), Some(&Value::Bool(true)));
        assert!(
            !f.router
                .sent_methods()
                .contains(&"eth_sendRawTransaction".to_string())
        );
    }

    #[test]
    fn read_contract_via_service() {
        let f = l2_fixture();
        create(&f, "w");
        f.service.add_network("w", PW, base_network()).unwrap();
        let out = f
            .service
            .read_contract(
                "w",
                PW,
                8453,
                &to_checksum(&[0xaa; 20]),
                "totalSupply()",
                &[],
                Some("uint256"),
            )
            .unwrap();
        assert_eq!(
            out.get("result").and_then(Value::as_array).unwrap()[0].as_str(),
            Some("0")
        );
        assert_eq!(str_field(&out, "function"), "totalSupply()");
        assert!(
            f.service
                .read_contract("w", PW, 8453, "bad", "x()", &[], None)
                .is_err()
        );
    }

    #[test]
    fn get_transaction_via_service() {
        let router = RouterMock::new()
            .rpc("eth_chainId", "\"0x2105\"")
            .rpc("eth_getTransactionByHash", "null");
        let f = fixture_with(router);
        create(&f, "w");
        f.service.add_network("w", PW, base_network()).unwrap();
        let out = f
            .service
            .get_transaction("w", PW, 8453, &format!("0x{}", "11".repeat(32)))
            .unwrap();
        assert_eq!(str_field(&out, "status"), "not_found");
        assert!(f.service.get_transaction("w", PW, 8453, "0x1234").is_err());
    }

    #[test]
    fn history_via_explorer() {
        let body = r#"{"items":[{"hash":"0xaaa","block_number":1,"value":"1000000000000000000","status":"ok","from":{"hash":"0xbbb"},"to":{"hash":"0xccc"}}],"next_page_params":null}"#;
        let router = RouterMock::new().rpc("eth_chainId", "\"0x2105\"").get(
            "base.blockscout.com/api/v2/addresses/",
            Ok(body.to_string()),
        );
        let f = fixture_with(router);
        create(&f, "w");
        f.service.add_network("w", PW, base_network()).unwrap();
        let out = f
            .service
            .get_transaction_history("w", PW, 8453, HistoryKind::Transactions, 10)
            .unwrap();
        assert_eq!(out.get("count"), Some(&num(1)));
        assert_eq!(out.get("has_more"), Some(&Value::Bool(false)));
        assert!(
            f.service
                .get_transaction_history("w", PW, 8453, HistoryKind::Transactions, 0)
                .is_err()
        );
    }

    #[test]
    fn revoke_flow() {
        let f = {
            let router = RouterMock::new()
                .rpc("eth_chainId", "\"0x2105\"")
                .rpc("eth_getBalance", "\"0xde0b6b3a7640000\"")
                .rpc("eth_getTransactionCount", "\"0x0\"")
                .rpc("eth_getBlockByNumber", r#"{"baseFeePerGas":"0x3b9aca00"}"#)
                .rpc("eth_maxPriorityFeePerGas", "\"0x3b9aca00\"")
                .rpc("eth_estimateGas", "\"0xb71b\"")
                .rpc("eth_call:313ce567", &word(6))
                .rpc(
                    "eth_call:95d89b41",
                    &format!(
                        "\"0x{}\"",
                        hex::encode(&crate::eth::abi::encode_values(&[
                            crate::eth::abi::AbiValue::Str("USDC".into())
                        ]))
                    ),
                )
                .rpc("eth_call:dd62ed3e", &word(500));
            fixture_with(router)
        };
        create(&f, "w");
        f.service.add_network("w", PW, base_network()).unwrap();
        let token = to_checksum(&[0xaa; 20]);
        let spender = to_checksum(&[0xbb; 20]);
        let out = f
            .service
            .revoke_token_approval("w", PW, 8453, &token, &spender, false)
            .unwrap();
        assert_eq!(str_field(&out, "token"), "USDC");
        assert_eq!(str_field(&out, "previous_allowance_raw"), "500");
        assert_eq!(out.get("broadcast"), Some(&Value::Bool(false)));
        assert!(
            f.service
                .revoke_token_approval("w", PW, 8453, "bad", &spender, false)
                .is_err()
        );
    }

    #[test]
    fn revoke_with_zero_allowance_is_a_no_op() {
        let router = RouterMock::new()
            .rpc("eth_chainId", "\"0x2105\"")
            .rpc("eth_call:313ce567", &word(18))
            .rpc("eth_call:95d89b41", &word(0))
            .rpc("eth_call:dd62ed3e", &word(0));
        let f = fixture_with(router);
        create(&f, "w");
        f.service.add_network("w", PW, base_network()).unwrap();
        let out = f
            .service
            .revoke_token_approval(
                "w",
                PW,
                8453,
                &to_checksum(&[1; 20]),
                &to_checksum(&[2; 20]),
                true,
            )
            .unwrap();
        assert_eq!(str_field(&out, "status"), "nothing_to_revoke");
    }

    #[test]
    fn passwords_never_appear_in_any_output_or_error() {
        let f = l2_fixture();
        let secret_pw = "unique-secret-password-xyz";
        f.service.create_wallet("ethereum", "w", secret_pw).unwrap();
        f.service
            .add_network("w", secret_pw, base_network())
            .unwrap();
        let outputs = vec![
            f.service.get_address("w", secret_pw).map(|v| v.to_json()),
            f.service
                .get_address("w", "wrong-secret-password-xyz")
                .map(|v| v.to_json()),
            f.service
                .get_balances("w", secret_pw, None, &[])
                .map(|v| v.to_json()),
            f.service.view_networks("w", secret_pw).map(|v| v.to_json()),
            f.service
                .delete_wallet("w", "wrong-secret-password-xyz", true)
                .map(|v| v.to_json()),
            f.service
                .rename_wallet("nope", "x", secret_pw)
                .map(|v| v.to_json()),
        ];
        for out in outputs {
            let text = out.unwrap_or_else(|e| e);
            assert!(!text.contains("secret-password-xyz"), "{text}");
        }
    }

    #[test]
    fn every_operation_leaves_the_vault_locked_on_disk() {
        let f = l2_fixture();
        create(&f, "w");
        let file = std::fs::read_dir(f.dir.0.join("wallets"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let before = std::fs::read_to_string(&file).unwrap();
        f.service.get_address("w", PW).unwrap();
        f.service.view_private_key("w", PW).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            before,
            "reads must not rewrite the vault"
        );
        assert!(!before.contains("mnemonic") && !before.contains("keyrings"));
    }
}

#[cfg(test)]
mod swap_tests {
    use super::testing::*;
    use super::*;
    use crate::eth::swap::SwapFee;
    use crate::http::mock::RouterMock;

    const RECIPIENT: &str = "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed";
    const ROUTER: &str = "0x6352a56caadc4f1e25cd6c75970fa768a3304e64";
    const OO_SWAP: &str = "open-api.de1.exchange/v4/1/swap";
    /// The address of private key 0x...01, which `setup` imports.
    const WALLET: &str = "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf";

    fn usdc() -> String {
        to_checksum(&[0xa0; 20])
    }

    fn usdc_lower() -> String {
        hex::encode_prefixed(&[0xa0; 20])
    }

    fn string_result(s: &str) -> String {
        format!(
            "\"0x{}\"",
            hex::encode(&abi::encode_values(&[abi::AbiValue::Str(s.into())]))
        )
    }

    /// Mainnet node: 6-decimal "USDC" at 0xa0a0..., 1 ETH, 100 USDC, no allowance.
    fn mainnet() -> RouterMock {
        RouterMock::new()
            .rpc("eth_chainId", "\"0x1\"")
            .rpc("eth_getBalance", "\"0xde0b6b3a7640000\"")
            .rpc("eth_getTransactionCount", "\"0x5\"")
            .rpc(
                "eth_getBlockByNumber",
                r#"{"baseFeePerGas":"0x3b9aca00","timestamp":"0x65"}"#,
            )
            .rpc("eth_maxPriorityFeePerGas", "\"0x3b9aca00\"")
            .rpc("eth_gasPrice", "\"0x3b9aca00\"")
            .rpc("eth_estimateGas", "\"0x30d40\"")
            .rpc("eth_blockNumber", "\"0x70\"")
            .rpc("eth_call:313ce567", &word(6))
            .rpc("eth_call:95d89b41", &string_result("USDC"))
            .rpc("eth_call:70a08231", &word(100_000_000))
            .rpc("eth_call:dd62ed3e", &word(0))
            .rpc(
                "eth_sendRawTransaction",
                &format!("\"0x{}\"", "cd".repeat(32)),
            )
            .rpc(
                "eth_getTransactionReceipt",
                r#"{"status":"0x1","blockNumber":"0x64"}"#,
            )
    }

    /// An OpenOcean `/swap` response (selling 25 USDC for ETH by default).
    struct Oo {
        in_addr: String,
        in_decimals: u32,
        out_addr: String,
        out_decimals: u32,
        in_amount: String,
        out_amount: String,
        min_out: String,
        to: String,
        value: String,
        impact: String,
        receiver: String,
    }

    impl Oo {
        fn usdc_to_eth() -> Oo {
            Oo {
                in_addr: usdc_lower(),
                in_decimals: 6,
                out_addr: "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into(),
                out_decimals: 18,
                in_amount: "25000000".into(),
                out_amount: "10300000000000000".into(),
                min_out: "10200000000000000".into(),
                to: ROUTER.into(),
                value: "0".into(),
                impact: "-0.05%".into(),
                receiver: WALLET.into(),
            }
        }

        fn eth_to_usdc() -> Oo {
            Oo {
                in_addr: "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into(),
                in_decimals: 18,
                out_addr: usdc_lower(),
                out_decimals: 6,
                in_amount: "10000000000000000".into(),
                out_amount: "24000000".into(),
                min_out: "23700000".into(),
                to: ROUTER.into(),
                value: "10000000000000000".into(),
                impact: "-0.10%".into(),
                receiver: WALLET.into(),
            }
        }

        fn json(&self) -> String {
            let addr = |a: &str| parse_address(a).unwrap();
            let calldata = hex::encode_prefixed(&swap::test_router_calldata(
                &addr(&self.in_addr),
                &addr(&self.out_addr),
                &addr(&self.receiver),
                &U256::from_dec_str(&self.in_amount).unwrap(),
                &U256::from_dec_str(&self.min_out).unwrap(),
            ));
            format!(
                r#"{{"code":200,"data":{{"inToken":{{"address":"{}","decimals":{},"symbol":"IN","usd":"1","volume":25}},"outToken":{{"address":"{}","decimals":{},"symbol":"OUT","usd":"1","volume":24.8}},"inAmount":"{}","outAmount":"{}","estimatedGas":174014,"minOutAmount":"{}","from":"{WALLET}","to":"{}","value":"{}","gasPrice":"1000000000","data":"{}","chainId":1,"price_impact":"{}","path":{{"routes":[{{"subRoutes":[{{"dexes":[{{"dex":"UniswapV4"}}]}}]}}]}}}}}}"#,
                self.in_addr,
                self.in_decimals,
                self.out_addr,
                self.out_decimals,
                self.in_amount,
                self.out_amount,
                self.min_out,
                self.to,
                self.value,
                calldata,
                self.impact
            )
        }
    }

    fn settings(with_fee: bool) -> SwapSettings {
        SwapSettings::new(if with_fee {
            SwapFee::from_parts(RECIPIENT, 70).unwrap()
        } else {
            None
        })
    }

    fn params() -> SwapParams {
        SwapParams {
            from_chain_id: 1,
            from_token: usdc(),
            to_chain_id: 1,
            to_token: "native".to_string(),
            amount: "25".to_string(),
            ..Default::default()
        }
    }

    fn setup_with(router: RouterMock, with_fee: bool) -> Fixture {
        let f = fixture_custom(router, |s| s.with_swap_settings(settings(with_fee)));
        let key = format!("0x{}01", "00".repeat(31));
        f.service
            .import_wallet("ethereum", "w", PW, None, Some(&key))
            .unwrap();
        f
    }

    fn setup(oo: &Oo, with_fee: bool) -> Fixture {
        setup_with(mainnet().get(OO_SWAP, Ok(oo.json())), with_fee)
    }

    fn sends(f: &Fixture) -> usize {
        f.router
            .sent_methods()
            .iter()
            .filter(|m| *m == "eth_sendRawTransaction")
            .count()
    }

    fn oo_requests(f: &Fixture) -> Vec<String> {
        f.router
            .sent_methods()
            .into_iter()
            .filter(|m| m.starts_with("GET https://open-api.de1.exchange"))
            .collect()
    }

    fn text<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
        let mut cur = v;
        for p in path {
            cur = cur.get(p)?;
        }
        cur.as_str()
    }

    // ---- quotes ----

    #[test]
    fn quote_shows_expected_amount_and_fees_and_sends_nothing() {
        let f = setup(&Oo::usdc_to_eth(), true);
        let q = f.service.get_swap_quote("w", PW, &params()).unwrap();
        assert_eq!(text(&q, &["type"]), Some("same_chain"));
        assert_eq!(text(&q, &["wallet"]), Some("w"));
        assert_eq!(text(&q, &["from", "amount"]), Some("25"));
        assert_eq!(text(&q, &["from", "symbol"]), Some("USDC"));
        assert_eq!(text(&q, &["to", "expected_amount"]), Some("0.0103"));
        assert_eq!(text(&q, &["to", "minimum_amount"]), Some("0.0102"));
        assert_eq!(text(&q, &["to", "symbol"]), Some("ETH"));
        assert_eq!(
            text(&q, &["to", "recipient"]),
            Some("0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf")
        );
        assert_eq!(text(&q, &["fees", "service_fee", "percent"]), Some("0.7"));
        assert_eq!(text(&q, &["fees", "service_fee", "amount"]), Some("0.175"));
        assert_eq!(
            text(&q, &["fees", "service_fee", "recipient"]),
            Some(RECIPIENT)
        );
        assert_eq!(text(&q, &["fees", "network_fee", "currency"]), Some("ETH"));
        let req = q.get("requirements").unwrap();
        assert_eq!(req.get("approval_needed"), Some(&Value::Bool(true)));
        assert_eq!(req.get("sufficient_balance"), Some(&Value::Bool(true)));
        assert_eq!(sends(&f), 0);
    }

    #[test]
    fn quote_request_carries_the_fee_and_needs_no_key_or_headers() {
        let f = setup(&Oo::usdc_to_eth(), true);
        f.service.get_swap_quote("w", PW, &params()).unwrap();
        let requests = oo_requests(&f);
        assert_eq!(requests.len(), 1);
        let url = &requests[0];
        assert!(url.contains("/v4/1/swap?inTokenAddress=0xa0a0"));
        assert!(url.contains("outTokenAddress=0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"));
        assert!(url.contains("amountDecimals=25000000"));
        assert!(url.contains("gasPriceDecimals=1000000000"));
        assert!(url.contains(&format!("account={WALLET}")));
        assert!(url.contains("referrer=0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed"));
        assert!(url.contains("referrerFee=0.7"));
        assert!(!url.to_lowercase().contains("apikey") && !url.contains("sender="));
    }

    #[test]
    fn without_a_fee_recipient_no_fee_is_requested() {
        let f = setup(&Oo::usdc_to_eth(), false);
        let q = f.service.get_swap_quote("w", PW, &params()).unwrap();
        assert!(!oo_requests(&f)[0].contains("referrer"));
        assert_eq!(
            q.get("fees").unwrap().get("service_fee"),
            Some(&Value::Null)
        );
    }

    #[test]
    fn selling_the_native_coin_and_erc20_to_erc20() {
        let f = setup(&Oo::eth_to_usdc(), true);
        let mut p = params();
        p.from_token = "native".into();
        p.to_token = usdc();
        p.amount = "0.01".into();
        let q = f.service.get_swap_quote("w", PW, &p).unwrap();
        assert_eq!(text(&q, &["from", "symbol"]), Some("ETH"));
        assert_eq!(text(&q, &["to", "symbol"]), Some("USDC"));
        assert_eq!(text(&q, &["to", "expected_amount"]), Some("24"));
        assert_eq!(
            q.get("requirements").unwrap().get("approval_needed"),
            Some(&Value::Bool(false))
        );
        assert!(oo_requests(&f)[0].contains("inTokenAddress=0xeeee"));
        // ERC-20 to ERC-20: both sides read their decimals from the chain
        let mut oo = Oo::usdc_to_eth();
        oo.out_addr = hex::encode_prefixed(&[0xb0; 20]);
        oo.out_decimals = 6;
        oo.out_amount = "24900000".into();
        oo.min_out = "24600000".into();
        let g = setup(&oo, true);
        let mut p = params();
        p.to_token = to_checksum(&[0xb0; 20]);
        let q = g.service.get_swap_quote("w", PW, &p).unwrap();
        assert_eq!(text(&q, &["to", "expected_amount"]), Some("24.9"));
    }

    #[test]
    fn a_different_receiver_is_sent_as_account_with_the_wallet_as_sender() {
        let f = setup(&Oo::usdc_to_eth(), true);
        let mut p = params();
        p.to_address = Some(to_checksum(&[0x22; 20]));
        let q = f.service.get_swap_quote("w", PW, &p).unwrap();
        let url = &oo_requests(&f)[0];
        assert!(url.contains("account=0x2222222222222222222222222222222222222222"));
        assert!(url.contains(&format!("sender={WALLET}")));
        assert_eq!(
            text(&q, &["to", "recipient"]),
            Some(to_checksum(&[0x22; 20]).as_str())
        );
        p.to_address = Some("nope".into());
        assert!(
            f.service
                .get_swap_quote("w", PW, &p)
                .unwrap_err()
                .contains("'to_address'")
        );
        p.to_address = Some(to_checksum(&[0; 20]));
        assert!(
            f.service
                .get_swap_quote("w", PW, &p)
                .unwrap_err()
                .contains("zero address")
        );
    }

    #[test]
    fn slippage_is_always_sent_defaulting_to_one_percent() {
        let f = setup(&Oo::usdc_to_eth(), true);
        f.service.get_swap_quote("w", PW, &params()).unwrap();
        assert!(
            oo_requests(&f)[0].contains("slippage=1&")
                || oo_requests(&f)[0].ends_with("slippage=1")
        );
    }

    #[test]
    fn slippage_is_passed_as_a_percentage() {
        let f = setup(&Oo::usdc_to_eth(), true);
        let mut p = params();
        p.slippage_bps = Some(50);
        f.service.get_swap_quote("w", PW, &p).unwrap();
        assert!(oo_requests(&f)[0].contains("slippage=0.5"));
    }

    #[test]
    fn quote_reports_but_does_not_fail_on_low_balance() {
        let f = setup(&Oo::usdc_to_eth(), true);
        let mut p = params();
        p.amount = "250".into();
        let q = f.service.get_swap_quote("w", PW, &p).unwrap();
        assert_eq!(
            q.get("requirements").unwrap().get("sufficient_balance"),
            Some(&Value::Bool(false))
        );
        assert!(
            !q.get("warnings")
                .and_then(Value::as_array)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn unsupported_requests_are_explained_before_any_api_call() {
        let f = setup(&Oo::usdc_to_eth(), true);
        let bad = |mutate: fn(&mut SwapParams)| {
            let mut p = params();
            mutate(&mut p);
            f.service.get_swap_quote("w", PW, &p).unwrap_err()
        };
        assert!(bad(|p| p.to_chain_id = 8453).contains("cross-chain swaps are not supported"));
        assert!(
            bad(|p| {
                p.from_chain_id = 999_999;
                p.to_chain_id = 999_999
            })
            .contains("does not support chain ID")
        );
        assert!(
            bad(|p| {
                p.from_chain_id = 56;
                p.to_chain_id = 56
            })
            .contains("add_network")
        );
        assert!(bad(|p| p.to_token = p.from_token.clone()).contains("same token"));
        assert!(
            bad(|p| {
                p.from_token = "native".into();
                p.to_token = "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE".into()
            })
            .contains("same token")
        );
        assert!(bad(|p| p.amount = "0".into()).contains("greater than zero"));
        assert!(bad(|p| p.amount = "abc".into()).contains("'amount'"));
        assert!(bad(|p| p.amount = "0.0000001".into()).contains("'amount'"));
        assert!(bad(|p| p.slippage_bps = Some(4)).contains("slippage_bps"));
        assert!(bad(|p| p.slippage_bps = Some(1001)).contains("slippage_bps"));
        assert!(bad(|p| p.from_token = "nope".into()).contains("token"));
        assert!(bad(|p| p.min_receive = Some("x".into())).contains("'min_receive'"));
        assert!(
            f.service
                .get_swap_quote("w", "wrong-password", &params())
                .unwrap_err()
                .contains("incorrect password")
        );
        assert!(oo_requests(&f).is_empty());
    }

    #[test]
    fn api_failures_are_readable() {
        for (err, expect) in [
            (
                "HTTP status 403 (blocked by Cloudflare bot protection)",
                "bot protection",
            ),
            ("HTTP status 429", "2 requests per second"),
            ("HTTP status 500: boom", "boom"),
        ] {
            let f = setup_with(mainnet().get(OO_SWAP, Err(err.into())), true);
            let e = f.service.get_swap_quote("w", PW, &params()).unwrap_err();
            assert!(e.contains(expect), "{e}");
            assert_eq!(sends(&f), 0);
        }
        let f = setup_with(
            mainnet().get(
                OO_SWAP,
                Ok(r#"{"code":400,"error":"INSUFFICIENT_LIQUIDITY"}"#.into()),
            ),
            true,
        );
        assert!(
            f.service
                .get_swap_quote("w", PW, &params())
                .unwrap_err()
                .contains("INSUFFICIENT_LIQUIDITY")
        );
    }

    // ---- swapping ----

    #[test]
    fn swapping_approves_exactly_the_sold_amount_then_swaps() {
        let f = setup(&Oo::usdc_to_eth(), true);
        let mut p = params();
        p.min_receive = Some("0.0102".into());
        let out = f.service.swap_tokens("w", PW, &p).unwrap();
        assert_eq!(text(&out, &["status"]), Some("submitted"));
        assert_eq!(sends(&f), 2, "one approval + one swap");
        let approvals = out.get("approvals").and_then(Value::as_array).unwrap();
        assert_eq!(approvals.len(), 1);
        assert_eq!(text(&approvals[0], &["purpose"]), Some("approve"));
        assert_eq!(text(&approvals[0], &["amount_raw"]), Some("25000000"));
        assert_eq!(
            text(&approvals[0], &["spender"]),
            Some("0x6352a56caadC4F1E25CD6c75970Fa768A3304e64")
        );
        assert_eq!(
            out.get("transaction").unwrap().get("broadcast"),
            Some(&Value::Bool(true))
        );
        assert_eq!(text(&out, &["to", "expected_amount"]), Some("0.0103"));
        assert_eq!(text(&out, &["fees", "service_fee", "percent"]), Some("0.7"));
        assert!(out.get("note").is_none());
        assert!(
            text(&out, &["next_step"])
                .unwrap()
                .contains("get_transaction")
        );
        assert_eq!(oo_requests(&f).len(), 2, "fresh quote after approving");
    }

    #[test]
    fn existing_allowances_are_reset_topped_up_or_reused() {
        // non-zero but too small: reset to zero, then approve
        let f = setup_with(
            mainnet()
                .rpc("eth_call:dd62ed3e", &word(5))
                .get(OO_SWAP, Ok(Oo::usdc_to_eth().json())),
            true,
        );
        let out = f.service.swap_tokens("w", PW, &params()).unwrap();
        let purposes: Vec<_> = out
            .get("approvals")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|a| text(a, &["purpose"]).unwrap())
            .collect();
        assert_eq!(purposes, vec!["reset_allowance", "approve"]);
        assert_eq!(sends(&f), 3);
        // already enough: no approval at all
        let g = setup_with(
            mainnet()
                .rpc("eth_call:dd62ed3e", &word(25_000_000))
                .get(OO_SWAP, Ok(Oo::usdc_to_eth().json())),
            true,
        );
        let out = g.service.swap_tokens("w", PW, &params()).unwrap();
        assert!(
            out.get("approvals")
                .and_then(Value::as_array)
                .unwrap()
                .is_empty()
        );
        assert_eq!(sends(&g), 1);
        assert_eq!(oo_requests(&g).len(), 1);
    }

    #[test]
    fn selling_the_native_coin_needs_no_approval() {
        let f = setup(&Oo::eth_to_usdc(), true);
        let mut p = params();
        p.from_token = "native".into();
        p.to_token = usdc();
        p.amount = "0.01".into();
        let out = f.service.swap_tokens("w", PW, &p).unwrap();
        assert_eq!(text(&out, &["from", "symbol"]), Some("ETH"));
        assert!(
            out.get("approvals")
                .and_then(Value::as_array)
                .unwrap()
                .is_empty()
        );
        assert_eq!(sends(&f), 1);
    }

    #[test]
    fn min_receive_blocks_a_worse_quote_before_anything_is_sent() {
        let f = setup(&Oo::usdc_to_eth(), true);
        let mut p = params();
        p.min_receive = Some("0.0103".into());
        assert!(
            f.service
                .swap_tokens("w", PW, &p)
                .unwrap_err()
                .contains("min_receive")
        );
        assert_eq!(sends(&f), 0);
    }

    #[test]
    fn insufficient_balance_stops_before_calling_openocean_or_approving() {
        let f = setup_with(
            mainnet()
                .rpc("eth_call:70a08231", &word(1_000_000))
                .get(OO_SWAP, Ok(Oo::usdc_to_eth().json())),
            true,
        );
        let err = f.service.swap_tokens("w", PW, &params()).unwrap_err();
        assert!(err.contains("insufficient USDC") && err.contains("holds 1"));
        assert_eq!(sends(&f), 0);
        assert!(oo_requests(&f).is_empty());
    }

    #[test]
    fn dangerous_quotes_are_refused_without_sending_anything() {
        type Mutation = Box<dyn Fn(&mut Oo)>;
        let cases: Vec<(Mutation, &str)> = vec![
            (
                Box::new(|o| o.to = "0x9999999999999999999999999999999999999999".into()),
                "published router",
            ),
            (Box::new(|o| o.in_amount = "25000001".into()), "only"),
            (Box::new(|o| o.value = "1".into()), "more native"),
            (Box::new(|o| o.out_amount = "0".into()), "nothing"),
            (Box::new(|o| o.impact = "-5.5%".into()), "price impact"),
            (
                Box::new(|o| o.in_addr = hex::encode_prefixed(&[0xcc; 20])),
                "different sell token",
            ),
            (Box::new(|o| o.out_decimals = 6), "decimals"),
        ];
        for (mutate, expect) in cases {
            let mut oo = Oo::usdc_to_eth();
            mutate(&mut oo);
            let f = setup(&oo, true);
            let err = f.service.swap_tokens("w", PW, &params()).unwrap_err();
            assert!(err.contains(expect), "{expect}: {err}");
            assert_eq!(sends(&f), 0, "{expect}");
        }
    }

    #[test]
    fn an_unconfirmed_approval_aborts_the_swap() {
        let f = setup_with(
            mainnet()
                .rpc("eth_getTransactionReceipt", "null")
                .get(OO_SWAP, Ok(Oo::usdc_to_eth().json())),
            true,
        );
        let err = f.service.swap_tokens("w", PW, &params()).unwrap_err();
        assert!(err.contains("NOT sent") && err.contains("not confirmed"));
        assert_eq!(sends(&f), 1, "only the approval went out");
    }

    #[test]
    fn a_failed_approval_aborts_the_swap() {
        let f = setup_with(
            mainnet()
                .rpc("eth_getTransactionReceipt", r#"{"status":"0x0"}"#)
                .get(OO_SWAP, Ok(Oo::usdc_to_eth().json())),
            true,
        );
        assert!(
            f.service
                .swap_tokens("w", PW, &params())
                .unwrap_err()
                .contains("failed on-chain")
        );
        assert_eq!(sends(&f), 1);
    }

    #[test]
    fn a_swap_that_would_revert_sends_nothing() {
        let router = mainnet()
            .rpc("eth_call:dd62ed3e", &word(25_000_000))
            .rpc_error(
                "eth_estimateGas",
                3,
                "execution reverted: Too little received",
                None,
            )
            .get(OO_SWAP, Ok(Oo::usdc_to_eth().json()));
        let f = setup_with(router, true);
        assert!(
            f.service
                .swap_tokens("w", PW, &params())
                .unwrap_err()
                .contains("Too little received")
        );
        assert_eq!(sends(&f), 0);
    }

    #[test]
    fn an_api_block_or_wrong_password_sends_nothing() {
        let f = setup_with(
            mainnet().rpc("eth_call:dd62ed3e", &word(25_000_000)).get(
                OO_SWAP,
                Err("HTTP status 403 (blocked by Cloudflare bot protection)".into()),
            ),
            true,
        );
        assert!(
            f.service
                .swap_tokens("w", PW, &params())
                .unwrap_err()
                .contains("bot protection")
        );
        assert!(
            f.service
                .swap_tokens("w", "wrong-password", &params())
                .is_err()
        );
        assert_eq!(sends(&f), 0);
    }

    #[test]
    fn the_swap_goes_to_a_different_receiver_when_asked() {
        let mut oo = Oo::usdc_to_eth();
        oo.receiver = to_checksum(&[0x22; 20]);
        let f = setup(&oo, true);
        let mut p = params();
        p.to_address = Some(to_checksum(&[0x22; 20]));
        let out = f.service.swap_tokens("w", PW, &p).unwrap();
        // A host that ignores the requested recipient is refused and nothing is sent.
        let f2 = setup(&Oo::usdc_to_eth(), true);
        let err = f2.service.swap_tokens("w", PW, &p).unwrap_err();
        assert!(err.contains("different address"), "{err}");
        assert_eq!(sends(&f2), 0);
        assert_eq!(
            text(&out, &["to", "recipient"]),
            Some(to_checksum(&[0x22; 20]).as_str())
        );
        assert!(oo_requests(&f).iter().all(|u| u.contains("sender=")));
    }

    #[test]
    fn a_wallet_without_the_chain_cannot_swap_on_it() {
        let f = setup(&Oo::usdc_to_eth(), true);
        let mut p = params();
        p.from_chain_id = 8453;
        p.to_chain_id = 8453;
        assert!(
            f.service
                .swap_tokens("w", PW, &p)
                .unwrap_err()
                .contains("add_network")
        );
    }
}
