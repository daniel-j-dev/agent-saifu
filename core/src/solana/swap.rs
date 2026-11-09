//! Swaps on Solana through Jupiter: request building, response parsing and safety checks.
//!
//! Jupiter's keyless `/build` endpoint (`<base>/build`, set in `swap_config.rs`) answers with the
//! instructions of a swap, not with a transaction. The wallet assembles the transaction itself
//! (its own blockhash, compute budget and, when a fee is configured, the fee recipient's token
//! account), and never trusts the result: the transaction is checked structurally and then
//! simulated, and it is only signed when the simulation shows the wallet spends no more than
//! requested and receives at least the quoted minimum (see `check_effects`).

use super::address::{self, Pubkey};
use super::describe::{self, Op};
use super::effects::Effects;
use super::tokens;
use super::tx::{self, AccountMeta, Instruction, LookupTable, Transaction};
use crate::crypto::base64;
use crate::eth::explorer::sanitize_text_max;
use crate::eth::swap::{
    MAX_FEE_BPS, MAX_PRICE_IMPACT_PERCENT, MAX_SLIPPAGE_BPS, MIN_FEE_BPS, MIN_SLIPPAGE_BPS,
    encode_query,
};
use crate::http::HttpClient;
use crate::json::{self, Value};
use crate::swap_config;

pub use crate::eth::swap::DEFAULT_SLIPPAGE_BPS;

/// SOL a swap may spend on top of the sold amount: network fees plus deposits for token
/// accounts it creates (about 0.002 SOL each).
const ACCOUNT_DEPOSIT_ALLOWANCE: u64 = 2_100_000;
/// The quote's guaranteed minimum may be this much looser than the requested slippage.
const SLIPPAGE_TOLERANCE_BPS: u32 = 50;
const MAX_INSTRUCTION_ACCOUNTS: usize = 256;
const MAX_LOOKUP_TABLES: usize = 16;
const MAX_TABLE_ADDRESSES: usize = 256;

// ---- settings ----------------------------------------------------------------------------------

/// The fee charged on every Solana swap, paid to `recipient` (Jupiter's platform fee).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolanaSwapFee {
    pub recipient: Pubkey,
    pub bps: u32,
}

impl SolanaSwapFee {
    /// An empty recipient means "no fee". Anything else must be a valid Solana address.
    pub fn from_parts(recipient: &str, bps: u32) -> Result<Option<SolanaSwapFee>, String> {
        let recipient = recipient.trim();
        if recipient.is_empty() {
            return Ok(None);
        }
        let key = address::parse(recipient)
            .map_err(|e| format!("SOLANA_SWAP_FEE_RECIPIENT in swap_config.rs is invalid: {e}"))?;
        if address::is_burn_or_program_address(&key) {
            return Err(
                "SOLANA_SWAP_FEE_RECIPIENT in swap_config.rs must not be a program or the system program address"
                    .to_string(),
            );
        }
        if !(MIN_FEE_BPS..=MAX_FEE_BPS).contains(&bps) {
            return Err(format!(
                "SOLANA_SWAP_FEE_BPS in swap_config.rs must be between {MIN_FEE_BPS} and {MAX_FEE_BPS}"
            ));
        }
        Ok(Some(SolanaSwapFee {
            recipient: key,
            bps,
        }))
    }

    pub fn from_config() -> Result<Option<SolanaSwapFee>, String> {
        SolanaSwapFee::from_parts(
            swap_config::SOLANA_SWAP_FEE_RECIPIENT,
            swap_config::SOLANA_SWAP_FEE_BPS,
        )
    }

    /// The fee taken from a swap that delivers `received` after the fee: the fee is a share of
    /// the gross amount, so `received * bps / (10_000 - bps)`, rounded down.
    pub fn estimate_for(&self, received: u64) -> u64 {
        let rest = 10_000u128.saturating_sub(u128::from(self.bps)).max(1);
        u64::try_from(u128::from(received) * u128::from(self.bps) / rest).unwrap_or(u64::MAX)
    }
}

#[derive(Debug, Clone)]
pub struct SolanaSwapSettings {
    pub fee: Option<SolanaSwapFee>,
    pub base_url: String,
}

impl Default for SolanaSwapSettings {
    fn default() -> Self {
        SolanaSwapSettings {
            fee: None,
            base_url: swap_config::JUPITER_API_BASE_URL.to_string(),
        }
    }
}

impl SolanaSwapSettings {
    pub fn new(fee: Option<SolanaSwapFee>) -> SolanaSwapSettings {
        SolanaSwapSettings {
            fee,
            ..SolanaSwapSettings::default()
        }
    }

    pub fn from_config() -> Result<SolanaSwapSettings, String> {
        Ok(SolanaSwapSettings::new(SolanaSwapFee::from_config()?))
    }
}

pub fn validate_slippage(bps: Option<u64>) -> Result<u32, String> {
    let bps = bps.unwrap_or(u64::from(DEFAULT_SLIPPAGE_BPS));
    u32::try_from(bps)
        .ok()
        .filter(|b| (MIN_SLIPPAGE_BPS..=MAX_SLIPPAGE_BPS).contains(b))
        .ok_or_else(|| {
            format!("'slippage_bps' must be between {MIN_SLIPPAGE_BPS} and {MAX_SLIPPAGE_BPS}")
        })
}

// ---- requests ----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapRequest {
    /// Token mints (the wrapped SOL mint for native SOL).
    pub in_token: Pubkey,
    pub out_token: Pubkey,
    pub amount: u64,
    pub slippage_bps: u32,
    pub taker: Pubkey,
    /// Limits how many accounts the route may use, to leave room in the transaction.
    pub max_accounts: Option<u32>,
}

/// The `/build` URL. A fee is passed as the token account that collects it (which must be an
/// account for the token being bought) and its size in basis points.
pub fn build_url(base: &str, req: &SwapRequest, fee: Option<(&Pubkey, u32)>) -> String {
    let mut q: Vec<(&str, String)> = vec![
        ("inputMint", address::to_text(&req.in_token)),
        ("outputMint", address::to_text(&req.out_token)),
        ("amount", req.amount.to_string()),
        ("taker", address::to_text(&req.taker)),
        ("slippageBps", req.slippage_bps.to_string()),
    ];
    if let Some(max) = req.max_accounts {
        q.push(("maxAccounts", max.to_string()));
    }
    if let Some((account, bps)) = fee {
        q.push(("platformFeeBps", bps.to_string()));
        q.push(("feeAccount", address::to_text(account)));
    }
    format!("{}/build?{}", base.trim_end_matches('/'), encode_query(&q))
}

/// Turns transport errors into readable, bounded messages.
pub fn api_error(e: &str) -> String {
    let detail = sanitize_text_max(e, 300);
    if e.contains("Cloudflare") || e.contains("HTTP status 403") {
        "Jupiter refused this request (HTTP 403). Its keyless API can challenge automated traffic from some networks. Try again later or from a different network. Nothing was sent.".to_string()
    } else if e.contains("HTTP status 429") {
        "Jupiter's keyless limit (about one request every two seconds) was hit; wait a few seconds and try again".to_string()
    } else {
        format!("Jupiter API error: {detail}")
    }
}

pub fn fetch(http: &dyn HttpClient, url: &str) -> Result<String, String> {
    http.get(url).map_err(|e| api_error(&e))
}

// ---- responses ---------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub in_token: Pubkey,
    pub out_token: Pubkey,
    pub in_amount: u64,
    /// What the wallet should receive, after any platform fee.
    pub out_amount: u64,
    pub min_out_amount: u64,
    /// Signed percentage, e.g. `-0.05`.
    pub price_impact: Option<f64>,
    pub route: Vec<String>,
    /// Setup, swap, cleanup and other instructions in the order to run them. The compute budget
    /// is the wallet's own business and is left out.
    pub instructions: Vec<Instruction>,
    /// The accounts of the swap instruction itself.
    pub swap_accounts: Vec<Pubkey>,
    /// Address lookup tables the swap's accounts may be loaded from, as Jupiter lists them.
    pub tables: Vec<LookupTable>,
    /// Jupiter's suggested priority fee, in micro-lamports per compute unit.
    pub compute_price: Option<u64>,
}

fn present(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !matches!(x, Value::Null))
}

fn float(v: Option<&Value>) -> Option<f64> {
    let text = match present(v)? {
        Value::String(s) | Value::Number(s) => s.as_str(),
        _ => return None,
    };
    text.trim().parse::<f64>().ok().filter(|f| f.is_finite())
}

fn pubkey(v: Option<&Value>, what: &str) -> Result<Pubkey, String> {
    present(v)
        .and_then(Value::as_str)
        .and_then(|a| address::parse(a).ok())
        .ok_or_else(|| format!("Jupiter's response has no valid {what}"))
}

fn route_names(v: &Value) -> Vec<String> {
    let mut names: Vec<String> = vec![];
    for step in v
        .get("routePlan")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let label = step
            .get("swapInfo")
            .and_then(|i| i.get("label"))
            .and_then(Value::as_str)
            .map(|l| sanitize_text_max(l, 40))
            .unwrap_or_default();
        if !label.is_empty() && !names.contains(&label) {
            names.push(label);
        }
    }
    names
}

fn parse_instruction(v: &Value, what: &str) -> Result<Instruction, String> {
    let program_id = pubkey(v.get("programId"), &format!("program in the {what}"))?;
    let accounts = present(v.get("accounts"))
        .and_then(Value::as_array)
        .ok_or_else(|| format!("Jupiter's {what} has no accounts"))?;
    if accounts.len() > MAX_INSTRUCTION_ACCOUNTS {
        return Err(format!("Jupiter's {what} has too many accounts"));
    }
    let mut metas = Vec::with_capacity(accounts.len());
    for account in accounts {
        let flag = |key: &str| {
            account
                .get(key)
                .and_then(Value::as_bool)
                .ok_or_else(|| format!("Jupiter's {what} has an account without '{key}'"))
        };
        metas.push(AccountMeta {
            pubkey: pubkey(account.get("pubkey"), &format!("account in the {what}"))?,
            is_signer: flag("isSigner")?,
            is_writable: flag("isWritable")?,
        });
    }
    let data = present(v.get("data"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Jupiter's {what} has no data"))
        .and_then(|d| {
            base64::decode(d).map_err(|_| format!("Jupiter's {what} has unreadable data"))
        })?;
    Ok(Instruction {
        program_id,
        accounts: metas,
        data,
    })
}

fn parse_list(v: &Value, key: &str) -> Result<Vec<Instruction>, String> {
    present(v.get(key))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|i| parse_instruction(i, key))
        .collect()
}

fn parse_tables(v: &Value) -> Result<Vec<LookupTable>, String> {
    let Some(Value::Object(entries)) = present(v.get("addressesByLookupTableAddress")) else {
        return Ok(vec![]);
    };
    if entries.len() > MAX_LOOKUP_TABLES {
        return Err("Jupiter's swap uses too many address lookup tables".to_string());
    }
    let mut tables = Vec::with_capacity(entries.len());
    for (table, addresses) in entries {
        let table = address::parse(table)
            .map_err(|_| "Jupiter listed an address lookup table with an invalid address")?;
        let addresses = addresses
            .as_array()
            .filter(|a| a.len() <= MAX_TABLE_ADDRESSES)
            .ok_or("Jupiter listed an address lookup table that cannot be read")?;
        let addresses = addresses
            .iter()
            .map(|a| {
                a.as_str()
                    .and_then(|a| address::parse(a).ok())
                    .ok_or("Jupiter listed an invalid address in a lookup table")
            })
            .collect::<Result<Vec<Pubkey>, _>>()?;
        tables.push((table, addresses));
    }
    Ok(tables)
}

/// The priority fee of the compute-budget instruction that sets one, if Jupiter sent it.
fn compute_price(v: &Value) -> Option<u64> {
    let budget = address::id(address::COMPUTE_BUDGET_PROGRAM);
    parse_list(v, "computeBudgetInstructions")
        .ok()?
        .iter()
        .filter(|i| i.program_id == budget)
        .find_map(|i| match i.data.as_slice() {
            [3, rest @ ..] if rest.len() == 8 => rest.try_into().ok().map(u64::from_le_bytes),
            _ => None,
        })
}

/// Parses a `/build` response. Error responses become messages.
pub fn parse_build(body: &str) -> Result<Quote, String> {
    let v = json::parse(body).map_err(|_| "Jupiter returned invalid JSON".to_string())?;
    if let Some(error) = present(v.get("error")).and_then(Value::as_str) {
        return Err(format!(
            "Jupiter could not build this swap: {}",
            sanitize_text_max(error, 200)
        ));
    }
    if v.get("swapMode").and_then(Value::as_str) != Some("ExactIn") {
        return Err("Jupiter did not answer with an exact-input swap".to_string());
    }
    let need = |key: &str| {
        present(v.get(key))
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("Jupiter's response is missing '{key}'"))
    };
    let swap = present(v.get("swapInstruction"))
        .ok_or("Jupiter did not return a swap instruction for this swap")?;
    let swap_instruction = parse_instruction(swap, "swap instruction")?;
    let budget = address::id(address::COMPUTE_BUDGET_PROGRAM);
    let mut instructions = parse_list(&v, "setupInstructions")?;
    let swap_accounts = swap_instruction.accounts.iter().map(|a| a.pubkey).collect();
    instructions.push(swap_instruction);
    if let Some(cleanup) = present(v.get("cleanupInstruction")) {
        instructions.push(parse_instruction(cleanup, "cleanup instruction")?);
    }
    instructions.extend(parse_list(&v, "otherInstructions")?);
    instructions.retain(|i| i.program_id != budget);
    Ok(Quote {
        in_token: pubkey(v.get("inputMint"), "input token")?,
        out_token: pubkey(v.get("outputMint"), "output token")?,
        in_amount: need("inAmount")?,
        out_amount: need("outAmount")?,
        min_out_amount: need("otherAmountThreshold")?,
        // Jupiter reports the impact as a fraction (-0.01 is -1%).
        price_impact: float(v.get("priceImpactPct")).map(|f| f * 100.0),
        route: route_names(&v),
        instructions,
        swap_accounts,
        tables: parse_tables(&v)?,
        compute_price: compute_price(&v),
    })
}

// ---- safety checks -----------------------------------------------------------------------------

/// What the caller asked for; the quote and its transaction are checked against it.
#[derive(Debug, Clone)]
pub struct Expectation {
    pub in_token: Pubkey,
    pub in_native: bool,
    pub out_token: Pubkey,
    pub out_native: bool,
    pub requested: u64,
    pub owner: Pubkey,
    pub slippage_bps: u32,
    pub min_receive: Option<u64>,
    /// The token account that must collect the fee, when one is configured.
    pub fee_account: Option<Pubkey>,
}

fn check_slippage(q: &Quote, e: &Expectation) -> Result<(), String> {
    let allowed = u128::from(e.slippage_bps + SLIPPAGE_TOLERANCE_BPS);
    let floor = u128::from(q.out_amount) * 10_000u128.saturating_sub(allowed) / 10_000;
    if u128::from(q.min_out_amount) < floor {
        return Err(format!(
            "refusing: the quote guarantees only {} base units, which is looser than the requested slippage allows",
            q.min_out_amount
        ));
    }
    Ok(())
}

/// Checks the quote's numbers against the request.
pub fn check_quote(q: &Quote, e: &Expectation) -> Result<(), String> {
    if q.in_token != e.in_token {
        return Err("refusing: the quote is for a different sell token than requested".to_string());
    }
    if q.out_token != e.out_token {
        return Err("refusing: the quote is for a different buy token than requested".to_string());
    }
    if q.in_amount == 0 || q.in_amount > e.requested {
        return Err(format!(
            "refusing: the quote would sell {} base units but {} were requested",
            q.in_amount, e.requested
        ));
    }
    if q.out_amount == 0 {
        return Err("the quote would return nothing".to_string());
    }
    if q.min_out_amount > q.out_amount {
        return Err("the quote's minimum exceeds its expected amount; refusing".to_string());
    }
    check_slippage(q, e)?;
    if let Some(floor) = e.min_receive
        && q.min_out_amount < floor
    {
        return Err(format!(
            "the guaranteed minimum ({}) is below your min_receive ({floor}); not swapping. Fetch a new quote or lower min_receive",
            q.min_out_amount
        ));
    }
    if let Some(impact) = q.price_impact
        && impact.abs() > MAX_PRICE_IMPACT_PERCENT
    {
        return Err(format!(
            "refusing: the price impact is {:.2}%, above the {MAX_PRICE_IMPACT_PERCENT}% safety limit",
            impact.abs()
        ));
    }
    if let Some(fee_account) = &e.fee_account
        && !q.swap_accounts.contains(fee_account)
    {
        return Err(
            "refusing: Jupiter's swap does not pay the configured fee account, so the swap fee would be lost"
                .to_string(),
        );
    }
    Ok(())
}

/// Checks the transaction's structure: the wallet pays and signs, nobody else's signature is
/// missing, and no instruction hands control of the wallet's funds to anyone.
pub fn check_transaction(
    transaction: &Transaction,
    owner: &Pubkey,
    accounts: &[Pubkey],
) -> Result<(), String> {
    let message = &transaction.message;
    if message.fee_payer() != owner {
        return Err("refusing: the swap transaction is built for a different wallet".to_string());
    }
    if !transaction.missing_signers(owner).is_empty() {
        return Err(
            "refusing: the swap transaction needs a signature from someone other than this wallet"
                .to_string(),
        );
    }
    let ops = describe::classify(message, accounts);
    if ops.iter().any(|op| matches!(op, Op::Unresolved)) {
        return Err(
            "refusing: the swap transaction uses accounts that could not be read".to_string(),
        );
    }
    let reasons = describe::forbidden_in_swap(&ops, owner);
    if let Some(reason) = reasons.first() {
        return Err(format!(
            "refusing: the swap transaction is unsafe: {reason}"
        ));
    }
    Ok(())
}

/// SOL the transaction may cost beyond the sold amount: fees, plus a deposit for every token
/// account it creates.
pub fn sol_allowance(transaction: &Transaction, accounts: &[Pubkey]) -> u64 {
    let creates = describe::classify(&transaction.message, accounts)
        .iter()
        .filter(|op| matches!(op, Op::CreateAssociatedToken { .. }))
        .count() as u64;
    tx::fee_estimate(&transaction.message, accounts)
        .saturating_add(creates.saturating_mul(ACCOUNT_DEPOSIT_ALLOWANCE))
}

/// Checks the simulated outcome: the wallet spends no more than requested, receives at least
/// the quote's guaranteed minimum, and loses nothing else.
pub fn check_effects(
    effects: &Effects,
    q: &Quote,
    e: &Expectation,
    allowance: u64,
) -> Result<(), String> {
    if let Some(error) = &effects.error {
        return Err(format!(
            "the swap would fail, so nothing was signed: {error}"
        ));
    }
    if let Some(warning) = effects.warnings.first() {
        return Err(format!("refusing: the swap would also do this: {warning}"));
    }
    let wsol = address::id(address::WRAPPED_SOL_MINT);
    let native_change = effects.sol_change + effects.token_change(&wsol);
    let (spent_floor, received_floor) = (i128::from(e.requested), i128::from(q.min_out_amount));
    let allowance = i128::from(allowance);
    // The leg in SOL counts the wrapped SOL account too, since swaps pass through it.
    if e.in_native {
        if native_change < -(spent_floor + allowance) {
            return Err("refusing: the swap would spend more SOL than requested".to_string());
        }
    } else if effects.token_change(&e.in_token) < -spent_floor {
        return Err("refusing: the swap would spend more of the token than requested".to_string());
    }
    if e.out_native {
        if native_change + allowance < received_floor {
            return Err(
                "refusing: the swap would return less SOL than the quote guarantees".to_string(),
            );
        }
    } else if effects.token_change(&e.out_token) < received_floor {
        return Err("refusing: the swap would return less than the quote guarantees".to_string());
    }
    if !e.in_native && !e.out_native && effects.sol_change < -allowance {
        return Err("refusing: the swap would spend SOL it was not asked to".to_string());
    }
    for change in &effects.token_changes {
        let leg = change.mint == e.in_token
            || change.mint == e.out_token
            || (change.mint == wsol && (e.in_native || e.out_native));
        if !leg && change.change < 0 {
            return Err(format!(
                "refusing: the swap would take {} from a token you did not ask to sell",
                tokens::token_label(&change.mint)
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solana::effects::TokenChange;
    use crate::solana::tx::{compile_legacy, system_transfer};

    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

    fn key(n: u8) -> Pubkey {
        [n; 32]
    }

    fn owner() -> Pubkey {
        key(1)
    }

    fn swap_tx(payer: &Pubkey) -> Transaction {
        let ix = system_transfer(payer, &key(2), 1);
        Transaction::unsigned(compile_legacy(payer, &[ix], [0u8; 32]).unwrap())
    }

    fn quote() -> Quote {
        Quote {
            in_token: address::id(address::WRAPPED_SOL_MINT),
            out_token: address::id(USDC),
            in_amount: 100_000_000,
            out_amount: 14_000_000,
            min_out_amount: 13_860_000,
            price_impact: Some(-0.12),
            route: vec!["Raydium".to_string()],
            instructions: vec![],
            swap_accounts: vec![key(40), key(41)],
            tables: vec![],
            compute_price: None,
        }
    }

    fn expectation() -> Expectation {
        Expectation {
            in_token: address::id(address::WRAPPED_SOL_MINT),
            in_native: true,
            out_token: address::id(USDC),
            out_native: false,
            requested: 100_000_000,
            owner: owner(),
            slippage_bps: 100,
            min_receive: None,
            fee_account: None,
        }
    }

    fn account_json(pubkey: &Pubkey, signer: bool, writable: bool) -> String {
        format!(
            r#"{{"pubkey":"{}","isSigner":{signer},"isWritable":{writable}}}"#,
            address::to_text(pubkey)
        )
    }

    fn instruction_json(program: &Pubkey, accounts: &[String], data: &[u8]) -> String {
        format!(
            r#"{{"programId":"{}","accounts":[{}],"data":"{}"}}"#,
            address::to_text(program),
            accounts.join(","),
            base64::encode(data)
        )
    }

    fn build_body() -> String {
        let budget = address::id(address::COMPUTE_BUDGET_PROGRAM);
        let mut price = vec![3u8];
        price.extend_from_slice(&7_000u64.to_le_bytes());
        let setup = instruction_json(
            &address::id(address::ASSOCIATED_TOKEN_PROGRAM),
            &[account_json(&owner(), true, true)],
            &[1],
        );
        let swap = instruction_json(
            &key(30),
            &[
                account_json(&owner(), true, false),
                account_json(&key(40), false, true),
                account_json(&key(41), false, false),
            ],
            &[9, 9],
        );
        let cleanup = instruction_json(
            &address::id(address::TOKEN_PROGRAM),
            &[account_json(&key(42), false, true)],
            &[9],
        );
        format!(
            r#"{{"inputMint":"{}","outputMint":"{USDC}","inAmount":"100000000","outAmount":"14000000","otherAmountThreshold":"13860000","swapMode":"ExactIn","slippageBps":100,"priceImpactPct":"-0.0012","routePlan":[{{"percent":60,"swapInfo":{{"label":"Raydium"}}}},{{"percent":40,"swapInfo":{{"label":"Orca"}}}},{{"percent":0,"swapInfo":{{"label":"Raydium"}}}}],"computeBudgetInstructions":[{}],"setupInstructions":[{setup}],"swapInstruction":{swap},"cleanupInstruction":{cleanup},"otherInstructions":[],"tipInstruction":null,"addressesByLookupTableAddress":{{"{}":["{}","{}"]}},"blockhashWithMetadata":{{"blockhash":[1],"lastValidBlockHeight":5}}}}"#,
            address::WRAPPED_SOL_MINT,
            instruction_json(&budget, &[], &price),
            address::to_text(&key(60)),
            address::to_text(&key(40)),
            address::to_text(&key(41)),
        )
    }

    #[test]
    fn fee_settings_validate() {
        assert_eq!(SolanaSwapFee::from_parts("", 70).unwrap(), None);
        assert_eq!(SolanaSwapFee::from_parts("  ", 0).unwrap(), None);
        let fee = SolanaSwapFee::from_parts(&address::to_text(&key(5)), 100)
            .unwrap()
            .unwrap();
        // 1% of the gross: 990_000 arrive, 10_000 go to the recipient
        assert_eq!(fee.estimate_for(990_000), 10_000);
        assert_eq!(fee.estimate_for(0), 0);
        for (recipient, bps) in [
            ("not an address", 70),
            (address::SYSTEM_PROGRAM, 70),
            (address::TOKEN_PROGRAM, 70),
            ("0xf8564095b5b0EdFc8f334b82BF04a57B27dcA4CD", 70),
        ] {
            assert!(
                SolanaSwapFee::from_parts(recipient, bps).is_err(),
                "{recipient}"
            );
        }
        assert!(SolanaSwapFee::from_parts(&address::to_text(&key(5)), 0).is_err());
        assert!(SolanaSwapFee::from_parts(&address::to_text(&key(5)), 501).is_err());
        assert!(SolanaSwapFee::from_parts(&address::to_text(&key(5)), 500).is_ok());
    }

    #[test]
    fn the_shipped_config_is_valid() {
        assert!(SolanaSwapSettings::from_config().is_ok());
        let url = SolanaSwapSettings::default().base_url;
        assert!(url.starts_with("https://") && url.contains("jup.ag"));
    }

    #[test]
    fn slippage_is_bounded() {
        assert_eq!(validate_slippage(None).unwrap(), 100);
        assert_eq!(validate_slippage(Some(5)).unwrap(), 5);
        assert_eq!(validate_slippage(Some(1000)).unwrap(), 1000);
        for bad in [0u64, 4, 1001, u64::MAX] {
            assert!(validate_slippage(Some(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn builds_the_swap_url() {
        let req = SwapRequest {
            in_token: address::id(address::WRAPPED_SOL_MINT),
            out_token: address::id(USDC),
            amount: 100_000_000,
            slippage_bps: 50,
            taker: owner(),
            max_accounts: None,
        };
        let url = build_url("https://host/swap/v2/", &req, None);
        assert!(url.starts_with("https://host/swap/v2/build?"));
        assert!(url.contains("inputMint=So11111111111111111111111111111111111111112"));
        assert!(url.contains(&format!("outputMint={USDC}")));
        assert!(url.contains("amount=100000000"));
        assert!(url.contains("slippageBps=50"));
        assert!(url.contains(&format!("taker={}", address::to_text(&owner()))));
        assert!(!url.contains("platformFeeBps") && !url.contains("feeAccount"));
        assert!(!url.contains("maxAccounts"));
        let capped = SwapRequest {
            max_accounts: Some(40),
            ..req.clone()
        };
        assert!(build_url("https://host/swap/v2", &capped, None).contains("maxAccounts=40"));
        let with_fee = build_url("https://host/swap/v2", &req, Some((&key(5), 70)));
        assert!(with_fee.contains("platformFeeBps=70"));
        assert!(with_fee.contains(&format!("feeAccount={}", address::to_text(&key(5)))));
    }

    #[test]
    fn parses_a_build_response() {
        let q = parse_build(&build_body()).unwrap();
        assert_eq!(q.in_token, address::id(address::WRAPPED_SOL_MINT));
        assert_eq!(q.out_token, address::id(USDC));
        assert_eq!(q.in_amount, 100_000_000);
        assert_eq!(q.out_amount, 14_000_000);
        assert_eq!(q.min_out_amount, 13_860_000);
        assert!((q.price_impact.unwrap() + 0.12).abs() < 1e-9);
        assert_eq!(q.route, ["Raydium", "Orca"]);
        assert_eq!(q.compute_price, Some(7_000));
        // setup, swap, cleanup; the compute budget is left out
        assert_eq!(q.instructions.len(), 3);
        assert_eq!(q.instructions[1].program_id, key(30));
        assert_eq!(q.instructions[1].data, [9, 9]);
        assert_eq!(
            q.instructions[1].accounts[0],
            AccountMeta::readonly(owner(), true)
        );
        assert_eq!(q.swap_accounts, [owner(), key(40), key(41)]);
        assert_eq!(q.tables, [(key(60), vec![key(40), key(41)])]);
    }

    #[test]
    fn compute_budget_instructions_are_never_passed_on() {
        let budget = address::id(address::COMPUTE_BUDGET_PROGRAM);
        let extra = instruction_json(&budget, &[], &[2, 1, 0, 0, 0]);
        let body = build_body().replace(
            r#""otherInstructions":[]"#,
            &format!(r#""otherInstructions":[{extra}]"#),
        );
        let q = parse_build(&body).unwrap();
        assert!(q.instructions.iter().all(|i| i.program_id != budget));
        let without_budget = build_body().replace(
            r#""computeBudgetInstructions":["#,
            r#""computeBudgetInstructions_":["#,
        );
        assert_eq!(parse_build(&without_budget).unwrap().compute_price, None);
    }

    #[test]
    fn explains_error_responses() {
        let failed = parse_build(r#"{"error":"No routes found"}"#).unwrap_err();
        assert!(failed.contains("No routes found"), "{failed}");
        for bad in ["not json", "{}", r#"{"swapMode":"ExactOut"}"#] {
            assert!(parse_build(bad).is_err(), "{bad}");
        }
        let good = build_body();
        for (from, to) in [
            (r#""inAmount":"100000000""#, r#""inAmount_":"1""#),
            (r#""otherAmountThreshold""#, r#""other_""#),
            (r#""swapInstruction""#, r#""swap_""#),
            (r#""inputMint""#, r#""input_""#),
            (r#""isWritable":true"#, r#""isWritable":"yes""#),
        ] {
            assert!(parse_build(&good.replacen(from, to, 1)).is_err(), "{from}");
        }
        let junk = good.replacen(&base64::encode(&[9, 9]), "***", 1);
        assert!(parse_build(&junk).unwrap_err().contains("unreadable"));
        let bad_table = good.replacen(&address::to_text(&key(60)), "nope", 1);
        assert!(parse_build(&bad_table).is_err());
        let long = format!(r#"{{"error":"{}"}}"#, "e".repeat(2000));
        assert!(parse_build(&long).unwrap_err().chars().count() < 400);
    }

    #[test]
    fn transport_errors_are_readable() {
        assert!(api_error("HTTP status 429: slow down").contains("keyless limit"));
        assert!(api_error("HTTP status 403 (blocked by Cloudflare)").contains("refused"));
        let other = api_error(&format!("HTTP status 500: {}", "x".repeat(2000)));
        assert!(other.starts_with("Jupiter API error") && other.chars().count() < 400);
    }

    #[test]
    fn a_matching_quote_passes() {
        assert!(check_quote(&quote(), &expectation()).is_ok());
    }

    #[test]
    fn mismatching_quotes_are_refused() {
        let e = expectation();
        type Change = Box<dyn Fn(&mut Quote)>;
        let cases: Vec<(&str, Change)> = vec![
            ("sell token", Box::new(|q| q.in_token = key(9))),
            ("buy token", Box::new(|q| q.out_token = key(9))),
            ("sells more", Box::new(|q| q.in_amount = 100_000_001)),
            ("sells nothing", Box::new(|q| q.in_amount = 0)),
            ("returns nothing", Box::new(|q| q.out_amount = 0)),
            (
                "min above expected",
                Box::new(|q| q.min_out_amount = q.out_amount + 1),
            ),
            (
                "loose slippage",
                Box::new(|q| q.min_out_amount = q.out_amount / 2),
            ),
            ("impact", Box::new(|q| q.price_impact = Some(-5.1))),
        ];
        for (name, change) in cases {
            let mut q = quote();
            change(&mut q);
            assert!(check_quote(&q, &e).is_err(), "{name}");
        }
        let mut floor = expectation();
        floor.min_receive = Some(13_860_001);
        assert!(
            check_quote(&quote(), &floor)
                .unwrap_err()
                .contains("min_receive")
        );
        floor.min_receive = Some(13_860_000);
        assert!(check_quote(&quote(), &floor).is_ok());
    }

    #[test]
    fn selling_less_than_requested_is_accepted() {
        let mut q = quote();
        q.in_amount = 99_000_000;
        assert!(check_quote(&q, &expectation()).is_ok());
    }

    #[test]
    fn the_fee_account_must_be_paid() {
        let mut e = expectation();
        e.fee_account = Some(key(41));
        assert!(check_quote(&quote(), &e).is_ok());
        e.fee_account = Some(key(77));
        assert!(
            check_quote(&quote(), &e)
                .unwrap_err()
                .contains("fee would be lost")
        );
    }

    #[test]
    fn transactions_must_be_for_this_wallet_and_signable_by_it() {
        let accounts = |t: &Transaction| t.message.account_keys.clone();
        let good = swap_tx(&owner());
        assert!(check_transaction(&good, &owner(), &accounts(&good)).is_ok());
        let other = swap_tx(&key(9));
        assert!(check_transaction(&other, &owner(), &accounts(&other)).is_err());

        // a second signer who has not signed
        let ix = tx::Instruction {
            program_id: key(99),
            accounts: vec![
                tx::AccountMeta::readonly(owner(), true),
                tx::AccountMeta::readonly(key(7), true),
            ],
            data: vec![],
        };
        let two = Transaction::unsigned(compile_legacy(&owner(), &[ix], [0u8; 32]).unwrap());
        assert!(
            check_transaction(&two, &owner(), &accounts(&two))
                .unwrap_err()
                .contains("someone other")
        );
        let mut signed = two.clone();
        signed.signatures[1] = [3u8; 64];
        assert!(check_transaction(&signed, &owner(), &accounts(&signed)).is_ok());
    }

    #[test]
    fn approvals_and_unresolved_accounts_are_refused() {
        let mut data = vec![4u8];
        data.extend_from_slice(&u64::MAX.to_le_bytes());
        let approve = tx::Instruction {
            program_id: address::id(address::TOKEN_PROGRAM),
            accounts: vec![
                tx::AccountMeta::writable(key(3), false),
                tx::AccountMeta::readonly(key(9), false),
                tx::AccountMeta::readonly(owner(), true),
            ],
            data,
        };
        let t = Transaction::unsigned(compile_legacy(&owner(), &[approve], [0u8; 32]).unwrap());
        let err = check_transaction(&t, &owner(), &t.message.account_keys).unwrap_err();
        assert!(err.contains("unsafe"), "{err}");
        let plain = swap_tx(&owner());
        assert!(
            check_transaction(&plain, &owner(), &plain.message.account_keys[..1])
                .unwrap_err()
                .contains("could not be read")
        );
    }

    #[test]
    fn allowance_counts_fees_and_new_accounts() {
        let ixs = [
            tx::set_compute_unit_limit(200_000),
            tx::set_compute_unit_price(5_000),
            tx::create_associated_token_account_idempotent(
                &owner(),
                &key(5),
                &owner(),
                &address::id(USDC),
                &address::id(address::TOKEN_PROGRAM),
            ),
        ];
        let t = Transaction::unsigned(compile_legacy(&owner(), &ixs, [0u8; 32]).unwrap());
        assert_eq!(
            sol_allowance(&t, &t.message.account_keys),
            5_000 + 1_000 + ACCOUNT_DEPOSIT_ALLOWANCE
        );
    }

    fn effects(sol: i128, changes: &[(Pubkey, i128)]) -> Effects {
        Effects {
            sol_change: sol,
            token_changes: changes
                .iter()
                .map(|(mint, change)| TokenChange {
                    mint: *mint,
                    change: *change,
                    accounts: 1,
                })
                .collect(),
            ..Effects::default()
        }
    }

    #[test]
    fn simulated_outcomes_must_match_the_quote() {
        let (q, e) = (quote(), expectation());
        let usdc = address::id(USDC);
        let allowance = 10_000;
        // sells 0.1 SOL (plus the fee), receives the expected USDC
        let good = effects(-100_005_000, &[(usdc, 14_000_000)]);
        assert!(check_effects(&good, &q, &e, allowance).is_ok());
        let exact_min = effects(-100_005_000, &[(usdc, 13_860_000)]);
        assert!(check_effects(&exact_min, &q, &e, allowance).is_ok());

        let cases = [
            (
                "spends too much SOL",
                effects(-100_015_001, &[(usdc, 14_000_000)]),
            ),
            (
                "receives too little",
                effects(-100_005_000, &[(usdc, 13_859_999)]),
            ),
            (
                "loses another token",
                effects(-100_005_000, &[(usdc, 14_000_000), (key(9), -1)]),
            ),
        ];
        for (name, bad) in cases {
            assert!(check_effects(&bad, &q, &e, allowance).is_err(), "{name}");
        }
        let gains_elsewhere = effects(-100_005_000, &[(usdc, 14_000_000), (key(9), 5)]);
        assert!(check_effects(&gains_elsewhere, &q, &e, allowance).is_ok());

        let mut failed = good.clone();
        failed.error = Some("boom".into());
        assert!(
            check_effects(&failed, &q, &e, allowance)
                .unwrap_err()
                .contains("boom")
        );
        let mut warned = good.clone();
        warned.warnings.push("a delegate".into());
        assert!(
            check_effects(&warned, &q, &e, allowance)
                .unwrap_err()
                .contains("delegate")
        );
    }

    #[test]
    fn token_for_sol_swaps_check_both_legs() {
        let usdc = address::id(USDC);
        let wsol = address::id(address::WRAPPED_SOL_MINT);
        let mut e = expectation();
        e.in_native = false;
        e.in_token = usdc;
        e.requested = 14_000_000;
        e.out_native = true;
        e.out_token = wsol;
        let mut q = quote();
        q.in_token = usdc;
        q.out_token = wsol;
        q.in_amount = 14_000_000;
        q.out_amount = 100_000_000;
        q.min_out_amount = 99_000_000;
        assert!(check_quote(&q, &e).is_ok());
        // SOL arrives (net of fee) and USDC leaves
        let good = effects(99_995_000, &[(usdc, -14_000_000)]);
        assert!(check_effects(&good, &q, &e, 10_000).is_ok());
        // ... or arrives as wrapped SOL
        let wrapped = effects(-5_000, &[(usdc, -14_000_000), (wsol, 99_500_000)]);
        assert!(check_effects(&wrapped, &q, &e, 10_000).is_ok());
        let short = effects(80_000_000, &[(usdc, -14_000_000)]);
        assert!(check_effects(&short, &q, &e, 10_000).is_err());
        let too_much_usdc = effects(99_995_000, &[(usdc, -14_000_001)]);
        assert!(check_effects(&too_much_usdc, &q, &e, 10_000).is_err());
    }

    #[test]
    fn token_for_token_swaps_may_not_burn_sol() {
        let usdc = address::id(USDC);
        let other = key(9);
        let mut e = expectation();
        e.in_native = false;
        e.in_token = usdc;
        e.requested = 1_000_000;
        e.out_token = other;
        let mut q = quote();
        q.in_token = usdc;
        q.out_token = other;
        q.in_amount = 1_000_000;
        q.out_amount = 5_000_000;
        q.min_out_amount = 4_950_000;
        assert!(check_quote(&q, &e).is_ok());
        let good = effects(-5_000, &[(usdc, -1_000_000), (other, 5_000_000)]);
        assert!(check_effects(&good, &q, &e, 10_000).is_ok());
        let drained = effects(-1_000_000_000, &[(usdc, -1_000_000), (other, 5_000_000)]);
        assert!(check_effects(&drained, &q, &e, 10_000).is_err());
    }
}
