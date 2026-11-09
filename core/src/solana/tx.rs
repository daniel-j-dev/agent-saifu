//! Solana transactions: the wire format (legacy and version 0 messages), building, and signing.
//!
//! A transaction is a list of signatures followed by a message. The message lists every account
//! it touches, grouped by role, and the instructions that act on them. Signatures are Ed25519
//! signatures over the message bytes exactly as they appear on the wire.

use super::address::{self, Pubkey};
use super::wallet::SolanaVault;
use crate::crypto::{base58, base64};

pub const SIGNATURE_LEN: usize = 64;
/// The largest serialized transaction the network accepts (one UDP packet).
pub const MAX_TX_BYTES: usize = 1232;
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
pub const LAMPORTS_PER_SOL: u64 = 1_000_000_000;
pub const SOL_DECIMALS: u32 = 9;
pub const BASE_FEE_PER_SIGNATURE: u64 = 5_000;
/// Compute units a transaction gets per instruction unless it sets a limit, and the most it can ask for.
pub const DEFAULT_UNITS_PER_INSTRUCTION: u64 = 200_000;
pub const MAX_COMPUTE_UNITS: u64 = 1_400_000;
const MAX_ACCOUNTS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    Legacy,
    V0,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub required_signatures: u8,
    pub readonly_signed: u8,
    pub readonly_unsigned: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledInstruction {
    pub program_id_index: u8,
    pub accounts: Vec<u8>,
    pub data: Vec<u8>,
}

/// Accounts a version 0 message loads from an on-chain address lookup table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressLookup {
    pub table: Pubkey,
    pub writable: Vec<u8>,
    pub readonly: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub version: Version,
    pub header: Header,
    pub account_keys: Vec<Pubkey>,
    pub recent_blockhash: [u8; 32],
    pub instructions: Vec<CompiledInstruction>,
    pub lookups: Vec<AddressLookup>,
    /// The message exactly as it was read or built; this is what gets signed.
    raw: Vec<u8>,
}

// ---- reading ---------------------------------------------------------------------------------

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Reader<'a> {
        Reader { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("length overflow")?;
        let slice = self
            .data
            .get(self.pos..end)
            .ok_or("unexpected end of data")?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    /// Solana's compact-u16: 1 to 3 bytes, 7 bits each, least significant first. Only the
    /// shortest encoding is accepted.
    fn short_len(&mut self) -> Result<usize, String> {
        let mut value: u32 = 0;
        for shift in 0..3 {
            let byte = self.u8()?;
            value |= u32::from(byte & 0x7f) << (7 * shift);
            if byte & 0x80 == 0 {
                if shift > 0 && byte == 0 {
                    return Err("non-canonical length".to_string());
                }
                return u16::try_from(value)
                    .map(usize::from)
                    .map_err(|_| "length too large".to_string());
            }
        }
        Err("length is too long".to_string())
    }

    fn bytes(&mut self) -> Result<Vec<u8>, String> {
        let len = self.short_len()?;
        Ok(self.take(len)?.to_vec())
    }

    fn finished(&self) -> bool {
        self.pos == self.data.len()
    }
}

pub fn encode_short_len(len: usize, out: &mut Vec<u8>) {
    let mut value = len as u32;
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

impl Message {
    /// Parses a message (legacy or version 0). All bytes must be used.
    pub fn parse(bytes: &[u8]) -> Result<Message, String> {
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err("message is too large".to_string());
        }
        let mut r = Reader::new(bytes);
        let first = r.u8()?;
        let (version, required) = if first & 0x80 != 0 {
            if first & 0x7f != 0 {
                return Err(format!(
                    "unsupported message version {}; only legacy and version 0 are supported",
                    first & 0x7f
                ));
            }
            (Version::V0, r.u8()?)
        } else {
            (Version::Legacy, first)
        };
        let header = Header {
            required_signatures: required,
            readonly_signed: r.u8()?,
            readonly_unsigned: r.u8()?,
        };
        let key_count = r.short_len()?;
        if key_count > MAX_ACCOUNTS {
            return Err("too many accounts".to_string());
        }
        let mut account_keys = Vec::with_capacity(key_count);
        for _ in 0..key_count {
            account_keys.push(r.array::<32>()?);
        }
        let recent_blockhash = r.array::<32>()?;
        let instruction_count = r.short_len()?;
        let mut instructions = Vec::with_capacity(instruction_count.min(64));
        for _ in 0..instruction_count {
            instructions.push(CompiledInstruction {
                program_id_index: r.u8()?,
                accounts: r.bytes()?,
                data: r.bytes()?,
            });
        }
        let mut lookups = vec![];
        if version == Version::V0 {
            for _ in 0..r.short_len()? {
                lookups.push(AddressLookup {
                    table: r.array::<32>()?,
                    writable: r.bytes()?,
                    readonly: r.bytes()?,
                });
            }
        }
        if !r.finished() {
            return Err("unexpected data after the message".to_string());
        }
        let message = Message {
            version,
            header,
            account_keys,
            recent_blockhash,
            instructions,
            lookups,
            raw: bytes.to_vec(),
        };
        message.validate()?;
        Ok(message)
    }

    fn validate(&self) -> Result<(), String> {
        let keys = self.account_keys.len();
        let required = usize::from(self.header.required_signatures);
        if required == 0 {
            return Err("the message needs at least one signer".to_string());
        }
        if required > keys {
            return Err("the message has more signers than accounts".to_string());
        }
        if usize::from(self.header.readonly_signed) >= required {
            return Err("the fee payer must be a writable signer".to_string());
        }
        if usize::from(self.header.readonly_unsigned) > keys - required {
            return Err("the message has more read-only accounts than accounts".to_string());
        }
        let total = self.total_accounts();
        if total > MAX_ACCOUNTS {
            return Err("too many accounts".to_string());
        }
        for ix in &self.instructions {
            let program = usize::from(ix.program_id_index);
            if program >= total {
                return Err("an instruction names a program that is not in the message".to_string());
            }
            if program == 0 {
                return Err("an instruction cannot run the fee payer as a program".to_string());
            }
            if ix.accounts.iter().any(|a| usize::from(*a) >= total) {
                return Err(
                    "an instruction names an account that is not in the message".to_string()
                );
            }
        }
        Ok(())
    }

    /// Accounts loaded from lookup tables (writable ones first, then read-only ones).
    pub fn loaded_account_count(&self) -> usize {
        self.lookups
            .iter()
            .map(|l| l.writable.len() + l.readonly.len())
            .sum()
    }

    pub fn total_accounts(&self) -> usize {
        self.account_keys.len() + self.loaded_account_count()
    }

    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    pub fn fee_payer(&self) -> &Pubkey {
        &self.account_keys[0]
    }

    pub fn signer_keys(&self) -> &[Pubkey] {
        &self.account_keys[..usize::from(self.header.required_signatures)]
    }

    pub fn signer_index(&self, key: &Pubkey) -> Option<usize> {
        self.signer_keys().iter().position(|k| k == key)
    }

    /// Whether the account at `index` (static keys, then loaded ones) is writable.
    pub fn is_writable(&self, index: usize) -> bool {
        let keys = self.account_keys.len();
        let required = usize::from(self.header.required_signatures);
        if index < required {
            index < required - usize::from(self.header.readonly_signed)
        } else if index < keys {
            index < keys - usize::from(self.header.readonly_unsigned)
        } else {
            let writable_loaded: usize = self.lookups.iter().map(|l| l.writable.len()).sum();
            index < keys + writable_loaded
        }
    }

    /// The version 0 lookups as `(table, index)` pairs in account order, writable ones first.
    pub fn loaded_slots(&self) -> Vec<(Pubkey, u8)> {
        let mut out = vec![];
        for lookup in &self.lookups {
            out.extend(lookup.writable.iter().map(|i| (lookup.table, *i)));
        }
        for lookup in &self.lookups {
            out.extend(lookup.readonly.iter().map(|i| (lookup.table, *i)));
        }
        out
    }

    pub fn program_id(&self, ix: &CompiledInstruction, accounts: &[Pubkey]) -> Option<Pubkey> {
        accounts.get(usize::from(ix.program_id_index)).copied()
    }
}

/// Every account of a message in index order, given the contents of the lookup tables it uses.
pub fn resolve_accounts(
    message: &Message,
    tables: &std::collections::HashMap<Pubkey, Vec<Pubkey>>,
) -> Result<Vec<Pubkey>, String> {
    let mut accounts = message.account_keys.clone();
    for (table, index) in message.loaded_slots() {
        let entries = tables
            .get(&table)
            .ok_or("an address lookup table could not be read")?;
        let key = entries
            .get(usize::from(index))
            .ok_or("the message names an entry that its address lookup table does not have")?;
        accounts.push(*key);
    }
    Ok(accounts)
}

// ---- transactions --------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    pub signatures: Vec<[u8; 64]>,
    pub message: Message,
}

impl Transaction {
    /// An unsigned transaction: one empty signature slot per required signer.
    pub fn unsigned(message: Message) -> Transaction {
        Transaction {
            signatures: vec![[0u8; 64]; usize::from(message.header.required_signatures)],
            message,
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Transaction, String> {
        if bytes.len() > MAX_TX_BYTES {
            return Err(format!(
                "the transaction is {} bytes; Solana transactions are at most {MAX_TX_BYTES}",
                bytes.len()
            ));
        }
        let mut r = Reader::new(bytes);
        let count = r.short_len()?;
        let mut signatures = Vec::with_capacity(count.min(16));
        for _ in 0..count {
            signatures.push(r.array::<64>()?);
        }
        let message = Message::parse(&bytes[r.pos..])?;
        if signatures.len() != usize::from(message.header.required_signatures) {
            return Err(format!(
                "the transaction has {} signature slots but its message needs {}",
                signatures.len(),
                message.header.required_signatures
            ));
        }
        Ok(Transaction {
            signatures,
            message,
        })
    }

    /// Standard base64, as dapps and RPC nodes carry transactions.
    pub fn from_base64(text: &str) -> Result<Transaction, String> {
        let trimmed = text.trim();
        if trimmed.len() > MAX_TX_BYTES * 2 {
            return Err("the transaction is too large".to_string());
        }
        let bytes = base64::decode(trimmed)
            .map_err(|_| "the transaction is not valid base64".to_string())?;
        Self::from_bytes(&bytes)
    }

    /// Accepts base64 or, for dapps that send it that way, base58.
    pub fn from_text(text: &str) -> Result<Transaction, String> {
        Self::from_base64(text).or_else(|e| {
            base58::decode(text.trim())
                .ok()
                .and_then(|bytes| Self::from_bytes(&bytes).ok())
                .ok_or(e)
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![];
        encode_short_len(self.signatures.len(), &mut out);
        for signature in &self.signatures {
            out.extend_from_slice(signature);
        }
        out.extend_from_slice(self.message.raw());
        out
    }

    pub fn to_base64(&self) -> String {
        base64::encode(&self.to_bytes())
    }

    pub fn is_signed(&self, index: usize) -> bool {
        self.signatures
            .get(index)
            .is_some_and(|s| s.iter().any(|b| *b != 0))
    }

    /// Signs with `vault`'s key in its signer slot. Fails when the wallet is not a signer.
    pub fn sign(&mut self, vault: &SolanaVault) -> Result<[u8; 64], String> {
        let public = vault.public_key()?;
        let slot = self
            .message
            .signer_index(&public)
            .ok_or("this wallet is not one of the transaction's signers")?;
        let signature = vault.sign(self.message.raw())?;
        self.signatures[slot] = signature;
        Ok(signature)
    }

    /// The transaction's ID: the fee payer's signature, once it has signed.
    pub fn id(&self) -> Option<String> {
        self.is_signed(0)
            .then(|| base58::encode(&self.signatures[0]))
    }

    /// Signers other than `owner` whose signature slot is still empty.
    pub fn missing_signers(&self, owner: &Pubkey) -> Vec<Pubkey> {
        self.message
            .signer_keys()
            .iter()
            .enumerate()
            .filter(|(i, key)| *key != owner && !self.is_signed(*i))
            .map(|(_, key)| *key)
            .collect()
    }
}

/// Whether a message is built on a durable nonce: its first instruction advances a nonce
/// account, and its "blockhash" is the nonce value rather than a recent blockhash.
pub fn uses_durable_nonce(message: &Message, accounts: &[Pubkey]) -> bool {
    message.instructions.first().is_some_and(|ix| {
        message.program_id(ix, accounts) == Some(address::id(address::SYSTEM_PROGRAM))
            && ix.data.get(..4) == Some(&[4, 0, 0, 0])
    })
}

/// Whether bytes could be a Solana transaction or message. Signing such bytes as a "message"
/// would hand over a valid transaction, so message signing refuses them (as Phantom does).
pub fn looks_like_transaction(bytes: &[u8]) -> bool {
    Message::parse(bytes).is_ok() || Transaction::from_bytes(bytes).is_ok()
}

// ---- building ----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountMeta {
    pub pubkey: Pubkey,
    pub is_signer: bool,
    pub is_writable: bool,
}

impl AccountMeta {
    pub fn writable(pubkey: Pubkey, is_signer: bool) -> AccountMeta {
        AccountMeta {
            pubkey,
            is_signer,
            is_writable: true,
        }
    }

    pub fn readonly(pubkey: Pubkey, is_signer: bool) -> AccountMeta {
        AccountMeta {
            pubkey,
            is_signer,
            is_writable: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub program_id: Pubkey,
    pub accounts: Vec<AccountMeta>,
    pub data: Vec<u8>,
}

struct KeyRole {
    key: Pubkey,
    signer: bool,
    writable: bool,
}

fn note_key(roles: &mut Vec<KeyRole>, key: &Pubkey, signer: bool, writable: bool) {
    match roles.iter_mut().find(|r| r.key == *key) {
        Some(role) => {
            role.signer |= signer;
            role.writable |= writable;
        }
        None => roles.push(KeyRole {
            key: *key,
            signer,
            writable,
        }),
    }
}

/// Builds a legacy message the way Solana's SDKs do: the fee payer first, then accounts ordered
/// writable signers, read-only signers, writable others, read-only others.
pub fn compile_legacy(
    payer: &Pubkey,
    instructions: &[Instruction],
    recent_blockhash: [u8; 32],
) -> Result<Message, String> {
    compile(payer, instructions, recent_blockhash, None).map(|(message, _)| message)
}

/// An address lookup table and the addresses it holds, in order.
pub type LookupTable = (Pubkey, Vec<Pubkey>);

/// Builds a version 0 message. Accounts that are neither signers nor invoked programs are loaded
/// from the first of `tables` that holds them. Returns the message and the full list of accounts
/// it resolves to (static keys, then loaded writable ones, then loaded read-only ones), so the
/// caller can compare it with what the tables really contain on chain.
pub fn compile_v0(
    payer: &Pubkey,
    instructions: &[Instruction],
    recent_blockhash: [u8; 32],
    tables: &[LookupTable],
) -> Result<(Message, Vec<Pubkey>), String> {
    compile(payer, instructions, recent_blockhash, Some(tables))
}

fn compile(
    payer: &Pubkey,
    instructions: &[Instruction],
    recent_blockhash: [u8; 32],
    tables: Option<&[LookupTable]>,
) -> Result<(Message, Vec<Pubkey>), String> {
    let v0 = tables.is_some();
    let tables = tables.unwrap_or(&[]);
    let mut roles: Vec<KeyRole> = vec![];
    note_key(&mut roles, payer, true, true);
    for ix in instructions {
        note_key(&mut roles, &ix.program_id, false, false);
        for meta in &ix.accounts {
            note_key(&mut roles, &meta.pubkey, meta.is_signer, meta.is_writable);
        }
    }
    let (payer_role, rest): (Vec<KeyRole>, Vec<KeyRole>) =
        roles.into_iter().partition(|r| r.key == *payer);
    let invoked: Vec<Pubkey> = instructions.iter().map(|ix| ix.program_id).collect();
    // (table index, position in the table, key, writable) for every account loaded from a table.
    let mut loaded: Vec<(usize, u8, Pubkey, bool)> = vec![];
    let mut kept: Vec<&KeyRole> = vec![];
    for role in &rest {
        let slot = if role.signer || invoked.contains(&role.key) {
            None
        } else {
            tables.iter().enumerate().find_map(|(t, (_, addresses))| {
                addresses
                    .iter()
                    .position(|a| *a == role.key)
                    .and_then(|p| u8::try_from(p).ok())
                    .map(|p| (t, p))
            })
        };
        match slot {
            Some((t, p)) => loaded.push((t, p, role.key, role.writable)),
            None => kept.push(role),
        }
    }
    let group = |signer: bool, writable: bool| -> Vec<&KeyRole> {
        kept.iter()
            .copied()
            .filter(|r| r.signer == signer && r.writable == writable)
            .collect()
    };
    let mut ordered: Vec<&KeyRole> = payer_role.iter().collect();
    let signed_writable = group(true, true);
    let signed_readonly = group(true, false);
    let unsigned_writable = group(false, true);
    let unsigned_readonly = group(false, false);
    ordered.extend(&signed_writable);
    ordered.extend(&signed_readonly);
    ordered.extend(&unsigned_writable);
    ordered.extend(&unsigned_readonly);
    if ordered.len() > MAX_ACCOUNTS {
        return Err("too many accounts".to_string());
    }
    let keys: Vec<Pubkey> = ordered.iter().map(|r| r.key).collect();
    // Loaded accounts follow the static keys: every table's writable ones, then every table's
    // read-only ones.
    let loaded_in_order = |writable: bool| -> Vec<Pubkey> {
        (0..tables.len())
            .flat_map(|t| {
                loaded
                    .iter()
                    .filter(move |l| l.0 == t && l.3 == writable)
                    .map(|l| l.2)
            })
            .collect()
    };
    let mut full = keys.clone();
    full.extend(loaded_in_order(true));
    full.extend(loaded_in_order(false));
    if full.len() > MAX_ACCOUNTS {
        return Err("too many accounts".to_string());
    }
    let index_of = |key: &Pubkey| -> Result<u8, String> {
        full.iter()
            .position(|k| k == key)
            .and_then(|i| u8::try_from(i).ok())
            .ok_or_else(|| "internal error: account missing".to_string())
    };
    let mut compiled = vec![];
    for ix in instructions {
        compiled.push(CompiledInstruction {
            program_id_index: index_of(&ix.program_id)?,
            accounts: ix
                .accounts
                .iter()
                .map(|m| index_of(&m.pubkey))
                .collect::<Result<Vec<u8>, String>>()?,
            data: ix.data.clone(),
        });
    }
    let required = payer_role.len() + signed_writable.len() + signed_readonly.len();
    let header = Header {
        required_signatures: u8::try_from(required).map_err(|_| "too many signers")?,
        readonly_signed: u8::try_from(signed_readonly.len()).map_err(|_| "too many signers")?,
        readonly_unsigned: u8::try_from(unsigned_readonly.len())
            .map_err(|_| "too many accounts")?,
    };
    let mut raw = vec![];
    if v0 {
        raw.push(0x80);
    }
    raw.extend_from_slice(&[
        header.required_signatures,
        header.readonly_signed,
        header.readonly_unsigned,
    ]);
    encode_short_len(keys.len(), &mut raw);
    for key in &keys {
        raw.extend_from_slice(key);
    }
    raw.extend_from_slice(&recent_blockhash);
    encode_short_len(compiled.len(), &mut raw);
    for ix in &compiled {
        raw.push(ix.program_id_index);
        encode_short_len(ix.accounts.len(), &mut raw);
        raw.extend_from_slice(&ix.accounts);
        encode_short_len(ix.data.len(), &mut raw);
        raw.extend_from_slice(&ix.data);
    }
    if v0 {
        let used: Vec<usize> = (0..tables.len())
            .filter(|t| loaded.iter().any(|l| l.0 == *t))
            .collect();
        encode_short_len(used.len(), &mut raw);
        for t in used {
            raw.extend_from_slice(&tables[t].0);
            for writable in [true, false] {
                let indexes: Vec<u8> = loaded
                    .iter()
                    .filter(|l| l.0 == t && l.3 == writable)
                    .map(|l| l.1)
                    .collect();
                encode_short_len(indexes.len(), &mut raw);
                raw.extend_from_slice(&indexes);
            }
        }
    }
    Ok((Message::parse(&raw)?, full))
}

pub fn system_transfer(from: &Pubkey, to: &Pubkey, lamports: u64) -> Instruction {
    let mut data = 2u32.to_le_bytes().to_vec();
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: address::id(address::SYSTEM_PROGRAM),
        accounts: vec![
            AccountMeta::writable(*from, true),
            AccountMeta::writable(*to, false),
        ],
        data,
    }
}

/// SPL Token `TransferChecked`: the token program verifies the mint and its decimals.
pub fn token_transfer_checked(
    token_program: &Pubkey,
    source: &Pubkey,
    mint: &Pubkey,
    destination: &Pubkey,
    owner: &Pubkey,
    amount: u64,
    decimals: u8,
) -> Instruction {
    let mut data = vec![12u8];
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(decimals);
    Instruction {
        program_id: *token_program,
        accounts: vec![
            AccountMeta::writable(*source, false),
            AccountMeta::readonly(*mint, false),
            AccountMeta::writable(*destination, false),
            AccountMeta::readonly(*owner, true),
        ],
        data,
    }
}

/// Creates the recipient's associated token account unless it already exists.
pub fn create_associated_token_account_idempotent(
    payer: &Pubkey,
    associated: &Pubkey,
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: address::id(address::ASSOCIATED_TOKEN_PROGRAM),
        accounts: vec![
            AccountMeta::writable(*payer, true),
            AccountMeta::writable(*associated, false),
            AccountMeta::readonly(*owner, false),
            AccountMeta::readonly(*mint, false),
            AccountMeta::readonly(address::id(address::SYSTEM_PROGRAM), false),
            AccountMeta::readonly(*token_program, false),
        ],
        data: vec![1],
    }
}

pub fn set_compute_unit_limit(units: u32) -> Instruction {
    let mut data = vec![2u8];
    data.extend_from_slice(&units.to_le_bytes());
    Instruction {
        program_id: address::id(address::COMPUTE_BUDGET_PROGRAM),
        accounts: vec![],
        data,
    }
}

/// The priority fee in micro-lamports per compute unit.
pub fn set_compute_unit_price(micro_lamports: u64) -> Instruction {
    let mut data = vec![3u8];
    data.extend_from_slice(&micro_lamports.to_le_bytes());
    Instruction {
        program_id: address::id(address::COMPUTE_BUDGET_PROGRAM),
        accounts: vec![],
        data,
    }
}

// ---- fees ----------------------------------------------------------------------------------------

/// The compute-budget settings a message carries: `(unit limit, micro-lamports per unit)`.
pub fn compute_budget(message: &Message, accounts: &[Pubkey]) -> (Option<u32>, Option<u64>) {
    let budget_program = address::id(address::COMPUTE_BUDGET_PROGRAM);
    let (mut limit, mut price) = (None, None);
    for ix in &message.instructions {
        if message.program_id(ix, accounts) != Some(budget_program) {
            continue;
        }
        match ix.data.first() {
            Some(2) if ix.data.len() == 5 => {
                limit = Some(u32::from_le_bytes(
                    ix.data[1..5].try_into().unwrap_or([0; 4]),
                ));
            }
            Some(3) if ix.data.len() == 9 => {
                price = Some(u64::from_le_bytes(
                    ix.data[1..9].try_into().unwrap_or([0; 8]),
                ));
            }
            _ => {}
        }
    }
    (limit, price)
}

/// The priority fee in lamports: limit times price, rounded up. Without a stated limit the
/// network assumes 200,000 units per instruction (at most 1.4 million).
pub fn priority_fee_lamports(message: &Message, accounts: &[Pubkey]) -> u64 {
    let (limit, price) = compute_budget(message, accounts);
    let Some(price) = price else { return 0 };
    let budget_program = address::id(address::COMPUTE_BUDGET_PROGRAM);
    let units = match limit {
        Some(l) => u64::from(l),
        None => {
            let others = message
                .instructions
                .iter()
                .filter(|ix| message.program_id(ix, accounts) != Some(budget_program))
                .count() as u64;
            (others * DEFAULT_UNITS_PER_INSTRUCTION).min(MAX_COMPUTE_UNITS)
        }
    };
    let micro = u128::from(units) * u128::from(price);
    u64::try_from(micro.div_ceil(1_000_000)).unwrap_or(u64::MAX)
}

/// Base fee (per signature) plus priority fee, in lamports.
pub fn fee_estimate(message: &Message, accounts: &[Pubkey]) -> u64 {
    u64::from(message.header.required_signatures)
        .saturating_mul(BASE_FEE_PER_SIGNATURE)
        .saturating_add(priority_fee_lamports(message, accounts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;
    use crate::solana::network::SolanaNetwork;

    fn key(n: u8) -> Pubkey {
        [n; 32]
    }

    fn vault() -> SolanaVault {
        SolanaVault::from_private_key(&"11".repeat(32), SolanaNetwork::Mainnet).unwrap()
    }

    fn owner() -> Pubkey {
        vault().public_key().unwrap()
    }

    fn transfer_message(lamports: u64) -> Message {
        compile_legacy(
            &owner(),
            &[system_transfer(&owner(), &key(2), lamports)],
            [9u8; 32],
        )
        .unwrap()
    }

    #[test]
    fn short_lengths_round_trip_and_reject_odd_encodings() {
        for len in [0usize, 1, 127, 128, 255, 16_383, 16_384, 65_535] {
            let mut out = vec![];
            encode_short_len(len, &mut out);
            let mut r = Reader::new(&out);
            assert_eq!(r.short_len().unwrap(), len);
            assert!(r.finished());
        }
        let mut out = vec![];
        encode_short_len(128, &mut out);
        assert_eq!(out, [0x80, 0x01]);
        // 0x80 0x00 is a long way of writing zero.
        assert!(Reader::new(&[0x80, 0x00]).short_len().is_err());
        assert!(Reader::new(&[0xff, 0xff, 0x7f]).short_len().is_err());
        assert!(Reader::new(&[0x80]).short_len().is_err());
        assert!(Reader::new(&[0x80, 0x80, 0x80, 0x01]).short_len().is_err());
    }

    // A transfer of 1 SOL from the key 0x11.. to key 0x02.., built and signed by an independent
    // implementation (Python: RFC 8032 Ed25519 and the documented message layout).
    #[test]
    fn matches_an_independently_built_and_signed_transfer() {
        let message = transfer_message(1_000_000_000);
        assert_eq!(hex::encode(message.raw()), EXPECTED_MESSAGE);
        let mut tx = Transaction::unsigned(message);
        let signature = tx.sign(&vault()).unwrap();
        assert_eq!(hex::encode(&signature), EXPECTED_SIGNATURE);
        assert_eq!(tx.id().unwrap(), base58::encode(&signature));
        let wire = tx.to_bytes();
        assert_eq!(wire.len(), 1 + 64 + tx.message.raw().len());
        assert_eq!(Transaction::from_bytes(&wire).unwrap(), tx);
        assert_eq!(Transaction::from_base64(&tx.to_base64()).unwrap(), tx);
    }

    const EXPECTED_MESSAGE: &str = "01000103d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c977873702020202020202020202020202020202020202020202020202020202020202020000000000000000000000000000000000000000000000000000000000000000090909090909090909090909090909090909090909090909090909090909090901020200010c0200000000ca9a3b00000000";
    const EXPECTED_SIGNATURE: &str = "1909b7fb791e12ad381746fba177d4f83811938a004645a1465959cba2ccfa23052ac9accdbbe73c862e89388d58a4b5efc6efbdab421d865bdad741d5339402";

    #[test]
    fn compile_orders_accounts_by_role() {
        let payer = key(1);
        let ix = Instruction {
            program_id: key(50),
            accounts: vec![
                AccountMeta::readonly(key(10), false),
                AccountMeta::writable(key(11), false),
                AccountMeta::readonly(key(12), true),
                AccountMeta::writable(key(13), true),
                AccountMeta::writable(payer, true),
            ],
            data: vec![1, 2, 3],
        };
        let m = compile_legacy(&payer, &[ix], [0u8; 32]).unwrap();
        assert_eq!(
            m.header,
            Header {
                required_signatures: 3,
                readonly_signed: 1,
                readonly_unsigned: 2
            }
        );
        assert_eq!(
            m.account_keys,
            vec![key(1), key(13), key(12), key(11), key(50), key(10)]
        );
        assert!(m.is_writable(0) && m.is_writable(1));
        assert!(!m.is_writable(2) && m.is_writable(3));
        assert!(!m.is_writable(4) && !m.is_writable(5));
        assert_eq!(m.instructions[0].program_id_index, 4);
        assert_eq!(m.instructions[0].accounts, vec![5, 3, 2, 1, 0]);
        assert_eq!(Message::parse(m.raw()).unwrap(), m);
    }

    #[test]
    fn duplicate_accounts_merge_their_roles() {
        let payer = key(1);
        let a = Instruction {
            program_id: key(50),
            accounts: vec![AccountMeta::readonly(key(7), false)],
            data: vec![],
        };
        let b = Instruction {
            program_id: key(50),
            accounts: vec![AccountMeta::writable(key(7), false)],
            data: vec![],
        };
        let m = compile_legacy(&payer, &[a, b], [0u8; 32]).unwrap();
        assert_eq!(m.account_keys, vec![key(1), key(7), key(50)]);
        assert!(m.is_writable(1));
        assert_eq!(m.header.readonly_unsigned, 1);
    }

    #[test]
    fn parses_version_0_messages_with_lookups() {
        let mut raw = vec![0x80, 1, 0, 1];
        encode_short_len(2, &mut raw);
        raw.extend_from_slice(&key(1));
        raw.extend_from_slice(&key(2));
        raw.extend_from_slice(&[5u8; 32]);
        encode_short_len(1, &mut raw);
        // program index 1, accounts [0, 2, 3], data [7]
        raw.extend_from_slice(&[1, 3, 0, 2, 3, 1, 7]);
        encode_short_len(1, &mut raw);
        raw.extend_from_slice(&key(60));
        raw.extend_from_slice(&[1, 4]);
        raw.extend_from_slice(&[1, 9]);
        let m = Message::parse(&raw).unwrap();
        assert_eq!(m.version, Version::V0);
        assert_eq!(m.total_accounts(), 4);
        assert_eq!(m.loaded_slots(), vec![(key(60), 4), (key(60), 9)]);
        assert!(m.is_writable(2) && !m.is_writable(3));
        let tables = std::collections::HashMap::from([(
            key(60),
            (0..16u8).map(|i| key(100 + i)).collect::<Vec<_>>(),
        )]);
        let accounts = resolve_accounts(&m, &tables).unwrap();
        assert_eq!(accounts, vec![key(1), key(2), key(104), key(109)]);
        assert!(resolve_accounts(&m, &std::collections::HashMap::new()).is_err());
        let short = std::collections::HashMap::from([(key(60), vec![key(1)])]);
        assert!(resolve_accounts(&m, &short).is_err());
    }

    #[test]
    fn rejects_malformed_messages() {
        let good = transfer_message(5);
        let raw = good.raw().to_vec();
        let mut trailing = raw.clone();
        trailing.push(0);
        let mut truncated = raw.clone();
        truncated.pop();
        let mut no_signers = raw.clone();
        no_signers[0] = 0;
        let mut readonly_payer = raw.clone();
        readonly_payer[1] = 1;
        let mut too_many_readonly = raw.clone();
        too_many_readonly[2] = 9;
        let mut future = raw.clone();
        future.insert(0, 0x81);
        // an instruction naming account index 9 of 3
        let mut bad_index = raw.clone();
        let program_pos = raw.len() - (1 + 1 + 2 + 1 + 12);
        bad_index[program_pos] = 9;
        for (name, bad) in [
            ("trailing", trailing),
            ("truncated", truncated),
            ("no signers", no_signers),
            ("readonly payer", readonly_payer),
            ("too many readonly", too_many_readonly),
            ("future version", future),
            ("bad index", bad_index),
            ("empty", vec![]),
        ] {
            assert!(Message::parse(&bad).is_err(), "{name}");
        }
    }

    #[test]
    fn a_program_cannot_be_the_fee_payer() {
        let mut raw = transfer_message(5).raw().to_vec();
        let program_pos = raw.len() - (1 + 1 + 2 + 1 + 12);
        raw[program_pos] = 0;
        assert!(Message::parse(&raw).is_err());
    }

    #[test]
    fn transactions_need_matching_signature_slots_and_a_sane_size() {
        let m = transfer_message(5);
        let mut wire = vec![2u8];
        wire.extend_from_slice(&[0u8; 128]);
        wire.extend_from_slice(m.raw());
        assert!(Transaction::from_bytes(&wire).is_err());
        assert!(Transaction::from_bytes(&vec![0u8; MAX_TX_BYTES + 1]).is_err());
        assert!(Transaction::from_base64("not base64 !!").is_err());
        assert!(Transaction::from_text("???").is_err());
        let tx = Transaction::unsigned(m);
        assert_eq!(
            Transaction::from_text(&base58::encode(&tx.to_bytes())).unwrap(),
            tx
        );
    }

    #[test]
    fn signing_needs_the_wallet_to_be_a_signer_and_keeps_other_slots() {
        let other = key(77);
        let ix = Instruction {
            program_id: key(50),
            accounts: vec![AccountMeta::readonly(other, true)],
            data: vec![],
        };
        let m = compile_legacy(&other, &[ix], [0u8; 32]).unwrap();
        let mut tx = Transaction::unsigned(m);
        assert!(tx.sign(&vault()).is_err());

        let both = Instruction {
            program_id: key(50),
            accounts: vec![
                AccountMeta::readonly(owner(), true),
                AccountMeta::readonly(other, true),
            ],
            data: vec![],
        };
        let m = compile_legacy(&other, &[both], [0u8; 32]).unwrap();
        let mut tx = Transaction::unsigned(m);
        tx.signatures[0] = [5u8; 64];
        let signature = tx.sign(&vault()).unwrap();
        assert_eq!(tx.signatures[0], [5u8; 64]);
        assert_eq!(tx.signatures[1], signature);
        assert!(crate::crypto::ed25519::verify(
            &owner(),
            tx.message.raw(),
            &signature
        ));
        assert!(tx.missing_signers(&owner()).is_empty());
        let mut unsigned_other = Transaction::unsigned(tx.message.clone());
        unsigned_other.sign(&vault()).unwrap();
        assert_eq!(unsigned_other.missing_signers(&owner()), vec![other]);
        assert!(unsigned_other.id().is_none());
    }

    #[test]
    fn transaction_looking_bytes_are_recognised() {
        let tx = Transaction::unsigned(transfer_message(5));
        assert!(looks_like_transaction(tx.message.raw()));
        assert!(looks_like_transaction(&tx.to_bytes()));
        assert!(!looks_like_transaction(b"Sign in to example.com"));
        assert!(!looks_like_transaction(&[]));
        assert!(!looks_like_transaction(&[0xff; 80]));
    }

    #[test]
    fn durable_nonce_transactions_are_recognised() {
        let advance = Instruction {
            program_id: address::id(address::SYSTEM_PROGRAM),
            accounts: vec![
                AccountMeta::writable(key(5), false),
                AccountMeta::readonly(key(6), false),
                AccountMeta::readonly(owner(), true),
            ],
            data: vec![4, 0, 0, 0],
        };
        let m = compile_legacy(
            &owner(),
            &[advance, system_transfer(&owner(), &key(2), 1)],
            [0u8; 32],
        )
        .unwrap();
        assert!(uses_durable_nonce(&m, &m.account_keys));
        let plain = transfer_message(1);
        assert!(!uses_durable_nonce(&plain, &plain.account_keys));
    }

    #[test]
    fn instruction_data_layouts() {
        let t = system_transfer(&key(1), &key(2), 0x0102);
        assert_eq!(t.data, [2, 0, 0, 0, 2, 1, 0, 0, 0, 0, 0, 0]);
        let c = token_transfer_checked(&key(9), &key(3), &key(4), &key(5), &key(1), 258, 6);
        assert_eq!(c.data, [12, 2, 1, 0, 0, 0, 0, 0, 0, 6]);
        assert_eq!(c.accounts.len(), 4);
        assert!(c.accounts[3].is_signer && !c.accounts[3].is_writable);
        assert!(c.accounts[0].is_writable && c.accounts[2].is_writable);
        let a =
            create_associated_token_account_idempotent(&key(1), &key(2), &key(3), &key(4), &key(9));
        assert_eq!(a.data, [1]);
        assert_eq!(a.accounts.len(), 6);
        assert_eq!(set_compute_unit_limit(0x0102_0304).data, [2, 4, 3, 2, 1]);
        assert_eq!(set_compute_unit_price(5).data, [3, 5, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn version_0_messages_load_accounts_from_tables() {
        // key(10) is a program, key(11) a writable account, key(12) a read-only one, key(13)
        // is not in any table
        let ix = Instruction {
            program_id: key(10),
            accounts: vec![
                AccountMeta::writable(owner(), true),
                AccountMeta::writable(key(11), false),
                AccountMeta::readonly(key(12), false),
                AccountMeta::writable(key(13), false),
                // listed in the table, but a signer, so it stays in the message
                AccountMeta::readonly(key(14), true),
                AccountMeta::readonly(key(15), false),
            ],
            data: vec![7],
        };
        let tables = vec![
            // the program is in a table too, but programs are never loaded
            (key(50), vec![key(10), key(12), key(14)]),
            (key(51), vec![key(15), key(11)]),
        ];
        let (message, accounts) = compile_v0(&owner(), &[ix], [3u8; 32], &tables).unwrap();
        assert_eq!(message.version, Version::V0);
        assert_eq!(
            message.account_keys,
            [owner(), key(14), key(13), key(10)],
            "payer, other signer, writable, then read-only static keys"
        );
        assert_eq!(message.header.required_signatures, 2);
        assert_eq!(message.header.readonly_signed, 1);
        assert_eq!(message.header.readonly_unsigned, 1);
        // loaded: every table's writable entries, then every table's read-only ones
        assert_eq!(
            accounts,
            [
                owner(),
                key(14),
                key(13),
                key(10),
                key(11),
                key(12),
                key(15)
            ]
        );
        assert_eq!(message.lookups.len(), 2);
        assert_eq!(message.lookups[0].table, key(50));
        assert_eq!(
            (&message.lookups[0].writable, &message.lookups[0].readonly),
            (&vec![], &vec![1])
        );
        assert_eq!(message.lookups[1].table, key(51));
        assert_eq!(
            (&message.lookups[1].writable, &message.lookups[1].readonly),
            (&vec![1], &vec![0])
        );

        // reading it back through the same tables gives the same accounts
        let contents: std::collections::HashMap<Pubkey, Vec<Pubkey>> =
            tables.iter().cloned().collect();
        assert_eq!(resolve_accounts(&message, &contents).unwrap(), accounts);
        let reparsed = Message::parse(message.raw()).unwrap();
        assert_eq!(reparsed, message);
        assert!(message.is_writable(4));
        assert!(!message.is_writable(5));
        assert!(!message.is_writable(6));

        // tables nobody needs are left out, and without tables it is a plain v0 message
        let (plain, all) = compile_v0(
            &owner(),
            &[system_transfer(&owner(), &key(2), 1)],
            [0u8; 32],
            &[(key(50), vec![key(70)])],
        )
        .unwrap();
        assert!(plain.lookups.is_empty());
        assert_eq!(all, plain.account_keys);
    }

    #[test]
    fn fees_include_the_priority_fee() {
        let ixs = [
            set_compute_unit_limit(200_000),
            set_compute_unit_price(5_000),
            system_transfer(&owner(), &key(2), 1),
        ];
        let m = compile_legacy(&owner(), &ixs, [0u8; 32]).unwrap();
        assert_eq!(
            compute_budget(&m, &m.account_keys),
            (Some(200_000), Some(5_000))
        );
        // 200,000 units * 5,000 micro-lamports = 1,000 lamports
        assert_eq!(priority_fee_lamports(&m, &m.account_keys), 1_000);
        assert_eq!(fee_estimate(&m, &m.account_keys), 6_000);

        let plain = transfer_message(1);
        assert_eq!(compute_budget(&plain, &plain.account_keys), (None, None));
        assert_eq!(fee_estimate(&plain, &plain.account_keys), 5_000);

        // no stated limit: 200,000 units per non-budget instruction
        let ixs = [
            set_compute_unit_price(1),
            system_transfer(&owner(), &key(2), 1),
        ];
        let m = compile_legacy(&owner(), &ixs, [0u8; 32]).unwrap();
        assert_eq!(priority_fee_lamports(&m, &m.account_keys), 1);
    }
}
