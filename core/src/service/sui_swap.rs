//! Sui swaps through the Cetus aggregator.
//!
//! The wallet asks the aggregator for the best route over Cetus's pools, checks the route against
//! Cetus's on-chain packages (see `sui::cetus`), builds the transaction itself, dry-runs it, and
//! only then signs. The aggregator's contract enforces the minimum output on-chain.

use super::sui::{Prepared, Session};
use super::*;
use crate::eth::prices::format_usd;
use crate::sui::cetus::{self, Amounts, SuiSwapFee, Verified};
use crate::sui::coins::{self, SUI_COIN_TYPE};
use crate::sui::tx::Holdings;
use sui_sdk_types::Address as SuiAddress;

struct SwapPlan {
    from_type: String,
    to_type: String,
    from_symbol: String,
    to_symbol: String,
    from_decimals: u32,
    to_decimals: u32,
    amount: u64,
    recipient: SuiAddress,
    verified: Verified,
    amounts: Amounts,
    /// Whether the quote was checked against a price that does not come from the aggregator.
    independent_price: bool,
    has_min_receive: bool,
    sui: Holdings,
    spend: Holdings,
}

fn percent_text(bps: u64) -> String {
    swap::format_percent(u32::try_from(bps).unwrap_or(u32::MAX))
}

fn token_json(coin_type: &str) -> Value {
    if coins::is_sui(coin_type) {
        Value::str("native")
    } else {
        Value::str(coin_type)
    }
}

impl WalletService {
    pub fn with_cetus_settings(mut self, settings: cetus::CetusSettings) -> WalletService {
        self.cetus = settings;
        self
    }

    fn sui_swap_plan(&self, s: &Session<'_>, p: &SwapParams) -> Result<SwapPlan, String> {
        if p.from_chain_id != 0 || p.to_chain_id != 0 {
            return Err("from_chain_id and to_chain_id are Ethereum parameters; a Sui wallet swaps on the one network it is bound to".to_string());
        }
        if !s.network.is_mainnet() {
            return Err(format!(
                "Cetus swaps are only available on Sui mainnet; this wallet is bound to {}",
                s.network.id()
            ));
        }
        let from_type =
            coins::normalize_coin_type(&p.from_token).map_err(|e| format!("'from_token': {e}"))?;
        let to_type =
            coins::normalize_coin_type(&p.to_token).map_err(|e| format!("'to_token': {e}"))?;
        if from_type == to_type {
            return Err("from_token and to_token are the same coin".to_string());
        }
        let (from_symbol, from_decimals) = Self::sui_required_decimals(s, &from_type)?;
        let (to_symbol, to_decimals) = Self::sui_required_decimals(s, &to_type)?;
        let amount =
            coins::parse_amount(&p.amount, from_decimals).map_err(|e| format!("'amount': {e}"))?;
        let recipient = match &p.to_address {
            Some(a) => {
                let addr = coins::parse_address(a).map_err(|e| format!("'to_address': {e}"))?;
                if addr == SuiAddress::ZERO {
                    return Err(
                        "'to_address' must not be the zero address (coins would be lost)"
                            .to_string(),
                    );
                }
                addr
            }
            None => s.owner,
        };
        let slippage_bps = cetus::parse_slippage(p.slippage_bps)?;
        let min_receive = match &p.min_receive {
            Some(m) => Some(
                coins::parse_amount(m, to_decimals).map_err(|e| format!("'min_receive': {e}"))?,
            ),
            None => None,
        };
        let sui = Self::sui_holdings(s, SUI_COIN_TYPE)?;
        let spend = if coins::is_sui(&from_type) {
            sui.clone()
        } else {
            Self::sui_holdings(s, &from_type)?
        };
        let route = cetus::fetch_route(&*self.http, &self.cetus, &from_type, &to_type, amount)?;
        cetus::check_route_shape(&route, &from_type, &to_type, amount)?;
        let verified = cetus::verify_on_chain(&s.gql, route)?;
        let amounts =
            cetus::compute_amounts(&verified.route, self.cetus.fee.as_ref(), slippage_bps);
        let independent_price =
            self.sui_independent_price_check(&from_type, &to_type, amount, amounts.gross_out)?;
        if amounts.expected_out == 0 || amounts.min_out == 0 {
            return Err(
                "the swap would deliver (almost) nothing; the amount is too small".to_string(),
            );
        }
        if let Some(floor) = min_receive
            && amounts.min_out < floor
        {
            return Err(format!(
                "the quote's guaranteed minimum is {} {to_symbol}, below your min_receive of {}; nothing was sent",
                coins::format_amount(u128::from(amounts.min_out), to_decimals),
                coins::format_amount(u128::from(floor), to_decimals)
            ));
        }
        Ok(SwapPlan {
            from_type,
            to_type,
            from_symbol,
            to_symbol,
            from_decimals,
            to_decimals,
            amount,
            recipient,
            verified,
            amounts,
            independent_price,
            has_min_receive: min_receive.is_some(),
            sui,
            spend,
        })
    }

    /// The aggregator supplies both the price and the numbers that guard it, so where an
    /// independent price exists (SUI against USDC, from CoinGecko) the quote is checked against
    /// it. Returns whether such a check was possible.
    fn sui_independent_price_check(
        &self,
        from_type: &str,
        to_type: &str,
        amount: u64,
        gross_out: u64,
    ) -> Result<bool, String> {
        use crate::sui::valuation::{USDC_COIN_TYPE, USDC_DECIMALS, sui_price_usd};
        let sui_is_sold = coins::is_sui(from_type) && to_type == USDC_COIN_TYPE;
        let sui_is_bought = coins::is_sui(to_type) && from_type == USDC_COIN_TYPE;
        if !sui_is_sold && !sui_is_bought {
            return Ok(false);
        }
        let Some(price) = sui_price_usd(&*self.http).filter(|p| *p > 0.0) else {
            return Ok(false);
        };
        let sui_units = |raw: u64| raw as f64 / 10f64.powi(coins::SUI_DECIMALS as i32);
        let usdc_units = |raw: u64| raw as f64 / 10f64.powi(USDC_DECIMALS as i32);
        let (got, fair) = if sui_is_sold {
            (usdc_units(gross_out), sui_units(amount) * price)
        } else {
            (sui_units(gross_out), usdc_units(amount) / price)
        };
        if got < fair * (1.0 - cetus::MAX_PRICE_DEVIATION) {
            return Err(format!(
                "the quote is more than {:.0}% below the market price (CoinGecko: 1 SUI = {price:.4} USD); refusing to swap",
                cetus::MAX_PRICE_DEVIATION * 100.0
            ));
        }
        Ok(true)
    }

    fn sui_swap_prepare(&self, s: &Session<'_>, plan: &SwapPlan) -> Result<Prepared, String> {
        let fee = self.cetus.fee.clone();
        let body = |t: &mut sui_transaction_builder::TransactionBuilder,
                    input: sui_transaction_builder::Argument| {
            cetus::add_swap(
                t,
                &plan.verified,
                &plan.amounts,
                fee.as_ref(),
                input,
                plan.recipient,
            )
        };
        Self::sui_prepare(
            s,
            &plan.sui,
            &plan.from_type,
            plan.amount,
            &plan.spend,
            &body,
        )
    }

    /// What the dry run says the recipient would receive of the bought coin.
    fn sui_swap_received(plan: &SwapPlan, owner: &SuiAddress, prepared: &Prepared) -> i128 {
        let recipient = plan.recipient.to_string();
        let received: i128 = prepared
            .simulation
            .balance_changes
            .iter()
            .filter(|c| c.owner == recipient && c.coin_type == plan.to_type)
            .map(|c| c.amount)
            .sum();
        // Gas is charged in SUI to the sender, so it is part of the sender's SUI change.
        if plan.recipient == *owner && coins::is_sui(&plan.to_type) {
            received + i128::from(prepared.simulation.gas.net_cost())
        } else {
            received
        }
    }

    fn sui_swap_report(
        &self,
        plan: &SwapPlan,
        s: &Session<'_>,
        dry_run: Value,
        fee: Option<&SuiSwapFee>,
    ) -> Value {
        let a = &plan.amounts;
        let service_fee = match fee {
            Some(f) => Value::object(vec![
                ("percent", Value::String(swap::format_percent(f.bps))),
                ("bps", Value::int(i64::from(f.bps))),
                (
                    "amount",
                    Value::String(coins::format_amount(u128::from(a.fee), plan.to_decimals)),
                ),
                ("symbol", Value::str(&plan.to_symbol)),
                ("recipient", Value::String(f.recipient.to_string())),
            ]),
            None => Value::Null,
        };
        let sufficient = plan.spend.total() >= u128::from(plan.amount);
        let mut warnings: Vec<Value> = vec![];
        if !sufficient {
            warnings.push(Value::String(format!(
                "insufficient {} balance: the wallet holds {} but the swap needs {}",
                plan.from_symbol,
                coins::format_amount(plan.spend.total(), plan.from_decimals),
                coins::format_amount(u128::from(plan.amount), plan.from_decimals)
            )));
        }
        if plan.verified.route.deviation.abs() > cetus::WARN_PRICE_DEVIATION {
            warnings.push(Value::String(format!(
                "the quoted price is {:.2}% away from the market price",
                plan.verified.route.deviation.abs() * 100.0
            )));
        }
        if plan.recipient != s.owner {
            warnings.push(Value::str(
                "the bought coins are sent to a different address than this wallet",
            ));
        }
        let fmt_to = |raw: u64| coins::format_amount(u128::from(raw), plan.to_decimals);
        let mut pairs = vec![
            ("type", Value::str("swap")),
            ("aggregator", Value::str("Cetus")),
            ("network", Value::str(s.network.id())),
            (
                "from",
                Value::object(vec![
                    ("token", token_json(&plan.from_type)),
                    ("symbol", Value::str(&plan.from_symbol)),
                    (
                        "amount",
                        Value::String(coins::format_amount(
                            u128::from(plan.amount),
                            plan.from_decimals,
                        )),
                    ),
                    ("amount_raw", Value::String(plan.amount.to_string())),
                ]),
            ),
            (
                "to",
                Value::object(vec![
                    ("token", token_json(&plan.to_type)),
                    ("symbol", Value::str(&plan.to_symbol)),
                    ("expected_amount", Value::String(fmt_to(a.expected_out))),
                    ("minimum_amount", Value::String(fmt_to(a.min_out))),
                    (
                        "expected_amount_raw",
                        Value::String(a.expected_out.to_string()),
                    ),
                    ("minimum_amount_raw", Value::String(a.min_out.to_string())),
                    ("recipient", Value::String(plan.recipient.to_string())),
                ]),
            ),
            (
                "fees",
                Value::object(vec![
                    ("service_fee", service_fee),
                    (
                        "network_fee",
                        dry_run.get("gas").cloned().unwrap_or(Value::Null),
                    ),
                    (
                        "note",
                        Value::str(
                            "The service fee is taken from the coins the swap produces and is already deducted from expected_amount. The pools' own trading fees are inside the price. The network fee is paid in SUI and comes from a dry run.",
                        ),
                    ),
                ]),
            ),
            (
                "slippage_percent",
                Value::String(percent_text(a.slippage_bps)),
            ),
            ("route", cetus::route_description(&plan.verified.route)),
            (
                "price_check",
                Value::str(if plan.independent_price {
                    "the quote is within 5% of CoinGecko's SUI price"
                } else {
                    "not checked against an independent price source: swap_tokens will require min_receive"
                }),
            ),
            (
                "price_deviation_percent",
                Value::String(format_usd(plan.verified.route.deviation.abs() * 100.0)),
            ),
            ("dry_run", dry_run),
        ];
        if !warnings.is_empty() {
            pairs.push(("warnings", Value::Array(warnings)));
        }
        Value::object(pairs)
    }

    /// Previews a swap on Sui. Nothing is signed or sent.
    pub fn sui_swap_quote(
        &self,
        name: &str,
        password: &str,
        p: &SwapParams,
    ) -> Result<Value, String> {
        let (record, vault) = self.unlock_sui(name, password)?;
        let s = self.sui_session(&vault)?;
        let plan = self.sui_swap_plan(&s, p)?;
        let sufficient = plan.spend.total() >= u128::from(plan.amount);
        let dry_run = if sufficient {
            match self.sui_swap_prepare(&s, &plan) {
                Ok(prepared) => {
                    let received = Self::sui_swap_received(&plan, &s.owner, &prepared);
                    Value::object(vec![
                        ("success", Value::Bool(true)),
                        (
                            "gas",
                            Self::sui_effects_json(&s, &prepared.simulation)
                                .get("gas")
                                .cloned()
                                .unwrap_or(Value::Null),
                        ),
                        (
                            "would_receive",
                            Value::String(coins::format_amount(
                                u128::try_from(received.max(0)).unwrap_or(0),
                                plan.to_decimals,
                            )),
                        ),
                    ])
                }
                Err(e) => Value::object(vec![
                    ("success", Value::Bool(false)),
                    ("error", Value::String(e)),
                ]),
            }
        } else {
            Value::object(vec![
                ("success", Value::Bool(false)),
                ("error", Value::str("not attempted: insufficient balance")),
            ])
        };
        let report = self.sui_swap_report(&plan, &s, dry_run, self.cetus.fee.as_ref());
        Ok(merge(
            report,
            vec![
                ("wallet", Value::String(record.name)),
                (
                    "note",
                    Value::str(
                        "Quote only: nothing was signed or sent. Quotes move with the market; swap_tokens fetches a fresh one and the aggregator's contract aborts the swap on-chain if the result would be below minimum_amount.",
                    ),
                ),
            ],
        ))
    }

    /// Swaps on Sui: quote, check, dry-run, sign, submit.
    pub fn sui_swap_tokens(
        &self,
        name: &str,
        password: &str,
        p: &SwapParams,
    ) -> Result<Value, String> {
        let (record, vault) = self.unlock_sui(name, password)?;
        let s = self.sui_session(&vault)?;
        let plan = self.sui_swap_plan(&s, p)?;
        if !plan.independent_price && !plan.has_min_receive {
            return Err("the quote's price could not be checked against an independent source (that is only possible for SUI against USDC while CoinGecko answers), so swap_tokens needs min_receive: the least of the coin you receive that you would accept, worked out from a price you trust".to_string());
        }
        if plan.spend.total() < u128::from(plan.amount) {
            return Err(format!(
                "insufficient {} balance: the wallet holds {} but the swap needs {}",
                plan.from_symbol,
                coins::format_amount(plan.spend.total(), plan.from_decimals),
                coins::format_amount(u128::from(plan.amount), plan.from_decimals)
            ));
        }
        let prepared = self.sui_swap_prepare(&s, &plan)?;
        let received = Self::sui_swap_received(&plan, &s.owner, &prepared);
        if received < i128::from(plan.amounts.min_out) {
            return Err(format!(
                "the dry run shows the swap would deliver only {} {}, below the minimum of {}; nothing was signed",
                coins::format_amount(
                    u128::try_from(received.max(0)).unwrap_or(0),
                    plan.to_decimals
                ),
                plan.to_symbol,
                coins::format_amount(u128::from(plan.amounts.min_out), plan.to_decimals)
            ));
        }
        let gas = Self::sui_effects_json(&s, &prepared.simulation)
            .get("gas")
            .cloned()
            .unwrap_or(Value::Null);
        let dry_run = Value::object(vec![("success", Value::Bool(true)), ("gas", gas)]);
        let submitted = Self::sui_submit(&s, &vault, &prepared.transaction)?;
        let report = self.sui_swap_report(&plan, &s, dry_run, self.cetus.fee.as_ref());
        Ok(merge(
            report,
            vec![
                ("wallet", Value::String(record.name)),
                ("status", Value::str("submitted")),
                ("transaction", Self::sui_submission_report(&s, &submitted)),
                (
                    "next_step",
                    Value::str("Confirm the swap with get_transaction (transaction_hash)."),
                ),
            ],
        ))
    }
}
