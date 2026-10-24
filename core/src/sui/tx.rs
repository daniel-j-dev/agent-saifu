//! Building, budgeting, describing and signing Sui transactions.
//!
//! Transactions are assembled offline with the Sui SDK's `TransactionBuilder` from data read
//! over GraphQL (coins, gas price, epoch), then dry-run before anything is signed.

use sui_crypto::SuiSigner;
use sui_crypto::ed25519::Ed25519PrivateKey;
use sui_sdk_types::bcs::{FromBcs, ToBcs};
use sui_sdk_types::{
    Address, Argument as CommandArgument, Command, Digest, Input, PersonalMessage, StructTag,
    Transaction, TransactionExpiration, TransactionKind, TypeTag, UserSignature,
};
use sui_transaction_builder::{Argument, ObjectInput, TransactionBuilder};

use super::coins::{self, SUI_COIN_TYPE};
use super::gql::{CoinObject, Effects, GasSummary};
use crate::crypto::{base64, random};
use crate::eth::explorer::sanitize_text;
use crate::json::Value;

/// Sui's protocol limits.
pub const MAX_GAS_BUDGET: u64 = 50_000_000_000;
pub const MAX_GAS_OBJECTS: usize = 256;
const MAX_MERGE_INPUTS: usize = 250;
/// Same safety margin the official TypeScript SDK adds on top of the simulated computation cost.
const BUDGET_OVERHEAD_UNITS: u64 = 1000;
pub const MAX_TRANSACTION_BYTES: usize = 128 * 1024;
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// What an address holds of one coin type: coin objects plus any address balance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Holdings {
    pub coins: Vec<CoinObject>,
    pub address_balance: u128,
}

impl Holdings {
    pub fn coin_total(&self) -> u128 {
        self.coins.iter().map(|c| u128::from(c.balance)).sum()
    }

    pub fn total(&self) -> u128 {
        self.coin_total() + self.address_balance
    }
}

/// Network facts every transaction needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GasEnv {
    pub price: u64,
    pub epoch: u64,
    pub chain: Digest,
    /// Makes otherwise identical address-balance transactions distinct.
    pub nonce: u32,
}

/// How the transaction's gas is paid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GasChoice {
    Coins(Vec<CoinObject>),
    AddressBalance,
}

pub fn fresh_nonce() -> Result<u32, String> {
    Ok(u32::from_le_bytes(random::random_array::<4>()?))
}

fn object_input(coin: &CoinObject) -> ObjectInput {
    ObjectInput::owned(coin.id, coin.version, coin.digest)
}

/// Chooses gas coins. Coins are taken largest first until they cover `budget + also_needed`
/// (SUI the transaction itself spends); when there are not enough coin objects but the address
/// balance can pay, gas comes from the address balance instead.
pub fn select_gas(sui: &Holdings, budget: u64, also_needed: u64) -> Result<GasChoice, String> {
    let budget128 = u128::from(budget);
    if sui.coin_total() >= budget128 {
        let target = budget128 + u128::from(also_needed);
        let mut chosen = vec![];
        let mut sum = 0u128;
        for coin in &sui.coins {
            if sum >= target || chosen.len() >= MAX_GAS_OBJECTS {
                break;
            }
            sum += u128::from(coin.balance);
            chosen.push(coin.clone());
        }
        if chosen.is_empty() {
            return Err("this wallet has no SUI to pay for gas".to_string());
        }
        return Ok(GasChoice::Coins(chosen));
    }
    if sui.address_balance >= budget128 {
        return Ok(GasChoice::AddressBalance);
    }
    Err(format!(
        "not enough SUI to pay for gas: {} SUI is needed; the wallet's SUI coin objects hold {} SUI and its address balance {} SUI",
        coins::format_amount(budget128, coins::SUI_DECIMALS),
        coins::format_amount(sui.coin_total(), coins::SUI_DECIMALS),
        coins::format_amount(sui.address_balance, coins::SUI_DECIMALS)
    ))
}

/// The largest budget this wallet could pay, for dry runs.
pub fn max_budget(sui: &Holdings) -> u64 {
    let available = sui.coin_total().max(sui.address_balance);
    u64::try_from(available.min(u128::from(MAX_GAS_BUDGET))).unwrap_or(MAX_GAS_BUDGET)
}

/// Budget for a transaction whose dry run cost `gas`: computation plus a margin, plus storage
/// minus the rebate, as the official SDK does.
pub fn budget_from_simulation(gas: &GasSummary, price: u64) -> u64 {
    let base = gas
        .computation
        .saturating_add(BUDGET_OVERHEAD_UNITS.saturating_mul(price));
    let with_storage = base.saturating_add(gas.storage).saturating_sub(gas.rebate);
    with_storage.max(base).min(MAX_GAS_BUDGET)
}

fn struct_tag(coin_type: &str) -> Result<StructTag, String> {
    coin_type
        .parse::<StructTag>()
        .map_err(|_| "invalid coin type".to_string())
}

/// Produces one `Coin<T>` of exactly `amount` from the wallet's coin objects and address balance
/// and returns it. For SUI the gas coin is the source whenever gas comes from coin objects.
fn extract_coin(
    tx: &mut TransactionBuilder,
    coin_type: &str,
    amount: u64,
    spend: &Holdings,
    gas: &GasChoice,
    budget: u64,
) -> Result<Argument, String> {
    let amount128 = u128::from(amount);
    let tag: TypeTag = struct_tag(coin_type)?.into();
    if coins::is_sui(coin_type) {
        return match gas {
            GasChoice::Coins(chosen) => {
                let have: u128 = chosen.iter().map(|c| u128::from(c.balance)).sum();
                let need = amount128 + u128::from(budget);
                let gas_coin = tx.gas();
                if have < need {
                    let shortfall = need - have;
                    if spend.address_balance < shortfall {
                        return Err(insufficient(coin_type, amount128, spend));
                    }
                    let topped_up = u64::try_from(shortfall).map_err(|_| "amount too large")?;
                    let withdrawn = tx.funds_withdrawal_coin(tag, topped_up);
                    tx.merge_coins(gas_coin, vec![withdrawn]);
                }
                let value = tx.pure(&amount);
                Ok(tx.split_coins(gas_coin, vec![value]).remove(0))
            }
            GasChoice::AddressBalance => {
                if spend.address_balance < amount128 + u128::from(budget) {
                    return Err(insufficient(
                        coin_type,
                        amount128 + u128::from(budget),
                        spend,
                    ));
                }
                Ok(tx.funds_withdrawal_coin(tag, amount))
            }
        };
    }
    if spend.total() < amount128 {
        return Err(insufficient(coin_type, amount128, spend));
    }
    let mut chosen: Vec<&CoinObject> = vec![];
    let mut sum = 0u128;
    for coin in &spend.coins {
        if sum >= amount128 || chosen.len() >= MAX_MERGE_INPUTS {
            break;
        }
        sum += u128::from(coin.balance);
        chosen.push(coin);
    }
    let mut inputs: Vec<Argument> = chosen.iter().map(|c| tx.object(object_input(c))).collect();
    if sum < amount128 {
        let shortfall = u64::try_from(amount128 - sum).map_err(|_| "amount too large")?;
        if spend.address_balance < u128::from(shortfall) {
            return Err(insufficient(coin_type, amount128, spend));
        }
        inputs.push(tx.funds_withdrawal_coin(tag, shortfall));
    }
    let primary = inputs.remove(0);
    if !inputs.is_empty() {
        tx.merge_coins(primary, inputs);
    }
    // Everything gathered is exactly the amount: splitting would leave an empty coin that
    // cannot be dropped.
    if sum <= amount128 {
        return Ok(primary);
    }
    let value = tx.pure(&amount);
    Ok(tx.split_coins(primary, vec![value]).remove(0))
}

pub fn insufficient(coin_type: &str, needed: u128, spend: &Holdings) -> String {
    format!(
        "insufficient balance: {} base units of {} are needed but the wallet holds {} (coin objects {} + address balance {})",
        needed,
        sanitize_text(&coins::fallback_symbol(coin_type)),
        spend.total(),
        spend.coin_total(),
        spend.address_balance
    )
}

/// What to do with the extracted coin: add commands that consume it.
pub type Body<'a> = &'a dyn Fn(&mut TransactionBuilder, Argument) -> Result<(), String>;

/// Builds the full transaction: extract `amount` of `coin_type`, run `body` on it, pay gas.
pub fn assemble(
    sender: Address,
    env: &GasEnv,
    budget: u64,
    sui: &Holdings,
    coin_type: &str,
    amount: u64,
    spend: &Holdings,
    body: Body<'_>,
) -> Result<(Transaction, GasChoice), String> {
    let also_needed = if coins::is_sui(coin_type) { amount } else { 0 };
    let gas = select_gas(sui, budget, also_needed)?;
    let mut tx = TransactionBuilder::new();
    let coin = extract_coin(&mut tx, coin_type, amount, spend, &gas, budget)?;
    body(&mut tx, coin)?;
    let transaction = finish(tx, sender, env, budget, &gas)?;
    Ok((transaction, gas))
}

fn finish(
    mut tx: TransactionBuilder,
    sender: Address,
    env: &GasEnv,
    budget: u64,
    gas: &GasChoice,
) -> Result<Transaction, String> {
    tx.set_sender(sender);
    tx.set_gas_budget(budget);
    tx.set_gas_price(env.price);
    match gas {
        GasChoice::Coins(chosen) => tx.add_gas_objects(chosen.iter().map(object_input)),
        // The builder insists on a gas object; it is removed below.
        GasChoice::AddressBalance => {
            tx.add_gas_objects([ObjectInput::owned(Address::ZERO, 1, Digest::ZERO)])
        }
    }
    let mut transaction = tx
        .try_build()
        .map_err(|e| format!("cannot build the transaction: {e}"))?;
    if *gas == GasChoice::AddressBalance {
        transaction.gas_payment.objects.clear();
        transaction.expiration = TransactionExpiration::ValidDuring {
            min_epoch: Some(env.epoch),
            max_epoch: Some(env.epoch),
            min_timestamp: None,
            max_timestamp: None,
            chain: env.chain,
            nonce: env.nonce,
        };
    }
    Ok(transaction)
}

/// Adds a transfer of `coin` to `recipient`.
pub fn transfer_body(
    recipient: Address,
) -> impl Fn(&mut TransactionBuilder, Argument) -> Result<(), String> {
    move |tx, coin| {
        let to = tx.pure(&recipient);
        tx.transfer_objects(vec![coin], to);
        Ok(())
    }
}

// ---- encoding, signing ------------------------------------------------------------------------

pub fn encode(transaction: &Transaction) -> Result<String, String> {
    transaction
        .to_bcs_base64()
        .map_err(|_| "cannot encode the transaction".to_string())
}

/// Decodes BCS `TransactionData` as produced by Sui SDKs (base64).
pub fn decode(base64_text: &str) -> Result<Transaction, String> {
    let bytes = base64::decode(base64_text.trim())
        .map_err(|_| "transaction_bytes must be base64".to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_TRANSACTION_BYTES {
        return Err(format!(
            "transaction_bytes must be 1 to {MAX_TRANSACTION_BYTES} bytes"
        ));
    }
    Transaction::from_bcs(&bytes).map_err(|_| {
        "transaction_bytes is not a valid Sui transaction (BCS TransactionData)".to_string()
    })
}

pub fn digest_of(transaction: &Transaction) -> String {
    transaction.digest().to_string()
}

/// A signature in the serialized `flag || signature || public key` form, base64 encoded.
pub fn sign_transaction(
    key: &Ed25519PrivateKey,
    transaction: &Transaction,
) -> Result<String, String> {
    key.sign_transaction(transaction)
        .map(|s: UserSignature| s.to_base64())
        .map_err(|_| "signing failed".to_string())
}

pub fn sign_personal_message(key: &Ed25519PrivateKey, message: &[u8]) -> Result<String, String> {
    key.sign_personal_message(&PersonalMessage(message.into()))
        .map(|s: UserSignature| s.to_base64())
        .map_err(|_| "signing failed".to_string())
}

/// Reads a message given as text (default), `hex` or `base64`.
pub fn parse_message(message: &str, format: Option<&str>) -> Result<Vec<u8>, String> {
    let bytes = match format.unwrap_or("text").to_lowercase().as_str() {
        "text" | "utf8" => message.as_bytes().to_vec(),
        "hex" => crate::crypto::hex::decode(message.trim())
            .map_err(|e| format!("message is not valid hex: {e}"))?,
        "base64" => {
            base64::decode(message.trim()).map_err(|_| "message is not valid base64".to_string())?
        }
        _ => return Err("message_format must be 'text', 'hex' or 'base64'".to_string()),
    };
    if bytes.is_empty() {
        return Err("message must not be empty".to_string());
    }
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(format!("message is larger than {MAX_MESSAGE_BYTES} bytes"));
    }
    Ok(bytes)
}

// ---- describing ---------------------------------------------------------------------------------

fn argument_text(a: &CommandArgument) -> String {
    match a {
        CommandArgument::Gas => "gas coin".to_string(),
        CommandArgument::Input(i) => format!("input {i}"),
        CommandArgument::Result(i) => format!("result of command {i}"),
        CommandArgument::NestedResult(i, j) => format!("result {j} of command {i}"),
    }
}

fn type_args_text(args: &[TypeTag]) -> String {
    if args.is_empty() {
        return String::new();
    }
    format!(
        "<{}>",
        args.iter()
            .map(|t| sanitize_text_long(&t.to_string()))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn sanitize_text_long(s: &str) -> String {
    crate::eth::explorer::sanitize_text_max(s, 160)
}

pub fn describe_command(command: &Command) -> String {
    match command {
        Command::MoveCall(call) => format!(
            "call {}::{}::{}{}",
            call.package,
            sanitize_text(call.module.as_str()),
            sanitize_text(call.function.as_str()),
            type_args_text(&call.type_arguments)
        ),
        Command::TransferObjects(t) => format!(
            "transfer {} to {}",
            t.objects
                .iter()
                .map(argument_text)
                .collect::<Vec<_>>()
                .join(", "),
            argument_text(&t.address)
        ),
        Command::SplitCoins(s) => format!(
            "split {} off {} ({} amount(s))",
            "coins",
            argument_text(&s.coin),
            s.amounts.len()
        ),
        Command::MergeCoins(m) => format!(
            "merge {} coin(s) into {}",
            m.coins_to_merge.len(),
            argument_text(&m.coin)
        ),
        Command::Publish(_) => "publish a new Move package".to_string(),
        Command::Upgrade(_) => "upgrade a Move package".to_string(),
        Command::MakeMoveVector(_) => "build a vector".to_string(),
        _ => "other command".to_string(),
    }
}

/// Plain-language summary of a transaction plus anything the signer should be warned about.
pub fn describe(transaction: &Transaction, wallet: &Address) -> (Value, Vec<String>) {
    let mut warnings = vec![];
    if transaction.sender != *wallet {
        warnings.push("the transaction's sender is not this wallet".to_string());
    }
    if transaction.gas_payment.owner != transaction.sender {
        warnings.push(
            "gas is paid by a sponsor; the sponsor must also sign before it can execute"
                .to_string(),
        );
    }
    let commands: Vec<String> = match &transaction.kind {
        TransactionKind::ProgrammableTransaction(ptb) => {
            for command in &ptb.commands {
                match command {
                    Command::Publish(_) | Command::Upgrade(_) => warnings.push(
                        "publishes or upgrades a Move package, which can run arbitrary code"
                            .to_string(),
                    ),
                    Command::TransferObjects(t)
                        if t.objects.iter().any(|a| matches!(a, CommandArgument::Gas)) =>
                    {
                        warnings.push(
                            "transfers the gas coin itself, which moves all of its SUI".to_string(),
                        )
                    }
                    _ => {}
                }
            }
            ptb.commands.iter().map(describe_command).collect()
        }
        _ => {
            warnings.push(
                "this is not a user transaction (system transactions cannot be signed by a wallet)"
                    .to_string(),
            );
            vec![]
        }
    };
    let inputs = match &transaction.kind {
        TransactionKind::ProgrammableTransaction(ptb) => ptb
            .inputs
            .iter()
            .filter(|i| matches!(i, Input::FundsWithdrawal(_)))
            .count(),
        _ => 0,
    };
    let summary = Value::object(vec![
        ("sender", Value::String(transaction.sender.to_string())),
        (
            "gas_budget_mist",
            Value::Number(transaction.gas_payment.budget.to_string()),
        ),
        (
            "gas_price_mist",
            Value::Number(transaction.gas_payment.price.to_string()),
        ),
        (
            "gas_paid_from",
            Value::str(if transaction.gas_payment.objects.is_empty() {
                "address balance"
            } else {
                "coin objects"
            }),
        ),
        (
            "address_balance_withdrawals",
            Value::Number(inputs.to_string()),
        ),
        (
            "commands",
            Value::Array(commands.into_iter().map(Value::String).collect()),
        ),
    ]);
    (summary, warnings)
}

/// Only user transactions sent by this wallet may be signed.
pub fn check_signable(transaction: &Transaction, wallet: &Address) -> Result<(), String> {
    if !matches!(
        transaction.kind,
        TransactionKind::ProgrammableTransaction(_)
    ) {
        return Err("only programmable (user) transactions can be signed".to_string());
    }
    if transaction.sender != *wallet {
        return Err("the transaction's sender is not this wallet; refusing to sign it".to_string());
    }
    if transaction.gas_payment.owner != *wallet {
        return Err("this wallet is not the gas owner of the transaction (sponsored transactions need the sponsor's signature); refusing to sign it".to_string());
    }
    Ok(())
}

/// Net change per coin type for one address, from simulation or execution effects.
pub fn wallet_changes(effects: &Effects, wallet: &Address) -> Vec<(String, i128)> {
    let me = wallet.to_string();
    let mut out: Vec<(String, i128)> = vec![];
    for change in &effects.balance_changes {
        if change.owner != me {
            continue;
        }
        match out.iter_mut().find(|(t, _)| *t == change.coin_type) {
            Some((_, total)) => *total += change.amount,
            None => out.push((change.coin_type.clone(), change.amount)),
        }
    }
    out
}

pub fn is_sui_type(coin_type: &str) -> bool {
    coin_type == SUI_COIN_TYPE
}

#[cfg(test)]
pub mod tests {
    use super::*;
    pub fn addr(n: u8) -> Address {
        Address::new([n; 32])
    }

    pub fn coin(n: u8, balance: u64) -> CoinObject {
        CoinObject {
            id: addr(n),
            version: 10 + u64::from(n),
            digest: Digest::new([n; 32]),
            balance,
        }
    }

    pub fn env() -> GasEnv {
        GasEnv {
            price: 1000,
            epoch: 5,
            chain: Digest::new([9; 32]),
            nonce: 77,
        }
    }

    pub const USDC: &str =
        "0x0000000000000000000000000000000000000000000000000000000000000abc::usdc::USDC";

    fn holdings(balances: &[u64], address_balance: u128) -> Holdings {
        let mut coins: Vec<CoinObject> = balances
            .iter()
            .enumerate()
            .map(|(i, b)| coin((i % 250) as u8 + 1, *b))
            .collect();
        coins.sort_by_key(|c| std::cmp::Reverse(c.balance));
        Holdings {
            coins,
            address_balance,
        }
    }

    fn commands(tx: &Transaction) -> Vec<Command> {
        match &tx.kind {
            TransactionKind::ProgrammableTransaction(p) => p.commands.clone(),
            _ => panic!("not a PTB"),
        }
    }

    fn inputs(tx: &Transaction) -> Vec<Input> {
        match &tx.kind {
            TransactionKind::ProgrammableTransaction(p) => p.inputs.clone(),
            _ => panic!("not a PTB"),
        }
    }

    #[test]
    fn gas_selection_takes_the_largest_coins_until_covered() {
        let h = holdings(&[5, 100, 50], 0);
        let GasChoice::Coins(chosen) = select_gas(&h, 60, 0).unwrap() else {
            panic!("coins")
        };
        assert_eq!(
            chosen.iter().map(|c| c.balance).collect::<Vec<_>>(),
            vec![100]
        );
        let GasChoice::Coins(chosen) = select_gas(&h, 60, 70).unwrap() else {
            panic!("coins")
        };
        assert_eq!(
            chosen.iter().map(|c| c.balance).collect::<Vec<_>>(),
            vec![100, 50]
        );
        let GasChoice::Coins(all) = select_gas(&h, 60, 10_000).unwrap() else {
            panic!("coins")
        };
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn gas_falls_back_to_the_address_balance_when_coin_objects_cannot_pay() {
        let only_balance = holdings(&[], 1_000);
        assert_eq!(
            select_gas(&only_balance, 500, 0).unwrap(),
            GasChoice::AddressBalance
        );
        assert!(select_gas(&only_balance, 5_000, 0).is_err());
        let tiny_coin = holdings(&[10], 1_000_000);
        assert_eq!(
            select_gas(&tiny_coin, 500, 0).unwrap(),
            GasChoice::AddressBalance
        );
        let err = select_gas(&holdings(&[10], 100), 500, 0).unwrap_err();
        assert!(err.contains("not enough SUI"), "{err}");
        assert!(select_gas(&holdings(&[], 0), 1, 0).is_err());
    }

    #[test]
    fn gas_object_count_is_capped() {
        let many: Vec<u64> = vec![1; 300];
        let h = holdings(&many, 0);
        let GasChoice::Coins(chosen) = select_gas(&h, 1, 1_000).unwrap() else {
            panic!("coins")
        };
        assert_eq!(chosen.len(), MAX_GAS_OBJECTS);
    }

    #[test]
    fn budgets_follow_the_dry_run_with_a_margin() {
        let gas = GasSummary {
            computation: 1_000_000,
            storage: 988_000,
            rebate: 978_120,
        };
        assert_eq!(
            budget_from_simulation(&gas, 1000),
            1_000_000 + 1_000_000 + 9_880
        );
        let refund = GasSummary {
            computation: 500_000,
            storage: 100,
            rebate: 5_000_000,
        };
        assert_eq!(
            budget_from_simulation(&refund, 1000),
            1_500_000,
            "a rebate never lowers the margin"
        );
        let huge = GasSummary {
            computation: u64::MAX / 2,
            storage: u64::MAX / 2,
            rebate: 0,
        };
        assert_eq!(budget_from_simulation(&huge, 1000), MAX_GAS_BUDGET);
    }

    #[test]
    fn dry_run_budget_is_capped_by_balance_and_protocol() {
        assert_eq!(max_budget(&holdings(&[10, 20], 5)), 30);
        assert_eq!(max_budget(&holdings(&[], 7)), 7);
        assert_eq!(max_budget(&holdings(&[u64::MAX], 0)), MAX_GAS_BUDGET);
        assert_eq!(max_budget(&holdings(&[], 0)), 0);
    }

    #[test]
    fn sending_sui_splits_the_gas_coin_and_transfers() {
        let sui = holdings(&[10_000_000_000], 0);
        let (tx, gas) = assemble(
            addr(1),
            &env(),
            5_000_000,
            &sui,
            SUI_COIN_TYPE,
            1_000_000_000,
            &sui,
            &transfer_body(addr(2)),
        )
        .unwrap();
        assert!(matches!(gas, GasChoice::Coins(ref c) if c.len() == 1));
        let cmds = commands(&tx);
        assert_eq!(cmds.len(), 2);
        assert!(matches!(&cmds[0], Command::SplitCoins(s) if s.coin == CommandArgument::Gas));
        assert!(matches!(&cmds[1], Command::TransferObjects(_)));
        assert_eq!(tx.sender, addr(1));
        assert_eq!(tx.gas_payment.budget, 5_000_000);
        assert_eq!(tx.gas_payment.price, 1000);
        assert_eq!(tx.gas_payment.objects.len(), 1);
        assert_eq!(tx.expiration, TransactionExpiration::None);
    }

    #[test]
    fn sending_sui_tops_up_from_the_address_balance() {
        let sui = holdings(&[2_000_000], 5_000_000_000);
        let (tx, _) = assemble(
            addr(1),
            &env(),
            1_000_000,
            &sui,
            SUI_COIN_TYPE,
            1_000_000_000,
            &sui,
            &transfer_body(addr(2)),
        )
        .unwrap();
        let cmds = commands(&tx);
        assert!(matches!(&cmds[0], Command::MoveCall(c) if c.function.as_str() == "redeem_funds"));
        assert!(matches!(&cmds[1], Command::MergeCoins(m) if m.coin == CommandArgument::Gas));
        assert!(matches!(&cmds[2], Command::SplitCoins(_)));
        assert!(
            inputs(&tx)
                .iter()
                .any(|i| matches!(i, Input::FundsWithdrawal(_)))
        );
        let too_much = assemble(
            addr(1),
            &env(),
            1_000_000,
            &sui,
            SUI_COIN_TYPE,
            9_000_000_000,
            &sui,
            &transfer_body(addr(2)),
        );
        assert!(too_much.unwrap_err().contains("insufficient balance"));
    }

    #[test]
    fn a_wallet_with_only_an_address_balance_pays_gas_from_it() {
        let sui = holdings(&[], 10_000_000_000);
        let (tx, gas) = assemble(
            addr(1),
            &env(),
            5_000_000,
            &sui,
            SUI_COIN_TYPE,
            1_000_000_000,
            &sui,
            &transfer_body(addr(2)),
        )
        .unwrap();
        assert_eq!(gas, GasChoice::AddressBalance);
        assert!(tx.gas_payment.objects.is_empty());
        assert!(matches!(
            tx.expiration,
            TransactionExpiration::ValidDuring {
                min_epoch: Some(5),
                max_epoch: Some(5),
                nonce: 77,
                ..
            }
        ));
        assert!(
            matches!(&commands(&tx)[0], Command::MoveCall(c) if c.function.as_str() == "redeem_funds")
        );
        // The amount and the gas must both fit in the balance.
        assert!(
            assemble(
                addr(1),
                &env(),
                5_000_000,
                &sui,
                SUI_COIN_TYPE,
                9_999_000_000,
                &sui,
                &transfer_body(addr(2)),
            )
            .is_err()
        );
    }

    #[test]
    fn sending_another_coin_uses_its_coin_objects() {
        let sui = holdings(&[10_000_000_000], 0);
        let usdc = holdings(&[300, 200, 100], 0);
        // Exactly one whole coin: sent as is.
        let (tx, _) = assemble(
            addr(1),
            &env(),
            1_000_000,
            &sui,
            USDC,
            300,
            &usdc,
            &transfer_body(addr(2)),
        )
        .unwrap();
        assert_eq!(commands(&tx).len(), 1);
        // Several coins are merged, then split.
        let (tx, _) = assemble(
            addr(1),
            &env(),
            1_000_000,
            &sui,
            USDC,
            450,
            &usdc,
            &transfer_body(addr(2)),
        )
        .unwrap();
        let cmds = commands(&tx);
        assert!(matches!(&cmds[0], Command::MergeCoins(m) if m.coins_to_merge.len() == 1));
        assert!(matches!(&cmds[1], Command::SplitCoins(_)));
        // A partial coin is split.
        let (tx, _) = assemble(
            addr(1),
            &env(),
            1_000_000,
            &sui,
            USDC,
            50,
            &usdc,
            &transfer_body(addr(2)),
        )
        .unwrap();
        assert!(matches!(&commands(&tx)[0], Command::SplitCoins(_)));
        // Not enough.
        let err = assemble(
            addr(1),
            &env(),
            1_000_000,
            &sui,
            USDC,
            601,
            &usdc,
            &transfer_body(addr(2)),
        )
        .unwrap_err();
        assert!(err.contains("insufficient balance") && err.contains("USDC"));
    }

    #[test]
    fn another_coin_can_come_from_its_address_balance() {
        let sui = holdings(&[10_000_000_000], 0);
        let only_balance = holdings(&[], 1_000);
        let (tx, _) = assemble(
            addr(1),
            &env(),
            1_000_000,
            &sui,
            USDC,
            400,
            &only_balance,
            &transfer_body(addr(2)),
        )
        .unwrap();
        let cmds = commands(&tx);
        // The withdrawal is exactly the amount, so it is sent without splitting (an empty
        // leftover coin could not be dropped).
        assert_eq!(cmds.len(), 2);
        assert!(matches!(&cmds[0], Command::MoveCall(c) if c.function.as_str() == "redeem_funds"));
        assert!(matches!(&cmds[1], Command::TransferObjects(_)));
        let mixed = holdings(&[100], 1_000);
        let (tx, _) = assemble(
            addr(1),
            &env(),
            1_000_000,
            &sui,
            USDC,
            400,
            &mixed,
            &transfer_body(addr(2)),
        )
        .unwrap();
        let cmds = commands(&tx);
        assert!(matches!(&cmds[0], Command::MoveCall(_)));
        assert!(matches!(&cmds[1], Command::MergeCoins(_)));
        assert_eq!(cmds.len(), 3);
    }

    #[test]
    fn exact_sums_of_several_coins_are_merged_without_a_split() {
        let sui = holdings(&[10_000_000_000], 0);
        let usdc = holdings(&[300, 200, 100], 0);
        let (tx, _) = assemble(
            addr(1),
            &env(),
            1_000_000,
            &sui,
            USDC,
            500,
            &usdc,
            &transfer_body(addr(2)),
        )
        .unwrap();
        let cmds = commands(&tx);
        assert_eq!(cmds.len(), 2);
        assert!(matches!(&cmds[0], Command::MergeCoins(_)));
    }

    #[test]
    fn transactions_round_trip_through_base64() {
        let sui = holdings(&[10_000_000_000], 0);
        let (tx, _) = assemble(
            addr(1),
            &env(),
            5_000_000,
            &sui,
            SUI_COIN_TYPE,
            7,
            &sui,
            &transfer_body(addr(2)),
        )
        .unwrap();
        let text = encode(&tx).unwrap();
        assert_eq!(decode(&text).unwrap(), tx);
        let sponsored = holdings(&[], 10_000_000_000);
        let (tx, _) = assemble(
            addr(1),
            &env(),
            5_000_000,
            &sponsored,
            SUI_COIN_TYPE,
            7,
            &sponsored,
            &transfer_body(addr(2)),
        )
        .unwrap();
        assert_eq!(decode(&encode(&tx).unwrap()).unwrap(), tx);
        for bad in ["", "!!!", "AAAA", &"A".repeat(300_000)] {
            assert!(decode(bad).is_err());
        }
    }

    #[test]
    fn digests_are_stable_for_identical_transactions() {
        let sui = holdings(&[10_000_000_000], 0);
        let build = || {
            assemble(
                addr(1),
                &env(),
                5_000_000,
                &sui,
                SUI_COIN_TYPE,
                7,
                &sui,
                &transfer_body(addr(2)),
            )
            .unwrap()
            .0
        };
        assert_eq!(digest_of(&build()), digest_of(&build()));
        let other = assemble(
            addr(1),
            &env(),
            5_000_000,
            &sui,
            SUI_COIN_TYPE,
            8,
            &sui,
            &transfer_body(addr(2)),
        )
        .unwrap()
        .0;
        assert_ne!(digest_of(&build()), digest_of(&other));
    }

    #[test]
    fn signatures_verify_against_the_transaction() {
        use sui_crypto::SuiVerifier;
        use sui_crypto::ed25519::Ed25519VerifyingKey;
        let key = Ed25519PrivateKey::new([3u8; 32]);
        let me = key.public_key().derive_address();
        let sui = holdings(&[10_000_000_000], 0);
        let (tx, _) = assemble(
            me,
            &env(),
            5_000_000,
            &sui,
            SUI_COIN_TYPE,
            7,
            &sui,
            &transfer_body(addr(2)),
        )
        .unwrap();
        let b64 = sign_transaction(&key, &tx).unwrap();
        let signature = UserSignature::from_base64(&b64).unwrap();
        let verifier = Ed25519VerifyingKey::new(&key.public_key()).unwrap();
        verifier.verify_transaction(&tx, &signature).unwrap();
        let (other, _) = assemble(
            me,
            &env(),
            5_000_000,
            &sui,
            SUI_COIN_TYPE,
            8,
            &sui,
            &transfer_body(addr(2)),
        )
        .unwrap();
        assert!(verifier.verify_transaction(&other, &signature).is_err());
        // 1 flag byte + 64 signature bytes + 32 public key bytes.
        assert_eq!(base64::decode(&b64).unwrap().len(), 97);
    }

    #[test]
    fn personal_messages_sign_and_verify() {
        use sui_crypto::SuiVerifier;
        use sui_crypto::ed25519::Ed25519VerifyingKey;
        let key = Ed25519PrivateKey::new([4u8; 32]);
        let b64 = sign_personal_message(&key, b"hello sui").unwrap();
        let sig = UserSignature::from_base64(&b64).unwrap();
        let verifier = Ed25519VerifyingKey::new(&key.public_key()).unwrap();
        verifier
            .verify_personal_message(&PersonalMessage(b"hello sui".as_slice().into()), &sig)
            .unwrap();
        assert!(
            verifier
                .verify_personal_message(&PersonalMessage(b"hello Sui".as_slice().into()), &sig)
                .is_err()
        );
    }

    #[test]
    fn a_personal_message_signature_is_not_a_transaction_signature() {
        use sui_crypto::SuiVerifier;
        use sui_crypto::ed25519::Ed25519VerifyingKey;
        // The intent prefix separates the two, so a signed message can never authorise a transfer.
        let key = Ed25519PrivateKey::new([5u8; 32]);
        let me = key.public_key().derive_address();
        let sui = holdings(&[10_000_000_000], 0);
        let (tx, _) = assemble(
            me,
            &env(),
            5_000_000,
            &sui,
            SUI_COIN_TYPE,
            7,
            &sui,
            &transfer_body(addr(2)),
        )
        .unwrap();
        let bytes = tx.to_bcs().unwrap();
        let as_message =
            UserSignature::from_base64(&sign_personal_message(&key, &bytes).unwrap()).unwrap();
        let verifier = Ed25519VerifyingKey::new(&key.public_key()).unwrap();
        assert!(verifier.verify_transaction(&tx, &as_message).is_err());
    }

    #[test]
    fn known_answer_vectors_match_an_independent_implementation() {
        // Computed independently in Python: BLAKE2b-256 over the intent-prefixed bytes, then
        // Ed25519, serialized as flag || signature || public key.
        const TX: &str = "AAACAAjoAwAAAAAAAAAgxzmZG7Y582Gfg7p1QM7XzEsF9jKzVuohMdCH8Za8EY4CAgABAQAAAQEDAAAAAAEBAMc5mRu2OfNhn4O6dUDO18xLBfYys1bqITHQh/GWvBGOAe5KFgKV9sXYxrQQ9X/WruZ3EL7k4iu6jeOkiSImSoO0BQAAAAAAAAAgIejPmaY4BBSQsI3EKYMD24yWq6SQEu2LufZWuZMa8J3HOZkbtjnzYZ+DunVAztfMSwX2MrNW6iEx0IfxlrwRjmQAAAAAAAAAgJaYAAAAAAAA";
        let key = Ed25519PrivateKey::new(
            crate::crypto::hex::decode_fixed::<32>(
                "beb2f065b1c72f05d509d676bc6ac5edaaaaa27e974e6f790249c779f19409a3",
            )
            .unwrap(),
        );
        let transaction = decode(TX).unwrap();
        assert_eq!(
            digest_of(&transaction),
            "HsRa9mb5MuZt32rrh1xdv9BHsZRekjV1pG7vKUFfh3zQ"
        );
        assert_eq!(
            sign_transaction(&key, &transaction).unwrap(),
            "AD/oi75Ej5n7afbtUlOwn/PWPU8ucH6AOdSOg7jlK+42hap5YxmcvPuLac3AFdRurEK32fXN1DF8klrhnQU+ygbyudqDNtuAQiO92/MRbv2htLRk0YZaU9P3Fap0PtVATw=="
        );
        assert_eq!(
            sign_personal_message(&key, b"hello from agent-saifu").unwrap(),
            "AMGEiu/BIjnAdy//IpMQ0Nzjnbe/XfIYHcD+j4mQWs4kgaIHucGZAWdaIbIaYncBrhyH0wn3uT+Izb6ntI04mgHyudqDNtuAQiO92/MRbv2htLRk0YZaU9P3Fap0PtVATw=="
        );
    }

    #[test]
    fn messages_parse_in_three_formats() {
        assert_eq!(parse_message("hi", None).unwrap(), b"hi");
        assert_eq!(parse_message("hi", Some("TEXT")).unwrap(), b"hi");
        assert_eq!(parse_message("0x6869", Some("hex")).unwrap(), b"hi");
        assert_eq!(parse_message("aGk=", Some("base64")).unwrap(), b"hi");
        assert!(parse_message("", None).is_err());
        assert!(parse_message("zz", Some("hex")).is_err());
        assert!(parse_message("???", Some("base64")).is_err());
        assert!(parse_message("a", Some("rot13")).is_err());
        assert!(parse_message(&"a".repeat(MAX_MESSAGE_BYTES + 1), None).is_err());
    }

    #[test]
    fn describing_lists_commands_and_warns() {
        let sui = holdings(&[10_000_000_000], 0);
        let (tx, _) = assemble(
            addr(1),
            &env(),
            5_000_000,
            &sui,
            SUI_COIN_TYPE,
            7,
            &sui,
            &transfer_body(addr(2)),
        )
        .unwrap();
        let (summary, warnings) = describe(&tx, &addr(1));
        assert!(warnings.is_empty());
        let commands = summary.get("commands").and_then(Value::as_array).unwrap();
        assert_eq!(commands.len(), 2);
        assert!(commands[0].as_str().unwrap().starts_with("split"));
        assert!(commands[1].as_str().unwrap().starts_with("transfer"));
        let (_, warnings) = describe(&tx, &addr(9));
        assert!(warnings.iter().any(|w| w.contains("not this wallet")));
    }

    #[test]
    fn describing_flags_gas_coin_transfers_and_publishing() {
        let mut tx = TransactionBuilder::new();
        let gas = tx.gas();
        let to = tx.pure(&addr(2));
        tx.transfer_objects(vec![gas], to);
        let _ = tx.publish(vec![vec![1, 2, 3]], vec![]);
        tx.set_sender(addr(1));
        tx.set_gas_budget(1_000_000);
        tx.set_gas_price(1000);
        tx.add_gas_objects([object_input(&coin(1, 5))]);
        let transaction = tx.try_build().unwrap();
        let (_, warnings) = describe(&transaction, &addr(1));
        assert!(warnings.iter().any(|w| w.contains("gas coin")));
        assert!(warnings.iter().any(|w| w.contains("publishes")));
    }

    #[test]
    fn only_transactions_from_this_wallet_may_be_signed() {
        let sui = holdings(&[10_000_000_000], 0);
        let (tx, _) = assemble(
            addr(1),
            &env(),
            5_000_000,
            &sui,
            SUI_COIN_TYPE,
            7,
            &sui,
            &transfer_body(addr(2)),
        )
        .unwrap();
        assert!(check_signable(&tx, &addr(1)).is_ok());
        assert!(
            check_signable(&tx, &addr(2))
                .unwrap_err()
                .contains("sender")
        );
        let mut sponsored = tx.clone();
        sponsored.gas_payment.owner = addr(7);
        assert!(
            check_signable(&sponsored, &addr(1))
                .unwrap_err()
                .contains("gas owner")
        );
    }

    #[test]
    fn wallet_changes_sum_per_coin_type_for_the_wallet_only() {
        use crate::sui::gql::BalanceChange;
        let me = addr(1);
        let change = |owner: &Address, t: &str, a: i128| BalanceChange {
            owner: owner.to_string(),
            coin_type: t.to_string(),
            amount: a,
        };
        let effects = Effects {
            digest: None,
            success: true,
            error: None,
            gas: GasSummary::default(),
            balance_changes: vec![
                change(&me, SUI_COIN_TYPE, -5),
                change(&addr(2), SUI_COIN_TYPE, 3),
                change(&me, SUI_COIN_TYPE, -2),
                change(&me, USDC, 9),
            ],
        };
        assert_eq!(
            wallet_changes(&effects, &me),
            vec![(SUI_COIN_TYPE.to_string(), -7), (USDC.to_string(), 9)]
        );
    }
}
