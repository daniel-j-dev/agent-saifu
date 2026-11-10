//! What a transaction did to one wallet, worked out from the transaction as the Esplora server
//! reports it.

use super::rpc::TxInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Received,
    Sent,
    /// Every output came back to the wallet; only the fee was spent.
    SelfTransfer,
    Unrelated,
}

impl Direction {
    pub fn name(&self) -> &'static str {
        match self {
            Direction::Received => "received",
            Direction::Sent => "sent",
            Direction::SelfTransfer => "self_transfer",
            Direction::Unrelated => "unrelated",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletEffect {
    /// Satoshis paid to the wallet's script.
    pub received: u64,
    /// Satoshis of the wallet's own coins that this transaction spent.
    pub spent: u64,
    /// False when the server did not say what one of the wallet's inputs was worth.
    pub spent_known: bool,
    /// Satoshis paid to scripts other than the wallet's.
    pub to_others: u64,
    pub direction: Direction,
}

impl WalletEffect {
    /// Net change of the wallet's balance, fee included.
    pub fn net(&self) -> i128 {
        i128::from(self.received) - i128::from(self.spent)
    }
}

pub fn wallet_effect(info: &TxInfo, script: &[u8]) -> WalletEffect {
    let received: u64 = info
        .outputs
        .iter()
        .filter(|o| o.script == script)
        .map(|o| o.value)
        .fold(0, u64::saturating_add);
    let to_others: u64 = info
        .outputs
        .iter()
        .filter(|o| o.script != script)
        .map(|o| o.value)
        .fold(0, u64::saturating_add);
    let own_inputs: Vec<_> = info
        .inputs
        .iter()
        .filter(|i| i.prevout_script.as_deref() == Some(script))
        .collect();
    let spent = own_inputs
        .iter()
        .filter_map(|i| i.prevout_value)
        .fold(0, u64::saturating_add);
    let spent_known = own_inputs.iter().all(|i| i.prevout_value.is_some());
    let direction = if own_inputs.is_empty() {
        if received > 0 {
            Direction::Received
        } else {
            Direction::Unrelated
        }
    } else if to_others == 0 {
        Direction::SelfTransfer
    } else {
        Direction::Sent
    };
    WalletEffect {
        received,
        spent,
        spent_known,
        to_others,
        direction,
    }
}

/// True when every input of the transaction spends a coin of this wallet, so that its change
/// output can only have come from the wallet's own payment (Bitcoin Core likewise spends the
/// wallet's own unconfirmed change, but never unconfirmed payments from other people).
pub fn funded_entirely_by(info: &TxInfo, script: &[u8]) -> bool {
    !info.inputs.is_empty()
        && info
            .inputs
            .iter()
            .all(|i| !i.is_coinbase && i.prevout_script.as_deref() == Some(script))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitcoin::rpc::{ChainStatus, InputInfo, OutputInfo};

    const OURS: &[u8] = &[0, 20, 1];
    const THEIRS: &[u8] = &[0, 20, 2];

    fn input(script: Option<&[u8]>, value: Option<u64>) -> InputInfo {
        InputInfo {
            txid: "00".repeat(32),
            vout: 0,
            is_coinbase: false,
            prevout_script: script.map(<[u8]>::to_vec),
            prevout_value: value,
            prevout_address: None,
        }
    }

    fn output(script: &[u8], value: u64) -> OutputInfo {
        OutputInfo {
            script: script.to_vec(),
            value,
            address: None,
        }
    }

    fn tx(inputs: Vec<InputInfo>, outputs: Vec<OutputInfo>) -> TxInfo {
        TxInfo {
            txid: "ab".repeat(32),
            fee: None,
            weight: None,
            size: None,
            status: ChainStatus {
                confirmed: false,
                block_height: None,
                block_time: None,
            },
            inputs,
            outputs,
        }
    }

    #[test]
    fn a_payment_to_the_wallet_is_received() {
        let effect = wallet_effect(
            &tx(
                vec![input(Some(THEIRS), Some(10_000))],
                vec![output(OURS, 9_000), output(THEIRS, 800)],
            ),
            OURS,
        );
        assert_eq!(effect.direction, Direction::Received);
        assert_eq!(effect.net(), 9_000);
        assert_eq!(effect.spent, 0);
    }

    #[test]
    fn a_payment_from_the_wallet_is_sent_and_includes_the_fee() {
        let effect = wallet_effect(
            &tx(
                vec![input(Some(OURS), Some(50_000))],
                vec![output(THEIRS, 30_000), output(OURS, 19_000)],
            ),
            OURS,
        );
        assert_eq!(effect.direction, Direction::Sent);
        assert_eq!(effect.net(), -31_000);
        assert_eq!(effect.to_others, 30_000);
        assert!(effect.spent_known);
    }

    #[test]
    fn paying_yourself_only_costs_the_fee() {
        let effect = wallet_effect(
            &tx(
                vec![input(Some(OURS), Some(50_000))],
                vec![output(OURS, 49_000)],
            ),
            OURS,
        );
        assert_eq!(effect.direction, Direction::SelfTransfer);
        assert_eq!(effect.net(), -1_000);
    }

    #[test]
    fn unrelated_transactions_are_recognised() {
        let effect = wallet_effect(
            &tx(vec![input(Some(THEIRS), Some(5))], vec![output(THEIRS, 4)]),
            OURS,
        );
        assert_eq!(effect.direction, Direction::Unrelated);
        assert_eq!(effect.net(), 0);
    }

    #[test]
    fn a_missing_input_value_is_flagged() {
        let effect = wallet_effect(
            &tx(vec![input(Some(OURS), None)], vec![output(THEIRS, 30_000)]),
            OURS,
        );
        assert!(!effect.spent_known);
        assert_eq!(effect.direction, Direction::Sent);
    }

    #[test]
    fn own_change_is_told_from_other_peoples_payments() {
        let own = tx(
            vec![input(Some(OURS), Some(1)), input(Some(OURS), Some(2))],
            vec![output(OURS, 1)],
        );
        assert!(funded_entirely_by(&own, OURS));
        let mixed = tx(
            vec![input(Some(OURS), Some(1)), input(Some(THEIRS), Some(2))],
            vec![output(OURS, 1)],
        );
        assert!(!funded_entirely_by(&mixed, OURS));
        let unknown = tx(vec![input(None, None)], vec![output(OURS, 1)]);
        assert!(!funded_entirely_by(&unknown, OURS));
        assert!(!funded_entirely_by(&tx(vec![], vec![]), OURS));
        let mut coinbase = tx(vec![input(Some(OURS), Some(1))], vec![]);
        coinbase.inputs[0].is_coinbase = true;
        assert!(!funded_entirely_by(&coinbase, OURS));
    }

    #[test]
    fn direction_names_are_stable() {
        assert_eq!(Direction::Received.name(), "received");
        assert_eq!(Direction::Sent.name(), "sent");
        assert_eq!(Direction::SelfTransfer.name(), "self_transfer");
        assert_eq!(Direction::Unrelated.name(), "unrelated");
    }
}
