//! Solana swaps through Jupiter.
//!
//! Jupiter's keyless `/build` endpoint answers with swap instructions. The wallet assembles the
//! transaction itself and does not trust the result: the quote is checked against the request,
//! the transaction is checked structurally, and it is simulated; it is only signed when the
//! simulation shows the wallet spends no more than requested, receives at least the quoted
//! minimum and loses nothing else (see `solana::swap::check_effects`). The configured fee is
//! asked of Jupiter as a platform fee, paid into the fee recipient's token account for the
//! token bought, which the transaction opens when it does not exist yet.

use super::solana::{MAX_PRIORITY_MICRO_LAMPORTS, Session, compute_limit_for};
use super::*;
use crate::solana::address::{self as saddr, Pubkey};
use crate::solana::effects::{self, Effects};
use crate::solana::swap as ssw;
use crate::solana::tokens;
use crate::solana::tx::{self, Instruction, Transaction};

/// Compute-unit limit of a quote that was not dry-run, for the fee estimate only.
const UNSIMULATED_COMPUTE_UNITS: u32 = 600_000;
/// Room kept for the priority-fee instruction that is added once the dry run is done.
const PRICE_INSTRUCTION_BYTES: usize = 16;
/// How many accounts the route may use, tried in turn until the transaction fits.
const ROUTE_ACCOUNT_LIMITS: [Option<u32>; 3] = [None, Some(40), Some(28)];

struct Leg {
    mint: Pubkey,
    native: bool,
    symbol: String,
    decimals: u32,
    /// The token program that owns the mint.
    program: Pubkey,
}

impl Leg {
    fn token_json(&self) -> Value {
        if self.native {
            Value::str("native")
        } else {
            Value::String(saddr::to_text(&self.mint))
        }
    }
}

/// Where the configured fee goes: the recipient's token account for the token bought.
struct FeePlan {
    recipient: Pubkey,
    bps: u32,
    account: Pubkey,
    /// The transaction opens the account (the recipient has none yet).
    opens_account: bool,
}

struct SwapPlan {
    from: Leg,
    to: Leg,
    amount: u64,
    quote: ssw::Quote,
    transaction: Transaction,
    /// The wallet's balance of the sold asset, in base units.
    balance: u128,
    /// The dry run; `None` when it was not attempted (not enough balance).
    effects: Option<Effects>,
    fee_lamports: u64,
    fee: Option<FeePlan>,
}

impl WalletService {
    fn solana_leg(s: &Session<'_>, token: &str, field: &str) -> Result<Leg, String> {
        let text = token.trim();
        if text.eq_ignore_ascii_case("native") || text.eq_ignore_ascii_case("sol") {
            return Ok(Leg {
                mint: saddr::id(saddr::WRAPPED_SOL_MINT),
                native: true,
                symbol: "SOL".to_string(),
                decimals: tx::SOL_DECIMALS,
                program: saddr::id(saddr::TOKEN_PROGRAM),
            });
        }
        let mint = saddr::parse(text).map_err(|e| {
            format!("'{field}': use \"native\" for SOL or a token mint address ({e})")
        })?;
        if mint == saddr::id(saddr::WRAPPED_SOL_MINT) {
            return Err(format!(
                "'{field}': use \"native\" for SOL; swaps of wrapped SOL tokens are not supported"
            ));
        }
        let (decimals, program) = Self::solana_mint_info(s, &mint)?;
        Ok(Leg {
            mint,
            native: false,
            symbol: tokens::token_label(&mint),
            decimals: u32::from(decimals),
            program,
        })
    }

    fn solana_swap_balance(s: &Session<'_>, from: &Leg) -> Result<u128, String> {
        if from.native {
            return Ok(u128::from(s.rpc.balance(&s.owner)?));
        }
        Ok(s.rpc
            .token_accounts_for_mint(&s.owner, &from.mint)?
            .iter()
            .map(|t| u128::from(t.amount))
            .sum())
    }

    /// The fee recipient's token account for the token bought, and whether it has to be opened.
    fn solana_fee_plan(&self, s: &Session<'_>, to: &Leg) -> Result<Option<FeePlan>, String> {
        let Some(fee) = &self.solana_swap.fee else {
            return Ok(None);
        };
        let account = saddr::associated_token_address(&fee.recipient, &to.mint, &to.program)?;
        let opens_account = match s.rpc.account(&account)? {
            Some(existing) if existing.owner == to.program => false,
            Some(_) => {
                return Err("the fee recipient's token account address is taken by something that is not a token account; cannot swap with a fee".to_string());
            }
            None => true,
        };
        Ok(Some(FeePlan {
            recipient: fee.recipient,
            bps: fee.bps,
            account,
            opens_account,
        }))
    }

    fn solana_swap_plan(
        &self,
        s: &Session<'_>,
        p: &SwapParams,
        require_balance: bool,
    ) -> Result<SwapPlan, String> {
        if p.from_chain_id != 0 || p.to_chain_id != 0 {
            return Err("from_chain_id and to_chain_id are Ethereum parameters; a Solana wallet swaps on the one cluster it is bound to".to_string());
        }
        if p.to_address.is_some() {
            return Err("'to_address' is not supported for Solana swaps: the bought tokens always arrive in this wallet".to_string());
        }
        if !s.network.is_mainnet() {
            return Err(format!(
                "Jupiter swaps are only available on Solana mainnet; this wallet is bound to {}",
                s.network.id()
            ));
        }
        let from = Self::solana_leg(s, &p.from_token, "from_token")?;
        let to = Self::solana_leg(s, &p.to_token, "to_token")?;
        if from.mint == to.mint {
            return Err("from_token and to_token are the same token".to_string());
        }
        let amount =
            tokens::parse_amount(&p.amount, from.decimals).map_err(|e| format!("'amount': {e}"))?;
        let slippage_bps = ssw::validate_slippage(p.slippage_bps)?;
        let min_receive = match &p.min_receive {
            Some(m) => Some(
                tokens::parse_amount(m, to.decimals).map_err(|e| format!("'min_receive': {e}"))?,
            ),
            None => None,
        };
        let balance = Self::solana_swap_balance(s, &from)?;
        if require_balance && balance < u128::from(amount) {
            return Err(format!(
                "insufficient {} balance: the wallet holds {} but the swap needs {}",
                from.symbol,
                tokens::format_amount(balance, from.decimals),
                tokens::format_amount(u128::from(amount), from.decimals)
            ));
        }
        let fee = self.solana_fee_plan(s, &to)?;
        let expect = ssw::Expectation {
            in_token: from.mint,
            in_native: from.native,
            out_token: to.mint,
            out_native: to.native,
            requested: amount,
            owner: s.owner,
            slippage_bps,
            min_receive,
            fee_account: fee.as_ref().map(|f| f.account),
        };
        let (blockhash, _) = s.rpc.latest_blockhash()?;
        let simulatable = balance >= u128::from(amount);
        let first_limit = if simulatable {
            u32::try_from(tx::MAX_COMPUTE_UNITS).unwrap_or(u32::MAX)
        } else {
            UNSIMULATED_COMPUTE_UNITS
        };
        // A route that leaves no room for the wallet's own instructions is asked again with fewer
        // accounts, which makes Jupiter pick a simpler route.
        let mut assembled = None;
        for max_accounts in ROUTE_ACCOUNT_LIMITS {
            let request = ssw::SwapRequest {
                in_token: from.mint,
                out_token: to.mint,
                amount,
                slippage_bps,
                taker: s.owner,
                max_accounts,
            };
            let url = ssw::build_url(
                &self.solana_swap.base_url,
                &request,
                fee.as_ref().map(|f| (&f.account, f.bps)),
            );
            let quote = ssw::parse_build(&ssw::fetch(&*self.http, &url)?)?;
            ssw::check_quote(&quote, &expect)?;
            let mut body: Vec<Instruction> = vec![];
            if let Some(f) = fee.as_ref().filter(|f| f.opens_account) {
                body.push(tx::create_associated_token_account_idempotent(
                    &s.owner,
                    &f.account,
                    &f.recipient,
                    &to.mint,
                    &to.program,
                ));
            }
            body.extend(quote.instructions.iter().cloned());
            let compile = |budget: Vec<Instruction>| {
                let mut all = budget;
                all.extend(body.iter().cloned());
                tx::compile_v0(&s.owner, &all, blockhash, &quote.tables)
                    .map(|(message, intended)| (Transaction::unsigned(message), intended))
            };
            let (draft, intended) = compile(vec![tx::set_compute_unit_limit(first_limit)])?;
            if draft.to_bytes().len() + PRICE_INSTRUCTION_BYTES <= tx::MAX_TX_BYTES {
                assembled = Some((quote, body, draft, intended));
                break;
            }
        }
        let Some((quote, body, draft, intended)) = assembled else {
            return Err("this swap's route is too large to fit in one Solana transaction; try a smaller amount or a different token pair".to_string());
        };
        let compile = |budget: Vec<Instruction>| {
            let mut all = budget;
            all.extend(body.iter().cloned());
            tx::compile_v0(&s.owner, &all, blockhash, &quote.tables)
                .map(|(message, intended)| (Transaction::unsigned(message), intended))
        };
        // The transaction names accounts through lookup tables. What matters is what those tables
        // really hold on chain, so the accounts Jupiter described must be exactly those.
        let accounts = effects::resolve_message_accounts(&s.rpc, &draft.message)?;
        if accounts != intended {
            return Err("refusing: Jupiter's address lookup tables do not match the ones on chain, so the swap would not do what it describes".to_string());
        }
        ssw::check_transaction(&draft, &s.owner, &accounts)?;
        let (effects, units) = if simulatable {
            let simulated = effects::simulate_effects(&s.rpc, &draft, &s.owner)?;
            let allowance = ssw::sol_allowance(&draft, &accounts);
            ssw::check_effects(&simulated, &quote, &expect, allowance)?;
            let units = compute_limit_for(
                simulated
                    .units_consumed
                    .unwrap_or(tx::DEFAULT_UNITS_PER_INSTRUCTION),
            );
            (Some(simulated), units)
        } else {
            (None, UNSIMULATED_COMPUTE_UNITS)
        };
        let price = quote
            .compute_price
            .unwrap_or_else(|| s.rpc.recent_priority_fee())
            .min(MAX_PRIORITY_MICRO_LAMPORTS);
        let mut budget = vec![tx::set_compute_unit_limit(units)];
        if price > 0 {
            budget.push(tx::set_compute_unit_price(price));
        }
        let (transaction, final_accounts) = compile(budget)?;
        if final_accounts != intended {
            return Err(
                "internal error: the swap's accounts changed while it was assembled".to_string(),
            );
        }
        if transaction.to_bytes().len() > tx::MAX_TX_BYTES {
            return Err("this swap's route is too large to fit in one Solana transaction; try a smaller amount or a different token pair".to_string());
        }
        Self::solana_check_signable(s, &transaction, &accounts)?;
        let fee_lamports = tx::fee_estimate(&transaction.message, &accounts);
        Ok(SwapPlan {
            from,
            to,
            amount,
            quote,
            transaction,
            balance,
            effects,
            fee_lamports,
            fee,
        })
    }

    /// What the simulation says the wallet gives and gets, in whole units.
    fn solana_swap_outcome(plan: &SwapPlan) -> Value {
        let Some(effects) = &plan.effects else {
            return Value::object(vec![
                ("simulated", Value::Bool(false)),
                ("note", Value::str("not attempted: insufficient balance")),
            ]);
        };
        let wsol = saddr::id(saddr::WRAPPED_SOL_MINT);
        let change = |leg: &Leg| -> i128 {
            if leg.native {
                effects.sol_change + effects.token_change(&wsol)
            } else {
                effects.token_change(&leg.mint)
            }
        };
        let (spent, received) = (-change(&plan.from), change(&plan.to));
        Value::object(vec![
            ("simulated", Value::Bool(true)),
            (
                "wallet_spends",
                Value::String(tokens::format_amount(
                    u128::try_from(spent.max(0)).unwrap_or(0),
                    plan.from.decimals,
                )),
            ),
            (
                "wallet_receives",
                Value::String(tokens::format_amount(
                    u128::try_from(received.max(0)).unwrap_or(0),
                    plan.to.decimals,
                )),
            ),
            (
                "note",
                Value::str(
                    "From a dry run of the exact transaction against the network. For SOL the amounts include the network fee and any deposit for new token accounts, so they differ slightly from the quote.",
                ),
            ),
        ])
    }

    fn solana_swap_report(&self, s: &Session<'_>, plan: &SwapPlan) -> Value {
        let q = &plan.quote;
        let mut warnings: Vec<Value> = vec![];
        if plan.balance < u128::from(plan.amount) {
            warnings.push(Value::String(format!(
                "insufficient {} balance: the wallet holds {} but the swap needs {}",
                plan.from.symbol,
                tokens::format_amount(plan.balance, plan.from.decimals),
                tokens::format_amount(u128::from(plan.amount), plan.from.decimals)
            )));
        }
        if let Some(impact) = q.price_impact
            && impact.abs() > swap::WARN_PRICE_IMPACT_PERCENT
        {
            warnings.push(Value::String(format!(
                "price impact is {:.2}%: the quoted price is noticeably worse than the market price",
                impact.abs()
            )));
        }
        if !plan.to.native && tokens::known_token(&plan.to.mint).is_none() {
            warnings.push(Value::str(
                "the token being bought is not one this wallet recognises; check the mint address before swapping",
            ));
        }
        let service_fee = match &plan.fee {
            Some(f) => {
                let estimate = ssw::SolanaSwapFee {
                    recipient: f.recipient,
                    bps: f.bps,
                }
                .estimate_for(q.out_amount);
                Value::object(vec![
                    ("percent", Value::String(swap::format_percent(f.bps))),
                    ("bps", Value::int(i64::from(f.bps))),
                    (
                        "estimated_amount",
                        Value::String(tokens::format_amount(
                            u128::from(estimate),
                            plan.to.decimals,
                        )),
                    ),
                    ("symbol", Value::str(&plan.to.symbol)),
                    ("recipient", Value::String(saddr::to_text(&f.recipient))),
                    (
                        "recipient_token_account",
                        Value::String(saddr::to_text(&f.account)),
                    ),
                    (
                        "opens_recipient_token_account",
                        Value::Bool(f.opens_account),
                    ),
                ])
            }
            None => Value::Null,
        };
        let fmt_to = |raw: u64| tokens::format_amount(u128::from(raw), plan.to.decimals);
        let mut pairs = vec![
            ("type", Value::str("same_chain")),
            ("aggregator", Value::str("Jupiter")),
            ("network", Value::str(s.network.id())),
            (
                "from",
                Value::object(vec![
                    ("token", plan.from.token_json()),
                    ("symbol", Value::str(&plan.from.symbol)),
                    (
                        "amount",
                        Value::String(tokens::format_amount(
                            u128::from(plan.amount),
                            plan.from.decimals,
                        )),
                    ),
                    ("amount_raw", Value::String(plan.amount.to_string())),
                ]),
            ),
            (
                "to",
                Value::object(vec![
                    ("token", plan.to.token_json()),
                    ("symbol", Value::str(&plan.to.symbol)),
                    ("expected_amount", Value::String(fmt_to(q.out_amount))),
                    ("minimum_amount", Value::String(fmt_to(q.min_out_amount))),
                    (
                        "expected_amount_raw",
                        Value::String(q.out_amount.to_string()),
                    ),
                    (
                        "minimum_amount_raw",
                        Value::String(q.min_out_amount.to_string()),
                    ),
                    ("recipient", Value::String(saddr::to_text(&s.owner))),
                ]),
            ),
            (
                "fees",
                Value::object(vec![
                    ("service_fee", service_fee),
                    (
                        "network_fee",
                        Value::object(vec![
                            (
                                "amount",
                                Value::String(tokens::format_sol(plan.fee_lamports)),
                            ),
                            ("currency", Value::str("SOL")),
                        ]),
                    ),
                    (
                        "note",
                        Value::str(
                            "The service fee (if any) is taken by Jupiter from the amount bought and is already deducted from expected_amount and minimum_amount, which are what the swap is quoted to deliver; Jupiter takes no fee of its own. When the fee recipient has no token account for the bought token yet, the transaction opens it, which costs this wallet a deposit of about 0.00204 SOL once. The network fee is the base fee plus the priority fee the transaction carries; opening token accounts needs a refundable deposit (about 0.00204 SOL each) on top.",
                        ),
                    ),
                ]),
            ),
            (
                "route",
                Value::Array(q.route.iter().map(|r| Value::str(r)).collect()),
            ),
            (
                "price_impact_percent",
                q.price_impact
                    .map(|p| Value::String(format!("{:.2}", p.abs())))
                    .unwrap_or(Value::Null),
            ),
            (
                "requirements",
                Value::object(vec![(
                    "sufficient_balance",
                    Value::Bool(plan.balance >= u128::from(plan.amount)),
                )]),
            ),
            (
                "verification",
                Value::object(vec![
                    (
                        "checks",
                        Value::str(
                            "the quote matches the request, the transaction is paid and signed only by this wallet, grants nobody spending rights, and a dry run shows this wallet spends no more than requested and receives at least minimum_amount",
                        ),
                    ),
                    ("dry_run", Self::solana_swap_outcome(plan)),
                ]),
            ),
        ];
        pairs.push(("warnings", Value::Array(warnings)));
        Value::object(pairs)
    }

    /// Previews a swap on Solana. Nothing is signed or sent.
    pub fn solana_swap_quote(
        &self,
        name: &str,
        password: &str,
        p: &SwapParams,
    ) -> Result<Value, String> {
        let (record, vault) = self.unlock_solana(name, password)?;
        let s = self.solana_session(&vault)?;
        let plan = self.solana_swap_plan(&s, p, false)?;
        Ok(merge(
            self.solana_swap_report(&s, &plan),
            vec![
                ("wallet", Value::String(record.name)),
                (
                    "note",
                    Value::str(
                        "Quote only: nothing was signed or sent. Quotes move with the market and the transaction expires after about a minute; swap_tokens fetches a fresh one and checks it again. Pass min_receive to swap_tokens to enforce a floor.",
                    ),
                ),
            ],
        ))
    }

    /// Swaps on Solana: quote, check, simulate, sign, submit.
    pub fn solana_swap_tokens(
        &self,
        name: &str,
        password: &str,
        p: &SwapParams,
    ) -> Result<Value, String> {
        let (record, vault) = self.unlock_solana(name, password)?;
        let s = self.solana_session(&vault)?;
        let mut plan = self.solana_swap_plan(&s, p, true)?;
        let submitted = self.solana_submit(&s, &vault, &mut plan.transaction)?;
        let report = self.solana_swap_report(&s, &plan);
        Ok(merge(
            Self::solana_submission_report(&s, &submitted),
            vec![("wallet", Value::String(record.name)), ("swap", report)],
        ))
    }
}
