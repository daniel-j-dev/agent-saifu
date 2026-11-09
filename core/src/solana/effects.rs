//! What a transaction would do to the wallet, learned by simulating it on the node.
//!
//! The simulation returns the state of every writable account after the transaction. Comparing
//! it with the state before shows exactly how the wallet's SOL and token balances would change,
//! whatever programs the transaction calls. This is what makes it safe to sign a transaction
//! built by someone else (a dapp, or a swap API).

use std::collections::HashMap;

use super::address::{self, Pubkey};
use super::rpc::{AccountInfo, MAX_ACCOUNTS_PER_CALL, SolanaRpc};
use super::tokens::{self, TokenAccountData};
use super::tx::{self, Message, Transaction};

const LOOKUP_TABLE_HEADER_LEN: usize = 56;
/// Version 0 messages use at most a handful of lookup tables; this bounds the extra reads.
const MAX_LOOKUP_TABLES: usize = 16;

/// How much of one token the wallet gains or loses, across all of its accounts for that mint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenChange {
    pub mint: Pubkey,
    pub change: i128,
    pub accounts: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Effects {
    /// Why the simulation failed; the other fields are empty then.
    pub error: Option<String>,
    pub logs: Vec<String>,
    pub units_consumed: Option<u64>,
    /// Change of the wallet's SOL balance, the network fee included.
    pub sol_change: i128,
    pub token_changes: Vec<TokenChange>,
    /// Things that could cost the wallet later: spending approvals, changed owners.
    pub warnings: Vec<String>,
}

impl Effects {
    pub fn succeeded(&self) -> bool {
        self.error.is_none()
    }

    pub fn token_change(&self, mint: &Pubkey) -> i128 {
        self.token_changes
            .iter()
            .find(|c| c.mint == *mint)
            .map_or(0, |c| c.change)
    }
}

/// Reads an address lookup table account: its entries, in order.
pub fn parse_lookup_table(account: &AccountInfo) -> Result<Vec<Pubkey>, String> {
    if account.owner != address::id(address::ADDRESS_LOOKUP_TABLE_PROGRAM) {
        return Err(
            "an address lookup table account is not owned by the lookup table program".to_string(),
        );
    }
    let body = account
        .data
        .get(LOOKUP_TABLE_HEADER_LEN..)
        .ok_or("an address lookup table is too short")?;
    if body.len() % 32 != 0 {
        return Err("an address lookup table has a damaged address list".to_string());
    }
    Ok(body
        .chunks_exact(32)
        .map(|c| <[u8; 32]>::try_from(c).unwrap_or([0; 32]))
        .collect())
}

/// Every account of the message in index order, reading lookup tables from the node.
pub fn resolve_message_accounts(
    rpc: &SolanaRpc<'_>,
    message: &Message,
) -> Result<Vec<Pubkey>, String> {
    if message.lookups.is_empty() {
        return Ok(message.account_keys.clone());
    }
    if message.lookups.len() > MAX_LOOKUP_TABLES {
        return Err("the transaction uses too many address lookup tables".to_string());
    }
    let tables: Vec<Pubkey> = message.lookups.iter().map(|l| l.table).collect();
    let fetched = rpc.accounts(&tables)?;
    let mut contents: HashMap<Pubkey, Vec<Pubkey>> = HashMap::new();
    for (table, account) in tables.iter().zip(fetched) {
        let account = account.ok_or("an address lookup table does not exist")?;
        contents.insert(*table, parse_lookup_table(&account)?);
    }
    tx::resolve_accounts(message, &contents)
}

fn is_token_program(key: &Pubkey) -> bool {
    *key == address::id(address::TOKEN_PROGRAM) || *key == address::id(address::TOKEN_2022_PROGRAM)
}

fn token_data(account: Option<&AccountInfo>) -> Option<TokenAccountData> {
    let account = account?;
    if !is_token_program(&account.owner) {
        return None;
    }
    tokens::parse_token_account(&account.data)
}

fn lamports(account: Option<&AccountInfo>) -> i128 {
    account.map_or(0, |a| i128::from(a.lamports))
}

fn add_token_change(changes: &mut Vec<TokenChange>, mint: Pubkey, change: i128) {
    match changes.iter_mut().find(|c| c.mint == mint) {
        Some(existing) => {
            existing.change += change;
            existing.accounts += 1;
        }
        None => changes.push(TokenChange {
            mint,
            change,
            accounts: 1,
        }),
    }
}

/// Compares the state of one writable account before and after, for the wallet `owner`.
fn compare_account(
    key: &Pubkey,
    pre: Option<&AccountInfo>,
    post: Option<&AccountInfo>,
    owner: &Pubkey,
    effects: &mut Effects,
) {
    if key == owner {
        effects.sol_change += lamports(post) - lamports(pre);
        if let Some(after) = post
            && after.owner != address::id(address::SYSTEM_PROGRAM)
        {
            effects
                .warnings
                .push("this wallet's own account would be assigned to another program".to_string());
        }
        return;
    }
    let (before, after) = (token_data(pre), token_data(post));
    let mine_before = before.as_ref().filter(|t| t.owner == *owner);
    let mine_after = after.as_ref().filter(|t| t.owner == *owner);
    if let Some(t) = mine_before.or(mine_after) {
        let change = i128::from(mine_after.map_or(0, |a| a.amount))
            - i128::from(mine_before.map_or(0, |b| b.amount));
        add_token_change(&mut effects.token_changes, t.mint, change);
    }
    if let (Some(b), Some(a)) = (mine_before, after.as_ref()) {
        if a.owner != *owner {
            effects.warnings.push(format!(
                "ownership of the token account {} would be handed to {}",
                address::to_text(key),
                address::to_text(&a.owner)
            ));
        }
        if a.delegate != b.delegate
            && let Some(delegate) = a.delegate
        {
            effects.warnings.push(format!(
                "{} would be allowed to spend tokens from the token account {}",
                address::to_text(&delegate),
                address::to_text(key)
            ));
        }
        if a.close_authority != b.close_authority
            && let Some(authority) = a.close_authority
        {
            effects.warnings.push(format!(
                "{} would be allowed to close the token account {}",
                address::to_text(&authority),
                address::to_text(key)
            ));
        }
    }
    if let Some(b) = before.as_ref()
        && mine_after.is_some()
        && b.owner != *owner
    {
        effects.warnings.push(format!(
            "the token account {} would become owned by this wallet",
            address::to_text(key)
        ));
    }
}

/// Simulates `transaction` and works out how it changes `owner`'s balances.
pub fn simulate_effects(
    rpc: &SolanaRpc<'_>,
    transaction: &Transaction,
    owner: &Pubkey,
) -> Result<Effects, String> {
    let message = &transaction.message;
    let accounts = resolve_message_accounts(rpc, message)?;
    let mut writable: Vec<Pubkey> = vec![];
    for (index, key) in accounts.iter().enumerate() {
        if message.is_writable(index) && !writable.contains(key) {
            writable.push(*key);
        }
    }
    if writable.len() > MAX_ACCOUNTS_PER_CALL {
        return Err("the transaction changes too many accounts to check".to_string());
    }
    let before = rpc.accounts(&writable)?;
    let simulation = rpc.simulate(&transaction.to_bytes(), &writable)?;
    let mut effects = Effects {
        error: simulation.error.clone(),
        logs: simulation.logs.clone(),
        units_consumed: simulation.units_consumed,
        ..Effects::default()
    };
    if effects.error.is_some() {
        return Ok(effects);
    }
    for ((key, pre), post) in writable.iter().zip(&before).zip(&simulation.accounts) {
        compare_account(key, pre.as_ref(), post.as_ref(), owner, &mut effects);
    }
    effects.token_changes.retain(|c| c.change != 0);
    Ok(effects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::base64;
    use crate::solana::testing::SolMock;
    use crate::solana::tokens::testing::{mint_bytes, token_account_bytes};
    use crate::solana::tx::{AccountMeta, Instruction, compile_legacy};

    fn key(n: u8) -> Pubkey {
        [n; 32]
    }

    fn account(owner_program: &Pubkey, lamports: u64, data: &[u8]) -> String {
        format!(
            r#"{{"lamports":{lamports},"owner":"{}","data":["{}","base64"]}}"#,
            address::to_text(owner_program),
            base64::encode(data)
        )
    }

    fn system() -> Pubkey {
        address::id(address::SYSTEM_PROGRAM)
    }

    fn token_program() -> Pubkey {
        address::id(address::TOKEN_PROGRAM)
    }

    fn transfer_tx(owner: &Pubkey) -> Transaction {
        let message = compile_legacy(
            owner,
            &[tx::system_transfer(owner, &key(2), 1_000)],
            [0u8; 32],
        )
        .unwrap();
        Transaction::unsigned(message)
    }

    #[test]
    fn sol_change_includes_the_fee() {
        let owner = key(1);
        let pre = format!(
            r#"{{"value":[{},{}]}}"#,
            account(&system(), 100_000, &[]),
            account(&system(), 50, &[])
        );
        let post = format!(
            r#"{{"value":{{"err":null,"logs":[],"unitsConsumed":150,"accounts":[{},{}]}}}}"#,
            account(&system(), 94_000, &[]),
            account(&system(), 1_050, &[])
        );
        let mock = SolMock::new()
            .on("getMultipleAccounts", &pre)
            .on("simulateTransaction", &post);
        let rpc = SolanaRpc::new(&mock, "https://x");
        let effects = simulate_effects(&rpc, &transfer_tx(&owner), &owner).unwrap();
        assert!(effects.succeeded());
        assert_eq!(effects.sol_change, -6_000);
        assert!(effects.token_changes.is_empty());
        assert!(effects.warnings.is_empty());
        assert_eq!(effects.units_consumed, Some(150));
    }

    #[test]
    fn failed_simulations_carry_the_error() {
        let owner = key(1);
        let mock = SolMock::new()
            .on(
                "getMultipleAccounts",
                &format!(r#"{{"value":[{},null]}}"#, account(&system(), 10, &[])),
            )
            .on(
                "simulateTransaction",
                r#"{"value":{"err":"InsufficientFundsForFee","logs":[],"accounts":null}}"#,
            );
        let rpc = SolanaRpc::new(&mock, "https://x");
        let effects = simulate_effects(&rpc, &transfer_tx(&owner), &owner).unwrap();
        assert!(!effects.succeeded());
        assert!(effects.error.unwrap().contains("network fee"));
        assert_eq!(effects.sol_change, 0);
    }

    #[test]
    fn token_changes_are_summed_per_mint_for_the_wallets_accounts_only() {
        let owner = key(1);
        let (usdc, other_mint) = (key(40), key(41));
        // writable accounts of the message below, in order: owner, a, b, c, d
        let ix = Instruction {
            program_id: key(99),
            accounts: vec![
                AccountMeta::writable(key(10), false),
                AccountMeta::writable(key(11), false),
                AccountMeta::writable(key(12), false),
                AccountMeta::writable(key(13), false),
            ],
            data: vec![],
        };
        let message = compile_legacy(&owner, &[ix], [0u8; 32]).unwrap();
        let transaction = Transaction::unsigned(message);
        let tp = token_program();
        // a, b: wallet's USDC accounts (-30, +5); c: someone else's USDC; d: wallet's other token (+7, new)
        let pre = format!(
            r#"{{"value":[{},{},{},{},null]}}"#,
            account(&system(), 1_000, &[]),
            account(&tp, 2, &token_account_bytes(&usdc, &owner, 100, None)),
            account(&tp, 2, &token_account_bytes(&usdc, &owner, 0, None)),
            account(&tp, 2, &token_account_bytes(&usdc, &key(77), 500, None)),
        );
        // the writable list is owner, then the four accounts; the program is read-only
        let post = format!(
            r#"{{"value":{{"err":null,"logs":[],"accounts":[{},{},{},{},{}]}}}}"#,
            account(&system(), 1_000, &[]),
            account(&tp, 2, &token_account_bytes(&usdc, &owner, 70, None)),
            account(&tp, 2, &token_account_bytes(&usdc, &owner, 5, None)),
            account(&tp, 2, &token_account_bytes(&usdc, &key(77), 400, None)),
            account(&tp, 2, &token_account_bytes(&other_mint, &owner, 7, None)),
        );
        let mock = SolMock::new()
            .on("getMultipleAccounts", &pre)
            .on("simulateTransaction", &post);
        let rpc = SolanaRpc::new(&mock, "https://x");
        let effects = simulate_effects(&rpc, &transaction, &owner).unwrap();
        assert_eq!(effects.token_changes.len(), 2);
        assert_eq!(effects.token_change(&usdc), -25);
        assert_eq!(effects.token_change(&other_mint), 7);
        assert_eq!(effects.token_changes[0].accounts, 2);
        assert_eq!(effects.sol_change, 0);
    }

    #[test]
    fn new_approvals_and_owner_changes_are_warned_about() {
        let owner = key(1);
        let usdc = key(40);
        let ix = Instruction {
            program_id: key(99),
            accounts: vec![AccountMeta::writable(key(10), false)],
            data: vec![],
        };
        let transaction = Transaction::unsigned(compile_legacy(&owner, &[ix], [0u8; 32]).unwrap());
        let tp = token_program();
        let pre = format!(
            r#"{{"value":[{},{}]}}"#,
            account(&system(), 1_000, &[]),
            account(&tp, 2, &token_account_bytes(&usdc, &owner, 100, None)),
        );
        let delegated = token_account_bytes(&usdc, &owner, 100, Some(&key(66)));
        let stolen = token_account_bytes(&usdc, &key(66), 100, None);
        for (after, needle) in [(delegated, "allowed to spend"), (stolen, "handed to")] {
            let post = format!(
                r#"{{"value":{{"err":null,"logs":[],"accounts":[{},{}]}}}}"#,
                account(&system(), 1_000, &[]),
                account(&tp, 2, &after),
            );
            let mock = SolMock::new()
                .on("getMultipleAccounts", &pre)
                .on("simulateTransaction", &post);
            let rpc = SolanaRpc::new(&mock, "https://x");
            let effects = simulate_effects(&rpc, &transaction, &owner).unwrap();
            assert!(
                effects.warnings.iter().any(|w| w.contains(needle)),
                "{needle}: {:?}",
                effects.warnings
            );
        }
    }

    #[test]
    fn assigning_the_wallet_account_is_warned_about() {
        let owner = key(1);
        let pre = format!(
            r#"{{"value":[{},{}]}}"#,
            account(&system(), 1_000, &[]),
            account(&system(), 0, &[])
        );
        let post = format!(
            r#"{{"value":{{"err":null,"logs":[],"accounts":[{},{}]}}}}"#,
            account(&key(9), 1_000, &[]),
            account(&system(), 0, &[]),
        );
        let mock = SolMock::new()
            .on("getMultipleAccounts", &pre)
            .on("simulateTransaction", &post);
        let rpc = SolanaRpc::new(&mock, "https://x");
        let effects = simulate_effects(&rpc, &transfer_tx(&owner), &owner).unwrap();
        assert_eq!(effects.warnings.len(), 1);
    }

    fn lookup_table_account(entries: &[Pubkey]) -> AccountInfo {
        let mut data = vec![0u8; LOOKUP_TABLE_HEADER_LEN];
        for e in entries {
            data.extend_from_slice(e);
        }
        AccountInfo {
            lamports: 1,
            owner: address::id(address::ADDRESS_LOOKUP_TABLE_PROGRAM),
            data,
        }
    }

    #[test]
    fn lookup_tables_are_parsed_and_validated() {
        let table = lookup_table_account(&[key(5), key(6)]);
        assert_eq!(parse_lookup_table(&table).unwrap(), vec![key(5), key(6)]);
        let mut foreign = table.clone();
        foreign.owner = key(1);
        assert!(parse_lookup_table(&foreign).is_err());
        let mut damaged = table.clone();
        damaged.data.push(0);
        assert!(parse_lookup_table(&damaged).is_err());
        let mut short = table;
        short.data.truncate(10);
        assert!(parse_lookup_table(&short).is_err());
    }

    #[test]
    fn version_0_messages_resolve_accounts_through_the_node() {
        let mut raw = vec![0x80, 1, 0, 1];
        tx::encode_short_len(2, &mut raw);
        raw.extend_from_slice(&key(1));
        raw.extend_from_slice(&key(2));
        raw.extend_from_slice(&[5u8; 32]);
        tx::encode_short_len(1, &mut raw);
        raw.extend_from_slice(&[1, 2, 0, 2, 1, 7]);
        tx::encode_short_len(1, &mut raw);
        raw.extend_from_slice(&key(60));
        raw.extend_from_slice(&[1, 3]);
        raw.extend_from_slice(&[0]);
        let message = Message::parse(&raw).unwrap();
        let table = lookup_table_account(&[key(10), key(11), key(12), key(13)]);
        let encoded = format!(r#"{{"value":[{}]}}"#, account(&table.owner, 1, &table.data));
        let mock = SolMock::new().on("getMultipleAccounts", &encoded);
        let rpc = SolanaRpc::new(&mock, "https://x");
        let accounts = resolve_message_accounts(&rpc, &message).unwrap();
        assert_eq!(accounts, vec![key(1), key(2), key(13)]);
        let none = SolMock::new().on("getMultipleAccounts", r#"{"value":[null]}"#);
        assert!(resolve_message_accounts(&SolanaRpc::new(&none, "https://x"), &message).is_err());
    }

    #[test]
    fn mint_accounts_are_not_mistaken_for_token_accounts() {
        let mint = mint_bytes(6);
        assert!(
            token_data(Some(&AccountInfo {
                lamports: 1,
                owner: token_program(),
                data: mint,
            }))
            .is_none()
        );
    }
}
