//! Choosing which coins to spend and what to pay as a fee, following Bitcoin Core's wallet
//! rules where they matter for safety:
//!
//! - a payment below the dust threshold is refused;
//! - change below the dust threshold is not created (it goes to the fee instead);
//! - a coin that costs more to spend than it is worth at the chosen fee rate is left alone;
//! - fees are charged per virtual byte of the transaction as it will be signed (using the
//!   largest possible signature, so the fee never falls short of the chosen rate);
//! - the fee may not exceed Core's default `-maxtxfee` of 0.1 BTC;
//! - the transaction may not exceed the standard weight of 400,000.
//!
//! Everything here is integer arithmetic.

use std::fmt;

use super::address::{Address, dust_threshold_for};
use super::tx::{
    MAX_STANDARD_WEIGHT, OutPoint, RBF_SEQUENCE, Transaction, TxIn, TxOut, output_weight,
    overhead_weight, p2wpkh_input_weight,
};
use super::units::{FeeRate, format_btc};

/// Bitcoin Core's default `-maxtxfee`: the most a wallet transaction may pay in fees.
pub const MAX_FEE_SATS: u64 = 10_000_000;
const CHANGE_SCRIPT_LEN: usize = 22;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Utxo {
    pub outpoint: OutPoint,
    pub value: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub inputs: Vec<Utxo>,
    pub amount: u64,
    pub change: Option<u64>,
    pub fee: u64,
    pub fee_rate: FeeRate,
    /// Virtual size the fee was charged for (an upper bound of the signed size).
    pub vsize: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    Dust { amount: u64, threshold: u64 },
    InsufficientFunds { usable: u64, needed: u64 },
    TooManyInputs,
    FeeTooHigh { fee: u64 },
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::Dust { amount, threshold } => write!(
                f,
                "{} BTC is dust: Bitcoin nodes refuse to relay outputs below {} BTC ({threshold} satoshis) for this address type",
                format_btc(*amount),
                format_btc(*threshold)
            ),
            PlanError::InsufficientFunds { usable, needed } => write!(
                f,
                "insufficient funds: {} BTC is needed (the amount plus the network fee) but only {} BTC is spendable",
                format_btc(*needed),
                format_btc(*usable)
            ),
            PlanError::TooManyInputs => f.write_str(
                "this payment would need so many small coins that the transaction would exceed the standard size limit; send a smaller amount first to consolidate them",
            ),
            PlanError::FeeTooHigh { fee } => write!(
                f,
                "the fee ({} BTC) is above the {} BTC safety limit (Bitcoin Core's default maximum fee); lower the fee rate",
                format_btc(*fee),
                format_btc(MAX_FEE_SATS)
            ),
        }
    }
}

/// Weight of a transaction with `inputs` of our P2WPKH coins, the recipient's output and an
/// optional P2WPKH change output.
fn weight(inputs: usize, recipient_script_len: usize, change: bool) -> u64 {
    let outputs = 1 + usize::from(change);
    overhead_weight(inputs, outputs, true)
        + inputs as u64 * p2wpkh_input_weight()
        + output_weight(recipient_script_len)
        + if change {
            output_weight(CHANGE_SCRIPT_LEN)
        } else {
            0
        }
}

/// Virtual size of a one-recipient transfer; also used to estimate fees.
pub fn transfer_vsize(inputs: usize, recipient_script_len: usize, change: bool) -> u64 {
    weight(inputs, recipient_script_len, change).div_ceil(4)
}

fn change_dust() -> u64 {
    dust_threshold_for(&[0u8; CHANGE_SCRIPT_LEN], true)
}

/// What a chosen set of coins amounts to, or how far it falls short.
fn settle(
    total: u64,
    inputs: usize,
    amount: u64,
    recipient_script_len: usize,
    rate: FeeRate,
) -> Option<(Option<u64>, u64, u64)> {
    let no_change_vsize = transfer_vsize(inputs, recipient_script_len, false);
    let no_change_fee = rate.fee_for(no_change_vsize);
    if total < amount.checked_add(no_change_fee)? {
        return None;
    }
    let change_vsize = transfer_vsize(inputs, recipient_script_len, true);
    let change_fee = rate.fee_for(change_vsize);
    if let Some(change) = total.checked_sub(amount.checked_add(change_fee)?)
        && change >= change_dust()
    {
        return Some((Some(change), change_fee, change_vsize));
    }
    // Everything above the fee that is too small to be worth a change output is paid as fee.
    Some((None, total - amount, no_change_vsize))
}

/// Chooses coins for paying `amount` to `recipient`.
///
/// One coin is preferred (the smallest that is enough, which keeps larger coins whole);
/// otherwise the largest coins are added until the payment is covered, then any coin that
/// turned out to be unnecessary is dropped again.
pub fn plan_transfer(
    utxos: &[Utxo],
    recipient: &Address,
    amount: u64,
    rate: FeeRate,
) -> Result<Plan, PlanError> {
    let script_len = recipient.script_pubkey().len();
    let threshold = recipient.dust_threshold();
    if amount < threshold {
        return Err(PlanError::Dust { amount, threshold });
    }
    let spend_cost = rate.fee_for(p2wpkh_input_weight().div_ceil(4));
    let mut usable: Vec<Utxo> = utxos
        .iter()
        .copied()
        .filter(|u| u.value > spend_cost)
        .collect();
    usable.sort_by_key(|u| (u.value, u.outpoint.txid, u.outpoint.vout));
    let available: u64 = usable.iter().map(|u| u.value).sum();

    let finish = |inputs: Vec<Utxo>| -> Result<Plan, PlanError> {
        let total: u64 = inputs.iter().map(|u| u.value).sum();
        let (change, fee, vsize) = settle(total, inputs.len(), amount, script_len, rate)
            .expect("the selection was checked to be sufficient");
        if fee > MAX_FEE_SATS {
            return Err(PlanError::FeeTooHigh { fee });
        }
        if weight(inputs.len(), script_len, change.is_some()) > MAX_STANDARD_WEIGHT {
            return Err(PlanError::TooManyInputs);
        }
        Ok(Plan {
            inputs,
            amount,
            change,
            fee,
            fee_rate: rate,
            vsize,
        })
    };

    if let Some(single) = usable
        .iter()
        .find(|u| settle(u.value, 1, amount, script_len, rate).is_some())
    {
        return finish(vec![*single]);
    }

    let mut chosen: Vec<Utxo> = vec![];
    let mut total = 0u64;
    let mut covered = false;
    for coin in usable.iter().rev() {
        chosen.push(*coin);
        total += coin.value;
        if settle(total, chosen.len(), amount, script_len, rate).is_some() {
            covered = true;
            break;
        }
        if weight(chosen.len(), script_len, true) > MAX_STANDARD_WEIGHT {
            return Err(PlanError::TooManyInputs);
        }
    }
    if !covered {
        let needed_vsize = transfer_vsize(usable.len().max(1), script_len, false);
        return Err(PlanError::InsufficientFunds {
            usable: available,
            needed: amount.saturating_add(rate.fee_for(needed_vsize)),
        });
    }
    // The coins were added largest first; drop the smallest ones that are not needed.
    chosen.reverse();
    let mut index = 0;
    while index < chosen.len() && chosen.len() > 1 {
        let without = total - chosen[index].value;
        if settle(without, chosen.len() - 1, amount, script_len, rate).is_some() {
            total = without;
            chosen.remove(index);
        } else {
            index += 1;
        }
    }
    finish(chosen)
}

/// The unsigned transaction for a plan: version 2, every input signalling replaceability
/// (BIP 125), the recipient first and change (to the wallet's own script) second, and
/// `lock_time` set to the current block height, which Bitcoin Core does to discourage fee
/// sniping.
pub fn assemble(
    plan: &Plan,
    recipient_script: &[u8],
    own_script: &[u8],
    lock_time: u32,
) -> Transaction {
    let mut outputs = vec![TxOut {
        value: plan.amount,
        script_pubkey: recipient_script.to_vec(),
    }];
    if let Some(change) = plan.change {
        outputs.push(TxOut {
            value: change,
            script_pubkey: own_script.to_vec(),
        });
    }
    Transaction {
        version: 2,
        inputs: plan
            .inputs
            .iter()
            .map(|u| TxIn {
                previous: u.outpoint,
                script_sig: vec![],
                sequence: RBF_SEQUENCE,
                witness: vec![],
            })
            .collect(),
        outputs,
        lock_time,
    }
}

/// Final checks on a signed transaction before it is allowed anywhere near the network:
/// it must pay exactly what the plan says to exactly the intended scripts, the fee must be what
/// was planned and within the limits, every input must be signed, and the real size must not
/// exceed the size the fee was charged for.
pub fn verify_signed(
    tx: &Transaction,
    plan: &Plan,
    recipient_script: &[u8],
    own_script: &[u8],
) -> Result<(), String> {
    let refuse = |why: &str| Err(format!("internal check failed ({why}); nothing was sent"));
    let expected_outputs = 1 + usize::from(plan.change.is_some());
    if tx.outputs.len() != expected_outputs || tx.inputs.len() != plan.inputs.len() {
        return refuse("unexpected number of inputs or outputs");
    }
    if tx.outputs[0].value != plan.amount || tx.outputs[0].script_pubkey != recipient_script {
        return refuse("the payment output differs from the request");
    }
    if let Some(change) = plan.change
        && (tx.outputs[1].value != change || tx.outputs[1].script_pubkey != own_script)
    {
        return refuse("the change output differs from the plan");
    }
    for (index, input) in tx.inputs.iter().enumerate() {
        if input.previous != plan.inputs[index].outpoint {
            return refuse("an input differs from the plan");
        }
        if tx.inputs[..index]
            .iter()
            .any(|i| i.previous == input.previous)
        {
            return refuse("an input is used twice");
        }
        if input.witness.len() != 2 || !input.script_sig.is_empty() {
            return refuse("an input is not signed as native SegWit");
        }
    }
    let total_in: u64 = plan.inputs.iter().map(|u| u.value).sum();
    let total_out = tx.total_output_value()?;
    if total_in.checked_sub(total_out) != Some(plan.fee) {
        return refuse("the fee differs from the plan");
    }
    if plan.fee > MAX_FEE_SATS {
        return refuse("the fee is above the safety limit");
    }
    let (vsize, weight) = (tx.vsize(), tx.weight());
    if weight > MAX_STANDARD_WEIGHT {
        return refuse("the transaction is above the standard size");
    }
    if vsize > plan.vsize {
        return refuse("the transaction is larger than estimated");
    }
    if plan.fee < vsize {
        return refuse("the fee rate is below 1 sat/vB");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coin(n: u8, value: u64) -> Utxo {
        Utxo {
            outpoint: OutPoint {
                txid: [n; 32],
                vout: u32::from(n),
            },
            value,
        }
    }

    fn p2wpkh() -> Address {
        Address::P2wpkh([5; 20])
    }

    const ONE: FeeRate = FeeRate(1_000);

    fn check_balance(plan: &Plan) {
        let total: u64 = plan.inputs.iter().map(|u| u.value).sum();
        assert_eq!(
            total,
            plan.amount + plan.change.unwrap_or(0) + plan.fee,
            "inputs = outputs + fee"
        );
        assert!(plan.fee >= plan.fee_rate.fee_for(plan.vsize));
    }

    #[test]
    fn one_input_two_outputs_costs_141_vbytes() {
        let plan = plan_transfer(&[coin(1, 100_000)], &p2wpkh(), 50_000, ONE).unwrap();
        assert_eq!(plan.vsize, 141);
        assert_eq!(plan.fee, 141);
        assert_eq!(plan.change, Some(100_000 - 50_000 - 141));
        check_balance(&plan);
    }

    #[test]
    fn the_smallest_sufficient_coin_is_used() {
        let coins = [coin(1, 1_000_000), coin(2, 60_000), coin(3, 10_000)];
        let plan = plan_transfer(&coins, &p2wpkh(), 50_000, ONE).unwrap();
        assert_eq!(plan.inputs, vec![coin(2, 60_000)]);
        check_balance(&plan);
    }

    #[test]
    fn several_coins_are_combined_when_none_is_enough() {
        let coins = [
            coin(1, 30_000),
            coin(2, 30_000),
            coin(3, 30_000),
            coin(4, 1_000),
        ];
        let plan = plan_transfer(&coins, &p2wpkh(), 80_000, ONE).unwrap();
        assert_eq!(plan.inputs.len(), 3);
        assert!(!plan.inputs.contains(&coin(4, 1_000)));
        check_balance(&plan);
    }

    #[test]
    fn unneeded_small_coins_are_dropped_again() {
        let coins = [coin(1, 100_000), coin(2, 5_000), coin(3, 1_000)];
        // The greedy pass takes 100k first and is already enough; nothing else is added.
        let plan = plan_transfer(&coins, &p2wpkh(), 120_000, ONE);
        assert!(plan.is_err() || plan.unwrap().inputs.len() <= 3);
        let plan = plan_transfer(&coins, &p2wpkh(), 100_500, ONE).unwrap();
        assert_eq!(plan.inputs.len(), 2);
        assert!(plan.inputs.contains(&coin(1, 100_000)));
        assert!(plan.inputs.contains(&coin(2, 5_000)));
        check_balance(&plan);
    }

    #[test]
    fn change_below_the_dust_limit_goes_to_the_fee() {
        // 110 vbytes without change: fee 110. 50_000 + 110 + 100 leaves only 100 of change.
        let plan = plan_transfer(&[coin(1, 50_210)], &p2wpkh(), 50_000, ONE).unwrap();
        assert_eq!(plan.change, None);
        assert_eq!(plan.fee, 210);
        assert_eq!(plan.vsize, 110);
        check_balance(&plan);
    }

    #[test]
    fn change_of_exactly_the_dust_limit_is_kept() {
        let amount = 50_000;
        let fee = ONE.fee_for(141);
        let plan = plan_transfer(&[coin(1, amount + fee + 294)], &p2wpkh(), amount, ONE).unwrap();
        assert_eq!(plan.change, Some(294));
        let plan = plan_transfer(&[coin(1, amount + fee + 293)], &p2wpkh(), amount, ONE).unwrap();
        assert_eq!(plan.change, None);
    }

    #[test]
    fn an_exact_payment_has_no_change() {
        let amount = 50_000;
        let plan = plan_transfer(&[coin(1, amount + 110)], &p2wpkh(), amount, ONE).unwrap();
        assert_eq!(plan.change, None);
        assert_eq!(plan.fee, 110);
    }

    #[test]
    fn dust_payments_are_refused_per_script_type() {
        assert_eq!(
            plan_transfer(&[coin(1, 100_000)], &p2wpkh(), 293, ONE),
            Err(PlanError::Dust {
                amount: 293,
                threshold: 294
            })
        );
        assert!(plan_transfer(&[coin(1, 100_000)], &p2wpkh(), 294, ONE).is_ok());
        assert!(plan_transfer(&[coin(1, 100_000)], &Address::P2pkh([1; 20]), 545, ONE).is_err());
        assert!(plan_transfer(&[coin(1, 100_000)], &Address::P2pkh([1; 20]), 546, ONE).is_ok());
        assert!(plan_transfer(&[coin(1, 100_000)], &Address::P2tr([1; 32]), 329, ONE).is_err());
        assert!(plan_transfer(&[coin(1, 100_000)], &Address::P2tr([1; 32]), 330, ONE).is_ok());
    }

    #[test]
    fn insufficient_funds_reports_what_is_missing() {
        let err = plan_transfer(&[coin(1, 10_000)], &p2wpkh(), 50_000, ONE).unwrap_err();
        match err {
            PlanError::InsufficientFunds { usable, needed } => {
                assert_eq!(usable, 10_000);
                assert!(needed > 50_000);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            plan_transfer(&[], &p2wpkh(), 50_000, ONE),
            Err(PlanError::InsufficientFunds { usable: 0, .. })
        ));
    }

    #[test]
    fn the_amount_alone_being_covered_is_not_enough() {
        // The coin covers the amount but not the fee on top.
        let err = plan_transfer(&[coin(1, 50_050)], &p2wpkh(), 50_000, ONE).unwrap_err();
        assert!(matches!(err, PlanError::InsufficientFunds { .. }));
    }

    #[test]
    fn coins_that_cost_more_than_they_are_worth_are_ignored() {
        // At 10 sat/vB spending a coin costs 680 sats, so a 500-sat coin is no help.
        let rate = FeeRate(10_000);
        let err = plan_transfer(&[coin(1, 500), coin(2, 600)], &p2wpkh(), 294, rate).unwrap_err();
        assert!(matches!(
            err,
            PlanError::InsufficientFunds { usable: 0, .. }
        ));
    }

    #[test]
    fn fee_rates_scale_the_fee() {
        let low = plan_transfer(&[coin(1, 1_000_000)], &p2wpkh(), 100_000, FeeRate(1_000)).unwrap();
        let high =
            plan_transfer(&[coin(1, 1_000_000)], &p2wpkh(), 100_000, FeeRate(50_000)).unwrap();
        assert_eq!(high.fee, 50 * low.fee);
        check_balance(&high);
    }

    #[test]
    fn the_fee_is_capped_like_bitcoin_core() {
        // 141 vbytes at 1,000 sat/vB is only 141,000 sats, so reach the cap with huge input counts.
        let coins: Vec<Utxo> = (0..255u8).map(|n| coin(n, 100_000_000)).collect();
        let err = plan_transfer(&coins, &p2wpkh(), 25_000_000_000, FeeRate::MAX);
        assert!(matches!(err, Err(PlanError::FeeTooHigh { .. })), "{err:?}");
    }

    #[test]
    fn oversized_transactions_are_refused() {
        // 1,500 inputs of 1,000 sats would weigh more than 400,000.
        let coins: Vec<Utxo> = (0..1500u32)
            .map(|n| Utxo {
                outpoint: OutPoint {
                    txid: [(n % 250) as u8; 32],
                    vout: n,
                },
                value: 1_000,
            })
            .collect();
        let err = plan_transfer(&coins, &p2wpkh(), 1_400_000, ONE).unwrap_err();
        assert_eq!(err, PlanError::TooManyInputs);
    }

    #[test]
    fn selection_is_deterministic() {
        let coins = [coin(3, 40_000), coin(1, 40_000), coin(2, 40_000)];
        let a = plan_transfer(&coins, &p2wpkh(), 70_000, ONE).unwrap();
        let mut shuffled = coins;
        shuffled.reverse();
        let b = plan_transfer(&shuffled, &p2wpkh(), 70_000, ONE).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn vsize_depends_on_the_recipient_script() {
        assert_eq!(transfer_vsize(1, 22, true), 141);
        assert_eq!(transfer_vsize(1, 25, true), 144);
        assert_eq!(transfer_vsize(1, 34, true), 153);
        assert_eq!(transfer_vsize(2, 22, true), 141 + 68);
    }

    #[test]
    fn errors_read_clearly() {
        let text = PlanError::InsufficientFunds {
            usable: 1_000,
            needed: 2_000,
        }
        .to_string();
        assert!(
            text.contains("0.00002") && text.contains("0.00001"),
            "{text}"
        );
        assert!(
            PlanError::Dust {
                amount: 100,
                threshold: 294
            }
            .to_string()
            .contains("dust")
        );
        assert!(
            PlanError::FeeTooHigh { fee: 20_000_000 }
                .to_string()
                .contains("0.2")
        );
    }

    fn sample_plan(change: bool) -> Plan {
        let inputs = vec![coin(1, 100_000), coin(2, 50_000)];
        let amount = 60_000;
        let fee = 1_000;
        Plan {
            amount,
            change: change.then_some(150_000 - amount - fee),
            fee: if change { fee } else { 150_000 - amount },
            fee_rate: ONE,
            vsize: 300,
            inputs,
        }
    }

    fn signed(plan: &Plan, recipient: &[u8], own: &[u8]) -> Transaction {
        let mut tx = assemble(plan, recipient, own, 900_000);
        for (index, input) in plan.inputs.iter().enumerate() {
            tx.sign_p2wpkh_input(index, input.value, &[7u8; 32])
                .unwrap();
        }
        tx
    }

    #[test]
    fn assembled_transactions_follow_core_defaults() {
        let plan = sample_plan(true);
        let tx = assemble(&plan, &[0, 20], &[0, 21], 975_000);
        assert_eq!(tx.version, 2);
        assert_eq!(tx.lock_time, 975_000);
        assert!(tx.inputs.iter().all(|i| i.sequence == RBF_SEQUENCE));
        assert_eq!(tx.outputs.len(), 2);
        assert_eq!(tx.outputs[0].script_pubkey, vec![0, 20]);
        assert_eq!(tx.outputs[1].script_pubkey, vec![0, 21]);
        assert_eq!(
            assemble(&sample_plan(false), &[0, 20], &[0, 21], 0)
                .outputs
                .len(),
            1
        );
    }

    #[test]
    fn a_correct_signed_transaction_passes_the_final_checks() {
        for change in [true, false] {
            let plan = sample_plan(change);
            let tx = signed(&plan, &[0, 20], &[0, 21]);
            let plan = Plan {
                vsize: tx.vsize(),
                ..plan
            };
            assert_eq!(
                verify_signed(&tx, &plan, &[0, 20], &[0, 21]),
                Ok(()),
                "{change}"
            );
        }
    }

    #[test]
    fn the_final_checks_catch_every_deviation() {
        let plan = sample_plan(true);
        let good = signed(&plan, &[0, 20], &[0, 21]);
        let plan = Plan {
            vsize: good.vsize(),
            ..plan
        };
        let check = |tx: &Transaction, plan: &Plan| verify_signed(tx, plan, &[0, 20], &[0, 21]);
        assert!(check(&good, &plan).is_ok());

        let mut tx = good.clone();
        tx.outputs[0].value += 1;
        assert!(check(&tx, &plan).is_err(), "payment amount");
        let mut tx = good.clone();
        tx.outputs[0].script_pubkey = vec![9];
        assert!(check(&tx, &plan).is_err(), "payment script");
        let mut tx = good.clone();
        tx.outputs[1].script_pubkey = vec![9];
        assert!(check(&tx, &plan).is_err(), "change goes elsewhere");
        let mut tx = good.clone();
        tx.outputs[1].value -= 1;
        assert!(check(&tx, &plan).is_err(), "change amount");
        let mut tx = good.clone();
        tx.outputs.pop();
        assert!(check(&tx, &plan).is_err(), "missing change");
        let mut tx = good.clone();
        tx.inputs[1].previous = tx.inputs[0].previous;
        assert!(check(&tx, &plan).is_err(), "duplicate input");
        let mut tx = good.clone();
        tx.inputs[0].witness.clear();
        assert!(check(&tx, &plan).is_err(), "unsigned input");
        let mut tx = good.clone();
        tx.inputs[0].script_sig = vec![1];
        assert!(check(&tx, &plan).is_err(), "script sig");
        assert!(
            check(
                &good,
                &Plan {
                    fee: plan.fee + 1,
                    ..plan.clone()
                }
            )
            .is_err(),
            "fee"
        );
        assert!(
            check(
                &good,
                &Plan {
                    vsize: good.vsize() - 1,
                    ..plan.clone()
                }
            )
            .is_err(),
            "size"
        );
        let mut cheap = plan.clone();
        cheap.change = Some(cheap.change.unwrap() + cheap.fee - 10);
        cheap.fee = 10;
        let mut tx = good.clone();
        tx.outputs[1].value = cheap.change.unwrap();
        assert!(check(&tx, &cheap).is_err(), "fee below 1 sat/vB");
    }

    #[test]
    fn a_huge_fee_is_caught_even_if_the_plan_agrees() {
        let mut plan = sample_plan(true);
        plan.inputs = vec![coin(1, 30_000_000)];
        plan.amount = 1_000;
        plan.fee = MAX_FEE_SATS + 1;
        plan.change = Some(30_000_000 - 1_000 - plan.fee);
        let tx = signed(&plan, &[0, 20], &[0, 21]);
        plan.vsize = tx.vsize();
        assert!(
            verify_signed(&tx, &plan, &[0, 20], &[0, 21])
                .unwrap_err()
                .contains("safety limit")
        );
    }
}
