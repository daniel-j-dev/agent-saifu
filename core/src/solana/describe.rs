//! Reading a Solana message in plain language: what each instruction does, and what to be
//! careful about. Only the system, token, associated-token, compute-budget and memo programs are
//! decoded; anything else is shown as an unrecognised program (the simulation is the only way to
//! learn what it does).

use super::address::{self, Pubkey};
use super::tokens;
use super::tx::{CompiledInstruction, Message};
use crate::eth::explorer::sanitize_text_max;
use crate::json::Value;

/// What an instruction does, for the programs this wallet understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    SolTransfer {
        from: Pubkey,
        to: Pubkey,
        lamports: u64,
    },
    CreateAccount {
        from: Pubkey,
        new_account: Pubkey,
        lamports: u64,
        space: u64,
        owner: Pubkey,
    },
    Assign {
        account: Pubkey,
        owner: Pubkey,
    },
    TokenTransfer {
        source: Pubkey,
        destination: Pubkey,
        authority: Pubkey,
        amount: u64,
        mint: Option<Pubkey>,
        decimals: Option<u8>,
    },
    TokenApprove {
        source: Pubkey,
        delegate: Pubkey,
        authority: Pubkey,
        amount: u64,
    },
    TokenRevoke {
        source: Pubkey,
    },
    TokenSetAuthority {
        account: Pubkey,
        kind: u8,
        new_authority: Option<Pubkey>,
    },
    TokenClose {
        account: Pubkey,
        destination: Pubkey,
    },
    TokenBurn {
        account: Pubkey,
        amount: u64,
    },
    CreateAssociatedToken {
        payer: Pubkey,
        associated: Pubkey,
        owner: Pubkey,
        mint: Pubkey,
        idempotent: bool,
    },
    ComputeLimit(u32),
    ComputePrice(u64),
    Memo(String),
    /// A recognised program with an action this wallet does not decode.
    Other {
        program: &'static str,
        action: String,
    },
    Unknown {
        program: Pubkey,
    },
    /// The instruction uses accounts from a lookup table that was not resolved.
    Unresolved,
}

fn le_u64(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

fn le_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn key_at(data: &[u8], at: usize) -> Option<Pubkey> {
    data.get(at..at + 32)?.try_into().ok()
}

/// The `n`th account an instruction names.
fn account(ix: &CompiledInstruction, n: usize, accounts: &[Pubkey]) -> Option<Pubkey> {
    let index = usize::from(*ix.accounts.get(n)?);
    accounts.get(index).copied()
}

fn system_op(ix: &CompiledInstruction, accounts: &[Pubkey]) -> Option<Op> {
    let tag = le_u32(&ix.data, 0)?;
    let acct = |n| account(ix, n, accounts);
    Some(match tag {
        0 => Op::CreateAccount {
            from: acct(0)?,
            new_account: acct(1)?,
            lamports: le_u64(&ix.data, 4)?,
            space: le_u64(&ix.data, 12)?,
            owner: key_at(&ix.data, 20)?,
        },
        1 => Op::Assign {
            account: acct(0)?,
            owner: key_at(&ix.data, 4)?,
        },
        2 => Op::SolTransfer {
            from: acct(0)?,
            to: acct(1)?,
            lamports: le_u64(&ix.data, 4)?,
        },
        // The base account signs for a derived source; the destination is the third account.
        11 => Op::SolTransfer {
            from: acct(0)?,
            to: acct(2)?,
            lamports: le_u64(&ix.data, 4)?,
        },
        3 => system_other("CreateAccountWithSeed"),
        4 => system_other("AdvanceNonceAccount"),
        5 => system_other("WithdrawNonceAccount"),
        6 => system_other("InitializeNonceAccount"),
        7 => system_other("AuthorizeNonceAccount"),
        8 => system_other("Allocate"),
        9 => system_other("AllocateWithSeed"),
        10 => Op::Assign {
            account: acct(0)?,
            owner: address::id(address::SYSTEM_PROGRAM),
        },
        12 => system_other("UpgradeNonceAccount"),
        _ => system_other("an unknown action"),
    })
}

fn system_other(action: &str) -> Op {
    Op::Other {
        program: "System Program",
        action: action.to_string(),
    }
}

fn token_op(ix: &CompiledInstruction, accounts: &[Pubkey]) -> Option<Op> {
    let tag = *ix.data.first()?;
    let acct = |n| account(ix, n, accounts);
    Some(match tag {
        3 => Op::TokenTransfer {
            source: acct(0)?,
            destination: acct(1)?,
            authority: acct(2)?,
            amount: le_u64(&ix.data, 1)?,
            mint: None,
            decimals: None,
        },
        12 => Op::TokenTransfer {
            source: acct(0)?,
            mint: Some(acct(1)?),
            destination: acct(2)?,
            authority: acct(3)?,
            amount: le_u64(&ix.data, 1)?,
            decimals: ix.data.get(9).copied(),
        },
        4 => Op::TokenApprove {
            source: acct(0)?,
            delegate: acct(1)?,
            authority: acct(2)?,
            amount: le_u64(&ix.data, 1)?,
        },
        13 => Op::TokenApprove {
            source: acct(0)?,
            delegate: acct(2)?,
            authority: acct(3)?,
            amount: le_u64(&ix.data, 1)?,
        },
        5 => Op::TokenRevoke { source: acct(0)? },
        6 => Op::TokenSetAuthority {
            account: acct(0)?,
            kind: *ix.data.get(1)?,
            new_authority: match ix.data.get(2)? {
                1 => Some(key_at(&ix.data, 3)?),
                _ => None,
            },
        },
        9 => Op::TokenClose {
            account: acct(0)?,
            destination: acct(1)?,
        },
        8 | 15 => Op::TokenBurn {
            account: acct(0)?,
            amount: le_u64(&ix.data, 1)?,
        },
        7 | 14 => token_other("MintTo"),
        17 => token_other("SyncNative"),
        1 | 16 | 18 | 0 => token_other("account initialisation"),
        10 => token_other("FreezeAccount"),
        11 => token_other("ThawAccount"),
        _ => token_other("an action this wallet does not decode"),
    })
}

fn token_other(action: &str) -> Op {
    Op::Other {
        program: "Token Program",
        action: action.to_string(),
    }
}

fn associated_op(ix: &CompiledInstruction, accounts: &[Pubkey]) -> Option<Op> {
    let tag = ix.data.first().copied().unwrap_or(0);
    let acct = |n| account(ix, n, accounts);
    match tag {
        0 | 1 => Some(Op::CreateAssociatedToken {
            payer: acct(0)?,
            associated: acct(1)?,
            owner: acct(2)?,
            mint: acct(3)?,
            idempotent: tag == 1,
        }),
        _ => Some(Op::Other {
            program: "Associated Token Account Program",
            action: "RecoverNested".to_string(),
        }),
    }
}

fn compute_op(ix: &CompiledInstruction) -> Op {
    match ix.data.first() {
        Some(2) => le_u32(&ix.data, 1).map(Op::ComputeLimit),
        Some(3) => le_u64(&ix.data, 1).map(Op::ComputePrice),
        _ => None,
    }
    .unwrap_or_else(|| Op::Other {
        program: "Compute Budget Program",
        action: "a compute setting".to_string(),
    })
}

/// Classifies every instruction. `accounts` is the message's account list in index order; when
/// lookup tables are not resolved it is only the static keys.
pub fn classify(message: &Message, accounts: &[Pubkey]) -> Vec<Op> {
    message
        .instructions
        .iter()
        .map(|ix| classify_instruction(message, ix, accounts))
        .collect()
}

fn classify_instruction(message: &Message, ix: &CompiledInstruction, accounts: &[Pubkey]) -> Op {
    let Some(program) = message.program_id(ix, accounts) else {
        return Op::Unresolved;
    };
    let named = |id: &str| program == address::id(id);
    let decoded = if named(address::SYSTEM_PROGRAM) {
        system_op(ix, accounts)
    } else if named(address::TOKEN_PROGRAM) || named(address::TOKEN_2022_PROGRAM) {
        token_op(ix, accounts)
    } else if named(address::ASSOCIATED_TOKEN_PROGRAM) {
        associated_op(ix, accounts)
    } else if named(address::COMPUTE_BUDGET_PROGRAM) {
        return compute_op(ix);
    } else if named(address::MEMO_PROGRAM) || named(address::MEMO_PROGRAM_V1) {
        return Op::Memo(
            std::str::from_utf8(&ix.data)
                .map(|t| sanitize_text_max(t, 200))
                .unwrap_or_else(|_| "(binary data)".to_string()),
        );
    } else {
        return Op::Unknown { program };
    };
    match decoded {
        Some(op) => op,
        None if ix
            .accounts
            .iter()
            .any(|a| usize::from(*a) >= accounts.len()) =>
        {
            Op::Unresolved
        }
        None => Op::Other {
            program: "a known program",
            action: "a malformed instruction".to_string(),
        },
    }
}

/// Instructions no swap needs and that could hand control of the wallet's funds to someone else.
pub fn forbidden_in_swap(ops: &[Op], owner: &Pubkey) -> Vec<String> {
    let mut reasons = vec![];
    for op in ops {
        match op {
            Op::TokenApprove { delegate, .. } => reasons.push(format!(
                "it lets {} spend tokens from a token account",
                address::to_text(delegate)
            )),
            Op::TokenSetAuthority { .. } => {
                reasons.push("it changes who controls an account".to_string())
            }
            Op::Assign { account, .. } if account == owner => {
                reasons.push("it assigns this wallet's own account to a program".to_string())
            }
            _ => {}
        }
    }
    reasons
}

fn who(key: &Pubkey, owner: &Pubkey) -> String {
    if key == owner {
        "this wallet".to_string()
    } else {
        address::to_text(key)
    }
}

fn token_amount(amount: u64, mint: Option<&Pubkey>, decimals: Option<u8>) -> String {
    match (mint, decimals) {
        (Some(m), Some(d)) => format!(
            "{} {}",
            tokens::format_amount(u128::from(amount), u32::from(d)),
            tokens::token_label(m)
        ),
        _ => format!("{amount} base units of a token"),
    }
}

/// The action name, plain-language description and any warnings for one operation.
pub fn describe_op(op: &Op, owner: &Pubkey) -> (&'static str, String, Vec<String>) {
    match op {
        Op::SolTransfer { from, to, lamports } => (
            "transfer",
            format!(
                "Sends {} SOL from {} to {}",
                tokens::format_sol(*lamports),
                who(from, owner),
                who(to, owner)
            ),
            vec![],
        ),
        Op::CreateAccount {
            from,
            new_account,
            lamports,
            space,
            owner: program,
        } => (
            "create_account",
            format!(
                "Creates account {} ({space} bytes, owned by program {}) funded with {} SOL from {}",
                who(new_account, owner),
                address::to_text(program),
                tokens::format_sol(*lamports),
                who(from, owner)
            ),
            vec![],
        ),
        Op::Assign {
            account,
            owner: program,
        } => {
            let mut warnings = vec![];
            if account == owner {
                warnings.push("this hands this wallet's own account to another program, which can make its funds unspendable".to_string());
            }
            (
                "assign",
                format!(
                    "Assigns {} to program {}",
                    who(account, owner),
                    address::to_text(program)
                ),
                warnings,
            )
        }
        Op::TokenTransfer {
            source,
            destination,
            authority,
            amount,
            mint,
            decimals,
        } => (
            "token_transfer",
            format!(
                "Moves {} from token account {} to token account {} (authorised by {})",
                token_amount(*amount, mint.as_ref(), *decimals),
                who(source, owner),
                who(destination, owner),
                who(authority, owner)
            ),
            vec![],
        ),
        Op::TokenApprove {
            source,
            delegate,
            authority,
            amount,
        } => {
            let unlimited = *amount == u64::MAX;
            let mut warnings = vec![format!(
                "this lets {} spend {}tokens from token account {} at any time until the approval is revoked",
                address::to_text(delegate),
                if unlimited {
                    "an UNLIMITED amount of "
                } else {
                    ""
                },
                who(source, owner)
            )];
            if unlimited {
                warnings.push("the approved amount is the maximum possible".to_string());
            }
            (
                "token_approve",
                format!(
                    "Lets {} spend {} base units from token account {} (authorised by {})",
                    address::to_text(delegate),
                    if unlimited {
                        "an UNLIMITED number of".to_string()
                    } else {
                        amount.to_string()
                    },
                    who(source, owner),
                    who(authority, owner)
                ),
                warnings,
            )
        }
        Op::TokenRevoke { source } => (
            "token_revoke",
            format!(
                "Removes the spending approval on token account {}",
                who(source, owner)
            ),
            vec![],
        ),
        Op::TokenSetAuthority {
            account,
            kind,
            new_authority,
        } => {
            let what = match kind {
                0 => "mint authority",
                1 => "freeze authority",
                2 => "owner",
                3 => "close authority",
                _ => "authority",
            };
            let to = match new_authority {
                Some(k) => who(k, owner),
                None => "nobody (removed)".to_string(),
            };
            (
                "set_authority",
                format!("Changes the {what} of {} to {to}", who(account, owner)),
                vec![format!(
                    "this changes who controls account {}; only continue if you expect it",
                    who(account, owner)
                )],
            )
        }
        Op::TokenClose {
            account,
            destination,
        } => {
            let mut warnings = vec![];
            if destination != owner {
                warnings.push(format!(
                    "the closed account's remaining SOL goes to {}, not to this wallet",
                    who(destination, owner)
                ));
            }
            (
                "token_close",
                format!(
                    "Closes token account {} and sends its SOL deposit to {}",
                    who(account, owner),
                    who(destination, owner)
                ),
                warnings,
            )
        }
        Op::TokenBurn { account, amount } => (
            "token_burn",
            format!(
                "Destroys {amount} base units of tokens from token account {}",
                who(account, owner)
            ),
            vec!["burned tokens cannot be recovered".to_string()],
        ),
        Op::CreateAssociatedToken {
            payer,
            associated,
            owner: token_owner,
            mint,
            idempotent,
        } => (
            "create_token_account",
            format!(
                "Creates {}the {} token account {} for {}, paid by {}",
                if *idempotent { "(if missing) " } else { "" },
                tokens::token_label(mint),
                who(associated, owner),
                who(token_owner, owner),
                who(payer, owner)
            ),
            vec![],
        ),
        Op::ComputeLimit(units) => (
            "compute_limit",
            format!("Allows up to {units} compute units"),
            vec![],
        ),
        Op::ComputePrice(price) => (
            "priority_fee",
            format!("Pays a priority fee of {price} micro-lamports per compute unit"),
            vec![],
        ),
        Op::Memo(text) => ("memo", format!("Attaches the note \"{text}\""), vec![]),
        Op::Other { program, action } => ("other", format!("{program}: {action}"), vec![]),
        Op::Unknown { program } => (
            "unknown_program",
            format!(
                "Calls the program {}, which this wallet does not recognise",
                address::to_text(program)
            ),
            vec![format!(
                "program {} is not recognised; only the simulation shows what it does",
                address::to_text(program)
            )],
        ),
        Op::Unresolved => (
            "unresolved",
            "Uses accounts from an address lookup table that could not be read".to_string(),
            vec![],
        ),
    }
}

fn program_name(message: &Message, ix: &CompiledInstruction, accounts: &[Pubkey]) -> String {
    let Some(program) = message.program_id(ix, accounts) else {
        return "unresolved".to_string();
    };
    let names = [
        (address::SYSTEM_PROGRAM, "System Program"),
        (address::TOKEN_PROGRAM, "Token Program"),
        (address::TOKEN_2022_PROGRAM, "Token-2022 Program"),
        (
            address::ASSOCIATED_TOKEN_PROGRAM,
            "Associated Token Account Program",
        ),
        (address::COMPUTE_BUDGET_PROGRAM, "Compute Budget Program"),
        (address::MEMO_PROGRAM, "Memo Program"),
        (address::MEMO_PROGRAM_V1, "Memo Program"),
    ];
    names
        .iter()
        .find(|(id, _)| program == address::id(id))
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| address::to_text(&program))
}

pub struct Described {
    pub instructions: Vec<Value>,
    pub warnings: Vec<String>,
    pub ops: Vec<Op>,
}

/// Describes every instruction of a message. Warnings are collected in order and de-duplicated.
pub fn describe(message: &Message, owner: &Pubkey, accounts: &[Pubkey]) -> Described {
    let ops = classify(message, accounts);
    let mut warnings: Vec<String> = vec![];
    let mut instructions = vec![];
    for (index, (ix, op)) in message.instructions.iter().zip(&ops).enumerate() {
        let (action, text, notes) = describe_op(op, owner);
        for note in notes {
            if !warnings.contains(&note) {
                warnings.push(note);
            }
        }
        instructions.push(Value::object(vec![
            ("index", Value::int(index as i64)),
            (
                "program",
                Value::String(program_name(message, ix, accounts)),
            ),
            ("action", Value::str(action)),
            ("description", Value::String(text)),
        ]));
    }
    if accounts.len() < message.total_accounts() {
        warnings.push(
            "this transaction loads accounts from address lookup tables that could not be read, so some accounts cannot be named".to_string(),
        );
    }
    Described {
        instructions,
        warnings,
        ops,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solana::tx::{self, AccountMeta, Instruction, compile_legacy};

    fn key(n: u8) -> Pubkey {
        [n; 32]
    }

    fn build(owner: &Pubkey, ixs: &[Instruction]) -> Message {
        compile_legacy(owner, ixs, [0u8; 32]).unwrap()
    }

    fn ops_of(owner: &Pubkey, ixs: &[Instruction]) -> Vec<Op> {
        let m = build(owner, ixs);
        classify(&m, &m.account_keys)
    }

    fn token_ix(tag_and_data: Vec<u8>, accounts: Vec<AccountMeta>) -> Instruction {
        Instruction {
            program_id: address::id(address::TOKEN_PROGRAM),
            accounts,
            data: tag_and_data,
        }
    }

    #[test]
    fn decodes_transfers_and_token_transfers() {
        let owner = key(1);
        let ops = ops_of(
            &owner,
            &[
                tx::system_transfer(&owner, &key(2), 1_500_000_000),
                tx::token_transfer_checked(
                    &address::id(address::TOKEN_PROGRAM),
                    &key(3),
                    &address::id(tokens::USDC_MINT),
                    &key(4),
                    &owner,
                    2_500_000,
                    6,
                ),
            ],
        );
        assert_eq!(
            ops[0],
            Op::SolTransfer {
                from: owner,
                to: key(2),
                lamports: 1_500_000_000
            }
        );
        let (action, text, warnings) = describe_op(&ops[0], &owner);
        assert_eq!(action, "transfer");
        assert!(text.contains("1.5 SOL") && text.contains("this wallet"));
        assert!(warnings.is_empty());
        let (_, text, _) = describe_op(&ops[1], &owner);
        assert!(text.contains("2.5 USDC"), "{text}");
    }

    #[test]
    fn plain_token_transfers_show_base_units() {
        let owner = key(1);
        let mut data = vec![3u8];
        data.extend_from_slice(&77u64.to_le_bytes());
        let ops = ops_of(
            &owner,
            &[token_ix(
                data,
                vec![
                    AccountMeta::writable(key(3), false),
                    AccountMeta::writable(key(4), false),
                    AccountMeta::readonly(owner, true),
                ],
            )],
        );
        let (_, text, _) = describe_op(&ops[0], &owner);
        assert!(text.contains("77 base units"), "{text}");
    }

    #[test]
    fn approvals_are_flagged_and_unlimited_ones_loudly() {
        let owner = key(1);
        for (amount, loud) in [(500u64, false), (u64::MAX, true)] {
            let mut data = vec![4u8];
            data.extend_from_slice(&amount.to_le_bytes());
            let ops = ops_of(
                &owner,
                &[token_ix(
                    data,
                    vec![
                        AccountMeta::writable(key(3), false),
                        AccountMeta::readonly(key(9), false),
                        AccountMeta::readonly(owner, true),
                    ],
                )],
            );
            let (action, text, warnings) = describe_op(&ops[0], &owner);
            assert_eq!(action, "token_approve");
            assert!(!warnings.is_empty());
            assert_eq!(text.contains("UNLIMITED"), loud, "{text}");
            assert_eq!(warnings.iter().any(|w| w.contains("UNLIMITED")), loud);
            assert_eq!(forbidden_in_swap(&ops, &owner).len(), 1);
        }
    }

    #[test]
    fn authority_changes_assignments_and_closes_are_flagged() {
        let owner = key(1);
        let set_authority = token_ix(
            {
                let mut d = vec![6u8, 2, 1];
                d.extend_from_slice(&key(9));
                d
            },
            vec![
                AccountMeta::writable(key(3), false),
                AccountMeta::readonly(owner, true),
            ],
        );
        let ops = ops_of(&owner, &[set_authority]);
        let (_, text, warnings) = describe_op(&ops[0], &owner);
        assert!(text.contains("owner") && !warnings.is_empty());
        assert_eq!(forbidden_in_swap(&ops, &owner).len(), 1);

        let mut assign = 1u32.to_le_bytes().to_vec();
        assign.extend_from_slice(&key(8));
        let assign_ix = Instruction {
            program_id: address::id(address::SYSTEM_PROGRAM),
            accounts: vec![AccountMeta::writable(owner, true)],
            data: assign,
        };
        let ops = ops_of(&owner, &[assign_ix]);
        assert!(!describe_op(&ops[0], &owner).2.is_empty());
        assert_eq!(forbidden_in_swap(&ops, &owner).len(), 1);

        let close = token_ix(
            vec![9u8],
            vec![
                AccountMeta::writable(key(3), false),
                AccountMeta::writable(key(9), false),
                AccountMeta::readonly(owner, true),
            ],
        );
        let ops = ops_of(&owner, &[close]);
        let (_, _, warnings) = describe_op(&ops[0], &owner);
        assert_eq!(warnings.len(), 1);
        assert!(forbidden_in_swap(&ops, &owner).is_empty());
    }

    #[test]
    fn compute_budget_memo_and_associated_accounts() {
        let owner = key(1);
        let memo = Instruction {
            program_id: address::id(address::MEMO_PROGRAM),
            accounts: vec![],
            data: b"hello \x1b[31mred".to_vec(),
        };
        let ops = ops_of(
            &owner,
            &[
                tx::set_compute_unit_limit(300_000),
                tx::set_compute_unit_price(7),
                tx::create_associated_token_account_idempotent(
                    &owner,
                    &key(5),
                    &key(6),
                    &address::id(tokens::USDC_MINT),
                    &address::id(address::TOKEN_PROGRAM),
                ),
                memo,
            ],
        );
        assert_eq!(ops[0], Op::ComputeLimit(300_000));
        assert_eq!(ops[1], Op::ComputePrice(7));
        assert!(matches!(
            ops[2],
            Op::CreateAssociatedToken {
                idempotent: true,
                ..
            }
        ));
        let (action, text, _) = describe_op(&ops[2], &owner);
        assert_eq!(action, "create_token_account");
        assert!(text.contains("USDC") && !text.contains("  "), "{text}");
        let Op::Memo(note) = &ops[3] else { panic!() };
        assert!(!note.contains('\u{1b}'));
    }

    #[test]
    fn unknown_programs_are_flagged() {
        let owner = key(1);
        let ix = Instruction {
            program_id: key(99),
            accounts: vec![AccountMeta::writable(owner, true)],
            data: vec![1, 2, 3],
        };
        let m = build(&owner, &[ix]);
        let described = describe(&m, &owner, &m.account_keys);
        assert_eq!(described.ops, vec![Op::Unknown { program: key(99) }]);
        assert_eq!(described.warnings.len(), 1);
        assert_eq!(described.instructions.len(), 1);
    }

    #[test]
    fn malformed_and_unresolved_instructions_do_not_panic() {
        let owner = key(1);
        // a system transfer with truncated data
        let ix = Instruction {
            program_id: address::id(address::SYSTEM_PROGRAM),
            accounts: vec![
                AccountMeta::writable(owner, true),
                AccountMeta::writable(key(2), false),
            ],
            data: vec![2, 0, 0],
        };
        let m = build(&owner, &[ix]);
        let ops = classify(&m, &m.account_keys);
        assert!(matches!(ops[0], Op::Other { .. }));
        // fewer accounts than the message names (unresolved lookups)
        let m2 = build(&owner, &[tx::system_transfer(&owner, &key(2), 5)]);
        let ops = classify(&m2, &m2.account_keys[..1]);
        assert_eq!(ops[0], Op::Unresolved);
        let described = describe(&m2, &owner, &m2.account_keys[..1]);
        assert!(!described.warnings.is_empty());
    }

    #[test]
    fn every_known_system_action_is_named() {
        let owner = key(1);
        for tag in 0u32..=14 {
            let mut data = tag.to_le_bytes().to_vec();
            data.extend_from_slice(&[0u8; 60]);
            let ix = Instruction {
                program_id: address::id(address::SYSTEM_PROGRAM),
                accounts: vec![
                    AccountMeta::writable(owner, true),
                    AccountMeta::writable(key(2), false),
                    AccountMeta::writable(key(3), false),
                ],
                data,
            };
            let ops = ops_of(&owner, &[ix]);
            let (_, text, _) = describe_op(&ops[0], &owner);
            assert!(!text.is_empty(), "tag {tag}");
        }
    }
}
